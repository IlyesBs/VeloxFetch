//! Browsers installed in a sandbox (Linux snap and Flatpak packages: Ubuntu's Firefox and Chromium, Flathub's
//! Chrome, Brave, Edge, Firefox...).
//!
//! Such a browser never sees the normal registration (`~/.config/<browser>/NativeMessagingHosts`,
//! `~/.mozilla/native-messaging-hosts`) nor the app's data folder, and it can only start programs from inside its
//! own folder. So for each one found, everything the helper needs is put in the browser's own folder:
//! * a copy of the helper program (built statically on Linux, so it runs on the sandbox's own libraries);
//! * the registration file, pointing at that copy;
//! * the app serves its local link there too, with a copy of the access token (see the desktop bridge).
//!
//! The helper finds that folder by itself: inside a Flatpak `XDG_DATA_HOME` already is the app's own data folder,
//! inside a snap `SNAP_USER_COMMON` is (see `browser_bridge::data_dir`). The same rules as a normal registration
//! apply: per user, only on the user's request, exactly the shipped extension ids.

use std::io;
use std::path::{Path, PathBuf};

use crate::native_messaging::{manifest_json, Browser, HostSpec};

/// The data folder's name inside a sandbox, as `browser_bridge::data_dir` builds it.
pub const DATA_DIR_NAME: &str = "app.velox.fetch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
    Snap,
    Flatpak,
}

/// One sandboxed browser package found on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxedBrowser {
    /// The settings line it belongs to (Chromium and its forks are on the Brave line).
    pub browser: Browser,
    pub sandbox: Sandbox,
    /// Snap name or Flatpak id.
    pub package: &'static str,
    /// Where its registration file goes.
    pub manifest: PathBuf,
    /// The folder the helper sees as its data folder: the helper copy, the link and the token go here.
    pub data_dir: PathBuf,
}

impl SandboxedBrowser {
    pub fn host_copy(&self) -> PathBuf {
        self.data_dir.join("velox-native-host")
    }
}

/// `(browser line, Flatpak id, registration folder inside ~/.var/app/<id>)`.
const FLATPAKS: &[(Browser, &str, &str)] = &[
    (Browser::Chrome, "com.google.Chrome", "config/google-chrome/NativeMessagingHosts"),
    (Browser::Edge, "com.microsoft.Edge", "config/microsoft-edge/NativeMessagingHosts"),
    (Browser::Brave, "com.brave.Browser", "config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
    (Browser::Brave, "org.chromium.Chromium", "config/chromium/NativeMessagingHosts"),
    (Browser::Brave, "io.github.ungoogled_software.ungoogled_chromium", "config/chromium/NativeMessagingHosts"),
    (Browser::Firefox, "org.mozilla.firefox", ".mozilla/native-messaging-hosts"),
    (Browser::Firefox, "io.gitlab.librewolf-community", ".librewolf/native-messaging-hosts"),
];

/// `(browser line, snap name, registration folder inside ~/snap/<name>)`. Firefox and Chromium keep their
/// profile in `common`; Brave keeps it under the current revision (snapd carries it over on updates).
const SNAPS: &[(Browser, &str, &str)] = &[
    (Browser::Firefox, "firefox", "common/.mozilla/native-messaging-hosts"),
    (Browser::Brave, "chromium", "common/chromium/NativeMessagingHosts"),
    (Browser::Brave, "brave", "current/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
];

/// The sandboxed browsers installed for this user. `root` is `/` (tests pass a scratch folder): a package
/// counts as installed when the system or the user has it, or it has already made its folder in `home`.
pub fn find(home: &Path, root: &Path, name: &str) -> Vec<SandboxedBrowser> {
    let file = format!("{name}.json");
    let mut out = Vec::new();
    for &(browser, id, dir) in FLATPAKS {
        let installed = root.join("var/lib/flatpak/app").join(id).is_dir()
            || home.join(".local/share/flatpak/app").join(id).is_dir()
            || home.join(".var/app").join(id).is_dir();
        if installed {
            let base = home.join(".var/app").join(id);
            out.push(SandboxedBrowser {
                browser,
                sandbox: Sandbox::Flatpak,
                package: id,
                manifest: base.join(dir).join(&file),
                data_dir: base.join("data").join(DATA_DIR_NAME),
            });
        }
    }
    for &(browser, snap, dir) in SNAPS {
        let base = home.join("snap").join(snap);
        // Its folders are snapd's: used only once the browser has made them (it has run once). `current` is a link
        // snapd moves on every update, never to be created by anyone else.
        let first = dir.split('/').next().unwrap_or_default();
        if root.join("snap").join(snap).is_dir() && base.join(first).is_dir() {
            out.push(SandboxedBrowser {
                browser,
                sandbox: Sandbox::Snap,
                package: snap,
                manifest: base.join(dir).join(&file),
                data_dir: base.join("common").join(DATA_DIR_NAME),
            });
        }
    }
    out
}

/// The registration as it must be for `b`: the normal one, pointing at the helper copy.
fn spec_for(b: &SandboxedBrowser, spec: &HostSpec) -> HostSpec {
    HostSpec { host_exe: b.host_copy(), ..spec.clone() }
}

/// Copy the helper (when it changed) and write the registration. Idempotent.
pub fn install(b: &SandboxedBrowser, spec: &HostSpec) -> io::Result<()> {
    if !spec.host_exe.is_absolute() || !spec.host_exe.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "the native host program was not found next to the application"));
    }
    copy_helper(&spec.host_exe, &b.data_dir)?;
    if let Some(dir) = b.manifest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&b.manifest, manifest_json(b.browser, &spec_for(b, spec)))
}

