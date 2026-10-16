//! VeloxFetch on the desktop (phases 16-18): the tray icon and its menu (with Game mode, feature #7), closing to the tray, starting hidden at
//! login, a notification when a download finishes while the window is not in front, and bringing the window back
//! (tray, dock, a second launch).
//!
//! The few texts shown here come from the same locale files as the UI (`src/locales/*.json`, embedded at build
//! time), in the language the UI reports. A desktop without a tray (some Linux desktops) never hides the window:
//! closing it then quits, as before.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use download_manager::{DownloadManager, DownloadStatus, ManagerEvent, MediaStatus, PauseQueue, SetGameMode};
use system_integration::system::{Notification, HIDDEN_ARG};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::authz;

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

const TRAY_ID: &str = "main";

/// Started at login with `--hidden`: stay in the tray until the user opens the window.
static START_HIDDEN: AtomicBool = AtomicBool::new(false);
/// The window was hidden to the tray once already this run (the "still running" hint is shown once).
static HINT_SHOWN: AtomicBool = AtomicBool::new(false);
/// Quit was chosen: closing the window must not hide it any more.
static QUITTING: AtomicBool = AtomicBool::new(false);
/// Game mode as the tray menu shows it (ticked), and that menu item.
static GAME_SHOWN: AtomicBool = AtomicBool::new(false);
static GAME_ITEM: Mutex<Option<CheckMenuItem<tauri::Wry>>> = Mutex::new(None);

/// The program to start at login: the AppImage file when running from one (its own folder vanishes when it
/// closes), else this program.
pub fn launcher() -> Option<PathBuf> {
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| cfg!(target_os = "linux") && p.is_absolute() && p.is_file());
    appimage.or_else(|| std::env::current_exe().ok())
}

/// The text for `key` in `lang`, else in English, else the key itself.
pub fn text(lang: &str, key: &str) -> String {
    let find = |code: &str| {
        LOCALES
            .iter()
            .find(|(c, _)| *c == code)
            .and_then(|(_, json)| serde_json::from_str::<HashMap<String, serde_json::Value>>(json).ok())
            .and_then(|m| m.get(key).and_then(|v| v.as_str()).map(str::to_string))
    };
    find(lang).or_else(|| find("en")).unwrap_or_else(|| key.to_string())
}

/// This desktop can show a tray icon. Linux needs the AppIndicator library (the tray code stops the program when
/// it is missing, so it is looked for first); Windows and macOS always can.
pub fn tray_possible() -> bool {
    #[cfg(target_os = "linux")]
    {
        ["libayatana-appindicator3.so.1", "libappindicator3.so.1", "libayatana-appindicator3.so", "libappindicator3.so"]
            .iter()
            // SAFETY: loading a system library by name only to see it exists; nothing from it is called here.
            .any(|name| unsafe { libloading::Library::new(name) }.is_ok())
    }
    #[cfg(not(target_os = "linux"))]
    true
}

/// Remember whether this run started hidden (only when there is a tray to come back from).
pub fn note_start_args(tray: bool) {
    let hidden = std::env::args().skip(1).any(|a| a == HIDDEN_ARG);
    START_HIDDEN.store(hidden && tray, Ordering::SeqCst);
}

pub fn starts_hidden() -> bool {
    START_HIDDEN.load(Ordering::SeqCst)
}

