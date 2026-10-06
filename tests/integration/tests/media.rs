//! Phase 9: the media engine (HLS / DASH) through the real manager, against a local site that
//! serves playlists, segments (with Range support) and misbehaves on demand. ffmpeg is replaced by
//! a stub (`fake-ffmpeg`) so the tests run everywhere.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use download_manager::{BatchRequest, DownloadManager, ItemState, ListFilter, ManagerConfig, MediaJobDto, MediaRequest, MediaStatus};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use velox_integration_tests::random_data;

// ------------------------------------------------------------------------------------- the site

#[derive(Clone, Default)]
struct Route {
    body: Arc<Vec<u8>>,
    /// Answer 503 to this many requests first.
    fail_first: usize,
    delay_ms: u64,
}

struct Site {
    addr: SocketAddr,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Site {
    async fn start() -> Site {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let routes: Arc<Mutex<HashMap<String, Route>>> = Default::default();
        let hits: Arc<Mutex<HashMap<String, usize>>> = Default::default();
        let (r, h) = (routes.clone(), hits.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let (routes, hits) = (r.clone(), h.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 2048];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let path = head.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
                    let range = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_string));
                    let (route, n) = {
                        let route = routes.lock().unwrap().get(&path).cloned();
                        let mut h = hits.lock().unwrap();
                        let n = h.entry(path.clone()).or_insert(0);
                        *n += 1;
                        (route, *n)
                    };
                    let Some(route) = route else {
                        let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                        return;
                    };
                    if n <= route.fail_first {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        return;
                    }
                    if route.delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(route.delay_ms)).await;
                    }
                    let total = route.body.len();
                    let resp = match range.and_then(|r| {
                        let (s, e) = r.trim().split_once('-')?;
                        Some((s.parse::<usize>().ok()?, e.parse::<usize>().ok()?))
                    }) {
                        Some((s, e)) if s <= e && e < total => {
                            let mut v = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{e}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", e - s + 1).into_bytes();
                            v.extend_from_slice(&route.body[s..=e]);
                            v
                        }
                        _ => {
                            let mut v = format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n").into_bytes();
                            v.extend_from_slice(&route.body);
                            v
                        }
                    };
                    let _ = sock.write_all(&resp).await;
                });
            }
        });
        Site { addr, routes, hits }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn put(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.routes.lock().unwrap().insert(path.to_string(), Route { body: Arc::new(body.into()), ..Default::default() });
    }

    fn put_route(&self, path: &str, body: impl Into<Vec<u8>>, fail_first: usize, delay_ms: u64) {
        self.routes.lock().unwrap().insert(path.to_string(), Route { body: Arc::new(body.into()), fail_first, delay_ms });
    }

    fn hits(&self, path: &str) -> usize {
        *self.hits.lock().unwrap().get(path).unwrap_or(&0)
    }
}

// ------------------------------------------------------------------------------------- helpers

struct Env {
    m: DownloadManager,
    root: PathBuf,
}

/// The stub reads its behaviour from environment variables, which are per process: the tests that
/// use it take turns.
static FFMPEG_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_ffmpeg() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-ffmpeg"))
}

async fn env(name: &str, ffmpeg: Option<PathBuf>) -> Env {
    let base = std::env::temp_dir().join(format!("velox-media-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("downloads");
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), root.clone());
    // "No ffmpeg" must mean none, even on a machine that has one on its PATH.
    c.ffmpeg_path = Some(ffmpeg.unwrap_or_else(|| base.join("no-ffmpeg-here")));
    Env { m: DownloadManager::new(c).await.unwrap(), root }
}

fn req(url: String) -> MediaRequest {
    MediaRequest { url, ..Default::default() }
}

