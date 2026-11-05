//! The download indicator: a small window on the desktop, over the other programs, that shows the downloads in
//! progress (the first one, and the others behind an arrow). It is there whenever something downloads, also while
//! the main window is in the tray, minimised or behind other programs, and goes when nothing downloads any more
//! (the window is closed then: no web page is kept in memory while the app waits; the next downloads open it again).
//!
//! - Rust decides when it shows: it follows the download events (`running_change`). Its page draws it, sizes it
//!   (`window_control` `fit`, its height; the bottom edge stays where it is, so the list unfolds upwards) and can
//!   hide it until the next downloads (`dismiss`).
//! - The user drags it anywhere (the page starts the move). Where it was left is remembered (`x`, bottom edge) and
//!   used the next time, if that place is still on a screen; the first time it sits in the bottom corner of the
//!   screen, on the side of the time and the tray.
//! - It never takes the keyboard from the program in front, has no taskbar button, and no frame: the page is the
//!   bar, rounded, on a see-through window.
//! - Settings > On the desktop turns it off (`desktop.indicator`).

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use download_manager::{DownloadManager, DownloadStatus, ManagerEvent, MediaStatus};
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::authz;

/// The bar's width (CSS pixels); its height is what it shows.
const WIDTH: f64 = 440.0;
/// Its height before the page has measured itself (never shown at this size).
const START_HEIGHT: f64 = 76.0;
/// Never shorter than this (CSS pixels): the bar alone is about 78.
const MIN_HEIGHT: f64 = 40.0;
/// The space kept between the bar and the edges of the screen the first time.
const MARGIN: i32 = 16;
/// Once nothing downloads, the page slides the bar away; the window goes after that.
const HIDE_AFTER: Duration = Duration::from_millis(400);
/// A drag sends many moves: the place is written once it has stopped for this long.
const SAVE_AFTER: Duration = Duration::from_millis(600);

/// A download or a video download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Running {
    Download(i64),
    Media(u64),
}

/// What an event changes in the set of running downloads: `(which, running now)`. The same statuses as the bar's
/// page (indicator.ts): a download downloading or checking its file, a video preparing, downloading or finishing.
pub fn running_change(ev: &ManagerEvent) -> Option<(Running, bool)> {
    match ev {
        ManagerEvent::Added { download } | ManagerEvent::Updated { download } => {
            Some((Running::Download(download.id), download_runs(download.status)))
        }
        ManagerEvent::Removed { id } => Some((Running::Download(*id), false)),
        ManagerEvent::Media { job } => Some((Running::Media(job.id), media_runs(job.status))),
        ManagerEvent::MediaRemoved { id } => Some((Running::Media(*id), false)),
        _ => None,
    }
}

fn download_runs(status: DownloadStatus) -> bool {
    matches!(status, DownloadStatus::Downloading | DownloadStatus::Verifying)
}

fn media_runs(status: MediaStatus) -> bool {
    matches!(status, MediaStatus::Preparing | MediaStatus::Downloading | MediaStatus::Combining)
}

#[derive(Default)]
struct State {
    running: HashSet<Running>,
    /// Hidden by the user (its ×) until nothing downloads.
    dismissed: bool,
    /// Bumped at every change: a pending hide only acts if nothing changed since.
    generation: u64,
    /// Bumped at every move: the place is written once the moves stop.
    moves: u64,
}

static STATE: std::sync::LazyLock<Mutex<State>> = std::sync::LazyLock::new(Mutex::default);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Follow a download event: show the bar when something starts, hide it when nothing runs any more.
pub fn on_event(app: &AppHandle, manager: &DownloadManager, ev: &ManagerEvent) {
    let Some((which, on)) = running_change(ev) else { return };
    let (changed, any) = {
        let mut s = state();
        let changed = if on { s.running.insert(which) } else { s.running.remove(&which) };
        if changed {
            s.generation += 1;
        }
        (changed, !s.running.is_empty())
    };
    if changed {
        refresh(app, manager, any);
    }
}

