//! The video tools (yt-dlp, Deno, ffmpeg) are fetched by VeloxFetch the first time a video page needs
//! them, from a local stand-in for their download servers: checked against the published checksum, put
//! in the data folder's `tools`, then used. A file that does not match its checksum is never installed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::{DownloadManager, ManagerConfig, ManagerError, MediaRequest, ToolSources};
use sha2::{Digest, Sha256};
use velox_integration_tests::{serve, Server};

fn hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// The release file names the manager asks for on this computer (same table as `manager/tools.rs`).
fn ytdlp_asset() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "yt-dlp.exe",
        ("windows", "aarch64") => "yt-dlp_arm64.exe",
        ("macos", _) => "yt-dlp_macos",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        _ => "yt-dlp_linux",
    }
}

fn deno_asset() -> String {
    let os = match std::env::consts::OS {
        "windows" => "pc-windows-msvc",
        "macos" => "apple-darwin",
        _ => "unknown-linux-gnu",
    };
    format!("deno-{}-{os}.zip", std::env::consts::ARCH)
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// A ZIP with stored (uncompressed) entries.
fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    }
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in files {
        let offset = out.len() as u32;
        let mut common = Vec::new();
        common.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // method stored, time, date
        common.extend_from_slice(&crc32(data).to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0]);
        out.extend_from_slice(&common);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0]);
        central.extend_from_slice(&common);
        central.extend_from_slice(&[0; 10]); // comment length, disk, internal and external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

fn ffmpeg_asset() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "ffmpeg-master-latest-winarm64-gpl.zip"
    } else {
        "ffmpeg-master-latest-win64-gpl.zip"
    }
}

