//! Clipboard detection: when the user copies a download link, tell the UI so it can offer it.
//!
//! Privacy comes first, because the clipboard often holds secrets:
//! * it is **off by default** and only reads the clipboard after the user turned it on in Settings;
//! * only *text* is read, only to check whether it consists of nothing but download links (strict
//!   rules in `download_manager::batch::detect_download_links`); everything else is dropped at once;
//! * nothing is stored or logged: the watcher keeps a hash of the last text to notice changes;
//! * what was on the clipboard when watching started is never reported (only later copies are);
//! * while the VeloxFetch window has the focus nothing is reported (copying inside the app is not a
//!   request).
//!
//! Works on Windows, macOS and Linux through `system-integration`. Where the OS does not let a
//! background program read the clipboard (some Wayland compositors), the read fails, the state
//! becomes "unavailable" and the UI says so instead of pretending.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use download_manager::{ApiError, DownloadManager, ManagerError};
use serde::Serialize;
use system_integration::clipboard::ClipboardSource;
use tauri::{AppHandle, Emitter, Manager, UserAttentionType};

/// Event the UI listens on.
pub const CLIPBOARD_EVENT: &str = "velox://clipboard";
const POLL: Duration = Duration::from_millis(800);
/// Consecutive read failures before the watcher reports itself unavailable.
const UNAVAILABLE_AFTER: u32 = 5;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardState {
    /// The user turned detection on.
    pub enabled: bool,
    /// The operating system lets us read the clipboard.
    pub available: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Found {
    links: Vec<String>,
}

#[derive(Clone)]
pub struct ClipboardWatch {
    enabled: Arc<AtomicBool>,
    available: Arc<AtomicBool>,
}

fn hash_of(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl ClipboardWatch {
    /// Start the background reader. `ignore_focus` is for tests that drive the app in the foreground.
    pub fn start(app: AppHandle, manager: DownloadManager, mut source: Box<dyn ClipboardSource>, ignore_focus: bool) -> Self {
        let watch = Self { enabled: Arc::new(AtomicBool::new(manager.clipboard_watch())), available: Arc::new(AtomicBool::new(true)) };
        let (enabled, available) = (watch.enabled.clone(), watch.available.clone());
        let spawned = std::thread::Builder::new().name("clipboard-watch".into()).spawn(move || {
            let mut last: Option<u64> = None;
            let mut armed = false;
            let mut failures = 0u32;
            loop {
                std::thread::sleep(POLL);
                if !enabled.load(Ordering::Relaxed) {
                    armed = false; // when it is turned on again, the current content is the new baseline
                    continue;
                }
                let text = match source.read_text() {
                    Ok(t) => {
                        failures = 0;
                        available.store(true, Ordering::Relaxed);
                        t.unwrap_or_default()
                    }
                    Err(e) => {
                        failures += 1;
                        if failures == UNAVAILABLE_AFTER && available.swap(false, Ordering::Relaxed) {
                            tracing::warn!(error = %e, "the clipboard cannot be read here; clipboard detection is unavailable");
                        }
                        continue;
                    }
                };
                let h = hash_of(&text);
                if !armed {
                    armed = true;
                    last = Some(h);
                    continue;
                }
                if last == Some(h) {
                    continue;
                }
                last = Some(h);
                let window = app.get_webview_window(crate::authz::MAIN);
                if !ignore_focus && window.as_ref().and_then(|w| w.is_focused().ok()).unwrap_or(false) {
                    continue;
                }
                let links = tauri::async_runtime::block_on(manager.new_download_links(&text));
                drop(text);
                if links.is_empty() {
                    continue;
                }
                if let Some(w) = &window {
                    let _ = app.emit_to(w.label(), CLIPBOARD_EVENT, Found { links });
                    // Taskbar flash / dock bounce / urgency hint: noticeable without stealing the focus.
                    let _ = w.request_user_attention(Some(UserAttentionType::Informational));
                }
            }
        });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start the clipboard watcher");
            watch.available.store(false, Ordering::Relaxed);
        }
        watch
    }

    pub fn state(&self) -> ClipboardState {
        ClipboardState { enabled: self.enabled.load(Ordering::Relaxed), available: self.available.load(Ordering::Relaxed) }
    }

    pub async fn set(&self, manager: &DownloadManager, on: bool) -> Result<ClipboardState, ApiError> {
        manager.set_clipboard_watch(on).await.map_err(|e: ManagerError| ApiError::from(e))?;
        self.enabled.store(on, Ordering::Relaxed);
        Ok(self.state())
    }
}