/// Show or hide the bar for the current state (after an event, or a change of the setting).
pub fn refresh(app: &AppHandle, manager: &DownloadManager, any: bool) {
    if any {
        let dismissed = state().dismissed;
        if manager.desktop_prefs().indicator && !dismissed {
            if let Err(e) = show(app, manager) {
                tracing::warn!(error = %e, "could not show the download indicator");
            }
        } else {
            hide(app);
        }
        return;
    }
    // Nothing runs: the next downloads show the bar again, even if it was hidden by hand. Its page slides it away
    // first, then the window closes (unless something started meanwhile).
    let generation = {
        let mut s = state();
        s.dismissed = false;
        s.generation
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(HIDE_AFTER).await;
        if state().generation == generation {
            close(&app);
        }
    });
}

/// Whether anything downloads now (as far as the events said).
pub fn any_running() -> bool {
    !state().running.is_empty()
}

fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(authz::INDICATOR) {
        let _ = w.hide();
    }
}

/// The user's × on the bar: hidden until nothing downloads (the next downloads bring it back).
pub fn dismiss(w: &WebviewWindow) -> tauri::Result<()> {
    state().dismissed = true;
    w.hide()
}

/// Show the bar: made the first time (hidden; its page shows it once it has measured itself, see `fit`), shown
/// again as it was otherwise. Never takes the keyboard from the program in front.
fn show(app: &AppHandle, manager: &DownloadManager) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window(authz::INDICATOR) {
        if !w.is_visible().unwrap_or(true) && w.outer_size().is_ok_and(|s| s.height > 1) {
            w.show()?;
        }
        return Ok(());
    }
    let builder = tauri::WebviewWindowBuilder::new(app, authz::INDICATOR, tauri::WebviewUrl::App("index.html".into()))
        .title("VeloxFetch")
        .inner_size(WIDTH, START_HEIGHT)
        // Linux (GTK): a window that cannot be resized keeps its web view at least 200 pixels tall, more than the bar.
        // There is no frame to resize it by anyway; its page sets its height (`fit`).
        .min_inner_size(WIDTH, MIN_HEIGHT)
        .resizable(true)
        .maximizable(false)
        .minimizable(false)
        .decorations(false)
        .background_color(tauri::webview::Color(0, 0, 0, 0))
        .shadow(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .disable_drag_drop_handler();
    // See-through around the rounded bar. macOS needs private APIs for that (not used): there the window is the bar's
    // rectangle.
    #[cfg(not(target_os = "macos"))]
    let builder = builder.transparent(true);
    let w = crate::chrome_script(crate::e2e_browser_args(builder)).build()?;
    place(&w, manager, None)?;
    Ok(())
}

/// The screen area (without the taskbar / panels) the bar is on, else the main screen's.
fn work_area(w: &WebviewWindow) -> Option<tauri::PhysicalRect<i32, u32>> {
    let monitor = w.current_monitor().ok().flatten().or_else(|| w.primary_monitor().ok().flatten())?;
    Some(*monitor.work_area())
}

/// Is this point on one of the screens (the place remembered may be on a screen that is gone)?
fn on_a_screen(w: &WebviewWindow, x: i32, y: i32) -> bool {
    w.available_monitors().unwrap_or_default().iter().any(|m| {
        let a = m.work_area();
        x >= a.position.x && y >= a.position.y && x < a.position.x + a.size.width as i32 && y <= a.position.y + a.size.height as i32
    })
}

/// Put the bar where the user left it, else in the bottom corner of the screen (where the time and the tray are:
/// the end side of the screen). `height`: its new height in screen pixels (its current one when `None`).
fn place(w: &WebviewWindow, manager: &DownloadManager, height: Option<u32>) -> tauri::Result<()> {
    // Just made, a window may not know its size yet (0 x 0 before it is first shown): its width is known anyway.
    let scale = w.scale_factor().unwrap_or(1.0);
    let size = tauri::PhysicalSize::new((WIDTH * scale).round() as u32, w.outer_size().map_or(0, |s| s.height));
    let h = height.unwrap_or(size.height) as i32;
    let (x, bottom) = match manager.indicator_place() {
        Some((x, bottom)) if on_a_screen(w, x + MARGIN, bottom - MARGIN) => (x, bottom),
        _ => match work_area(w) {
            Some(a) => (a.position.x + a.size.width as i32 - size.width as i32 - MARGIN, a.position.y + a.size.height as i32 - MARGIN),
            None => (MARGIN, h + MARGIN),
        },
    };
    w.set_position(tauri::PhysicalPosition::new(x, (bottom - h).max(0)))
}