/// The UI has painted: show the window, unless this run started hidden in the tray (then only once it is opened).
pub fn window_ready(window: &tauri::WebviewWindow) {
    if START_HIDDEN.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// Show the main window, in front.
pub fn show_main(app: &AppHandle) {
    START_HIDDEN.store(false, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window(authz::MAIN) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// The user chose Quit (tray menu, or the app is ending): from now on closing really closes.
pub fn quitting() {
    QUITTING.store(true, Ordering::SeqCst);
}

/// The main window's close button: hide to the tray when the user wants that and there is a tray. Returns true when
/// the window was hidden (the close must then be prevented).
pub fn close_to_tray(app: &AppHandle, manager: &DownloadManager) -> bool {
    let prefs = manager.desktop_prefs();
    if QUITTING.load(Ordering::SeqCst) || !prefs.close_to_tray || !prefs.tray_available {
        return false;
    }
    if let Some(w) = app.get_webview_window(authz::MAIN) {
        let _ = w.hide();
    }
    if !HINT_SHOWN.swap(true, Ordering::SeqCst) && prefs.notify_finished {
        let lang = &prefs.language;
        notify(manager, &text(lang, "notify.inTrayTitle"), &text(lang, "notify.inTrayBody"));
    }
    true
}

fn notify(manager: &DownloadManager, title: &str, body: &str) {
    let system = manager.system();
    let n = Notification::new(title, body);
    std::thread::spawn(move || {
        if let Err(e) = system.0.notify(&n) {
            tracing::debug!(error = %e, "notification not shown");
        }
    });
}

/// Make the tray icon with its menu. Returns false when this desktop has no tray.
pub fn create_tray(app: &AppHandle, manager: &DownloadManager) -> bool {
    if !tray_possible() {
        tracing::info!("no tray on this desktop: closing the window quits");
        return false;
    }
    let lang = manager.desktop_prefs().language;
    GAME_SHOWN.store(manager.game_mode_on(), Ordering::Relaxed);
    let built = (|| -> tauri::Result<()> {
        let menu = tray_menu(app, &lang)?;
        let mut tray = TrayIconBuilder::with_id(TRAY_ID).menu(&menu).tooltip(tray_tooltip(&lang)).show_menu_on_left_click(false);
        if let Some(icon) = app.default_window_icon() {
            tray = tray.icon(icon.clone());
        }
        let m = manager.clone();
        tray.on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "pause" => {
                let m = m.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = m.pause_queue(PauseQueue { pause_running: true }).await;
                });
            }
            "resume" => {
                let m = m.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = m.resume_queue().await;
                });
            }
            // Game mode in one click, for the usual time (Settings › Speed); the same click turns it off.
            "game" => {
                let m = m.clone();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let on = !m.game_mode_on();
                    if let Err(e) = m.set_game_mode(SetGameMode { on: Some(on), ..Default::default() }).await {
                        tracing::warn!(error = %e, "game mode not changed from the tray");
                    }
                    sync_tray_game(&app, m.game_mode_on(), &m.desktop_prefs().language);
                });
            }
            "quit" => {
                quitting();
                app.exit(0);
            }
            _ => {}
        })
        // A click on the icon opens the window (Windows, macOS; Linux trays only have the menu).
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
        Ok(())
    })();
    match built {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "could not make the tray icon: closing the window quits");
            false
        }
    }
}

fn tray_menu(app: &AppHandle, lang: &str) -> tauri::Result<Menu<tauri::Wry>> {
    let item = |id: &str, key: &str| MenuItem::with_id(app, id, text(lang, key), true, None::<&str>);
    let game = CheckMenuItem::with_id(app, "game", text(lang, "tray.gameMode"), true, GAME_SHOWN.load(Ordering::Relaxed), None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &item("open", "tray.open")?,
            &PredefinedMenuItem::separator(app)?,
            &game,
            &PredefinedMenuItem::separator(app)?,
            &item("pause", "tray.pauseAll")?,
            &item("resume", "tray.resumeAll")?,
            &PredefinedMenuItem::separator(app)?,
            &item("quit", "tray.quit")?,
        ],
    )?;
    *GAME_ITEM.lock().unwrap_or_else(|p| p.into_inner()) = Some(game);
    Ok(menu)
}

/// The tray icon's tooltip: says Game mode while it is on.
fn tray_tooltip(lang: &str) -> String {
    if GAME_SHOWN.load(Ordering::Relaxed) {
        format!("VeloxFetch · {}", text(lang, "tray.gameModeOn"))
    } else {
        "VeloxFetch".to_string()
    }
}

/// The language changed: the tray menu follows.
pub fn relabel_tray(app: &AppHandle, lang: &str) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), tray_menu(app, lang)) {
        let _ = tray.set_menu(Some(menu));
        let _ = tray.set_tooltip(Some(tray_tooltip(lang)));
    }
}

