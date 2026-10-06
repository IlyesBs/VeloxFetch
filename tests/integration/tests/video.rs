//! Videos from the browser's "Download video" bar and video pages (YouTube...) read through yt-dlp,
//! through the real manager. yt-dlp is replaced by a stub (`fake-ytdlp`), ffmpeg by `fake-ffmpeg`, so
//! nothing here needs those programs or the internet.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use download_manager::{
    BrowserContext, ConfirmPending, DownloadManager, ManagerConfig, ManagerError, MediaJobDto, MediaRequest, MediaStatus, PendingMedia,
};

struct Env {
    m: DownloadManager,
    root: PathBuf,
}

async fn env(name: &str, ytdlp: bool, ffmpeg: bool) -> Env {
    let base = std::env::temp_dir().join(format!("velox-video-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("downloads");
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), root.clone());
    c.ytdlp_path = Some(if ytdlp { PathBuf::from(env!("CARGO_BIN_EXE_fake-ytdlp")) } else { base.join("no-ytdlp-here") });
    c.ffmpeg_path = Some(if ffmpeg { PathBuf::from(env!("CARGO_BIN_EXE_fake-ffmpeg")) } else { base.join("no-ffmpeg-here") });
    Env { m: DownloadManager::new(c).await.unwrap(), root }
}

/// A video id no other test or earlier run uses (the stub's log is shared and kept).
fn uid(tag: &str) -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() % 1_000_000_000);
    format!("{tag}{}x{t}", std::process::id())
}