/// The bar's page measured itself (`height`, CSS pixels): take that height, keeping the bottom edge where it is (the
/// list unfolds upwards) and the bar on its screen. The first time, put it in its place and show it.
pub fn fit(w: &WebviewWindow, height: f64) -> tauri::Result<()> {
    if !height.is_finite() || height < 8.0 {
        return Ok(());
    }
    let scale = w.scale_factor()?;
    let size = w.outer_size()?;
    let area = work_area(w);
    let max = area.map_or(f64::from(u32::MAX), |a| f64::from(a.size.height) * 0.9);
    let new_h = (height.max(MIN_HEIGHT) * scale).round().clamp(1.0, max) as u32;
    let first = !w.is_visible()?;
    if new_h.abs_diff(size.height) > 1 {
        w.set_size(tauri::PhysicalSize::new(size.width, new_h))?;
    }
    if first {
        if let Some(state) = w.try_state::<crate::AppState>() {
            place(w, &state.manager, Some(new_h))?;
        }
        // Only while something downloads (the page may answer after the last one ended), and not if hidden by hand.
        let show = {
            let s = state();
            !s.running.is_empty() && !s.dismissed
        };
        if show && w.try_state::<crate::AppState>().is_some_and(|s| s.manager.desktop_prefs().indicator) {
            w.show()?;
            // A window not yet on screen may not take a new size (GTK): once more, now that it is.
            if w.outer_size().is_ok_and(|s| s.height.abs_diff(new_h) > 1) {
                w.set_size(tauri::PhysicalSize::new(size.width.max(1), new_h))?;
                if let Some(state) = w.try_state::<crate::AppState>() {
                    place(w, &state.manager, Some(new_h))?;
                }
            }
        }
        return Ok(());
    }
    let pos = w.outer_position()?;
    let bottom = pos.y + size.height as i32;
    let mut y = bottom - new_h as i32;
    if let Some(a) = area {
        y = y.max(a.position.y).min(a.position.y + a.size.height as i32 - new_h as i32);
    }
    if y != pos.y {
        w.set_position(tauri::PhysicalPosition::new(pos.x, y))?;
    }
    Ok(())
}

/// The bar moved (dragged by the user, or grown by `fit`): remember where, once it stops moving.
pub fn moved(w: &tauri::Window) {
    if !w.is_visible().unwrap_or(false) {
        return; // placed by `show`, not by the user
    }
    let moves = {
        let mut s = state();
        s.moves += 1;
        s.moves
    };
    let w = w.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SAVE_AFTER).await;
        if state().moves != moves {
            return;
        }
        let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) else { return };
        if let Some(state) = w.try_state::<crate::AppState>() {
            let manager = state.manager.clone();
            let _ = tokio::task::spawn_blocking(move || manager.set_indicator_place(pos.x, pos.y + size.height as i32)).await;
        }
    });
}

/// Close the bar's window: nothing downloads any more, or the main window is gone for good (the app is ending, and
/// the bar must not keep it running).
pub fn close(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(authz::INDICATOR) {
        let _ = w.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_running_downloads_and_videos_count() {
        use DownloadStatus::*;
        for (status, on) in [
            (Downloading, true),
            (Verifying, true),
            (Queued, false),
            (Paused, false),
            (Completed, false),
            (Failed, false),
            (Cancelled, false),
        ] {
            assert_eq!(download_runs(status), on, "{status:?}");
        }
        for (status, on) in [
            (MediaStatus::Preparing, true),
            (MediaStatus::Downloading, true),
            (MediaStatus::Combining, true),
            (MediaStatus::Queued, false),
            (MediaStatus::Paused, false),
            (MediaStatus::Completed, false),
            (MediaStatus::Failed, false),
            (MediaStatus::Cancelled, false),
        ] {
            assert_eq!(media_runs(status), on, "{status:?}");
        }
    }

    #[test]
    fn removed_ones_stop_and_other_events_change_nothing() {
        assert_eq!(running_change(&ManagerEvent::Removed { id: 7 }), Some((Running::Download(7), false)));
        assert_eq!(running_change(&ManagerEvent::MediaRemoved { id: 3 }), Some((Running::Media(3), false)));
        assert_eq!(running_change(&ManagerEvent::PendingRemoved { token: 1 }), None);
        assert_ne!(Running::Download(1), Running::Media(1), "a download and a video with the same number differ");
    }
}