/// Game mode changed (the tray, the window, its time ran out): the tray's tick and tooltip follow. Cheap when nothing
/// changed (called every few seconds).
pub fn sync_tray_game(app: &AppHandle, on: bool, lang: &str) {
    if GAME_SHOWN.swap(on, Ordering::Relaxed) == on {
        return;
    }
    if let Some(item) = GAME_ITEM.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        let _ = item.set_checked(on);
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tray_tooltip(lang)));
    }
}

/// Game mode's time ran out (not turned off by the user): say so, when notifications are on and the window is not in
/// front (the window shows it itself).
pub fn notify_game_mode_ended(app: &AppHandle, manager: &DownloadManager) {
    let prefs = manager.desktop_prefs();
    let in_front = app
        .get_webview_window(authz::MAIN)
        .is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) && w.is_focused().unwrap_or(false));
    tracing::info!(told = prefs.notify_finished && !in_front, in_front, "game mode ran out");
    if prefs.notify_finished && !in_front {
        notify(manager, &text(&prefs.language, "gm.endedTitle"), &text(&prefs.language, "gm.endedBody"));
    }
}

/// Linux / macOS: a second launch (the menu, the dock, a sandboxed browser through the desktop) asks the running copy
/// to show its window over this socket in the data folder. A connection carries nothing: it only means "show".
/// (Windows finds the running window directly, see `focus_running_instance`.)
#[cfg(unix)]
pub fn listen_for_show(app: AppHandle, data_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = show_socket(data_dir) else {
        tracing::warn!("a second launch cannot bring this window forward (no short enough folder for its socket)");
        return;
    };
    let _ = std::fs::remove_file(&path); // this copy owns the browser link: any socket here is a leftover
    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "a second launch cannot bring this window forward");
            return;
        }
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    std::thread::Builder::new()
        .name("show-window".into())
        .spawn(move || {
            for conn in listener.incoming() {
                drop(conn);
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || show_main(&handle));
            }
        })
        .ok();
}

/// A second launch: ask the running copy to show itself. True when it was reached.
#[cfg(unix)]
pub fn ask_running_copy_to_show(data_dir: &std::path::Path) -> bool {
    show_socket(data_dir).is_some_and(|p| std::os::unix::net::UnixStream::connect(p).is_ok())
}

#[cfg(unix)]
const SHOW_SOCKET: &str = "show.sock";

/// Where the show socket is: in the data folder, or (a very long folder path: Unix sockets allow ~100 characters)
/// in the user's private runtime folder. Both copies of the app work it out the same way.
#[cfg(unix)]
fn show_socket(data_dir: &std::path::Path) -> Option<PathBuf> {
    show_socket_in(data_dir, std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
}

#[cfg(unix)]
fn show_socket_in(data_dir: &std::path::Path, runtime_dir: Option<PathBuf>) -> Option<PathBuf> {
    const MAX: usize = if cfg!(target_os = "linux") { 107 } else { 103 };
    let fits = |p: &PathBuf| p.as_os_str().len() <= MAX;
    Some(data_dir.join(SHOW_SOCKET))
        .filter(fits)
        .or_else(|| runtime_dir.filter(|d| d.is_absolute()).map(|d| d.join("app.velox.fetch-show.sock")).filter(fits))
}

/// What a download's change means for the user: a notification title key, when it just ended.
pub fn ending(before: Option<DownloadStatus>, now: DownloadStatus) -> Option<&'static str> {
    let was_running =
        matches!(before, Some(s) if !matches!(s, DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled));
    match now {
        DownloadStatus::Completed if was_running => Some("notify.finishedTitle"),
        DownloadStatus::Failed if was_running => Some("notify.failedTitle"),
        _ => None,
    }
}

