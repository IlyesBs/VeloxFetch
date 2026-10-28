//! Phases 16-18: how the app behaves on the desktop (close to tray, start at login, notifications, language).
//! Everything goes through the pretend system: nothing here touches the machine's login items.

use std::path::PathBuf;
use std::sync::Arc;

use download_manager::{DownloadManager, LoginStart, ManagerConfig, ManagerError, SetDesktopPrefs, SystemHandle};
use system_integration::system::SystemCall;
use system_integration::{DryRunSystem, SystemIntegration};

fn config(name: &str, launcher: Option<&str>, system: Arc<DryRunSystem>) -> ManagerConfig {
    let base = std::env::temp_dir().join(format!("velox-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut cfg = ManagerConfig::new(base.join("state/velox.db"), base.join("downloads"));
    cfg.system = SystemHandle(system);
    cfg.app_launcher = launcher.map(PathBuf::from);
    cfg
}

#[tokio::test]
async fn desktop_defaults_keep_the_app_in_the_tray_and_never_start_it_at_login_on_their_own() {
    let system = Arc::new(DryRunSystem::new());
    let m = DownloadManager::new(config("defaults", Some("/opt/VeloxFetch/velox-desktop"), system.clone())).await.unwrap();
    let p = m.desktop_prefs();
    assert!(p.close_to_tray && p.notify_finished && !p.start_at_login);
    assert_eq!(p.login_start, LoginStart::Tray, "started at login: hidden in the tray, as before the choice existed");
    assert_eq!(p.language, "en");
    assert!(p.start_at_login_available);
    assert!(!p.tray_available, "unknown until the app has made its icon");
    m.set_tray_available(true);
    assert!(m.desktop_prefs().tray_available);
    m.refresh_start_at_login();
    assert!(system.calls().iter().all(|c| !matches!(c, SystemCall::StartAtLogin(_))), "nothing set up without the user");
    m.shutdown().await;
}

#[tokio::test]
async fn starting_at_login_is_set_up_removed_and_kept_pointing_at_the_app() {
    let system = Arc::new(DryRunSystem::new());
    let exe = "/home/u/Apps/VeloxFetch.AppImage";
    let m = DownloadManager::new(config("login", Some(exe), system.clone())).await.unwrap();

    let p = m.set_desktop_prefs(SetDesktopPrefs { start_at_login: Some(true), ..Default::default() }).await.unwrap();
    assert!(p.start_at_login);
    assert_eq!(system.calls().last(), Some(&SystemCall::StartAtLogin(Some(PathBuf::from(exe)))));

    // The entry was removed behind the app's back (system settings): shown off, and put back at the next start.
    system.set_start_at_login(None).unwrap();
    assert!(!m.desktop_prefs().start_at_login);
    m.refresh_start_at_login();
    assert!(m.desktop_prefs().start_at_login, "the user's choice is kept");

    let p = m.set_desktop_prefs(SetDesktopPrefs { start_at_login: Some(false), ..Default::default() }).await.unwrap();
    assert!(!p.start_at_login);
    m.refresh_start_at_login();
    assert!(!m.desktop_prefs().start_at_login, "turned off stays off");
    m.shutdown().await;
}

#[tokio::test]
async fn desktop_choices_survive_a_restart_and_bad_values_are_refused() {
    let system = Arc::new(DryRunSystem::new());
    let cfg = config("persist", None, system.clone());
    let (db, root) = (cfg.db_path.clone(), cfg.download_root.clone());
    let m = DownloadManager::new(cfg).await.unwrap();
    assert!(!m.desktop_prefs().start_at_login_available);
    let refused = m.set_desktop_prefs(SetDesktopPrefs { start_at_login: Some(true), ..Default::default() }).await;
    assert!(matches!(refused, Err(ManagerError::InvalidInput(_))), "no program to start: not offered");
    for bad in ["../../etc", "EN", "english", ""] {
        let r = m.set_desktop_prefs(SetDesktopPrefs { language: Some(bad.into()), ..Default::default() }).await;
        assert!(matches!(r, Err(ManagerError::InvalidInput(_))), "{bad:?}");
    }
    let p = m
        .set_desktop_prefs(SetDesktopPrefs {
            close_to_tray: Some(false),
            notify_finished: Some(false),
            language: Some("ar".into()),
            login_start: Some(LoginStart::Minimized),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!p.close_to_tray && !p.notify_finished);
    assert_eq!(p.language, "ar");
    assert_eq!(p.login_start, LoginStart::Minimized);
    m.shutdown().await;

    let mut cfg = ManagerConfig::new(db, root);
    cfg.system = SystemHandle(system);
    let m = DownloadManager::new(cfg).await.unwrap();
    let p = m.desktop_prefs();
    assert!(!p.close_to_tray && !p.notify_finished);
    assert_eq!(p.language, "ar");
    assert_eq!(p.login_start, LoginStart::Minimized, "the way to start at login survives a restart");
    m.shutdown().await;
}

#[test]
fn only_the_three_ways_to_start_are_accepted_from_the_window() {
    for (json, want) in [
        (r#"{"loginStart":"tray"}"#, LoginStart::Tray),
        (r#"{"loginStart":"minimized"}"#, LoginStart::Minimized),
        (r#"{"loginStart":"window"}"#, LoginStart::Window),
    ] {
        let req: SetDesktopPrefs = serde_json::from_str(json).unwrap();
        assert_eq!(req.login_start, Some(want));
    }
    for bad in [r#"{"loginStart":"hidden"}"#, r#"{"loginStart":"Tray"}"#, r#"{"loginStart":1}"#, r#"{"loginStart":"tray","x":1}"#] {
        assert!(serde_json::from_str::<SetDesktopPrefs>(bad).is_err(), "{bad}");
    }
}
