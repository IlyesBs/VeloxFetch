//! Registering the native messaging host with browsers.
//!
//! A registration tells the browser "this program may be started by extension X". That is a grant
//! of the ability to run an executable, so:
//! * it is done for this user's own browsers only: from Settings, and by the app itself for a browser installed on
//!   this computer that the user did not disconnect (never at install time, never system-wide);
//! * the executable path comes from our own installation, never from user or browser input;
//! * the manifest lists exactly the extension ids we ship (`allowed_origins` /
//!   `allowed_extensions`), which the browser enforces;
//! * everything is per-user (HKCU / the user's profile): no administrator rights, no system-wide
//!   change.
//!
//! Windows registers through the registry (the manifest file lives in our data folder); Linux and
//! macOS put the manifest file in the browser's per-user `NativeMessagingHosts` folder.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::Platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Browser {
    Chrome,
    Edge,
    Brave,
    Firefox,
}

impl Browser {
    pub const ALL: [Browser; 4] = [Self::Chrome, Self::Edge, Self::Brave, Self::Firefox];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Brave => "brave",
            Self::Firefox => "firefox",
        }
    }

    fn is_chromium(self) -> bool {
        !matches!(self, Self::Firefox)
    }
}

/// What the manifest says about the host.
#[derive(Debug, Clone)]
pub struct HostSpec {
    /// Registration name, e.g. `app.velox.fetch`.
    pub name: String,
    pub description: String,
    /// Absolute path of the native host executable.
    pub host_exe: PathBuf,
    /// `chrome-extension://<id>/` origins allowed to start it (Chrome, Edge).
    pub chromium_origins: Vec<String>,
    /// Add-on ids allowed to start it (Firefox).
    pub firefox_ids: Vec<String>,
}

/// The JSON manifest for one browser family.
pub fn manifest_json(browser: Browser, spec: &HostSpec) -> String {
    let mut m = json!({
        "name": spec.name,
        "description": spec.description,
        "path": spec.host_exe.to_string_lossy(),
        "type": "stdio",
    });
    if browser.is_chromium() {
        m["allowed_origins"] = json!(spec.chromium_origins);
    } else {
        m["allowed_extensions"] = json!(spec.firefox_ids);
    }
    serde_json::to_string_pretty(&m).expect("manifest serialises")
}

/// Where the browser looks for the registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Windows: `HKCU\<subkey>` whose default value is the manifest file's path.
    Registry { subkey: String },
    /// Linux / macOS: the manifest file itself, at this path.
    File { path: PathBuf },
}

/// The real, per-user location for `browser`. `home` is the user's home directory (Unix only).
pub fn default_target(browser: Browser, platform: Platform, name: &str, home: Option<&Path>) -> Option<Target> {
    match platform {
        Platform::Windows => {
            let base = match browser {
                Browser::Chrome => r"Software\Google\Chrome\NativeMessagingHosts",
                Browser::Edge => r"Software\Microsoft\Edge\NativeMessagingHosts",
                // Verified against a real Brave: on Windows it reads the Chromium key (and Chrome's),
                // NOT a BraveSoftware key. Registering there would show "connected" while Brave says
                // "native messaging host not found". Vivaldi, Opera and other forks read it too.
                Browser::Brave => r"Software\Chromium\NativeMessagingHosts",
                Browser::Firefox => r"Software\Mozilla\NativeMessagingHosts",
            };
            Some(Target::Registry { subkey: format!(r"{base}\{name}") })
        }
        Platform::Linux => {
            let dir = match browser {
                Browser::Chrome => ".config/google-chrome/NativeMessagingHosts",
                Browser::Edge => ".config/microsoft-edge/NativeMessagingHosts",
                Browser::Brave => ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
                Browser::Firefox => ".mozilla/native-messaging-hosts",
            };
            Some(Target::File { path: home?.join(dir).join(format!("{name}.json")) })
        }
        Platform::MacOs => {
            let dir = match browser {
                Browser::Chrome => "Library/Application Support/Google/Chrome/NativeMessagingHosts",
                Browser::Edge => "Library/Application Support/Microsoft Edge/NativeMessagingHosts",
                Browser::Brave => "Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts",
                Browser::Firefox => "Library/Application Support/Mozilla/NativeMessagingHosts",
            };
            Some(Target::File { path: home?.join(dir).join(format!("{name}.json")) })
        }
        Platform::Other => None,
    }
}