/// Notify when a download (or a stream job) ends while the window is hidden, minimised or behind another one.
pub fn watch_for_notifications(app: AppHandle, manager: DownloadManager) {
    let mut rx = manager.subscribe();
    let seen: Arc<Mutex<HashMap<i64, DownloadStatus>>> = Default::default();
    let media_seen: Arc<Mutex<HashMap<u64, MediaStatus>>> = Default::default();
    tauri::async_runtime::spawn(async move {
        loop {
            let ev = match rx.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let (key, name) = match ev {
                ManagerEvent::Added { download } | ManagerEvent::Updated { download } => {
                    let before = seen.lock().unwrap_or_else(|p| p.into_inner()).insert(download.id, download.status);
                    match ending(before, download.status) {
                        Some(key) => (key, download.filename),
                        None => continue,
                    }
                }
                ManagerEvent::Removed { id } => {
                    seen.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
                    continue;
                }
                ManagerEvent::Media { job } => {
                    let before = media_seen.lock().unwrap_or_else(|p| p.into_inner()).insert(job.id, job.status);
                    let running =
                        matches!(before, Some(s) if !matches!(s, MediaStatus::Completed | MediaStatus::Failed | MediaStatus::Cancelled));
                    match job.status {
                        MediaStatus::Completed if running => ("notify.finishedTitle", job.title),
                        MediaStatus::Failed if running => ("notify.failedTitle", job.title),
                        _ => continue,
                    }
                }
                _ => continue,
            };
            let prefs = manager.desktop_prefs();
            let in_front = app
                .get_webview_window(authz::MAIN)
                .is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) && w.is_focused().unwrap_or(false));
            if prefs.notify_finished && !in_front {
                notify(&manager, &text(&prefs.language, key), &name);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_has_the_desktop_texts_and_unknown_ones_fall_back_to_english() {
        assert!(LOCALES.iter().any(|(c, _)| *c == "en"));
        for key in [
            "tray.open",
            "tray.pauseAll",
            "tray.resumeAll",
            "tray.quit",
            "tray.gameMode",
            "tray.gameModeOn",
            "gm.endedTitle",
            "gm.endedBody",
            "notify.finishedTitle",
            "notify.failedTitle",
            "notify.inTrayTitle",
            "notify.inTrayBody",
        ] {
            let en = text("en", key);
            assert_ne!(en, key, "{key} missing from en.json");
            for (code, _) in LOCALES {
                assert_ne!(text(code, key), key, "{key} in {code}");
            }
            assert_eq!(text("xx", key), en);
        }
        assert_eq!(text("en", "no.such.key"), "no.such.key");
    }

    #[cfg(unix)]
    #[test]
    fn a_second_launch_reaches_the_running_copy_only_through_its_own_socket() {
        let dir = std::env::temp_dir().join(format!("velox-show-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!ask_running_copy_to_show(&dir), "nobody listening");
        let listener = std::os::unix::net::UnixListener::bind(show_socket(&dir).unwrap()).unwrap();
        assert!(ask_running_copy_to_show(&dir));
        drop(listener);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_very_long_data_folder_moves_the_show_socket_to_the_private_runtime_folder() {
        let short = std::path::Path::new("/home/u/.local/share/app.velox.fetch");
        assert_eq!(show_socket_in(short, Some("/run/user/1000".into())), Some(short.join(SHOW_SOCKET)));
        let long = PathBuf::from(format!("/home/u/{}", "d".repeat(120)));
        assert_eq!(show_socket_in(&long, Some("/run/user/1000".into())), Some(PathBuf::from("/run/user/1000/app.velox.fetch-show.sock")));
        assert_eq!(show_socket_in(&long, None), None, "no runtime folder: no socket rather than a shared one");
        assert_eq!(show_socket_in(&long, Some("relative".into())), None);
    }

    #[test]
    fn only_a_download_that_was_running_and_just_ended_is_announced() {
        use DownloadStatus::*;
        assert_eq!(ending(Some(Downloading), Completed), Some("notify.finishedTitle"));
        assert_eq!(ending(Some(Queued), Failed), Some("notify.failedTitle"));
        assert_eq!(ending(Some(Completed), Completed), None, "a finished download that changed again (renamed...)");
        assert_eq!(ending(None, Completed), None, "seen for the first time already finished (history)");
        assert_eq!(ending(Some(Downloading), Paused), None);
        assert_eq!(ending(Some(Downloading), Cancelled), None, "the user cancelled it themselves");
    }
}
