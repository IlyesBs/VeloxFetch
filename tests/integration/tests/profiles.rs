//! Download Profiles through the real manager, engine and database: presets over the user's settings, switched
//! while downloads run without restarting them, never resuming a paused one, never changing the user's own
//! settings, and recovering from bad saved data.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use download_manager::profiles::{ProfileAction, ProfileSettings, DEFAULT_ID, FAST_CONNECTIONS, LOW_ACTIVE, LOW_CONNECTIONS};
use download_manager::{
    DownloadDto, DownloadManager, ManagerConfig, NewDownload, SetQueueConfig, SetSmartThrottle, SetSpeedLimit, SpeedTarget,
};
use downloader_core::DownloadStatus;
use velox_integration_tests::{random_data, serve};

const MB: usize = 1024 * 1024;

fn paths(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("velox-profiles-{}-{name}", std::process::id()));
    (base.join("state").join("velox.db"), base.join("downloads"))
}

fn wipe(name: &str) {
    let (db, _) = paths(name);
    let _ = std::fs::remove_dir_all(db.parent().unwrap().parent().unwrap());
}

async fn open(name: &str) -> DownloadManager {
    let (db, root) = paths(name);
    let mut c = ManagerConfig::new(db, root);
    c.download.min_segment_size = 64 * 1024;
    DownloadManager::new(c).await.unwrap()
}

async fn select(m: &DownloadManager, id: &str) {
    m.profile_action(ProfileAction::Select { id: id.into() }).await.unwrap();
}

async fn settle(m: &DownloadManager, id: i64) -> DownloadDto {
    let t = Instant::now();
    loop {
        let d = m.get_download(id).await.unwrap();
        if matches!(d.status, DownloadStatus::Completed | DownloadStatus::Failed) {
            return d;
        }
        assert!(t.elapsed() < Duration::from_secs(60), "stuck in {:?}", d.status);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn until(what: &str, secs: u64, mut f: impl AsyncFnMut() -> bool) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn without_profiles_nothing_changes_and_default_is_the_users_settings() {
    wipe("default");
    let m = open("default").await;
    m.set_queue_config(SetQueueConfig { max_active: Some(3), max_connections: Some(12) }).await.unwrap();
    m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Global, bytes_per_sec: Some(800_000) }).await.unwrap();
    let p = m.get_profiles();
    assert_eq!(p.active, DEFAULT_ID);
    assert_eq!(p.effective, p.base, "Default = the user's own settings");
    assert_eq!((p.base.max_active, p.base.max_connections, p.base.speed_limit_bps), (3, 12, Some(800_000)));
    // A normal download works exactly as before.
    let srv = serve(random_data(MB, 1)).await;
    let d = m.create_download(NewDownload { url: srv.url("/a.bin"), ..Default::default() }).await.unwrap();
    assert_eq!(settle(&m, d.id).await.status, DownloadStatus::Completed);
}

#[tokio::test]
async fn a_profile_never_overwrites_the_users_settings() {
    wipe("base");
    let m = open("base").await;
    m.set_queue_config(SetQueueConfig { max_active: Some(3), max_connections: Some(12) }).await.unwrap();
    m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Global, bytes_per_sec: Some(800_000) }).await.unwrap();
    select(&m, "lowResource").await;
    let p = m.get_profiles();
    assert_eq!((p.effective.max_active, p.effective.max_connections), (LOW_ACTIVE, LOW_CONNECTIONS));
    assert_eq!(p.effective.speed_limit_bps, Some(800_000), "Low resource leaves the user's limit alone");
    let q = m.get_queue_state().await.unwrap();
    assert_eq!((q.max_active, q.max_connections), (3, 12), "the Settings page still shows the user's own values");
    assert_eq!(m.speed_limits().global, Some(800_000));
    // The user changes a setting meanwhile: it is theirs (kept for Default), the profile still decides that value.
    m.set_queue_config(SetQueueConfig { max_active: Some(5), max_connections: None }).await.unwrap();
    assert_eq!(m.get_profiles().effective.max_active, LOW_ACTIVE);
    // A custom profile lifts the limit; Default brings everything back.
    let open_line = ProfileSettings { speed_limit_bps: Some(0), ..Default::default() };
    m.profile_action(ProfileAction::Create { name: "No limit".into(), settings: open_line }).await.unwrap();
    select(&m, "c1").await;
    assert_eq!(m.get_profiles().effective.speed_limit_bps, None);
    select(&m, DEFAULT_ID).await;
    let p = m.get_profiles();
    assert_eq!(p.effective, p.base);
    assert_eq!((p.effective.max_active, p.effective.max_connections, p.effective.speed_limit_bps), (5, 12, Some(800_000)));
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_while_downloading_keeps_progress_and_restarts_nothing() {
    wipe("switch");
    let m = open("switch").await;
    let data = random_data(8 * MB, 2);
    let srv = serve(data.clone()).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(20);
    // A profile of one's own with a single connection, to see the ceiling change while the download runs.
    let one = ProfileSettings { max_connections: Some(1), ..Default::default() };
    m.profile_action(ProfileAction::Create { name: "One".into(), settings: one }).await.unwrap();
    select(&m, "fast").await;
    let d = m.create_download(NewDownload { url: srv.url("/big.bin"), ..Default::default() }).await.unwrap();
    until("several connections", 20, async || m.get_download(d.id).await.unwrap().connections > 1).await;
    let before = m.get_download(d.id).await.unwrap().downloaded;
    let requests = srv.state.request_heads.lock().unwrap().len();
    select(&m, "c1").await;
    // Fewer connections while it runs (they give way; nothing restarts, nothing is lost).
    until("the lower ceiling", 20, async || m.get_download(d.id).await.unwrap().connections <= 1).await;
    let now = m.get_download(d.id).await.unwrap();
    assert_eq!(now.status, DownloadStatus::Downloading);
    assert!(now.downloaded >= before, "progress kept: {} < {before}", now.downloaded);
    let heads = srv.state.request_heads.lock().unwrap().clone();
    assert!(!heads[requests..].iter().any(|h| h.contains("bytes=0-")), "the file was not asked for from the start again");
    // And back to Fast: room to climb again.
    select(&m, "fast").await;
    assert_eq!(m.get_profiles().effective.max_connections, FAST_CONNECTIONS);
    let done = settle(&m, d.id).await;
    assert_eq!(done.status, DownloadStatus::Completed);
    assert_eq!(std::fs::read(&done.destination).unwrap(), *data, "byte-exact");
}