/// The stub's calls for one video id (each test uses its own id).
fn calls(id: &str) -> Vec<Vec<String>> {
    std::fs::read_to_string(std::env::temp_dir().join("velox-fake-ytdlp.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(id))
        .map(|l| l.split('\u{1f}').map(String::from).collect())
        .collect()
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

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn invalid(r: Result<impl std::fmt::Debug, ManagerError>) -> String {
    match r {
        Err(ManagerError::InvalidInput(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn browser() -> BrowserContext {
    BrowserContext {
        referrer: Some("https://www.youtube.com/".into()),
        cookies: Some("SID=secret-cookie-value".into()),
        user_agent: Some("Mozilla/5.0 Test".into()),
        incognito: false,
    }
}

#[tokio::test]
async fn a_video_page_is_read_and_downloaded_through_ytdlp() {
    let e = env("page", true, true).await;
    let id = uid("vidpage");
    let url = format!("https://www.youtube.com/watch?v={id}");
    let url = url.as_str();
    let a = e.m.analyze_media(url.into()).await.unwrap();
    let ids: Vec<&str> = a.analysis.streams.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["h1080", "h720", "h360", "audio"]);
    assert_eq!(a.analysis.title, "Fake clip_ test_1", "the title is made safe for a file name");
    assert!(listing(&e.root).is_empty(), "looking writes nothing");

    let job = e
        .m
        .start_media(MediaRequest { url: url.into(), stream: Some("h720".into()), filename: Some("My clip".into()), ..Default::default() })
        .await
        .unwrap();
    assert!(job.site);
    let done = finish(&e.m, job.id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert_eq!(done.percent, Some(100.0));
    assert_eq!((done.segments_done, done.segments_total), (0, 0), "a page has no pieces to count");
    assert_eq!(done.files, ["My clip.mp4"]);
    let body = std::fs::read_to_string(e.root.join("My clip.mp4")).unwrap();
    assert!(body.starts_with("FAKEVIDEO bv*[height<=720]+ba"), "{body}");
    assert_eq!(listing(&e.root), ["My clip.mp4"], "no working folder or part file is left behind");

    let runs = calls(&id);
    assert_eq!(runs.len(), 2, "one look, one download");
    for args in &runs {
        assert_eq!(args[0], "--ignore-config");
        assert_eq!(&args[args.len() - 2..], ["--", url]);
        assert!(args.iter().any(|a| a == "--no-exec"));
    }
    let dl = &runs[1];
    assert!(dl
        .windows(2)
        .any(|w| w[0] == "--ffmpeg-location" && w[1].ends_with(if cfg!(windows) { "fake-ffmpeg.exe" } else { "fake-ffmpeg" })));
    assert!(dl.windows(2).any(|w| w[0] == "--output" && w[1] == "media.%(ext)s"));
}

#[tokio::test]
async fn the_user_chooses_the_file_type_mp4_by_default() {
    let e = env("types", true, true).await;
    let id = uid("vidtypes");
    let url = format!("https://www.youtube.com/watch?v={id}");
    let start = |stream: &str, format: Option<&str>, name: &str| MediaRequest {
        url: url.clone(),
        stream: Some(stream.into()),
        format: format.map(String::from),
        filename: Some(name.into()),
        ..Default::default()
    };
    let pair = |args: &[String], k: &str, v: &str| args.windows(2).any(|w| w[0] == k && w[1] == v);

    // [request, expected file, a pair the download must have been asked with]
    let cases = [
        (start("h1080", None, "default"), "default.mp4", ("--merge-output-format", "mp4")),
        (start("h720", Some("mp4"), "as mp4"), "as mp4.mp4", ("--remux-video", "mp4")),
        (start("h720", Some("mkv"), "as mkv"), "as mkv.mkv", ("--remux-video", "mkv")),
        (start("h720", Some("webm"), "as webm"), "as webm.webm", ("--recode-video", "webm")),
        (start("audio", None, "song"), "song.mp3", ("--audio-format", "mp3")),
        (start("audio", Some("m4a"), "song2"), "song2.m4a", ("--audio-format", "m4a")),
        (start("h720", Some("opus"), "song3"), "song3.opus", ("--audio-format", "opus")),
    ];
    for (req, file, (k, v)) in cases {
        let before = calls(&id).len();
        let job = e.m.start_media(req).await.unwrap();
        let done = finish(&e.m, job.id).await;
        assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
        assert_eq!(done.files, [file]);
        assert!(done.notes.is_empty(), "{:?}", done.notes);
        let runs = calls(&id);
        let dl = runs[before..].iter().find(|a| a.iter().any(|x| x == "--paths")).expect("a download run");
        assert!(pair(dl, k, v), "{file}: {dl:?}");
        assert!(!dl.iter().any(|a| a.contains("mkv")) || file.ends_with(".mkv") || file.ends_with(".webm"), "{file}: {dl:?}");
    }
    // Anything outside the list is refused before anything runs.
    let before = calls(&id).len();
    for bad in ["exe", "avi", "mp4 --exec x", "../mp4"] {
        assert!(invalid(e.m.start_media(start("h720", Some(bad), "x")).await).contains("file type"), "{bad}");
    }
    assert_eq!(calls(&id).len(), before);
}

#[tokio::test]
async fn without_ffmpeg_only_single_file_qualities_are_offered() {
    let e = env("noffmpeg", true, false).await;
    let id = uid("vidnoff");
    let url = format!("https://youtu.be/{id}");
    let url = url.as_str();
    let a = e.m.analyze_media(url.into()).await.unwrap();
    let ids: Vec<&str> = a.analysis.streams.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["h360", "audio"]);
    let job = e
        .m
        .start_media(MediaRequest { url: url.into(), stream: Some("h360".into()), filename: Some("small".into()), ..Default::default() })
        .await
        .unwrap();
    let done = finish(&e.m, job.id).await;
    assert_eq!(done.status, MediaStatus::Completed);
    assert!(std::fs::read_to_string(e.root.join("small.mp4")).unwrap().starts_with("FAKEVIDEO b[height<=360]"));
    assert!(done.notes.iter().any(|n| n.contains("required dependencies") && !n.contains("ffmpeg")), "no program name: {:?}", done.notes);
    assert!(!calls(&id)[1].iter().any(|a| a == "--ffmpeg-location"));
}

#[tokio::test]
async fn a_video_from_the_browser_waits_for_the_dialog_and_never_hands_cookies_to_ytdlp() {
    let e = env("pending", true, true).await;
    let mut rx = e.m.subscribe();
    let id = uid("vidpend");
    let url = format!("https://www.youtube.com/watch?v={id}");
    let url = url.as_str();
    let p = e.m.add_pending_media(url, browser(), true).await.unwrap();
    assert_eq!(p.media, Some(PendingMedia::Site));
    assert!(!p.probing, "nothing is fetched before the user picks a quality");
    assert!(e.m.list_pending().iter().any(|x| x.token == p.token));
    assert!(matches!(rx.try_recv(), Ok(download_manager::ManagerEvent::Pending { .. })));

    // It is a video: the file confirmation does not apply.
    let m = invalid(e.m.confirm_pending(ConfirmPending { token: p.token, filename: None, start: None, folder_token: None }).await);
    assert!(m.contains("video"), "{m}");

    // The dialog looks at it through the waiting request (the UI cannot swap the link).
    let a = e.m.analyze_media_for("https://evil.test/other".into(), Some(p.token)).await.unwrap();
    assert_eq!(a.analysis.streams[0].id, "h1080");
    let job =
        e.m.start_media(MediaRequest {
            url: "https://evil.test/other".into(),
            pending: Some(p.token),
            filename: Some("from browser".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(job.host, "www.youtube.com");
    assert!(e.m.list_pending().is_empty(), "answered: it leaves the waiting list");
    assert_eq!(finish(&e.m, job.id).await.status, MediaStatus::Completed);

    let runs = calls(&id);
    assert_eq!(runs.len(), 2);
    for args in &runs {
        assert!(args.windows(2).any(|w| w[0] == "--referer" && w[1] == "https://www.youtube.com/"));
        assert!(args.windows(2).any(|w| w[0] == "--user-agent" && w[1] == "Mozilla/5.0 Test"));
        assert!(!args.iter().any(|a| a.contains("secret-cookie-value") || a.starts_with("--cookies")), "cookies never reach yt-dlp");
    }
    assert!(!calls(&id).iter().flatten().any(|a| a.contains("evil.test")), "the link typed in the dialog is ignored");
    // A token that was answered (or never existed) is refused.
    assert!(e.m.analyze_media_for(String::new(), Some(p.token)).await.is_err());
    assert!(e.m.analyze_media_for(String::new(), Some(999)).await.is_err());
}

#[tokio::test]
async fn closing_the_dialog_answers_no_and_a_stream_from_the_browser_is_a_stream() {
    let e = env("dismiss", true, false).await;
    let p = e.m.add_pending_media("https://cdn.example.test/live/master.m3u8", BrowserContext::default(), false).await.unwrap();
    assert_eq!(p.media, Some(PendingMedia::Stream));
    e.m.dismiss_pending(p.token).unwrap();
    assert!(e.m.list_pending().is_empty());
    // "page" is only believed for sites VeloxFetch knows.
    let m = invalid(e.m.add_pending_media("https://example.test/watch?v=1", BrowserContext::default(), true).await);
    assert!(m.contains("video site"), "{m}");
    // Browser headers are checked like any browser hand-over.
    let bad = BrowserContext { cookies: Some("a=b\r\nX-Evil: 1".into()), ..Default::default() };
    assert!(e.m.add_pending_media("https://youtu.be/x", bad, true).await.is_err());
    let private = BrowserContext { cookies: Some("a=b".into()), incognito: true, ..Default::default() };
    assert!(e.m.add_pending_media("https://youtu.be/x", private, true).await.is_err());
}

#[tokio::test]
async fn drm_services_are_refused_with_a_clear_reason_and_nothing_runs() {
    let e = env("drm", true, true).await;
    for url in [
        "https://www.netflix.com/watch/81234567",
        "https://shahid.mbc.net/ar/player/episodes/x",
        "https://www.stctv.com/watch/1",
        "https://www.disneyplus.com/video/x",
    ] {
        let m = invalid(e.m.analyze_media(url.into()).await);
        assert!(m.contains("DRM"), "{url}: {m}");
        assert!(invalid(e.m.start_media(MediaRequest { url: url.into(), ..Default::default() }).await).contains("DRM"));
        assert!(invalid(e.m.add_pending_media(url, BrowserContext::default(), false).await).contains("DRM"));
    }
    assert!(e.m.list_media().is_empty() && e.m.list_pending().is_empty());
    // A video that turns out to be protected on an ordinary site: yt-dlp's own answer, in plain words.
    let m = invalid(e.m.analyze_media("https://vimeo.com/drm-vid01".into()).await);
    assert!(m.contains("DRM"), "{m}");
    let job =
        e.m.start_media(MediaRequest { url: "https://vimeo.com/drm-vid02".into(), filename: Some("x".into()), ..Default::default() })
            .await
            .unwrap();
    let done = finish(&e.m, job.id).await;
    assert_eq!(done.status, MediaStatus::Failed);
    assert!(done.error.unwrap().contains("DRM"));
    assert!(listing(&e.root).is_empty());
}

#[tokio::test]
async fn without_ytdlp_the_user_is_told_what_to_do() {
    let e = env("noytdlp", false, false).await;
    let m = invalid(e.m.analyze_media("https://www.youtube.com/watch?v=vidnone01".into()).await);
    // What to do (Settings > Required dependencies), never a program name, a download site or a path.
    assert!(m.contains("Required dependencies") && !m.contains("yt-dlp") && !m.contains("github"), "{m}");
    let m = invalid(e.m.start_media(MediaRequest { url: "https://youtu.be/vidnone02".into(), ..Default::default() }).await);
    assert!(m.contains("Required dependencies") && !m.contains("yt-dlp"), "{m}");
    assert!(e.m.list_media().is_empty());
}

#[tokio::test]
async fn cancelling_stops_ytdlp_and_cleans_up() {
    let e = env("cancel", true, true).await;
    let job =
        e.m.start_media(MediaRequest { url: "https://vimeo.com/slow-vid01".into(), filename: Some("long".into()), ..Default::default() })
            .await
            .unwrap();
    let t = Instant::now();
    while e.m.list_media()[0].status != MediaStatus::Downloading {
        assert!(t.elapsed() < Duration::from_secs(20), "it starts downloading");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    e.m.cancel_media(job.id).await.unwrap();
    assert_eq!(finish(&e.m, job.id).await.status, MediaStatus::Cancelled);
    let t = Instant::now();
    while !listing(&e.root).is_empty() && t.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(listing(&e.root).is_empty(), "the working folder is removed: {:?}", listing(&e.root));
}

async fn until(m: &DownloadManager, id: u64, what: &str, ok: impl Fn(&MediaJobDto) -> bool) -> MediaJobDto {
    let t = Instant::now();
    loop {
        let job = m.list_media().into_iter().find(|j| j.id == id).expect("the job is listed");
        if ok(&job) {
            return job;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "timed out waiting for {what}; job is {job:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn a_video_can_be_paused_resumed_watched_and_counted_while_it_downloads() {
    let e = env("pause", true, true).await;
    let id = uid("slowpause");
    let url = format!("https://www.youtube.com/watch?v={id}");
    let job = e.m.start_media(MediaRequest { url: url.clone(), filename: Some("long".into()), ..Default::default() }).await.unwrap();
    let running = until(&e.m, job.id, "downloading with a speed", |j| j.status == MediaStatus::Downloading && j.playable).await;
    assert!(!running.resumable);
    // The dashboard counts it like a download.
    let metrics = e.m.get_metrics().await.unwrap();
    assert!(metrics.active >= 1, "{metrics:?}");

    // Watch it while it downloads: only bytes already written, never more.
    let r = e.m.read_media_preview(job.id, Some("bytes=0-")).await;
    assert_eq!(r.status, 206, "{:?}", r.content_range);
    assert_eq!(r.content_type, "video/mp4");
    assert!(!r.body.is_empty() && r.body.len() as u64 <= 300 * 1024 - 64 * 1024, "{}", r.body.len());
    assert!(r.body.iter().all(|b| *b == 7));
    assert_eq!(e.m.read_media_preview(999, None).await.status, 404);

    // Pause: it stops, keeps its pieces, and can be resumed.
    e.m.pause_media(job.id).await.unwrap();
    let paused = until(&e.m, job.id, "paused", |j| j.status == MediaStatus::Paused).await;
    assert!(paused.resumable && paused.error.is_none(), "{paused:?}");
    let kept = listing(&e.root);
    assert!(kept.iter().any(|n| n.starts_with(".velox-media-")), "the pieces are kept while paused: {kept:?}");
    assert_eq!(e.m.get_metrics().await.unwrap().paused, 1);
    assert!(e.m.pause_media(job.id).await.is_err(), "a paused job cannot be paused again");

    // Resume: the same work folder (yt-dlp continues its .part files from it).
    let before = calls(&id).len();
    e.m.resume_media(job.id).await.unwrap();
    until(&e.m, job.id, "running again", |j| j.status == MediaStatus::Downloading).await;
    let runs = calls(&id);
    let paths = |a: &[String]| a.windows(2).find(|w| w[0] == "--paths").map(|w| w[1].clone());
    assert!(runs.len() > before);
    assert_eq!(paths(&runs[before]), paths(&runs[before - 1]), "the same work folder");
    assert!(runs[before].iter().any(|a| a == "--continue") && !runs[before].iter().any(|a| a == "--no-part"));

    // Cancel while paused or running: the pieces go.
    e.m.pause_media(job.id).await.unwrap();
    until(&e.m, job.id, "paused again", |j| j.status == MediaStatus::Paused).await;
    e.m.cancel_media(job.id).await.unwrap();
    assert!(!until(&e.m, job.id, "cancelled", |j| j.status == MediaStatus::Cancelled).await.resumable);
    assert!(listing(&e.root).is_empty(), "{:?}", listing(&e.root));
    assert!(e.m.resume_media(job.id).await.is_err(), "a cancelled job cannot be resumed");
}

#[tokio::test]
async fn a_video_that_fails_part_way_is_tried_again_by_itself_and_lands_in_the_history() {
    let e = env("flaky", true, true).await;
    let id = uid("flaky");
    let url = format!("https://www.youtube.com/watch?v={id}");
    let job = e.m.start_media(MediaRequest { url, filename: Some("again".into()), ..Default::default() }).await.unwrap();
    let t = Instant::now();
    let done = loop {
        let j = e.m.list_media().into_iter().find(|j| j.id == job.id).unwrap();
        if j.status == MediaStatus::Completed || j.status == MediaStatus::Failed {
            break j;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "{j:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(done.status, MediaStatus::Completed, "the second try works: {done:?}");
    assert!(done.error.is_none() && done.notes.is_empty(), "{done:?}");
    assert_eq!(done.files, ["again.mp4"]);
    assert!(done.playable);
    let downloads = calls(&id).into_iter().filter(|a| a.iter().any(|x| x == "--paths")).count();
    assert_eq!(downloads, 2, "one failure, one retry");
    assert_eq!(listing(&e.root), ["again.mp4"], "no work folder is left");
    // In the history, with its size; "Open" / "Show in folder" find the file.
    let h = e.m.list_history(10).await.unwrap();
    assert!(h.iter().any(|x| x.filename == "again.mp4" && x.url.contains(&id)), "{h:?}");
    let r = e.m.read_media_preview(job.id, Some("bytes=0-")).await;
    assert_eq!(r.status, 206);
    assert!(r.content_range.unwrap().ends_with(&format!("/{}", std::fs::metadata(e.root.join("again.mp4")).unwrap().len())));
}

#[tokio::test]
async fn a_failure_that_cannot_change_is_not_retried() {
    let e = env("noretry", true, true).await;
    let id = uid("drmonce");
    let job =
        e.m.start_media(MediaRequest { url: format!("https://vimeo.com/{id}"), filename: Some("x".into()), ..Default::default() })
            .await
            .unwrap();
    let done = finish(&e.m, job.id).await;
    assert_eq!(done.status, MediaStatus::Failed);
    assert!(!done.resumable, "DRM never offers Retry");
    assert_eq!(calls(&id).iter().filter(|a| a.iter().any(|x| x == "--paths")).count(), 1, "tried once");
}

/// On some computers Deno hangs (or fails) while yt-dlp itself works. The page is read again without any
/// JavaScript runtime, the rest of the session skips Deno, and the video downloads: nothing for the user to fix.
#[tokio::test]
async fn a_javascript_runtime_that_hangs_is_dropped_and_the_video_still_downloads() {
    let base = std::env::temp_dir().join(format!("velox-video-{}-jshang", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("downloads");
    let tools = base.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    // A Deno is installed (the fake yt-dlp never runs it: it hangs when told to use one).
    std::fs::write(tools.join(if cfg!(windows) { "deno.exe" } else { "deno" }), b"not run").unwrap();
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), root.clone());
    c.ytdlp_path = Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ytdlp")));
    c.ffmpeg_path = Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ffmpeg")));
    c.tools_dir = Some(tools);
    c.site_js_timeout = Duration::from_secs(2);
    let m = DownloadManager::new(c).await.unwrap();

    let id = uid("jshang");
    let url = format!("https://www.youtube.com/watch?v={id}");
    let t = Instant::now();
    let a = m.analyze_media(url.clone()).await.expect("read without the JavaScript runtime");
    assert!(!a.analysis.streams.is_empty());
    assert!(t.elapsed() < Duration::from_secs(20), "fell back quickly: {:?}", t.elapsed());
    let c1 = calls(&id);
    assert_eq!(c1.len(), 2, "{c1:?}");
    assert!(c1[0].iter().any(|x| x == "--js-runtimes"), "first with Deno");
    assert!(c1[1].iter().any(|x| x == "--no-js-runtimes") && !c1[1].iter().any(|x| x == "--js-runtimes"), "then without");

    // The rest of the session goes straight to what works: the next page and the download.
    m.analyze_media(url.clone()).await.unwrap();
    assert_eq!(calls(&id).len(), 3, "no second wait on Deno");
    let job = m
        .start_media(MediaRequest { url: url.clone(), stream: Some("h720".into()), filename: Some("clip".into()), ..Default::default() })
        .await
        .unwrap();
    let done = finish(&m, job.id).await;
    assert_eq!(done.status, MediaStatus::Completed, "{done:?}");
    assert!(calls(&id).last().unwrap().iter().any(|x| x == "--no-js-runtimes"), "the download skips Deno too");
}

/// Where Deno works, nothing changes: it is used, once, and never dropped.
#[tokio::test]
async fn a_working_javascript_runtime_is_kept() {
    let base = std::env::temp_dir().join(format!("velox-video-{}-jsok", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let tools = base.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(tools.join(if cfg!(windows) { "deno.exe" } else { "deno" }), b"not run").unwrap();
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.ytdlp_path = Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ytdlp")));
    c.ffmpeg_path = Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ffmpeg")));
    c.tools_dir = Some(tools);
    let m = DownloadManager::new(c).await.unwrap();
    let id = uid("jsok");
    let url = format!("https://www.youtube.com/watch?v={id}");
    m.analyze_media(url.clone()).await.unwrap();
    m.analyze_media(url).await.unwrap();
    let all = calls(&id);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|c| c.iter().any(|x| x == "--js-runtimes") && !c.iter().any(|x| x == "--no-js-runtimes")), "{all:?}");
}
