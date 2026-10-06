//! Browser integration, kept apart from the downloader:
//!
//! ```text
//! Chrome / Firefox -> extension -> native messaging host -> [this crate's local link] -> app
//! ```
//!
//! This crate contains what both ends share and nothing about downloading itself:
//! * [`frame`]: length-prefixed JSON frames with hard size limits;
//! * [`protocol`]: the closed command set and its strict validation;
//! * [`caller`]: which extensions may talk to us (identified from the host's command line);
//! * [`auth`]: mutual challenge-response over a per-run token (the token never crosses the link);
//! * [`transport`], [`server`], [`client`]: the per-user pipe / socket between host and app.
//!
//! The app implements [`server::Handler`] on top of its download manager.

pub mod auth;
pub mod caller;
pub mod client;
pub mod frame;
pub mod host;
pub mod protocol;
pub mod server;
pub mod transport;

pub use auth::Token;
pub use caller::{chromium_ids, Allowlist, Caller, CallerError, CHROME_EXTENSION_ID, CHROME_STORE_ID, FIREFOX_EXTENSION_ID, HOST_NAME};
pub use client::{Client, ClientError};
pub use protocol::{DownloadRequest, MediaHint, Message, ProtocolError, Reply, Source};
pub use server::{serve, DownloadAccepted, Handler, HandlerError, ServerConfig};
pub use transport::{owned_by_another_instance, Endpoint, Listener};

/// The app's data directory as the host must find it (it has no Tauri to ask). Matches Tauri's
/// `app_data_dir()` for the identifier `app.velox.fetch`. Debug builds honour `VELOX_DATA_DIR`
/// (used by the end-to-end tests); release builds never do.
pub fn data_dir() -> Option<std::path::PathBuf> {
    data_dir_from(|name| std::env::var_os(name))
}

/// [`data_dir`] with the environment given (tests).
fn data_dir_from(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if cfg!(debug_assertions) {
        if let Some(d) = env("VELOX_DATA_DIR") {
            return Some(d.into());
        }
    }
    let base = if cfg!(windows) {
        env("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else if let Some(common) = env("SNAP_USER_COMMON").filter(|_| env("SNAP_NAME").is_some()) {
        // Started by a browser installed as a snap: only its own folder is reachable, and the app serves the link
        // there (see `system_integration::sandboxed_browsers`). Inside a Flatpak, XDG_DATA_HOME below already is the
        // browser's own data folder.
        Some(PathBuf::from(common))
    } else {
        env("XDG_DATA_HOME").map(PathBuf::from).or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.map(|b| b.join("app.velox.fetch"))
}

#[cfg(all(test, target_os = "linux"))]
mod data_dir_tests {
    use std::path::PathBuf;

    fn with(vars: &[(&str, &str)]) -> Option<PathBuf> {
        super::data_dir_from(|n| vars.iter().find(|(k, _)| *k == n).map(|(_, v)| v.into()))
    }

    #[test]
    fn the_helper_finds_the_link_inside_and_outside_sandboxes() {
        assert_eq!(with(&[("HOME", "/home/u")]), Some(PathBuf::from("/home/u/.local/share/app.velox.fetch")));
        // Flatpak: XDG_DATA_HOME is the browser's own data folder.
        assert_eq!(
            with(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/home/u/.var/app/com.brave.Browser/data")]),
            Some(PathBuf::from("/home/u/.var/app/com.brave.Browser/data/app.velox.fetch"))
        );
        // Snap: its common folder.
        assert_eq!(
            with(&[("HOME", "/home/u/snap/firefox/7"), ("SNAP_NAME", "firefox"), ("SNAP_USER_COMMON", "/home/u/snap/firefox/common")]),
            Some(PathBuf::from("/home/u/snap/firefox/common/app.velox.fetch"))
        );
        // SNAP_USER_COMMON alone (not started by a snap) changes nothing.
        assert_eq!(with(&[("HOME", "/home/u"), ("SNAP_USER_COMMON", "/x")]), Some(PathBuf::from("/home/u/.local/share/app.velox.fetch")));
    }
}
