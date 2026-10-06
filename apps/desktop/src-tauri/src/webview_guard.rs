//! Keep the window alive when the web view under it dies.
//!
//! On Windows the UI is drawn by a WebView2 browser process. Third-party programs that inject themselves
//! into every process (seen: the RivaTuner Statistics Server overlay, `RTSSHooks64.dll`, which comes with
//! MSI Afterburner) can crash that process, often right after start. Without this guard the window then
//! stays an empty dark rectangle until the user kills the app: the "black screen".
//!
//! * the page's renderer died: reload the page (the engine and the downloads are not affected);
//! * the whole browser process died: the caller rebuilds the window, at most a few times in a row.
//!
//! Other platforms: nothing to do here (no separate browser process to lose).

/// What the caller must do after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))] // only the Windows guard reports failures
pub enum Recovery {
    /// The web view is gone for good: destroy the window and create a new one.
    RebuildWindow,
}

#[cfg(windows)]
pub fn watch<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>, on_failure: impl Fn(Recovery) + Send + 'static) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
    };
    use webview2_com::ProcessFailedEventHandler;

    let registered = window.with_webview(move |pw| {
        // SAFETY: plain COM calls on the live controller, on the thread that owns it (with_webview runs
        // the closure on the main thread).
        let core = match unsafe { pw.controller().CoreWebView2() } {
            Ok(core) => core,
            Err(e) => {
                tracing::warn!(error = %e, "web view guard: no web view to watch");
                return;
            }
        };
        let handler = ProcessFailedEventHandler::create(Box::new(move |sender, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            // SAFETY: out-parameter of a COM getter on the event's own arguments.
            unsafe { args.ProcessFailedKind(&mut kind)? };
            if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
                tracing::warn!("the window's page stopped (renderer process exited): reloading it");
                if let Some(webview) = sender {
                    // SAFETY: COM call on the web view that raised the event, on its own thread.
                    let _ = unsafe { webview.Reload() };
                }
            } else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
                tracing::warn!("the window's web view stopped (browser process exited): rebuilding the window");
                on_failure(Recovery::RebuildWindow);
            } else {
                // GPU and helper processes are restarted by WebView2 itself; an unresponsive page may recover.
                tracing::info!(kind = kind.0, "a web view helper process stopped; WebView2 handles it");
            }
            Ok(())
        }));
        let mut token = 0i64;
        // SAFETY: registers our handler on the live web view; the token is a plain out-parameter.
        if let Err(e) = unsafe { core.add_ProcessFailed(&handler, &mut token) } {
            tracing::warn!(error = %e, "web view guard: could not watch the web view");
        }
    });
    if let Err(e) = registered {
        tracing::warn!(error = %e, "web view guard: could not reach the web view");
    }
}

#[cfg(not(windows))]
pub fn watch<R: tauri::Runtime>(_window: &tauri::WebviewWindow<R>, _on_failure: impl Fn(Recovery) + Send + 'static) {}

/// Allows a rebuild unless there were already `max` in the last `window`: a web view that dies again and
/// again (a broken runtime, an overlay that crashes it every time) must not turn into an endless loop.
#[derive(Debug, Default)]
pub struct RebuildBudget {
    pub(crate) recent: std::collections::VecDeque<std::time::Instant>,
}

impl RebuildBudget {
    pub fn allow(&mut self, now: std::time::Instant, max: usize, window: std::time::Duration) -> bool {
        while self.recent.front().is_some_and(|t| now.duration_since(*t) > window) {
            self.recent.pop_front();
        }
        if self.recent.len() >= max {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn rebuilds_are_limited_per_time_window() {
        let mut b = RebuildBudget::default();
        let t = Instant::now();
        let w = Duration::from_secs(120);
        assert!(b.allow(t, 3, w));
        assert!(b.allow(t + Duration::from_secs(1), 3, w));
        assert!(b.allow(t + Duration::from_secs(2), 3, w));
        assert!(!b.allow(t + Duration::from_secs(3), 3, w), "a fourth crash in a row is not rebuilt");
        assert!(b.allow(t + Duration::from_secs(200), 3, w), "later on it is allowed again");
    }
}