#[tokio::test]
async fn a_paused_download_stays_paused_and_low_resource_runs_one_at_a_time() {
    wipe("paused");
    let m = open("paused").await;
    let srv = serve(random_data(4 * MB, 3)).await;
    *srv.state.chunk_delay.lock().unwrap() = Duration::from_millis(15);
    m.set_queue_config(SetQueueConfig { max_active: Some(1), max_connections: None }).await.unwrap();
    let a = m.create_download(NewDownload { url: srv.url("/a.bin"), ..Default::default() }).await.unwrap();
    let b = m.create_download(NewDownload { url: srv.url("/b.bin"), ..Default::default() }).await.unwrap();
    until("a runs", 20, async || m.get_download(a.id).await.unwrap().status == DownloadStatus::Downloading).await;
    // b waits for a slot; the user pauses a.
    m.pause_download(a.id).await.unwrap();
    // Fast (more slots come from the user's own 1 here: Fast leaves it) and a custom profile with 4 slots.
    select(&m, "fast").await;
    let wide = ProfileSettings { max_active: Some(4), ..Default::default() };
    m.profile_action(ProfileAction::Create { name: "Wide".into(), settings: wide }).await.unwrap();
    select(&m, "c1").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(m.get_download(a.id).await.unwrap().status, DownloadStatus::Paused, "a manual pause is never undone");
    until("b, which was waiting, may start", 20, async || {
        matches!(m.get_download(b.id).await.unwrap().status, DownloadStatus::Downloading | DownloadStatus::Completed)
    })
    .await;
    // Low resource: one at a time. a is resumed by the user; b keeps its slot, a waits (nothing is stopped).
    select(&m, "lowResource").await;
    m.resume_download(a.id).await.unwrap();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(2) {
        let (sa, sb) = (m.get_download(a.id).await.unwrap().status, m.get_download(b.id).await.unwrap().status);
        assert!(!(sa == DownloadStatus::Downloading && sb == DownloadStatus::Downloading), "two at once under Low resource");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(settle(&m, a.id).await.status, DownloadStatus::Completed);
    assert_eq!(settle(&m, b.id).await.status, DownloadStatus::Completed);
}

#[tokio::test]
async fn balanced_uses_smart_throttle_without_touching_the_users_switches() {
    wipe("smart");
    let m = open("smart").await;
    let user = m.smart_throttle();
    assert!(!m.smart_throttle_enabled());
    select(&m, "balanced").await;
    let s = m.smart_throttle();
    assert!(m.smart_throttle_enabled(), "Balanced turns Smart throttle's automatic mode on");
    assert_eq!(s.profile_adaptive, Some(true));
    assert_eq!((s.enabled, s.auto), (user.enabled, user.auto), "the user's own switches are unchanged");
    select(&m, "fast").await;
    let s = m.smart_throttle();
    assert_eq!(s.profile_adaptive, Some(false));
    assert!(!m.smart_throttle_enabled(), "Fast does not turn Smart throttle on");
    select(&m, DEFAULT_ID).await;
    assert_eq!(m.smart_throttle().profile_adaptive, None);
    assert!(!m.smart_throttle_enabled());
    // A per-download limit is not a profile's business: it stays.
    let srv = serve(random_data(2 * MB, 4)).await;
    let d = m.create_download(NewDownload { url: srv.url("/x.bin"), start: Some(false), ..Default::default() }).await.unwrap();
    m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Download { id: d.id }, bytes_per_sec: Some(300_000) }).await.unwrap();
    select(&m, "fast").await;
    assert_eq!(m.get_download(d.id).await.unwrap().speed_limit, Some(300_000));
}