/// The other browsers that read their registration from a folder of their own but belong to `browser`'s settings
/// line (Linux / macOS): Chrome's beta and dev channels, Edge's, and on the Chromium line Chromium itself, Vivaldi,
/// Opera, Arc and Brave's other channels; Firefox's forks. Only those this user has (their profile folder exists):
/// nothing is created for a browser that is not there. Windows has none: the forks read the Chromium key.
pub fn sibling_targets(browser: Browser, platform: Platform, name: &str, home: Option<&Path>) -> Vec<Target> {
    let Some(home) = home else { return Vec::new() };
    let profiles: &[&str] = match (platform, browser) {
        (Platform::Linux, Browser::Chrome) => &[".config/google-chrome-beta", ".config/google-chrome-unstable"],
        (Platform::Linux, Browser::Edge) => &[".config/microsoft-edge-beta", ".config/microsoft-edge-dev"],
        (Platform::Linux, Browser::Brave) => &[
            ".config/chromium",
            ".config/vivaldi",
            ".config/vivaldi-snapshot",
            ".config/opera",
            ".config/opera-beta",
            ".config/BraveSoftware/Brave-Browser-Beta",
            ".config/BraveSoftware/Brave-Browser-Nightly",
        ],
        (Platform::Linux, Browser::Firefox) => &[".librewolf", ".waterfox", ".floorp"],
        (Platform::MacOs, Browser::Chrome) => &[
            "Library/Application Support/Google/Chrome Beta",
            "Library/Application Support/Google/Chrome Dev",
            "Library/Application Support/Google/Chrome Canary",
        ],
        (Platform::MacOs, Browser::Edge) => &[
            "Library/Application Support/Microsoft Edge Beta",
            "Library/Application Support/Microsoft Edge Dev",
            "Library/Application Support/Microsoft Edge Canary",
        ],
        (Platform::MacOs, Browser::Brave) => &[
            "Library/Application Support/Chromium",
            "Library/Application Support/Vivaldi",
            "Library/Application Support/Arc/User Data",
            "Library/Application Support/BraveSoftware/Brave-Browser-Beta",
            "Library/Application Support/BraveSoftware/Brave-Browser-Nightly",
        ],
        (Platform::MacOs, Browser::Firefox) => &["Library/Application Support/LibreWolf"],
        _ => &[],
    };
    let folder = if browser == Browser::Firefox && platform == Platform::Linux { "native-messaging-hosts" } else { "NativeMessagingHosts" };
    profiles
        .iter()
        .map(|p| home.join(p))
        .filter(|p| p.is_dir())
        .map(|p| Target::File { path: p.join(folder).join(format!("{name}.json")) })
        .collect()
}

/// Where our own copy of the manifest lives (Windows registers a path to it).
pub fn manifest_path(manifest_dir: &Path, browser: Browser, name: &str) -> PathBuf {
    manifest_dir.join(format!("{name}.{}.json", browser.as_str()))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The browser will find and accept the host.
    pub registered: bool,
    /// The host executable exists on disk.
    pub host_present: bool,
}

/// Register `spec` for `browser`. Idempotent. Refuses a relative or missing executable path.
pub fn install(browser: Browser, spec: &HostSpec, manifest_dir: &Path, target: &Target) -> io::Result<()> {
    if !spec.host_exe.is_absolute() || !spec.host_exe.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "the native host program was not found next to the application"));
    }
    let json = manifest_json(browser, spec);
    match target {
        Target::File { path } => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, json)
        }
        Target::Registry { subkey } => {
            std::fs::create_dir_all(manifest_dir)?;
            let file = manifest_path(manifest_dir, browser, &spec.name);
            std::fs::write(&file, json)?;
            registry::set(subkey, &file.to_string_lossy())
        }
    }
}