async fn finish(m: &DownloadManager, id: u64) -> MediaJobDto {
    let t = Instant::now();
    loop {
        let job = m.list_media().into_iter().find(|j| j.id == id).expect("the job is listed");
        if !job.status.is_active() {
            return job;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "timed out; job is {job:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn read(root: &Path, name: &str) -> Vec<u8> {
    std::fs::read(root.join(name)).unwrap_or_else(|e| panic!("{name}: {e} (folder has {:?})", listing(root)))
}

fn listing(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn seg(n: usize, seed: u64) -> Vec<u8> {
    random_data(n, seed).to_vec()
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn media_playlist(dir: &str, names: &[&str], extra: &str) -> String {
    let mut s = format!("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n{extra}");
    for n in names {
        s.push_str(&format!("#EXTINF:6.0,\n{dir}/{n}\n"));
    }
    s.push_str("#EXT-X-ENDLIST\n");
    s
}

/// A site with a master playlist (two qualities, one audio track, one subtitle track).
type Pieces = Vec<Vec<u8>>;

fn hls_site(site: &Site) -> (Pieces, Pieces, Pieces) {
    site.put(
        "/hls/master.m3u8",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"audio.m3u8\"\n\
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",URI=\"subs.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO=\"aud\"\nlow.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,AUDIO=\"aud\",SUBTITLES=\"subs\"\nhigh.m3u8\n",
    );
    let high: Vec<Vec<u8>> = (0..5).map(|i| seg(30_000 + i * 1000, 10 + i as u64)).collect();
    let low: Vec<Vec<u8>> = (0..3).map(|i| seg(10_000, 20 + i as u64)).collect();
    let audio: Vec<Vec<u8>> = (0..4).map(|i| seg(5_000, 30 + i as u64)).collect();
    let names: Vec<String> = (0..5).map(|i| format!("s{i}.ts")).collect();
    site.put("/hls/high.m3u8", media_playlist("high", &names.iter().map(String::as_str).collect::<Vec<_>>(), ""));
    for (i, b) in high.iter().enumerate() {
        site.put(&format!("/hls/high/s{i}.ts"), b.clone());
    }
    site.put("/hls/low.m3u8", media_playlist("low", &["s0.ts", "s1.ts", "s2.ts"], ""));
    for (i, b) in low.iter().enumerate() {
        site.put(&format!("/hls/low/s{i}.ts"), b.clone());
    }
    let anames: Vec<String> = (0..4).map(|i| format!("a{i}.aac")).collect();
    site.put("/hls/audio.m3u8", media_playlist("audio", &anames.iter().map(String::as_str).collect::<Vec<_>>(), ""));
    for (i, b) in audio.iter().enumerate() {
        site.put(&format!("/hls/audio/a{i}.aac"), b.clone());
    }
    site.put("/hls/subs.m3u8", media_playlist("subs", &["p0.vtt", "p1.vtt"], ""));
    site.put("/hls/subs/p0.vtt", "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000\n\n00:00.000 --> 00:02.000\nHello\n");
    site.put("/hls/subs/p1.vtt", "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000\n\n00:06.000 --> 00:08.000\nWorld\n");
    (high, low, audio)
}

// ---------------------------------------------------------------------------------- the tests

#[tokio::test]
async fn analysis_lists_qualities_audio_and_subtitles_without_writing_anything() {
    let site = Site::start().await;
    hls_site(&site);
    let e = env("analysis", None).await;
    let a = e.m.analyze_media(site.url("/hls/master.m3u8")).await.unwrap();
    assert_eq!(a.analysis.streams.len(), 2);
    assert_eq!(a.analysis.streams[0].height, Some(720), "best first");
    assert_eq!((a.analysis.audio.len(), a.analysis.subtitles.len()), (1, 1));
    assert!(!a.combine_available, "no ffmpeg here");
    assert_eq!(a.analysis.title, "master");
    assert!(listing(&e.root).is_empty(), "analysis writes nothing");
    assert_eq!(site.hits("/hls/high/s0.ts"), 0, "and downloads no segment");
    assert!(e.m.list_media().is_empty());
}

#[tokio::test]
async fn hls_without_ffmpeg_keeps_video_and_audio_as_separate_files() {
    let site = Site::start().await;
    let (high, _, audio) = hls_site(&site);
    let e = env("hls-plain", None).await;
    let job = e.m.start_media(req(site.url("/hls/master.m3u8"))).await.unwrap();
    let done = finish(&e.m, job.id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(done.files, ["master.ts", "master.audio.aac"]);
    assert_eq!(read(&e.root, "master.ts"), concat(&high), "the best quality, segments in order");
    assert_eq!(read(&e.root, "master.audio.aac"), concat(&audio));
    assert!(done.notes.iter().any(|n| n.contains("required dependencies") && !n.contains("ffmpeg")), "no program name: {:?}", done.notes);
    assert_eq!(done.segments_total, 9);
    assert_eq!(done.percent, Some(100.0));
    assert_eq!(listing(&e.root), ["master.audio.aac", "master.ts"], "no scratch folder left behind");
    assert!(e.m.list_downloads(ListFilter::default()).await.unwrap().is_empty(), "a stream job is not a row in the download list");
}

#[tokio::test]
async fn a_chosen_quality_and_no_audio() {
    let site = Site::start().await;
    let (_, low, _) = hls_site(&site);
    let e = env("hls-low", None).await;
    let r = MediaRequest {
        stream: Some("v1".into()),
        audio: Some("none".into()),
        filename: Some("Holiday.mp4".into()),
        subdir: Some("trips/2024".into()),
        ..req(site.url("/hls/master.m3u8"))
    };
    let done = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(done.files, ["Holiday.ts"], "the .mp4 the user typed is not doubled");
    assert_eq!(read(&e.root.join("trips").join("2024"), "Holiday.ts"), concat(&low));
}

#[tokio::test]
async fn removing_a_finished_job_can_delete_its_files_too() {
    let site = Site::start().await;
    hls_site(&site);
    let e = env("delete-files", None).await;
    let r = |name: &str| MediaRequest {
        stream: Some("v1".into()),
        audio: Some("none".into()),
        filename: Some(name.into()),
        ..req(site.url("/hls/master.m3u8"))
    };
    let kept = finish(&e.m, e.m.start_media(r("Kept.ts")).await.unwrap().id).await;
    let gone = finish(&e.m, e.m.start_media(r("Gone.ts")).await.unwrap().id).await;
    assert_eq!((kept.status, gone.status), (MediaStatus::Completed, MediaStatus::Completed));
    // Removed from the list only: the file stays.
    e.m.clear_media(kept.id, false).await.unwrap();
    assert!(e.root.join("Kept.ts").is_file(), "{:?}", listing(&e.root));
    // Removed and deleted: the file goes, the other one stays.
    e.m.clear_media(gone.id, true).await.unwrap();
    assert!(!e.root.join("Gone.ts").exists(), "{:?}", listing(&e.root));
    assert!(e.root.join("Kept.ts").is_file());
    assert!(e.m.list_media().is_empty());
}

#[tokio::test]
async fn subtitles_are_saved_next_to_the_video_with_one_header() {
    let site = Site::start().await;
    hls_site(&site);
    let e = env("hls-subs", None).await;
    let r = MediaRequest { audio: Some("none".into()), subtitles: vec!["s0".into()], ..req(site.url("/hls/master.m3u8")) };
    let done = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(done.files, ["master.ts", "master.en.vtt"]);
    let vtt = String::from_utf8(read(&e.root, "master.en.vtt")).unwrap();
    assert_eq!(vtt.matches("WEBVTT").count(), 1, "later pieces lose their header: {vtt}");
    assert!(vtt.contains("Hello") && vtt.contains("World"));
    assert!(vtt.find("Hello").unwrap() < vtt.find("World").unwrap());
}

#[tokio::test]
async fn ffmpeg_joins_video_and_audio_with_safe_arguments() {
    let _turn = FFMPEG_ENV.lock().await;
    let site = Site::start().await;
    let (high, _, audio) = hls_site(&site);
    let e = env("hls-mux", Some(fake_ffmpeg())).await;
    let log = e.root.parent().unwrap().join("ffmpeg-args.log");
    std::env::set_var("FAKE_FFMPEG_LOG", &log);
    assert!(e.m.analyze_media(site.url("/hls/master.m3u8")).await.unwrap().combine_available);
    let r = MediaRequest { filename: Some("a b; echo pwned $(id)".into()), ..req(site.url("/hls/master.m3u8")) };
    // Shell characters in a name are just characters: they are replaced in the saved name and never
    // reach a command (there is no shell, and the output goes to a scratch name anyway).
    let odd = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!(odd.status, MediaStatus::Completed, "{odd:?}");
    assert_eq!(odd.files, ["a b_ echo pwned _(id).mp4"]);
    let r = MediaRequest { filename: Some("Movie (2024)".into()), ..req(site.url("/hls/master.m3u8")) };
    let done = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(done.files, ["Movie (2024).mp4"]);
    let out = read(&e.root, "Movie (2024).mp4");
    assert_eq!(out, [b"FAKEMUX\n".to_vec(), concat(&high), concat(&audio)].concat(), "video then audio went in");
    assert_eq!(listing(&e.root), ["Movie (2024).mp4", "a b_ echo pwned _(id).mp4"], "the separate tracks and the scratch folder are gone");

    let log = std::fs::read_to_string(&log).unwrap();
    assert_eq!(log.lines().count(), 2, "one ffmpeg run per job");
    for line in log.lines() {
        assert!(!line.contains("pwned") && !line.contains("$("), "no user text reaches ffmpeg: {line}");
    }
    let args: Vec<&str> = log.lines().last().unwrap().split('\u{1f}').collect();
    let has = |seq: &[&str]| args.windows(seq.len()).any(|w| w == seq);
    assert!(has(&["-f", "mpegts", "-i"]) && has(&["-f", "aac", "-i"]), "input formats are forced: {args:?}");
    assert!(has(&["-protocol_whitelist", "file"]) && args.contains(&"-nostdin") && args.contains(&"-n"), "{args:?}");
    assert!(has(&["-c", "copy"]));
    for a in &args {
        assert!(!a.contains("http://"), "nothing from the network reaches a command: {a}");
    }
    let inputs: Vec<&&str> =
        args.iter().enumerate().filter(|(i, a)| *i > 0 && args[*i - 1] == "-i" && !a.is_empty()).map(|(_, a)| a).collect();
    assert!(inputs.iter().all(|p| Path::new(p).is_absolute()), "{inputs:?}");
    std::env::remove_var("FAKE_FFMPEG_LOG");
}

#[tokio::test]
async fn if_ffmpeg_fails_nothing_is_lost() {
    let _turn = FFMPEG_ENV.lock().await;
    let site = Site::start().await;
    let (high, _, audio) = hls_site(&site);
    let e = env("hls-mux-fail", Some(fake_ffmpeg())).await;
    std::env::set_var("FAKE_FFMPEG_FAIL", "1");
    let done = finish(&e.m, e.m.start_media(req(site.url("/hls/master.m3u8"))).await.unwrap().id).await;
    std::env::remove_var("FAKE_FFMPEG_FAIL");
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(done.files, ["master.ts", "master.audio.aac"]);
    assert_eq!(read(&e.root, "master.ts"), concat(&high));
    assert_eq!(read(&e.root, "master.audio.aac"), concat(&audio));
    assert!(done.notes.iter().any(|n| n.contains("could not be combined") && n.contains("Invalid data")), "{:?}", done.notes);
}

#[tokio::test]
async fn a_stream_is_saved_in_the_chosen_video_type() {
    let _turn = FFMPEG_ENV.lock().await;
    let site = Site::start().await;
    hls_site(&site);
    let e = env("hls-types", Some(fake_ffmpeg())).await;
    let log = e.root.parent().unwrap().join("ffmpeg-args.log");
    std::env::set_var("FAKE_FFMPEG_LOG", &log);
    let r = MediaRequest { format: Some("mkv".into()), filename: Some("film".into()), ..req(site.url("/hls/master.m3u8")) };
    let done = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    // Even a single track (no separate audio) is repackaged when another type is chosen.
    let r = MediaRequest {
        format: Some("mkv".into()),
        audio: Some("none".into()),
        filename: Some("solo".into()),
        ..req(site.url("/hls/master.m3u8"))
    };
    let solo = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    std::env::remove_var("FAKE_FFMPEG_LOG");
    assert_eq!(done.files, ["film.mkv"], "{done:?}");
    assert_eq!(solo.files, ["solo.mkv"], "{solo:?}");
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.lines().all(|l| l.split('\u{1f}').collect::<Vec<_>>().windows(2).any(|w| w == ["-f", "matroska"])), "{log}");
    // A stream is a video: an audio type, or anything unknown, is refused.
    for bad in ["mp3", "exe"] {
        let r = MediaRequest { format: Some(bad.into()), ..req(site.url("/hls/master.m3u8")) };
        assert!(matches!(e.m.start_media(r).await, Err(download_manager::ManagerError::InvalidInput(_))), "{bad}");
    }
}

#[tokio::test]
async fn combining_can_be_switched_off() {
    let site = Site::start().await;
    hls_site(&site);
    let e = env("hls-nocombine", Some(fake_ffmpeg())).await;
    let r = MediaRequest { combine: Some(false), ..req(site.url("/hls/master.m3u8")) };
    let done = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!(done.files, ["master.ts", "master.audio.aac"]);
}

#[tokio::test]
async fn fragmented_mp4_with_byte_ranges() {
    let site = Site::start().await;
    let file = seg(900, 77);
    site.put("/f/media.mp4", file.clone());
    site.put(
        "/f/media.m3u8",
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:4\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"100@0\"\n\
#EXTINF:4,\n#EXT-X-BYTERANGE:300@100\nmedia.mp4\n#EXTINF:4,\n#EXT-X-BYTERANGE:300\nmedia.mp4\n#EXTINF:4,\n#EXT-X-BYTERANGE:200\nmedia.mp4\n#EXT-X-ENDLIST\n",
    );
    let e = env("fmp4", None).await;
    let done = finish(&e.m, e.m.start_media(req(site.url("/f/media.m3u8"))).await.unwrap().id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(done.files, ["media.mp4"]);
    assert_eq!(read(&e.root, "media.mp4"), file, "init + three ranges = the file");
}

#[tokio::test]
async fn dash_video_audio_and_subtitles() {
    let site = Site::start().await;
    let v_init = seg(400, 1);
    let v: Vec<Vec<u8>> = (0..3).map(|i| seg(8_000, 40 + i)).collect();
    let a_init = seg(200, 2);
    let a: Vec<Vec<u8>> = (0..3).map(|i| seg(3_000, 50 + i)).collect();
    site.put(
        "/dash/manifest.mpd",
        r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT30S">
 <Period>
  <AdaptationSet contentType="video" mimeType="video/mp4">
   <SegmentTemplate timescale="1000" duration="10000" initialization="v/$RepresentationID$/init.mp4" media="v/$RepresentationID$/$Number$.m4s"/>
   <Representation id="hd" bandwidth="3000000" width="1920" height="1080"/>
   <Representation id="sd" bandwidth="600000" width="640" height="360"/>
  </AdaptationSet>
  <AdaptationSet contentType="audio" mimeType="audio/mp4" lang="en">
   <SegmentTemplate timescale="1000" duration="10000" initialization="a/init.mp4" media="a/$Number$.m4s"/>
   <Representation id="aud" bandwidth="128000"/>
  </AdaptationSet>
  <AdaptationSet contentType="text" mimeType="text/vtt" lang="fr"><Representation id="fr" bandwidth="1"><BaseURL>t/fr.vtt</BaseURL></Representation></AdaptationSet>
 </Period>
</MPD>"#,
    );
    site.put("/dash/v/hd/init.mp4", v_init.clone());
    for (i, b) in v.iter().enumerate() {
        site.put(&format!("/dash/v/hd/{}.m4s", i + 1), b.clone());
    }
    site.put("/dash/a/init.mp4", a_init.clone());
    for (i, b) in a.iter().enumerate() {
        site.put(&format!("/dash/a/{}.m4s", i + 1), b.clone());
    }
    site.put("/dash/t/fr.vtt", "WEBVTT\n\n00:00.000 --> 00:01.000\nBonjour\n");
    let e = env("dash", None).await;
    let an = e.m.analyze_media(site.url("/dash/manifest.mpd")).await.unwrap();
    assert_eq!(an.analysis.duration_secs, Some(30.0));
    assert_eq!(an.analysis.streams[0].label, "1080p · 3.0 Mbit/s");
    let r = MediaRequest { subtitles: vec!["s0".into()], ..req(site.url("/dash/manifest.mpd")) };
    let done = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(done.files, ["manifest.mp4", "manifest.audio.m4a", "manifest.fr.vtt"]);
    assert_eq!(read(&e.root, "manifest.mp4"), [v_init, concat(&v)].concat(), "init segment first");
    assert_eq!(read(&e.root, "manifest.audio.m4a"), [a_init, concat(&a)].concat());
    assert!(String::from_utf8(read(&e.root, "manifest.fr.vtt")).unwrap().contains("Bonjour"));
}

#[tokio::test]
async fn transient_errors_are_retried_and_persistent_ones_fail_the_job() {
    let site = Site::start().await;
    hls_site(&site);
    // Two 503s then success: within the three tries.
    site.put_route("/hls/high/s2.ts", seg(32_000, 12), 2, 0);
    let e = env("retry", None).await;
    let r = MediaRequest { audio: Some("none".into()), ..req(site.url("/hls/master.m3u8")) };
    let done = finish(&e.m, e.m.start_media(r.clone()).await.unwrap().id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(site.hits("/hls/high/s2.ts"), 3);

    // Always failing: the job tries again by itself, then fails and says why. Its pieces are kept for Retry.
    site.put_route("/hls/high/s2.ts", seg(32_000, 12), 100, 0);
    let before = listing(&e.root);
    let s0 = site.hits("/hls/high/s0.ts");
    let failed = finish(&e.m, e.m.start_media(r.clone()).await.unwrap().id).await;
    assert_eq!(failed.status, MediaStatus::Failed);
    assert!(failed.error.as_deref().is_some_and(|m| m.contains("503")), "{:?}", failed.error);
    assert!(failed.resumable, "a network failure offers Retry");
    assert_eq!(site.hits("/hls/high/s0.ts"), s0 + 1, "the pieces already written are not fetched again by the automatic tries");
    assert!(listing(&e.root).iter().any(|n| n.starts_with(".velox-media-")), "the pieces are kept: {:?}", listing(&e.root));

    // Retry once the server is fine again: it continues after the pieces it has.
    site.put_route("/hls/high/s2.ts", seg(32_000, 12), 0, 0);
    e.m.resume_media(failed.id).await.unwrap();
    let done = finish(&e.m, failed.id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(site.hits("/hls/high/s0.ts"), s0 + 1, "Retry continued, it did not start over");
    assert_eq!(done.files, ["master (1).ts"]);

    // A failed job removed from the list takes its pieces with it.
    site.put_route("/hls/high/s2.ts", seg(32_000, 12), 100, 0);
    let failed = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!(failed.status, MediaStatus::Failed);
    e.m.clear_media(failed.id, false).await.unwrap();
    let mut expected = before.clone();
    expected.push("master (1).ts".into());
    expected.sort();
    assert_eq!(listing(&e.root), expected, "no partial files and no work folder are left");
}

#[tokio::test]
async fn a_running_job_can_be_cancelled_and_cleans_up() {
    let site = Site::start().await;
    hls_site(&site);
    for i in 0..5 {
        site.put_route(&format!("/hls/high/s{i}.ts"), seg(20_000, i), 0, 30_000);
    }
    let e = env("cancel", None).await;
    let r = MediaRequest { audio: Some("none".into()), ..req(site.url("/hls/master.m3u8")) };
    let job = e.m.start_media(r).await.unwrap();
    let t = Instant::now();
    while site.hits("/hls/high/s0.ts") == 0 {
        assert!(t.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    e.m.cancel_media(job.id).await.unwrap();
    let start = Instant::now();
    let done = finish(&e.m, job.id).await;
    assert_eq!(done.status, MediaStatus::Cancelled, "{done:?}");
    // The server holds its answer back for 30 s: Cancel must not wait for it (it used to, up to the request timeout).
    assert!(start.elapsed() < Duration::from_secs(3), "cancel waited for the slow server: {:?}", start.elapsed());
    assert!(listing(&e.root).is_empty(), "{:?}", listing(&e.root));
    // A finished job can be removed from the list; an active one cannot.
    e.m.clear_media(job.id, false).await.unwrap();
    assert!(e.m.list_media().is_empty());
}

#[tokio::test]
async fn at_most_three_stream_jobs_run_at_once() {
    let site = Site::start().await;
    hls_site(&site);
    for i in 0..5 {
        site.put_route(&format!("/hls/high/s{i}.ts"), seg(20_000, i), 0, 1500);
    }
    let e = env("limit", None).await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(e.m.start_media(MediaRequest { audio: Some("none".into()), ..req(site.url("/hls/master.m3u8")) }).await.unwrap().id);
    }
    let err = e.m.start_media(req(site.url("/hls/master.m3u8"))).await.unwrap_err().to_string();
    assert!(err.contains("at most 3"), "{err}");
    for id in ids {
        e.m.cancel_media(id).await.unwrap();
        finish(&e.m, id).await;
    }
    assert!(e.m.start_media(req(site.url("/hls/master.m3u8"))).await.is_ok(), "slots free up again");
    e.m.shutdown().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!listing(&e.root).iter().any(|n| n.starts_with(".velox-media")), "shutdown stops jobs and removes scratch folders");
}

#[tokio::test]
async fn unsupported_and_hostile_streams_are_refused_with_a_reason() {
    let site = Site::start().await;
    site.put("/x/enc.m3u8", "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:5,\na.ts\n#EXT-X-ENDLIST\n");
    site.put("/x/live.m3u8", "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:5,\na.ts\n");
    site.put("/x/file.m3u8", "#EXTM3U\n#EXTINF:5,\nfile:///etc/passwd\n#EXT-X-ENDLIST\n");
    site.put("/x/creds.m3u8", "#EXTM3U\n#EXTINF:5,\nhttp://user:pw@127.0.0.1/a.ts\n#EXT-X-ENDLIST\n");
    site.put("/x/drm.mpd", r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4"><ContentProtection schemeIdUri="urn:uuid:x"/><Representation id="1" bandwidth="1"><BaseURL>a.mp4</BaseURL></Representation></AdaptationSet></Period></MPD>"#);
    site.put("/x/dtd.mpd", r#"<?xml version="1.0"?><!DOCTYPE MPD [<!ENTITY a "b">]><MPD type="static"><Period/></MPD>"#);
    site.put("/x/page.m3u8", "<html><body>Not a playlist</body></html>");
    site.put("/x/huge.m3u8", vec![b'#'; 5 * 1024 * 1024]);
    let e = env("refused", None).await;
    for (path, word) in [
        ("enc.m3u8", "encrypted"),
        ("live.m3u8", "live"),
        ("file.m3u8", "file"),
        ("creds.m3u8", "password"),
        ("drm.mpd", "DRM"),
        ("dtd.mpd", "DTD"),
        ("page.m3u8", "not"),
        ("huge.m3u8", "4 MiB"),
    ] {
        let err = e.m.analyze_media(site.url(&format!("/x/{path}"))).await.unwrap_err().to_string();
        assert!(err.to_lowercase().contains(&word.to_lowercase()), "{path}: {err}");
        // Starting one fails the job (not the app) with the same reason, and writes nothing.
        let done = finish(&e.m, e.m.start_media(req(site.url(&format!("/x/{path}")))).await.unwrap().id).await;
        assert_eq!(done.status, MediaStatus::Failed, "{path}");
        assert!(done.error.unwrap().to_lowercase().contains(&word.to_lowercase()), "{path}");
    }
    assert!(listing(&e.root).is_empty(), "nothing was written: {:?}", listing(&e.root));
    for bad in ["file:///etc/passwd", "ftp://h/x.m3u8", "javascript:alert(1)", "", "http://"] {
        assert!(e.m.analyze_media(bad.into()).await.is_err(), "{bad}");
    }
}

#[tokio::test]
async fn names_folders_and_ids_are_validated() {
    let site = Site::start().await;
    hls_site(&site);
    let e = env("validate", None).await;
    let u = site.url("/hls/master.m3u8");
    let bad = |r: MediaRequest| {
        let m = e.m.clone();
        async move { m.start_media(r).await.is_err() }
    };
    for sub in ["../escape", "..\\escape", "/abs", "C:\\x", "a/../../b", "con", "a:b"] {
        assert!(bad(MediaRequest { subdir: Some(sub.into()), ..req(u.clone()) }).await, "subdir {sub:?}");
    }
    for name in ["a/b", "a\\b", "..", "con", "a|b", "a\nb"] {
        assert!(bad(MediaRequest { filename: Some(name.into()), ..req(u.clone()) }).await, "filename {name:?}");
    }
    for id in ["", "v0; rm -rf", "../v0", "v0\u{0}", &"v".repeat(40)] {
        assert!(bad(MediaRequest { stream: Some(id.into()), ..req(u.clone()) }).await, "stream {id:?}");
        assert!(bad(MediaRequest { audio: Some(id.into()), ..req(u.clone()) }).await, "audio {id:?}");
    }
    assert!(bad(MediaRequest { subtitles: vec!["s0".into(); 51], ..req(u.clone()) }).await);
    // A well-formed id that does not exist fails the job, clearly.
    let done = finish(&e.m, e.m.start_media(MediaRequest { stream: Some("v9".into()), ..req(u.clone()) }).await.unwrap().id).await;
    assert_eq!(done.status, MediaStatus::Failed);
    assert!(done.error.unwrap().contains("unknown quality"));
    assert!(listing(&e.root).iter().all(|n| !n.contains("escape")), "nothing escaped: {:?}", listing(&e.root));
    assert!(!e.root.parent().unwrap().join("escape").exists());
}

#[tokio::test]
async fn saving_twice_never_overwrites() {
    let site = Site::start().await;
    hls_site(&site);
    let e = env("collide", None).await;
    let r = MediaRequest { audio: Some("none".into()), ..req(site.url("/hls/master.m3u8")) };
    let a = finish(&e.m, e.m.start_media(r.clone()).await.unwrap().id).await;
    let b = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!((a.files, b.files), (vec!["master.ts".to_string()], vec!["master (1).ts".to_string()]));
    assert_eq!(read(&e.root, "master.ts"), read(&e.root, "master (1).ts"));
}

#[tokio::test]
async fn stream_links_are_not_mistaken_for_files() {
    let e = env("routing", None).await;
    assert!(e.m.review_link("https://cdn.example.com/a/video.m3u8".into()).await.is_err());
    assert!(e.m.review_link("https://cdn.example.com/a/video.MPD?x=1".into()).await.is_err());
    let text = "https://cdn.example.com/a/video.m3u8\nhttps://cdn.example.com/a/file.zip\n";
    let p = e.m.preview_batch(BatchRequest { links: Some(text.into()), ..Default::default() }).await.unwrap();
    assert!(matches!(p.items[0].state, ItemState::Invalid { .. }), "{:?}", p.items[0]);
    assert!(matches!(p.items[1].state, ItemState::Ok));
    assert_eq!((p.ok, p.invalid), (1, 1));
    // Copied stream links are noticed by clipboard detection (the UI then opens Stream download).
    assert_eq!(
        download_manager::batch::detect_download_links("https://cdn.example.com/a/video.m3u8"),
        ["https://cdn.example.com/a/video.m3u8"]
    );
    assert!(download_manager::is_stream_link(&url::Url::parse("https://h/x.m3u8").unwrap()));
    assert!(!download_manager::is_stream_link(&url::Url::parse("https://h/x.zip").unwrap()));
}

// -------------------------------------------------------------------------- with a real ffmpeg

/// Runs only when `VELOX_TEST_FFMPEG` points at an ffmpeg (ffprobe next to it): makes real HLS
/// (MPEG-TS and fragmented MP4) and DASH streams with it, downloads them through the engine, lets
/// the same ffmpeg join them, and checks with ffprobe that the result is one playable file with a
/// video and an audio track. It is what proves the arguments we build are ones ffmpeg accepts.
#[tokio::test]
async fn real_ffmpeg_joins_real_streams() {
    let Some(ffmpeg) = std::env::var_os("VELOX_TEST_FFMPEG").map(PathBuf::from) else {
        eprintln!("SKIP real_ffmpeg_joins_real_streams: set VELOX_TEST_FFMPEG to an ffmpeg to run it");
        return;
    };
    let probe = ffmpeg.with_file_name(if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" });
    let e = env("real-ffmpeg", Some(ffmpeg.clone())).await;
    let fixtures = e.root.parent().unwrap().join("fixtures");

    let make = |sub: &str, extra: &[&str]| {
        let dir = fixtures.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        let mut args: Vec<&str> = "-hide_banner -loglevel error -f lavfi -i testsrc=size=320x240:rate=25 -f lavfi -i sine=frequency=440:sample_rate=44100 -t 6 -c:v libx264 -pix_fmt yuv420p -g 50 -c:a aac -map 0:v -map 1:a".split(' ').collect();
        args.extend_from_slice(extra);
        let out = std::process::Command::new(&ffmpeg).args(&args).current_dir(&dir).output().expect("run ffmpeg");
        assert!(out.status.success(), "ffmpeg could not make the {sub} fixture: {}", String::from_utf8_lossy(&out.stderr));
        dir
    };
    let hls_ts = make(
        "hls-ts",
        &[
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-var_stream_map",
            "v:0,agroup:aud a:0,agroup:aud,default:yes",
            "-master_pl_name",
            "master.m3u8",
            "%v.m3u8",
        ],
    );
    let hls_fmp4 = make(
        "hls-fmp4",
        &[
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_type",
            "fmp4",
            "-hls_fmp4_init_filename",
            "init_%v.mp4",
            "-hls_segment_filename",
            "seg_%v_%03d.m4s",
            "-var_stream_map",
            "v:0,agroup:aud a:0,agroup:aud,default:yes",
            "-master_pl_name",
            "master.m3u8",
            "%v.m3u8",
        ],
    );
    let dash = make(
        "dash",
        &[
            "-f",
            "dash",
            "-seg_duration",
            "2",
            "-use_template",
            "1",
            "-use_timeline",
            "1",
            "-adaptation_sets",
            "id=0,streams=v id=1,streams=a",
            "manifest.mpd",
        ],
    );

    let site = Site::start().await;
    for (prefix, dir) in [("hls-ts", &hls_ts), ("hls-fmp4", &hls_fmp4), ("dash", &dash)] {
        for f in std::fs::read_dir(dir).unwrap() {
            let f = f.unwrap();
            site.put(&format!("/{prefix}/{}", f.file_name().to_string_lossy()), std::fs::read(f.path()).unwrap());
        }
    }

    for (name, url, want_file) in [
        ("hls-ts", site.url("/hls-ts/master.m3u8"), "hls-ts.mp4"),
        ("hls-fmp4", site.url("/hls-fmp4/master.m3u8"), "hls-fmp4.mp4"),
        ("dash", site.url("/dash/manifest.mpd"), "dash.mp4"),
    ] {
        let r = MediaRequest { filename: Some(name.into()), ..req(url) };
        let done = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
        assert_eq!(done.status, MediaStatus::Completed, "{name}: {done:?}");
        assert_eq!(done.files, [want_file], "{name}: one joined file; notes: {:?}", done.notes);
        assert!(done.notes.is_empty(), "{name}: {:?}", done.notes);
        assert_eq!(listing(&e.root).iter().filter(|n| n.starts_with(name)).count(), 1, "{name}: no leftovers {:?}", listing(&e.root));

        let out = std::process::Command::new(&probe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name",
                "-show_entries",
                "format=duration",
                "-of",
                "default=nw=1",
            ])
            .arg(e.root.join(want_file))
            .output()
            .expect("run ffprobe");
        let info = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success() && info.contains("codec_type=video") && info.contains("codec_type=audio"),
            "{name}: not a playable video+audio file: {info} {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(info.contains("codec_name=h264") && info.contains("codec_name=aac"), "{name}: streams were copied, not changed: {info}");
        let secs: f64 = info.lines().find_map(|l| l.strip_prefix("duration=")).and_then(|d| d.parse().ok()).unwrap_or(0.0);
        assert!((5.5..=6.6).contains(&secs), "{name}: expected about 6 s, got {secs}: {info}");
    }

    // A video-only transport stream is turned into an ordinary .mp4 too.
    let r = MediaRequest { filename: Some("video-only".into()), audio: Some("none".into()), ..req(site.url("/hls-ts/master.m3u8")) };
    let done = finish(&e.m, e.m.start_media(r).await.unwrap().id).await;
    assert_eq!(done.files, ["video-only.mp4"], "{done:?}");
    let out = std::process::Command::new(&probe)
        .args(["-v", "error", "-show_entries", "stream=codec_type", "-of", "default=nw=1"])
        .arg(e.root.join("video-only.mp4"))
        .output()
        .unwrap();
    let info = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(info.contains("video") && !info.contains("audio"), "{info}");
    let _ = std::fs::remove_dir_all(&fixtures);
}