#[tokio::test]
async fn fast_is_full_speed_and_never_gives_way_not_even_to_a_listed_program() {
    wipe("fastfull");
    let m = open("fastfull").await;
    m.set_speed_limit(SetSpeedLimit { target: SpeedTarget::Global, bytes_per_sec: Some(400_000) }).await.unwrap();
    m.set_smart_throttle(SetSmartThrottle { enabled: Some(true), apps: Some(vec!["game".into()]), ..Default::default() }).await.unwrap();
    let game = vec!["game".to_string()];
    m.smart_throttle_observe(Some(&game));
    assert!(m.smart_throttle().limit_bps.is_some(), "the user's setup: the game slows downloads");
    select(&m, "fast").await;
    m.smart_throttle_observe(Some(&game));
    let s = m.smart_throttle();
    assert!(!m.smart_throttle_enabled() && s.limit_bps.is_none() && s.reason == "off", "Fast: no cap at all: {s:?}");
    assert!(s.enabled, "the user's own switch is kept");
    assert_eq!(m.get_profiles().effective.speed_limit_bps, None, "Fast: no speed limit either");
    // Back to Default: everything the user set applies again.
    select(&m, DEFAULT_ID).await;
    m.smart_throttle_observe(Some(&game));
    assert!(m.smart_throttle().limit_bps.is_some());
    assert_eq!(m.get_profiles().effective.speed_limit_bps, Some(400_000));
    m.shutdown().await;
}

#[tokio::test]
async fn profiles_survive_a_restart_and_bad_data_never_stops_the_app() {
    wipe("restart");
    let m = open("restart").await;
    m.set_queue_config(SetQueueConfig { max_active: Some(4), max_connections: None }).await.unwrap();
    let night = ProfileSettings { max_active: Some(2), max_connections: Some(8), ..Default::default() };
    m.profile_action(ProfileAction::Create { name: "Night".into(), settings: night }).await.unwrap();
    select(&m, "c1").await;
    // Invalid requests change nothing and are not saved.
    let bad = ProfileSettings { max_connections: Some(500), ..Default::default() };
    assert!(m.profile_action(ProfileAction::Create { name: "Bad".into(), settings: bad }).await.is_err());
    assert!(m.profile_action(ProfileAction::Delete { id: "fast".into() }).await.is_err());
    m.shutdown().await;
    drop(m);
    let m = open("restart").await;
    let p = m.get_profiles();
    assert_eq!(p.active, "c1");
    assert_eq!(p.profiles.iter().filter(|x| !x.built_in).count(), 1);
    assert_eq!((p.effective.max_active, p.effective.max_connections), (2, 8));
    // Deleting the active profile falls back to Default; the user's settings are intact.
    m.profile_action(ProfileAction::Delete { id: "c1".into() }).await.unwrap();
    let p = m.get_profiles();
    assert_eq!((p.active.as_str(), p.effective.max_active), (DEFAULT_ID, 4));
    m.shutdown().await;
    drop(m);

    // Corrupt saved data: the app starts on Default with the user's settings.
    let (db, _) = paths("restart");
    persistence::SqliteStore::open(&db).unwrap().set_setting("profiles", "{ this is not json").unwrap();
    let m = open("restart").await;
    let p = m.get_profiles();
    assert_eq!((p.active.as_str(), p.effective.max_active), (DEFAULT_ID, 4));
    assert_eq!(m.get_queue_state().await.unwrap().max_active, 4, "unrelated settings untouched");
    // A saved active profile that no longer exists, next to a valid one.
    m.shutdown().await;
    drop(m);
    let text = r#"{"version":1,"active":"c7","nextId":3,"custom":[{"id":"c2","name":"Kept","settings":{"maxActive":6}}]}"#;
    persistence::SqliteStore::open(&db).unwrap().set_setting("profiles", text).unwrap();
    let m = open("restart").await;
    let p = m.get_profiles();
    assert_eq!(p.active, DEFAULT_ID);
    assert!(p.profiles.iter().any(|x| x.name.as_deref() == Some("Kept")));
}