/// Keep a copy of the helper program `src` in `dir` (only rewritten when it changed). Returns the copy.
/// Also used for an AppImage, whose own files vanish when it closes while browsers must still find the helper.
pub fn copy_helper(src: &Path, dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let copy = dir.join(src.file_name().unwrap_or_else(|| "velox-native-host".as_ref()));
    if !same_file_content(src, &copy) {
        // Write beside, then rename: a browser starting the helper meanwhile never runs half a file.
        let tmp = dir.join(".velox-native-host.new");
        let _ = std::fs::remove_file(&tmp);
        std::fs::copy(src, &tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, &copy)?;
    }
    Ok(copy)
}

/// Remove the registration and the helper copy (the token and the link go when the app stops serving there).
pub fn uninstall(b: &SandboxedBrowser) -> io::Result<()> {
    for f in [&b.manifest, &b.host_copy()] {
        match std::fs::remove_file(f) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Registered exactly as [`install`] would write it, with an up-to-date helper copy.
pub fn registered(b: &SandboxedBrowser, spec: &HostSpec) -> bool {
    let expected = manifest_json(b.browser, &spec_for(b, spec));
    std::fs::read_to_string(&b.manifest).is_ok_and(|actual| actual == expected) && same_file_content(&spec.host_exe, &b.host_copy())
}

fn same_file_content(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) if ma.len() == mb.len() => std::fs::read(a).ok() == std::fs::read(b).ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-sandbox-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn spec(exe: PathBuf) -> HostSpec {
        HostSpec {
            name: "app.velox.fetch".into(),
            description: "VeloxFetch".into(),
            host_exe: exe,
            chromium_origins: vec!["chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/".into()],
            firefox_ids: vec!["velox-fetch@velox.app".into()],
        }
    }

    #[test]
    fn finds_snap_and_flatpak_browsers_and_their_folders() {
        let d = scratch("find");
        let (home, root) = (d.join("home"), d.join("root"));
        assert!(find(&home, &root, "app.velox.fetch").is_empty(), "nothing installed");
        std::fs::create_dir_all(root.join("snap/firefox")).unwrap();
        std::fs::create_dir_all(root.join("snap/brave")).unwrap();
        assert!(find(&home, &root, "app.velox.fetch").is_empty(), "a snap that never ran has no folders to use");
        std::fs::create_dir_all(home.join("snap/firefox/common")).unwrap();
        std::fs::create_dir_all(root.join("var/lib/flatpak/app/com.brave.Browser")).unwrap();
        std::fs::create_dir_all(home.join(".var/app/com.google.Chrome")).unwrap();
        let found = find(&home, &root, "app.velox.fetch");
        let by = |p: &str| found.iter().find(|b| b.package == p).unwrap().clone();
        assert_eq!(found.len(), 3);

        let ff = by("firefox");
        assert_eq!((ff.browser, ff.sandbox), (Browser::Firefox, Sandbox::Snap));
        assert_eq!(ff.manifest, home.join("snap/firefox/common/.mozilla/native-messaging-hosts/app.velox.fetch.json"));
        assert_eq!(ff.data_dir, home.join("snap/firefox/common/app.velox.fetch"));

        let brave = by("com.brave.Browser");
        assert_eq!((brave.browser, brave.sandbox), (Browser::Brave, Sandbox::Flatpak));
        assert_eq!(
            brave.manifest,
            home.join(".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser/NativeMessagingHosts/app.velox.fetch.json")
        );
        assert_eq!(brave.data_dir, home.join(".var/app/com.brave.Browser/data/app.velox.fetch"));
        assert_eq!(by("com.google.Chrome").browser, Browser::Chrome);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn install_copies_the_helper_and_points_the_registration_at_the_copy() {
        let d = scratch("install");
        let exe = d.join("velox-native-host");
        std::fs::write(&exe, b"helper v1").unwrap();
        let s = spec(exe.clone());
        std::fs::create_dir_all(d.join("root/snap/chromium")).unwrap();
        std::fs::create_dir_all(d.join("home/snap/chromium/common")).unwrap();
        let b = find(&d.join("home"), &d.join("root"), "app.velox.fetch").remove(0);
        assert!(!registered(&b, &s));

        install(&b, &s).unwrap();
        assert!(registered(&b, &s));
        assert_eq!(std::fs::read(b.host_copy()).unwrap(), b"helper v1");
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&b.manifest).unwrap()).unwrap();
        assert_eq!(m["path"].as_str().unwrap(), b.host_copy().to_string_lossy());
        assert_eq!(m["allowed_origins"][0], "chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(b.host_copy()).unwrap().permissions().mode() & 0o777, 0o755);
        }

        // An updated app: the copy is out of date until installed again.
        std::fs::write(&exe, b"helper v2!").unwrap();
        assert!(!registered(&b, &s));
        install(&b, &s).unwrap();
        assert_eq!(std::fs::read(b.host_copy()).unwrap(), b"helper v2!");

        uninstall(&b).unwrap();
        uninstall(&b).unwrap(); // twice is fine
        assert!(!b.manifest.exists() && !b.host_copy().exists());
        assert!(install(&b, &spec(d.join("missing"))).is_err(), "no helper, no registration");
        let _ = std::fs::remove_dir_all(d);
    }
}