pub fn uninstall(browser: Browser, spec: &HostSpec, manifest_dir: &Path, target: &Target) -> io::Result<()> {
    match target {
        Target::File { path } => match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Target::Registry { subkey } => {
            registry::delete(subkey)?;
            match std::fs::remove_file(manifest_path(manifest_dir, browser, &spec.name)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }
}

/// Is `spec` registered for `browser` exactly as we would write it? A stale registration (moved
/// install, different extension list) counts as *not* registered.
pub fn status(browser: Browser, spec: &HostSpec, manifest_dir: &Path, target: &Target) -> Status {
    let manifest_file = match target {
        Target::File { path } => Some(path.clone()),
        Target::Registry { subkey } => registry::get(subkey).map(PathBuf::from),
    };
    let expected = manifest_json(browser, spec);
    let registered = manifest_file.and_then(|f| std::fs::read_to_string(f).ok()).is_some_and(|actual| actual == expected);
    let _ = manifest_dir;
    Status { registered, host_present: spec.host_exe.is_file() }
}

/// A registration of ours that only needs writing again: the manifest the browser finds names our host and our
/// helper program (or a helper that no longer exists), but differs from what we would write now (an update added
/// an extension id, the store one). A registration pointing at another copy of VeloxFetch that still exists, or
/// anything that is not ours, is left alone.
pub fn needs_refresh(browser: Browser, spec: &HostSpec, target: &Target) -> bool {
    let manifest_file = match target {
        Target::File { path } => Some(path.clone()),
        Target::Registry { subkey } => registry::get(subkey).map(PathBuf::from),
    };
    let Some(actual) = manifest_file.and_then(|f| std::fs::read_to_string(f).ok()) else { return false };
    if actual == manifest_json(browser, spec) {
        return false;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&actual) else { return false };
    v["name"].as_str() == Some(spec.name.as_str())
        && v["path"].as_str().map(PathBuf::from).is_some_and(|p| p == spec.host_exe || !p.is_file())
}

// ------------------------------------------------------------------------------ registry access

#[cfg(windows)]
mod registry {
    use std::io;

    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    pub fn set(subkey: &str, value: &str) -> io::Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(subkey)?;
        key.set_value("", &value)
    }

    pub fn get(subkey: &str) -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(subkey, KEY_READ).ok()?.get_value::<String, _>("").ok()
    }

    pub fn delete(subkey: &str) -> io::Result<()> {
        match RegKey::predef(HKEY_CURRENT_USER).delete_subkey(subkey) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(not(windows))]
mod registry {
    use std::io;

    pub fn set(_: &str, _: &str) -> io::Result<()> {
        Err(io::Error::other("the registry only exists on Windows"))
    }
    pub fn get(_: &str) -> Option<String> {
        None
    }
    pub fn delete(_: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(exe: &Path) -> HostSpec {
        HostSpec {
            name: "app.velox.fetch".into(),
            description: "VeloxFetch".into(),
            host_exe: exe.to_path_buf(),
            chromium_origins: vec!["chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/".into()],
            firefox_ids: vec!["velox-fetch@velox.app".into()],
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-nm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_host(dir: &Path) -> PathBuf {
        let p = dir.join(if cfg!(windows) { "velox-native-host.exe" } else { "velox-native-host" });
        std::fs::write(&p, b"stub").unwrap();
        p
    }

    #[test]
    fn manifests_grant_exactly_the_shipped_extensions() {
        let d = scratch("manifest");
        let s = spec(&fake_host(&d));
        let c: serde_json::Value = serde_json::from_str(&manifest_json(Browser::Chrome, &s)).unwrap();
        assert_eq!(c["type"], "stdio");
        assert_eq!(c["allowed_origins"], json!(["chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/"]));
        assert!(c.get("allowed_extensions").is_none());
        assert_eq!(manifest_json(Browser::Edge, &s), manifest_json(Browser::Chrome, &s), "Edge uses the Chromium format");
        assert_eq!(manifest_json(Browser::Brave, &s), manifest_json(Browser::Chrome, &s), "Brave uses the Chromium format");
        let f: serde_json::Value = serde_json::from_str(&manifest_json(Browser::Firefox, &s)).unwrap();
        assert_eq!(f["allowed_extensions"], json!(["velox-fetch@velox.app"]));
        assert!(f.get("allowed_origins").is_none());
        // Never a wildcard.
        assert!(!manifest_json(Browser::Chrome, &s).contains('*'));
        assert_eq!(c["path"].as_str().unwrap(), s.host_exe.to_string_lossy());
    }

    #[test]
    fn real_locations_per_platform() {
        let n = "app.velox.fetch";
        let home = Path::new("/home/u");
        assert_eq!(
            default_target(Browser::Chrome, Platform::Windows, n, None),
            Some(Target::Registry { subkey: r"Software\Google\Chrome\NativeMessagingHosts\app.velox.fetch".into() })
        );
        assert_eq!(
            default_target(Browser::Brave, Platform::Windows, n, None),
            Some(Target::Registry { subkey: r"Software\Chromium\NativeMessagingHosts\app.velox.fetch".into() })
        );
        assert_eq!(
            default_target(Browser::Firefox, Platform::Windows, n, None),
            Some(Target::Registry { subkey: r"Software\Mozilla\NativeMessagingHosts\app.velox.fetch".into() })
        );
        assert_eq!(
            default_target(Browser::Chrome, Platform::Linux, n, Some(home)),
            Some(Target::File { path: home.join(".config/google-chrome/NativeMessagingHosts/app.velox.fetch.json") })
        );
        assert_eq!(
            default_target(Browser::Firefox, Platform::MacOs, n, Some(home)),
            Some(Target::File { path: home.join("Library/Application Support/Mozilla/NativeMessagingHosts/app.velox.fetch.json") })
        );
        assert_eq!(default_target(Browser::Chrome, Platform::Linux, n, None), None, "no home directory: no guess");
        assert_eq!(default_target(Browser::Chrome, Platform::Other, n, Some(home)), None);
    }

    #[test]
    fn file_registration_lifecycle() {
        let d = scratch("file");
        let s = spec(&fake_host(&d));
        let target = Target::File { path: d.join("nmh").join("app.velox.fetch.json") };
        assert_eq!(status(Browser::Chrome, &s, &d, &target), Status { registered: false, host_present: true });
        install(Browser::Chrome, &s, &d, &target).unwrap();
        assert!(status(Browser::Chrome, &s, &d, &target).registered);
        install(Browser::Chrome, &s, &d, &target).unwrap(); // idempotent
        assert!(status(Browser::Chrome, &s, &d, &target).registered);

        // A stale registration (the install moved) is not "registered".
        let mut moved = s.clone();
        moved.host_exe = d.join("elsewhere").join("velox-native-host");
        assert!(!status(Browser::Chrome, &moved, &d, &target).registered);

        uninstall(Browser::Chrome, &s, &d, &target).unwrap();
        assert!(!status(Browser::Chrome, &s, &d, &target).registered);
        uninstall(Browser::Chrome, &s, &d, &target).unwrap(); // and removing twice is fine
    }

    #[test]
    fn an_older_registration_of_ours_is_refreshed_and_nothing_else() {
        let d = scratch("refresh");
        let s = spec(&fake_host(&d));
        let target = Target::File { path: d.join("nmh").join("app.velox.fetch.json") };
        assert!(!needs_refresh(Browser::Chrome, &s, &target), "nothing registered: nothing to refresh");
        install(Browser::Chrome, &s, &d, &target).unwrap();
        assert!(!needs_refresh(Browser::Chrome, &s, &target), "up to date");

        // An update adds the store extension: the old registration (the developer id only) is ours and refreshed.
        let mut now = s.clone();
        now.chromium_origins.push("chrome-extension://keobnkoppcchnbpmmfodidkcedglbhnn/".into());
        assert!(!status(Browser::Chrome, &now, &d, &target).registered);
        assert!(needs_refresh(Browser::Chrome, &now, &target));
        install(Browser::Chrome, &now, &d, &target).unwrap();
        assert!(status(Browser::Chrome, &now, &d, &target).registered && !needs_refresh(Browser::Chrome, &now, &target));

        // Another copy of VeloxFetch whose helper exists: left alone. One whose helper is gone: ours to take over.
        let other_dir = d.join("other");
        std::fs::create_dir_all(&other_dir).unwrap();
        let other = HostSpec { host_exe: fake_host(&other_dir), ..s.clone() };
        install(Browser::Chrome, &other, &d, &target).unwrap();
        assert!(!needs_refresh(Browser::Chrome, &now, &target));
        std::fs::remove_file(&other.host_exe).unwrap();
        assert!(needs_refresh(Browser::Chrome, &now, &target));

        // Not ours (another program's host under the same file), or not JSON: never touched.
        std::fs::write(target_path(&target), r#"{"name":"com.other.app","path":"/nope"}"#).unwrap();
        assert!(!needs_refresh(Browser::Chrome, &now, &target));
        std::fs::write(target_path(&target), "garbage").unwrap();
        assert!(!needs_refresh(Browser::Chrome, &now, &target));
    }

    #[test]
    fn the_other_browsers_of_a_line_only_where_this_user_has_them() {
        let d = scratch("siblings");
        let n = "app.velox.fetch";
        assert!(sibling_targets(Browser::Brave, Platform::Linux, n, Some(&d)).is_empty(), "none installed: nothing");
        assert!(sibling_targets(Browser::Brave, Platform::Linux, n, None).is_empty());
        std::fs::create_dir_all(d.join(".config/chromium")).unwrap();
        std::fs::create_dir_all(d.join(".config/vivaldi")).unwrap();
        std::fs::create_dir_all(d.join(".librewolf")).unwrap();
        assert_eq!(
            sibling_targets(Browser::Brave, Platform::Linux, n, Some(&d)),
            vec![
                Target::File { path: d.join(".config/chromium/NativeMessagingHosts/app.velox.fetch.json") },
                Target::File { path: d.join(".config/vivaldi/NativeMessagingHosts/app.velox.fetch.json") },
            ]
        );
        assert_eq!(
            sibling_targets(Browser::Firefox, Platform::Linux, n, Some(&d)),
            vec![Target::File { path: d.join(".librewolf/native-messaging-hosts/app.velox.fetch.json") }]
        );
        assert!(sibling_targets(Browser::Chrome, Platform::Linux, n, Some(&d)).is_empty());
        assert!(sibling_targets(Browser::Brave, Platform::Windows, n, Some(&d)).is_empty(), "Windows: the Chromium key serves them");
        std::fs::create_dir_all(d.join("Library/Application Support/Vivaldi")).unwrap();
        assert_eq!(
            sibling_targets(Browser::Brave, Platform::MacOs, n, Some(&d)),
            vec![Target::File { path: d.join("Library/Application Support/Vivaldi/NativeMessagingHosts/app.velox.fetch.json") }]
        );
        let _ = std::fs::remove_dir_all(d);
    }

    fn target_path(t: &Target) -> PathBuf {
        match t {
            Target::File { path } => path.clone(),
            Target::Registry { .. } => unreachable!(),
        }
    }

    #[test]
    fn refuses_to_register_a_missing_or_relative_host() {
        let d = scratch("refuse");
        let target = Target::File { path: d.join("m.json") };
        assert!(install(Browser::Chrome, &spec(&d.join("nope.exe")), &d, &target).is_err());
        assert!(install(Browser::Chrome, &spec(Path::new("relative/host.exe")), &d, &target).is_err());
        assert!(!target_exists(&target));
    }

    fn target_exists(t: &Target) -> bool {
        matches!(t, Target::File { path } if path.exists())
    }

    #[cfg(windows)]
    #[test]
    fn registry_registration_lifecycle_in_a_scratch_key() {
        // A private key under HKCU: the real Chrome / Edge / Firefox keys are never touched by tests.
        let sub = format!(r"Software\VeloxFetchTest-{}\NativeMessagingHosts\app.velox.fetch", std::process::id());
        let d = scratch("reg");
        let s = spec(&fake_host(&d));
        let target = Target::Registry { subkey: sub.clone() };
        assert!(!status(Browser::Chrome, &s, &d, &target).registered);
        install(Browser::Chrome, &s, &d, &target).unwrap();
        assert!(status(Browser::Chrome, &s, &d, &target).registered);
        assert!(manifest_path(&d, Browser::Chrome, &s.name).is_file());
        // The registry value points at our manifest file.
        assert_eq!(registry::get(&sub).unwrap(), manifest_path(&d, Browser::Chrome, &s.name).to_string_lossy());
        uninstall(Browser::Chrome, &s, &d, &target).unwrap();
        assert!(!status(Browser::Chrome, &s, &d, &target).registered);
        assert!(!manifest_path(&d, Browser::Chrome, &s.name).exists());
        uninstall(Browser::Chrome, &s, &d, &target).unwrap();
        // Remove the scratch parent key.
        let _ = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .delete_subkey_all(format!(r"Software\VeloxFetchTest-{}", std::process::id()));
    }
}
