//! Starting the desktop app when a browser sends a download and VeloxFetch is closed.
//!
//! The helper finds the app by itself, never from a message:
//! * where each installer puts it relative to the helper (beside it on Windows and in portable folders, `bin/`
//!   next to `lib/VeloxFetch/` in the Linux packages, `MacOS/` next to `Resources/` in the macOS bundle);
//! * otherwise the path the app wrote in its data folder the last time it ran (an AppImage lives wherever the
//!   user put it, and its helper is a copy in the data folder).
//!
//! A browser installed as a snap or a Flatpak may not start programs outside its sandbox at all: there the helper
//! asks the desktop (the XDG desktop portal) to open `veloxfetch://start`, a link the app registers for itself.

use std::io;
use std::path::{Path, PathBuf};

/// The file in the app's data folder that holds the path to start the app with.
pub const APP_PATH_FILE: &str = "app-path";
/// The link scheme the app registers so the desktop can start it for a sandboxed browser.
pub const SCHEME: &str = "veloxfetch";
pub const START_URI: &str = "veloxfetch://start";

const APP_NAME: &str = if cfg!(windows) { "velox-desktop.exe" } else { "velox-desktop" };

/// Remember how to start this app (written by the app on every start; replaced atomically).
pub fn record_app(data_dir: &Path, app: &Path) -> io::Result<()> {
    if !app.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the app's path must be absolute"));
    }
    std::fs::create_dir_all(data_dir)?;
    let tmp = data_dir.join(format!(".{APP_PATH_FILE}.new"));
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, app.to_string_lossy().as_bytes())?;
    std::fs::rename(&tmp, data_dir.join(APP_PATH_FILE))
}

/// The app to start, for a helper in `host_dir` reading `data_dir`. `None` when none is found.
pub fn find_app(host_dir: &Path, data_dir: &Path) -> Option<PathBuf> {
    let beside = [
        host_dir.join(APP_NAME),
        host_dir.join("../../bin").join(APP_NAME), // Linux packages: /usr/lib/VeloxFetch -> /usr/bin
        host_dir.join("../MacOS").join(APP_NAME),  // macOS: Contents/Resources -> Contents/MacOS
    ];
    let recorded = std::fs::read_to_string(data_dir.join(APP_PATH_FILE))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_absolute() && !p.as_os_str().is_empty());
    beside.into_iter().chain(recorded).find(|p| p.is_file())
}

/// True when this helper runs inside a snap or a Flatpak (it can only reach the app through the desktop).
pub fn in_sandbox() -> bool {
    cfg!(target_os = "linux") && (std::env::var_os("SNAP_NAME").is_some() || Path::new("/.flatpak-info").exists())
}

/// Start `exe` on its own: it outlives the browser and the helper.
pub fn start_program(exe: &Path) -> io::Result<()> {
    let mut cmd = std::process::Command::new(exe);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Outlive the browser and the host; if the browser's job forbids breakaway, fall back.
        if cmd.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB).spawn().is_ok() {
            return Ok(());
        }
        cmd.creation_flags(DETACHED_PROCESS).spawn().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: a browser ending the helper's group never takes the app with it.
        cmd.process_group(0).spawn().map(|_| ())
    }
}

/// Ask the desktop to open [`START_URI`] (Linux: the XDG desktop portal, reachable from snaps and Flatpaks).
#[cfg(target_os = "linux")]
pub async fn start_through_desktop() -> io::Result<()> {
    use std::collections::HashMap;
    let fail = |e: zbus::Error| io::Error::other(e.to_string());
    let conn = zbus::Connection::session().await.map_err(fail)?;
    let options: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.OpenURI"),
        "OpenURI",
        &("", START_URI, options),
    )
    .await
    .map_err(fail)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub async fn start_through_desktop() -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "only Linux sandboxes start the app through the desktop"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-launch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"app").unwrap();
    }

    #[test]
    fn the_app_is_found_where_each_installer_puts_it() {
        let d = scratch("layouts");
        let data = d.join("data");

        // Windows installer, portable folders, dev builds: side by side.
        let win = d.join("win");
        touch(&win.join(APP_NAME));
        assert_eq!(find_app(&win, &data), Some(win.join(APP_NAME)));

        // .deb / .rpm: /usr/lib/VeloxFetch/velox-native-host and /usr/bin/velox-desktop.
        let usr = d.join("usr");
        std::fs::create_dir_all(usr.join("lib/VeloxFetch")).unwrap();
        touch(&usr.join("bin").join(APP_NAME));
        let found = find_app(&usr.join("lib/VeloxFetch"), &data).unwrap();
        assert_eq!(found.canonicalize().unwrap(), usr.join("bin").join(APP_NAME).canonicalize().unwrap());

        // macOS: VeloxFetch.app/Contents/Resources/velox-native-host and Contents/MacOS/velox-desktop.
        let contents = d.join("VeloxFetch.app/Contents");
        std::fs::create_dir_all(contents.join("Resources")).unwrap();
        touch(&contents.join("MacOS").join(APP_NAME));
        let found = find_app(&contents.join("Resources"), &data).unwrap();
        assert_eq!(found.canonicalize().unwrap(), contents.join("MacOS").join(APP_NAME).canonicalize().unwrap());

        // Nothing beside the helper and nothing recorded: nothing to start.
        let lone = d.join("lone");
        std::fs::create_dir_all(&lone).unwrap();
        assert_eq!(find_app(&lone, &data), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn an_app_that_moved_is_found_from_where_it_last_ran() {
        let d = scratch("recorded");
        let (data, host_dir) = (d.join("data"), d.join("data"));
        let appimage = d.join("Downloads/VeloxFetch_1.0_amd64.AppImage");
        touch(&appimage);

        assert!(record_app(&data, Path::new("relative/VeloxFetch.AppImage")).is_err(), "only absolute paths");
        record_app(&data, &appimage).unwrap();
        assert_eq!(find_app(&host_dir, &data), Some(appimage.clone()));

        // Deleted since: nothing is started.
        std::fs::remove_file(&appimage).unwrap();
        assert_eq!(find_app(&host_dir, &data), None);

        // A relative path written by hand is never used.
        std::fs::write(data.join(APP_PATH_FILE), "VeloxFetch.AppImage").unwrap();
        assert_eq!(find_app(&host_dir, &data), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[cfg(unix)]
    #[test]
    fn a_started_app_runs_on_its_own() {
        let d = scratch("start");
        let (marker, script) = (d.join("started"), d.join("fake-app.sh"));
        std::fs::write(&script, format!("#!/bin/sh\necho started > '{}'\n", marker.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        start_program(&script).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(marker.exists());
        assert!(start_program(&d.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    /// Real desktop: run by hand with a `veloxfetch` handler registered (it is started).
    #[cfg(target_os = "linux")]
    #[ignore]
    #[tokio::test]
    async fn the_desktop_is_asked_to_start_the_app() {
        start_through_desktop().await.unwrap();
    }
}
