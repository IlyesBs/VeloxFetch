//! Where downloads go: a default folder the user can change, a folder chosen for one download, and the
//! optional sorting of new files into Video / Audio / Documents / ... folders.
//!
//! Folders are only ever chosen through `register_pick` (what the native folder dialog hands over), which
//! refuses system, program and autostart folders; the UI holds a token, never a path.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use download_manager::{
    ConfirmPending, DownloadDto, DownloadManager, ListFilter, ManagerConfig, ManagerError, MediaRequest, MediaStatus, NewDownload,
    SetFolderNames,
};
use downloader_core::DownloadStatus;
use persistence::SqliteStore;
use velox_integration_tests::{random_data, serve, Server};

struct Env {
    base: PathBuf,
    root: PathBuf,
    m: DownloadManager,
    srv: Server,
    data: std::sync::Arc<Vec<u8>>,
}

fn config(base: &Path, organize: bool) -> ManagerConfig {
    let mut c = ManagerConfig::new(base.join("state").join("velox.db"), base.join("downloads"));
    c.progress_interval = Duration::from_millis(40);
    c.download.min_segment_size = 64 * 1024;
    c.ffmpeg_path = Some(base.join("no-ffmpeg-here"));
    c.organize_default = organize;
    c
}

fn base_dir(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("velox-storage-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    base
}

async fn env(name: &str, organize: bool) -> Env {
    let base = base_dir(name);
    let data = random_data(150 * 1024, 601);
    let srv = serve(data.clone()).await;
    let m = DownloadManager::new(config(&base, organize)).await.unwrap();
    Env { root: base.join("downloads"), base, m, srv, data }
}

async fn done(m: &DownloadManager, id: i64) -> DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if matches!(d.status, DownloadStatus::Completed | DownloadStatus::Failed) {
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "stuck in {:?}", d.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn get(e: &Env, path: &str) -> DownloadDto {
    let d = e.m.create_download(NewDownload { url: e.srv.url(path), ..Default::default() }).await.unwrap();
    let d = done(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{path}: {:?}", d.error);
    d
}

/// Path of a finished download relative to `root`, with `/` separators.
fn rel(d: &DownloadDto, root: &Path) -> String {
    Path::new(&d.destination)
        .strip_prefix(root)
        .unwrap_or_else(|_| panic!("{} is outside {}", d.destination, root.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

fn folder(dir: &Path) -> PathBuf {
    let d = dir.join(format!("picked-{}", rand_suffix()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

// ============================================================================= the old behaviour is unchanged

#[tokio::test]
async fn nothing_is_sorted_unless_asked_and_files_land_where_they_always_did() {
    let e = env("default-off", false).await;
    assert!(!e.m.get_storage().organize);
    for path in ["/song.mp3", "/paper.pdf", "/setup.exe", "/x.bin"] {
        let d = get(&e, path).await;
        assert_eq!(rel(&d, &e.root), path.trim_start_matches('/'), "{path}");
    }
    let s = e.m.get_storage();
    assert!(s.is_default && s.root == s.default_root);
}

// ============================================================================= sorting by type

#[tokio::test]
async fn turning_sorting_on_puts_each_new_file_in_the_folder_of_its_type() {
    let e = env("sort-on", false).await;
    assert!(e.m.set_organize(true).await.unwrap().organize);
    for (path, expected) in [
        ("/a.mp3", "Audio/a.mp3"),
        ("/b.PDF", "Documents/b.PDF"),
        ("/c.zip", "Archives/c.zip"),
        ("/d.exe", "Programs/d.exe"),
        ("/e.mkv", "Video/e.mkv"),
        ("/f.png", "Images/f.png"),
        ("/g.xyz", "Other/g.xyz"),
        ("/h.tar.gz", "Archives/h.tar.gz"),
        ("/noext", "Other/noext"),
    ] {
        let d = get(&e, path).await;
        assert_eq!(rel(&d, &e.root), expected, "{path}");
        assert_eq!(std::fs::read(&d.destination).unwrap(), *e.data);
    }
    // Turned off again: back to the folder itself.
    e.m.set_organize(false).await.unwrap();
    assert_eq!(rel(&get(&e, "/i.mp3").await, &e.root), "i.mp3");
    // What the user chose is remembered whatever the app's default is.
    e.m.shutdown().await;
    let m2 = DownloadManager::new(config(&e.base, true)).await.unwrap();
    assert!(!m2.get_storage().organize, "an explicit \"off\" beats the default \"on\"");
    m2.set_organize(true).await.unwrap();
    m2.shutdown().await;
    let m3 = DownloadManager::new(config(&e.base, false)).await.unwrap();
    assert!(m3.get_storage().organize, "an explicit \"on\" beats the default \"off\"");
    m3.shutdown().await;
}

#[tokio::test]
async fn the_name_the_server_chooses_decides_the_type_and_an_explicit_folder_or_name_is_respected() {
    let e = env("sort-server", true).await;
    *e.srv.state.content_disposition.lock().unwrap() = Some("attachment; filename=\"Quarterly report.pdf\"".into());
    assert_eq!(rel(&get(&e, "/download?id=7").await, &e.root), "Documents/Quarterly report.pdf");
    *e.srv.state.content_disposition.lock().unwrap() = None;
    // A sub-folder chosen by the user is used as given: no sorting inside it.
    let d =
        e.m.create_download(NewDownload { url: e.srv.url("/x.mp3"), subdir: Some("Mine/Music".into()), ..Default::default() })
            .await
            .unwrap();
    assert_eq!(rel(&done(&e.m, d.id).await, &e.root), "Mine/Music/x.mp3");
    // A name typed by the user counts (the file is called .mp3 whatever the address says).
    let d =
        e.m.create_download(NewDownload { url: e.srv.url("/opaque"), filename: Some("song.mp3".into()), ..Default::default() })
            .await
            .unwrap();
    assert_eq!(rel(&done(&e.m, d.id).await, &e.root), "Audio/song.mp3");
}

#[tokio::test]
async fn the_type_folders_can_be_renamed_with_strict_checks_and_the_names_survive_a_restart() {
    let e = env("sort-names", true).await;
    let mut names = std::collections::BTreeMap::new();
    names.insert("audio".to_string(), "Music".to_string());
    names.insert("program".to_string(), "Applications".to_string());
    let s = e.m.set_folder_names(SetFolderNames { names }).await.unwrap();
    assert_eq!(s.folders.iter().find(|f| f.kind == downloader_core::FileKind::Audio).unwrap().folder, "Music");
    assert_eq!(rel(&get(&e, "/a.mp3").await, &e.root), "Music/a.mp3");
    assert_eq!(rel(&get(&e, "/s.exe").await, &e.root), "Applications/s.exe");
    e.m.shutdown().await;
    let m2 = DownloadManager::new(config(&e.base, true)).await.unwrap();
    let s = m2.get_storage();
    assert_eq!(
        s.folders.iter().find(|f| f.kind == downloader_core::FileKind::Audio).unwrap().folder,
        "Music",
        "the names survive a restart"
    );

    // Hostile or clashing names are refused and change nothing.
    let bad = |pairs: &[(&str, &str)]| SetFolderNames { names: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    for pairs in [
        vec![("audio", "../escape")],
        vec![("audio", "a/b")],
        vec![("audio", "a\\b")],
        vec![("audio", "C:")],
        vec![("audio", "CON")],
        vec![("audio", "nul.txt")],
        vec![("audio", "trailing.")],
        vec![("audio", "x\u{0}y")],
        vec![("audio", "..")],
        vec![("audio", "Video")],                      // clashes with the default of another kind
        vec![("audio", "docs"), ("document", "DOCS")], // two kinds, one folder
        vec![("nonsense", "Whatever")],
    ] {
        let r = m2.set_folder_names(bad(&pairs)).await;
        assert!(matches!(r, Err(ManagerError::InvalidInput(_))), "{pairs:?} should be refused, got {r:?}");
    }
    assert_eq!(m2.get_storage().folders.iter().find(|f| f.kind == downloader_core::FileKind::Audio).unwrap().folder, "Music");
    // An empty name means "the default".
    let s = m2.set_folder_names(bad(&[("audio", ""), ("program", "")])).await.unwrap();
    assert_eq!(s.folders.iter().find(|f| f.kind == downloader_core::FileKind::Audio).unwrap().folder, "Audio");
    m2.shutdown().await;
}

// ============================================================================= a folder of your own

#[tokio::test]
async fn the_default_folder_can_be_changed_old_downloads_stay_valid_and_it_survives_a_restart() {
    let e = env("change-default", true).await;
    let old = get(&e, "/before.mp3").await;
    assert_eq!(rel(&old, &e.root), "Audio/before.mp3");

    let elsewhere = folder(&e.base);
    let pick = e.m.register_pick(&elsewhere).unwrap();
    let s = e.m.use_folder_as_default(pick.token).await.unwrap();
    assert!(!s.is_default);
    assert_eq!(PathBuf::from(&s.root), PathBuf::from(&pick.path));
    let new = get(&e, "/after.mp3").await;
    assert_eq!(rel(&new, Path::new(&pick.path)), "Audio/after.mp3", "new downloads go to the new folder, sorted");
    assert!(PathBuf::from(&old.destination).exists(), "nothing was moved");

    // The old download still belongs to the app: it can be removed together with its file.
    e.m.delete_download(old.id, true).await.unwrap();
    assert!(!PathBuf::from(&old.destination).exists());

    // Restart: the choice is remembered.
    e.m.shutdown().await;
    let m2 = DownloadManager::new(config(&e.base, false)).await.unwrap();
    assert_eq!(PathBuf::from(m2.get_storage().root), PathBuf::from(&pick.path));
    // A download from before the restart can be deleted with its file in the (still allowed) new folder too.
    m2.delete_download(new.id, true).await.unwrap();
    assert!(!PathBuf::from(&new.destination).exists());
    // Reset goes back to the app's own folder.
    let s = m2.reset_default_folder().await.unwrap();
    assert!(s.is_default && Path::new(&s.root) == e.root);
    m2.shutdown().await;
}

#[tokio::test]
async fn a_download_queued_for_the_old_default_stays_there_and_is_still_sorted_there() {
    let e = env("queued-old", true).await;
    let queued = e.m.create_download(NewDownload { url: e.srv.url("/late.mp3"), start: Some(false), ..Default::default() }).await.unwrap();
    let pick = e.m.register_pick(&folder(&e.base)).unwrap();
    e.m.use_folder_as_default(pick.token).await.unwrap();
    e.m.start_download(queued.id).await.unwrap();
    let d = done(&e.m, queued.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(rel(&d, &e.root), "Audio/late.mp3", "it was queued for the old folder");
}

#[tokio::test]
async fn one_download_can_go_to_a_folder_picked_for_it_and_it_is_not_sorted_there() {
    let e = env("one-off", true).await;
    let target = folder(&e.base);
    let pick = e.m.register_pick(&target).unwrap();
    let d =
        e.m.create_download(NewDownload { url: e.srv.url("/m.mp3"), folder_token: Some(pick.token), ..Default::default() }).await.unwrap();
    let d = done(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(rel(&d, Path::new(&pick.path)), "m.mp3", "exactly the folder that was picked");
    // The default folder is unchanged.
    assert_eq!(PathBuf::from(e.m.get_storage().root), e.root);

    // The same folder can be used again without picking it again.
    let d2 =
        e.m.create_download(NewDownload { url: e.srv.url("/n.mp3"), folder_token: Some(pick.token), ..Default::default() }).await.unwrap();
    assert_eq!(rel(&done(&e.m, d2.id).await, Path::new(&pick.path)), "n.mp3");

    // It survives a restart (pause, restart, resume in the custom folder): the folder stays allowed.
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(500);
    let slow =
        e.m.create_download(NewDownload { url: e.srv.url("/slow.mp3"), folder_token: Some(pick.token), ..Default::default() })
            .await
            .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    e.m.shutdown().await;
    *e.srv.state.chunk_delay.lock().unwrap() = Duration::ZERO;
    let m2 = DownloadManager::new(config(&e.base, true)).await.unwrap();
    m2.resume_download(slow.id).await.unwrap();
    let d3 = done(&m2, slow.id).await;
    assert_eq!(d3.status, DownloadStatus::Completed, "{:?}", d3.error);
    assert_eq!(std::fs::read(&d3.destination).unwrap(), *e.data);
    m2.shutdown().await;
}

#[tokio::test]
async fn a_folder_token_is_only_ever_what_the_native_dialog_produced() {
    let e = env("tokens", true).await;
    // Made-up and stale tokens.
    for token in [0u64, 1, 999, u64::MAX] {
        let r = e.m.create_download(NewDownload { url: e.srv.url("/t.mp3"), folder_token: Some(token), ..Default::default() }).await;
        assert!(matches!(r, Err(ManagerError::InvalidInput(_))), "token {token}: {r:?}");
        assert!(e.m.use_folder_as_default(token).await.is_err());
    }
    // A folder and a sub-folder together are ambiguous.
    let pick = e.m.register_pick(&folder(&e.base)).unwrap();
    let r =
        e.m.create_download(NewDownload {
            url: e.srv.url("/t.mp3"),
            folder_token: Some(pick.token),
            subdir: Some("x".into()),
            ..Default::default()
        })
        .await;
    assert!(r.is_err());
    // Folders the dialog result is not allowed to be.
    let mut refused: Vec<PathBuf> = vec![
        PathBuf::from("relative"),
        PathBuf::new(),
        e.base.join("does-not-exist"),
        e.base.join("state"), // the app's own data
        e.base.clone(),       // ... and a folder that contains it
    ];
    if cfg!(windows) {
        refused.extend(["C:\\", "C:\\Windows", "C:\\Windows\\System32", "C:\\Program Files"].map(PathBuf::from));
    } else {
        refused.extend(["/", "/etc", "/usr/bin", "/System"].map(PathBuf::from));
    }
    let ssh = e.base.join("home").join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    refused.push(ssh);
    for p in refused {
        let r = e.m.register_pick(&p);
        assert!(matches!(r, Err(ManagerError::InvalidInput(_))), "{p:?} must be refused, got {r:?}");
    }
    // A file is not a folder.
    let f = e.base.join("a-file.txt");
    std::fs::write(&f, b"x").unwrap();
    assert!(e.m.register_pick(&f).is_err());
    // Nothing was written anywhere by all of this.
    assert!(std::fs::read_dir(&e.root).map(|d| d.count()).unwrap_or(0) == 0);
}

#[tokio::test]
async fn an_unavailable_chosen_folder_does_not_stop_the_app_it_falls_back_to_the_default() {
    let e = env("gone", true).await;
    e.m.shutdown().await;
    // Point the stored folder somewhere that cannot be created (under a file), as an unplugged drive would be.
    let blocker = e.base.join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let store = SqliteStore::open(e.base.join("state").join("velox.db")).unwrap();
    store.set_setting("storage.root", &blocker.join("Downloads").to_string_lossy()).unwrap();
    drop(store);
    let m2 = DownloadManager::new(config(&e.base, true)).await.expect("the app must start");
    let s = m2.get_storage();
    assert_eq!(PathBuf::from(&s.root), e.root, "fell back to the default folder");
    let d = m2.create_download(NewDownload { url: e.srv.url("/ok.mp3"), ..Default::default() }).await.unwrap();
    assert_eq!(done(&m2, d.id).await.status, DownloadStatus::Completed);
    m2.shutdown().await;
}

// ============================================================================= the browser confirmation and streams

#[tokio::test]
async fn the_confirmation_shows_where_the_file_will_go_and_can_send_it_elsewhere() {
    let e = env("pending", true).await;
    let p = e.m.add_pending(NewDownload { url: e.srv.url("/movie.mkv"), ..Default::default() }, Default::default()).await.unwrap();
    assert_eq!(PathBuf::from(&p.save_dir), e.root.join("Video"), "the dialog shows the sorted folder");
    let settle = |token: u64| {
        let m = e.m.clone();
        async move {
            let t = Instant::now();
            loop {
                let cur = m.list_pending().into_iter().find(|x| x.token == token).unwrap();
                if !cur.probing {
                    return cur;
                }
                assert!(t.elapsed() < Duration::from_secs(10));
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    };
    let first = settle(p.token).await; // its answer must be in before the server starts naming files differently
    assert_eq!(PathBuf::from(&first.save_dir), e.root.join("Video"));
    // After the server answers, the prediction uses the real name.
    *e.srv.state.content_disposition.lock().unwrap() = Some("attachment; filename=\"tune.flac\"".into());
    let p2 = e.m.add_pending(NewDownload { url: e.srv.url("/dl?id=1"), ..Default::default() }, Default::default()).await.unwrap();
    let settled = settle(p2.token).await;
    assert_eq!(settled.filename, "tune.flac");
    assert_eq!(PathBuf::from(&settled.save_dir), e.root.join("Audio"));
    *e.srv.state.content_disposition.lock().unwrap() = None;

    // Confirm the first one normally (sorted), the second into a folder chosen in the dialog.
    let d1 = e.m.confirm_pending(ConfirmPending { token: p.token, filename: None, start: None, folder_token: None }).await.unwrap();
    assert_eq!(rel(&done(&e.m, d1.id).await, &e.root), "Video/movie.mkv");
    let target = folder(&e.base);
    let pick = e.m.register_pick(&target).unwrap();
    let d2 =
        e.m.confirm_pending(ConfirmPending {
            token: p2.token,
            filename: Some("mine.flac".into()),
            start: None,
            folder_token: Some(pick.token),
        })
        .await
        .unwrap();
    assert_eq!(rel(&done(&e.m, d2.id).await, Path::new(&pick.path)), "mine.flac");
    let listed = e.m.list_downloads(ListFilter::default()).await.unwrap();
    assert_eq!(listed.len(), 2);
}

#[tokio::test]
async fn a_stream_goes_to_the_video_folder_when_sorting_is_on() {
    let e = env("stream", true).await;
    e.srv.put(
        "/hls/media.m3u8",
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0,\nseg0.ts\n#EXTINF:6.0,\nseg1.ts\n#EXT-X-ENDLIST\n",
    );
    e.srv.put("/hls/seg0.ts", vec![1u8; 5000]);
    e.srv.put("/hls/seg1.ts", vec![2u8; 5000]);
    let job =
        e.m.start_media(MediaRequest { url: e.srv.url("/hls/media.m3u8"), filename: Some("clip".into()), ..Default::default() })
            .await
            .unwrap();
    let t = Instant::now();
    let j = loop {
        let j = e.m.list_media().into_iter().find(|j| j.id == job.id).unwrap();
        if !j.status.is_active() {
            break j;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "{j:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(j.status, MediaStatus::Completed, "{j:?}");
    assert!(
        e.root.join("Video").join("clip.ts").exists(),
        "{:?}",
        std::fs::read_dir(&e.root).map(|d| d.flatten().map(|x| x.path()).collect::<Vec<_>>())
    );
}

#[tokio::test]
async fn a_folder_choice_that_was_only_partly_saved_before_a_kill_still_works() {
    // The choice is stored in three settings; a kill after the first leaves the other two stale. The default
    // folder must still be usable (and sorted), because the app adds it to what it allows at start-up.
    let e = env("partial", true).await;
    e.m.shutdown().await;
    let target = folder(&e.base);
    let store = SqliteStore::open(e.base.join("state").join("velox.db")).unwrap();
    store.set_setting("storage.root", &target.to_string_lossy()).unwrap();
    drop(store);
    let m2 = DownloadManager::new(config(&e.base, true)).await.unwrap();
    let d = m2.create_download(NewDownload { url: e.srv.url("/late.mp3"), ..Default::default() }).await.unwrap();
    let d = done(&m2, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(Path::new(&d.destination).strip_prefix(&target).unwrap(), Path::new("Audio").join("late.mp3"));
    m2.shutdown().await;
}

// ============================================================================= show in folder, per download

#[tokio::test]
async fn show_in_folder_opens_each_downloads_own_folder() {
    let base = base_dir("show-own");
    let data = random_data(150 * 1024, 602);
    let srv = serve(data).await;
    let shell = std::sync::Arc::new(system_integration::files::DryRunShell::default());
    let mut c = config(&base, false);
    c.shell = download_manager::ShellHandle(shell.clone());
    let m = DownloadManager::new(c).await.unwrap();
    let last = || shell.calls.lock().unwrap().last().unwrap().1.clone();
    let show = |id| m.show_in_folder(download_manager::FileTarget::Download { id });

    // Usual folder.
    let usual = m.create_download(NewDownload { url: srv.url("/usual.bin"), ..Default::default() }).await.unwrap();
    let usual = done(&m, usual.id).await;
    // A folder picked for this one download: finished, and queued (not started yet).
    let pick = m.register_pick(&folder(&base)).unwrap();
    let own =
        m.create_download(NewDownload { url: srv.url("/own.bin"), folder_token: Some(pick.token), ..Default::default() }).await.unwrap();
    let own = done(&m, own.id).await;
    let waiting = m
        .create_download(NewDownload {
            url: srv.url("/later.bin"),
            folder_token: Some(pick.token),
            start: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();

    show(own.id).await.unwrap();
    assert_eq!(last(), PathBuf::from(&own.destination));
    assert!(last().starts_with(&pick.path), "the picked folder, not the usual one");
    show(waiting.id).await.unwrap();
    assert_eq!(last(), PathBuf::from(&pick.path));
    show(usual.id).await.unwrap();
    assert_eq!(last(), PathBuf::from(&usual.destination));

    // Changing the default later moves nothing: each download keeps its own folder.
    let other = m.register_pick(&folder(&base)).unwrap();
    m.use_folder_as_default(other.token).await.unwrap();
    show(usual.id).await.unwrap();
    assert_eq!(last(), PathBuf::from(&usual.destination));
    show(own.id).await.unwrap();
    assert_eq!(last(), PathBuf::from(&own.destination));
    m.shutdown().await;
}

#[tokio::test]
async fn a_download_without_a_size_finishes_and_then_knows_its_size() {
    let e = env("no-size", false).await;
    let d = e.m.create_download(NewDownload { url: e.srv.url("/chunked/export.json"), ..Default::default() }).await.unwrap();
    let d = done(&e.m, d.id).await;
    assert_eq!(d.status, DownloadStatus::Completed, "{:?}", d.error);
    assert_eq!(std::fs::read(&d.destination).unwrap(), *e.data);
    assert_eq!(d.downloaded, e.data.len() as u64);
    assert_eq!(d.total_size, Some(e.data.len() as u64), "once finished, the size is what was received");
}