/// A stand-in for the download servers. `good`: yt-dlp's checksum matches. `ffmpeg_main_good`: the main
/// ffmpeg source's checksum matches (else the fallback source is used). The main ffmpeg archive is the
/// server's ranged data (over 8 MiB: fetched in parallel parts); everything else is plain files.
async fn tool_server(good: bool, ffmpeg_main_good: bool) -> (Server, Vec<u8>, Vec<u8>) {
    let ytdlp = std::fs::read(env!("CARGO_BIN_EXE_fake-ytdlp")).unwrap();
    let ffmpeg = std::fs::read(env!("CARGO_BIN_EXE_fake-ffmpeg")).unwrap();
    let padding: Vec<u8> = (0..9 * 1024 * 1024u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    let main_zip =
        zip_of(&[("ffmpeg-master-latest-win64-gpl/doc/big.bin", &padding), ("ffmpeg-master-latest-win64-gpl/bin/ffmpeg.exe", &ffmpeg)]);
    let srv = serve(std::sync::Arc::new(main_zip.clone())).await;
    srv.put(
        "/ff/checksums.sha256",
        format!(
            "{}  ffmpeg-other.zip
{}  {}
",
            "1".repeat(64),
            if ffmpeg_main_good { hex(&main_zip) } else { hex(b"no") },
            ffmpeg_asset()
        ),
    );
    let listed = if good { hex(&ytdlp) } else { hex(b"something else") };
    srv.put(&format!("/yt/{}", ytdlp_asset()), ytdlp.clone());
    srv.put("/yt/SHA2-256SUMS", format!("{}  yt-dlp_other\n{listed}  {}\n", "0".repeat(64), ytdlp_asset()));
    let deno_zip = zip_of(&[(&exe("deno"), b"stand-in deno")]);
    srv.put("/deno/release-latest.txt", "v9.9.9\n");
    srv.put(&format!("/deno/release/v9.9.9/{}", deno_asset()), deno_zip.clone());
    srv.put(
        &format!("/deno/release/v9.9.9/{}.sha256sum", deno_asset()),
        format!("\r\nAlgorithm : SHA256\r\nHash      : {}\r\n", hex(&deno_zip).to_uppercase()),
    );
    let ff_zip = zip_of(&[("ffmpeg-9.0-essentials_build/README.txt", b"readme"), ("ffmpeg-9.0-essentials_build/bin/ffmpeg.exe", &ffmpeg)]);
    srv.put("/gy/ffmpeg-release-essentials.zip", ff_zip.clone());
    srv.put("/gy/ffmpeg-release-essentials.zip.sha256", hex(&ff_zip));
    (srv, ytdlp, ffmpeg)
}

async fn manager(name: &str, srv: &Server) -> (DownloadManager, PathBuf) {
    let base = std::env::temp_dir().join(format!("velox-tools-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.tools = ToolSources {
        ytdlp: Some(srv.url("/yt")),
        deno: Some(srv.url("/deno")),
        ffmpeg: Some(srv.url("/ff")),
        ffmpeg_fallback: Some(srv.url("/gy")),
        ffmpeg_unix: None,
    };
    (DownloadManager::new(c).await.unwrap(), base.join("state").join("tools"))
}

/// A tool of this name the manager would find on its own (on PATH): then nothing needs fetching.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.is_absolute() && d.join(exe(name)).is_file()))
}

#[tokio::test]
async fn video_tools_are_fetched_verified_and_used_on_first_need() {
    if on_path("yt-dlp") {
        eprintln!("yt-dlp is on PATH here: nothing to fetch, skipping");
        return;
    }
    let (srv, ytdlp, ffmpeg) = tool_server(true, true).await;
    let (m, tools) = manager("fetch", &srv).await;
    let vid = format!("tools{}", std::process::id());
    let analysis =
        m.analyze_media(format!("https://www.youtube.com/watch?v={vid}")).await.expect("the page is read with the fetched yt-dlp");
    assert!(!analysis.analysis.streams.is_empty(), "{analysis:?}");
    assert_eq!(std::fs::read(tools.join(exe("yt-dlp"))).unwrap(), ytdlp, "yt-dlp installed byte-exact");
    assert_eq!(std::fs::read(tools.join(exe("deno"))).unwrap(), b"stand-in deno", "Deno copied out of its archive");
    // yt-dlp was told where Deno is.
    let log = std::fs::read_to_string(std::env::temp_dir().join("velox-fake-ytdlp.log")).unwrap_or_default();
    let call = log.lines().rfind(|l| l.contains(&vid)).expect("yt-dlp ran");
    assert!(call.contains("--js-runtimes") && call.contains(&format!("deno:{}", tools.join(exe("deno")).display())), "{call}");
    if cfg!(all(windows, target_arch = "x86_64")) && !on_path("ffmpeg") {
        // ffmpeg comes in the background; a download waits for it and then joins video and audio.
        let t = Instant::now();
        while !tools.join("ffmpeg.exe").is_file() {
            assert!(t.elapsed() < Duration::from_secs(20), "ffmpeg never arrived");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(std::fs::read(tools.join("ffmpeg.exe")).unwrap(), ffmpeg, "only bin/ffmpeg.exe is copied out");
        assert!(srv.state.hits.load(std::sync::atomic::Ordering::SeqCst) >= 8, "the archive came in parallel parts");
        let job = m
            .start_media(MediaRequest {
                url: format!("https://www.youtube.com/watch?v={vid}"),
                stream: Some("h720".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let t = Instant::now();
        let done = loop {
            let j = m.list_media().into_iter().find(|j| j.id == job.id).unwrap();
            if !j.status.is_active() {
                break j;
            }
            assert!(t.elapsed() < Duration::from_secs(30), "{j:?}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert!(done.error.is_none(), "{done:?}");
    }
    // Nothing else is left in the tools folder (no half-written files).
    let mut names: Vec<String> =
        std::fs::read_dir(&tools).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert!(names.iter().all(|n| !n.starts_with('.')), "{names:?}");
}

#[tokio::test]
async fn a_tool_that_does_not_match_its_checksum_is_never_installed() {
    if on_path("yt-dlp") {
        return;
    }
    let (srv, _, _) = tool_server(false, true).await;
    let (m, tools) = manager("badsum", &srv).await;
    let why = match m.analyze_media("https://www.youtube.com/watch?v=badsum".into()).await {
        Err(ManagerError::InvalidInput(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    };
    // The user is told in plain words, without tool names (the checksum mismatch itself goes to the log).
    assert!(why.contains("could not download the required dependencies"), "{why}");
    assert!(!why.to_ascii_lowercase().contains("yt-dlp"), "{why}");
    assert!(!tools.join(exe("yt-dlp")).exists(), "nothing was installed");
    assert!(std::fs::read_dir(&tools).map(|d| d.flatten().all(|e| !e.file_name().to_string_lossy().contains("yt-dlp"))).unwrap_or(true));
}

/// Against the real download servers (needs the internet; run by hand):
/// `VELOX_TEST_REAL_TOOLS=1 cargo test -p velox-integration-tests --test tools real -- --nocapture`
#[tokio::test]
async fn real_sources_install_when_reachable() {
    if std::env::var_os("VELOX_TEST_REAL_TOOLS").is_none() {
        return;
    }
    let base = std::env::temp_dir().join(format!("velox-tools-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.tools = ToolSources::official();
    let m = DownloadManager::new(c).await.unwrap();
    let t = Instant::now();
    let st = m.fetch_video_tools().await;
    eprintln!("after {:?}: {st:#?}", t.elapsed());
    for (name, s) in [("yt-dlp", &st.ytdlp), ("deno", &st.deno), ("ffmpeg", &st.ffmpeg)] {
        let p = base.join("state").join("tools").join(exe(name));
        eprintln!("{name}: installed={} file={} size={:?}", s.installed, p.display(), std::fs::metadata(&p).map(|m| m.len()).ok());
        if p.is_file() {
            let out = std::process::Command::new(&p).arg(if name == "ffmpeg" { "-version" } else { "--version" }).output().unwrap();
            eprintln!("  runs: {} {}", out.status, String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or(""));
            assert!(out.status.success());
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn ffmpeg_comes_from_the_fallback_source_when_the_main_one_fails() {
    if !cfg!(all(windows, target_arch = "x86_64")) || on_path("ffmpeg") {
        return;
    }
    let (srv, _, ffmpeg) = tool_server(true, false).await;
    let (m, tools) = manager("fallback", &srv).await;
    let st = m.fetch_video_tools().await;
    assert!(st.ffmpeg.installed && st.ffmpeg.error.is_none(), "{st:?}");
    assert_eq!(std::fs::read(tools.join("ffmpeg.exe")).unwrap(), ffmpeg);
}

/// A real YouTube video, end to end, with the tools VeloxFetch fetches itself (needs the internet; by hand):
/// `VELOX_TEST_REAL_YOUTUBE=<watch link> cargo test -p velox-integration-tests --test tools youtube -- --nocapture`
#[tokio::test]
async fn real_youtube_video_downloads_in_hd() {
    let Some(link) = std::env::var_os("VELOX_TEST_REAL_YOUTUBE").map(|v| v.to_string_lossy().into_owned()) else {
        return;
    };
    // The tools are kept between runs in this folder (they are large).
    let base = std::env::temp_dir().join("velox-tools-real-youtube");
    let _ = std::fs::remove_dir_all(base.join("downloads"));
    let _ = std::fs::remove_file(base.join("state").join("velox.db"));
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.tools = ToolSources::official();
    let m = DownloadManager::new(c).await.unwrap();
    let t = Instant::now();
    let tools = m.fetch_video_tools().await;
    eprintln!("tools ready after {:?}: {}", t.elapsed(), tools.ytdlp.installed && tools.deno.installed && tools.ffmpeg.installed);
    let a = m.analyze_media(link.clone()).await.expect("YouTube page read");
    eprintln!("title: {}\nqualities: {:?}", a.analysis.title, a.analysis.streams.iter().map(|s| s.label.clone()).collect::<Vec<_>>());
    assert!(!a.analysis.streams.is_empty());
    // `VELOX_TEST_REAL_YOUTUBE_QUALITY` (default h720) and `VELOX_TEST_REAL_YOUTUBE_TYPES` (default mp4,mp3).
    let want = std::env::var("VELOX_TEST_REAL_YOUTUBE_QUALITY").unwrap_or_else(|_| "h720".into());
    let pick = a.analysis.streams.iter().find(|s| s.id == want).or(a.analysis.streams.first()).unwrap().id.clone();
    let types = std::env::var("VELOX_TEST_REAL_YOUTUBE_TYPES").unwrap_or_else(|_| "mp4,mp3".into());
    for format in types.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let req = MediaRequest { url: link.clone(), stream: Some(pick.clone()), format: Some(format.into()), ..Default::default() };
        let job = m.start_media(req).await.unwrap();
        let t = Instant::now();
        let done = loop {
            let j = m.list_media().into_iter().find(|j| j.id == job.id).unwrap();
            if !j.status.is_active() {
                break j;
            }
            assert!(t.elapsed() < Duration::from_secs(900), "{j:?}");
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        eprintln!("{pick} as {format} done after {:?}: {done:?}", t.elapsed());
        assert!(done.error.is_none(), "{done:?}");
        assert!(done.notes.is_empty(), "{format}: {:?}", done.notes);
        let file = std::path::Path::new(&done.folder).join(&done.files[0]);
        assert_eq!(file.extension().unwrap().to_string_lossy(), format, "saved in the chosen type");
        let bytes = std::fs::read(&file).unwrap();
        eprintln!("file: {} ({} bytes)", file.display(), bytes.len());
        assert!(bytes.len() > 100_000);
        // The content really is that type (not just the name).
        let real = match format {
            "mp4" | "m4a" => &bytes[4..8] == b"ftyp",
            "mkv" | "webm" => bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
            "mp3" => bytes.starts_with(b"ID3") || (bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0),
            "opus" => bytes.starts_with(b"OggS"),
            _ => true,
        };
        assert!(real, "{format}: starts with {:02x?}", &bytes[..12]);
    }
}

/// The app was closed (or the network dropped) in the middle of downloading ffmpeg: the next start continues
/// the parts it has instead of starting again (on a slow line, starting over each time would never finish).
#[tokio::test]
async fn an_interrupted_tool_download_continues_where_it_stopped() {
    if !cfg!(all(windows, target_arch = "x86_64")) || on_path("ffmpeg") {
        return;
    }
    let (srv, _, ffmpeg) = tool_server(true, true).await;
    let (m, tools) = manager("resume", &srv).await;
    let archive = srv.state.data.lock().unwrap().clone();
    let url = format!("{}/{}", srv.url("/ff"), ffmpeg_asset());
    // What an earlier run left: the first MiB of part 0, and which file the parts belong to.
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(tools.join(".ffmpeg.download.id"), format!("{url}\n{}\n\"v1\"", archive.len())).unwrap();
    std::fs::write(tools.join(".ffmpeg.download.p0"), &archive[..1024 * 1024]).unwrap();
    srv.state.request_heads.lock().unwrap().clear();

    let st = m.fetch_video_tools().await;
    assert!(st.ffmpeg.installed, "{st:?}");
    assert_eq!(std::fs::read(tools.join("ffmpeg.exe")).unwrap(), ffmpeg, "the joined archive is exact (checksum matched)");
    let heads = srv.state.request_heads.lock().unwrap().join("\n").to_ascii_lowercase();
    assert!(heads.contains("range: bytes=1048576-"), "part 0 continued after its first MiB:\n{heads}");
    let step = (archive.len() as u64).div_ceil(8);
    assert!(!heads.contains(&format!("range: bytes=0-{}", step - 1)), "part 0 was not fetched again from the start");
    let left: Vec<String> = std::fs::read_dir(&tools).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(left.iter().all(|n| !n.starts_with(".ffmpeg")), "the parts are removed once joined: {left:?}");

    // Parts of another file (the server's file changed): thrown away, never mixed in.
    let (m2, tools2) = manager("resume-other", &srv).await;
    std::fs::create_dir_all(&tools2).unwrap();
    std::fs::write(tools2.join(".ffmpeg.download.id"), format!("{url}\n{}\n\"old\"", archive.len())).unwrap();
    std::fs::write(tools2.join(".ffmpeg.download.p0"), vec![0u8; 1024 * 1024]).unwrap();
    let st = m2.fetch_video_tools().await;
    assert!(st.ffmpeg.installed, "{st:?}");
    assert_eq!(std::fs::read(tools2.join("ffmpeg.exe")).unwrap(), ffmpeg);
}

/// Tools the installer put next to the program are used; the app's own copies win over them.
#[tokio::test]
async fn tools_from_the_installer_are_used() {
    if on_path("yt-dlp") {
        return;
    }
    let base = std::env::temp_dir().join(format!("velox-tools-{}-shared", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let shared = base.join("program").join("tools");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_fake-ytdlp"), shared.join(exe("yt-dlp"))).unwrap();
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.shared_tools_dir = Some(shared.clone());
    let m = DownloadManager::new(c).await.unwrap();
    let st = m.video_tools();
    assert!(st.ytdlp.installed, "{st:?}");
    let vid = format!("shared{}", std::process::id());
    m.analyze_media(format!("https://www.youtube.com/watch?v={vid}")).await.expect("read with the installer's yt-dlp");
}
