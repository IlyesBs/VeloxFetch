//! Offer the extension from the browser's own store, the way download managers do it ("external extensions").
//!
//! The app writes a small, documented pointer for the current user: "extension <id>, updated from <store>". The
//! browser reads it at its next start, shows its own "New extension added" prompt, and the user enables it with
//! one click. Nothing is installed silently and no browser file is touched:
//! * Windows: `HKCU\Software\<browser>\Extensions\<id>`, value `update_url`;
//! * macOS: `~/Library/Application Support/<browser>/External Extensions/<id>.json`;
//! * Linux: Chromium browsers read these pointers only from system folders, so there is no per-user way:
//!   [`Support::None`]. The deb / rpm packages write them there for every user when VeloxFetch is installed
//!   ([`LINUX_SYSTEM_FOLDERS`], `packaging/postinst.sh`), and the Windows setup writes the machine-wide keys
//!   ([`WINDOWS_MACHINE_KEYS`], `installer-hooks.nsh`).
//!
//! Only for Chromium browsers (Chrome, Edge, Brave), and only once the extension is published in a store
//! (an id is configured). Firefox installs add-ons from addons.mozilla.org pages only.

use std::io;
use std::path::{Path, PathBuf};

use crate::native_messaging::Browser;
use crate::Platform;

/// Chrome Web Store's update service: what the browser contacts to fetch the published extension.
pub const CHROME_STORE_UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";

/// Linux: the folders the deb / rpm packages put `<id>.json` in, for every user: Chrome's own, Chromium's (which
/// Brave and other Chromium browsers read too: checked against a real Brave), and Edge's.
pub const LINUX_SYSTEM_FOLDERS: [&str; 3] =
    ["/usr/share/google-chrome/extensions", "/usr/share/chromium/extensions", "/usr/share/microsoft-edge/extensions"];

/// Windows: the machine-wide keys the setup writes `<id>\update_url` under (HKLM, the 32-bit view: where the
/// browsers read them), for every user. Brave reads Chrome's key; its own is written as well.
pub const WINDOWS_MACHINE_KEYS: [&str; 3] =
    [r"Software\Google\Chrome\Extensions", r"Software\Microsoft\Edge\Extensions", r"Software\BraveSoftware\Brave-Browser\Extensions"];

/// How this browser can be offered the store extension on this system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    /// Through a value in the current user's registry (Windows).
    Registry { key: String },
    /// Through a JSON file in the user's browser folder (macOS).
    File { path: PathBuf },
    /// Not possible for one user here (Linux, Firefox, unknown system).
    None,
}

/// Where the pointer for extension `id` goes.
pub fn support(browser: Browser, platform: Platform, home: Option<&Path>, id: &str) -> Support {
    if browser == Browser::Firefox || !valid_id(id) {
        return Support::None;
    }
    match platform {
        Platform::Windows => {
            let vendor = match browser {
                Browser::Chrome => r"Software\Google\Chrome",
                Browser::Edge => r"Software\Microsoft\Edge",
                Browser::Brave => r"Software\BraveSoftware\Brave-Browser",
                Browser::Firefox => return Support::None,
            };
            Support::Registry { key: format!(r"{vendor}\Extensions\{id}") }
        }
        Platform::MacOs => {
            let Some(home) = home else { return Support::None };
            let dir = match browser {
                Browser::Chrome => "Google/Chrome",
                Browser::Edge => "Microsoft Edge",
                Browser::Brave => "BraveSoftware/Brave-Browser",
                Browser::Firefox => return Support::None,
            };
            Support::File {
                path: home.join("Library/Application Support").join(dir).join("External Extensions").join(format!("{id}.json")),
            }
        }
        Platform::Linux | Platform::Other => Support::None,
    }
}

/// A Chromium extension id: 32 letters a to p.
pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// The JSON pointer (macOS).
pub fn pointer_json() -> String {
    serde_json::json!({ "external_update_url": CHROME_STORE_UPDATE_URL }).to_string()
}

/// Write the pointer. `Ok(false)` when this system has no per-user way.
pub fn offer(support: &Support) -> io::Result<bool> {
    match support {
        Support::Registry { key } => registry::set(key, "update_url", CHROME_STORE_UPDATE_URL).map(|_| true),
        Support::File { path } => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, pointer_json()).map(|_| true)
        }
        Support::None => Ok(false),
    }
}

/// Remove the pointer (Disconnect). The browser keeps an extension the user already enabled; it only stops
/// offering it.
pub fn withdraw(support: &Support) -> io::Result<()> {
    match support {
        Support::Registry { key } => registry::delete(key),
        Support::File { path } => match std::fs::remove_file(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
        Support::None => Ok(()),
    }
}

/// Whether the pointer is in place.
pub fn offered(support: &Support) -> bool {
    match support {
        Support::Registry { key } => registry::get(key, "update_url").is_some(),
        Support::File { path } => path.is_file(),
        Support::None => false,
    }
}

#[cfg(windows)]
mod registry {
    use std::io;

    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    pub fn set(subkey: &str, name: &str, value: &str) -> io::Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(subkey)?;
        key.set_value(name, &value)
    }

    pub fn get(subkey: &str, name: &str) -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(subkey, KEY_READ).ok()?.get_value::<String, _>(name).ok()
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

    pub fn set(_: &str, _: &str, _: &str) -> io::Result<()> {
        Err(io::Error::other("the registry only exists on Windows"))
    }
    pub fn get(_: &str, _: &str) -> Option<String> {
        None
    }
    pub fn delete(_: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "gbcanjaaelioalfdggkhbbnbinmfgdbc";

    #[test]
    fn each_browser_has_its_place_per_system() {
        assert_eq!(
            support(Browser::Chrome, Platform::Windows, None, ID),
            Support::Registry { key: format!(r"Software\Google\Chrome\Extensions\{ID}") }
        );
        assert_eq!(
            support(Browser::Edge, Platform::Windows, None, ID),
            Support::Registry { key: format!(r"Software\Microsoft\Edge\Extensions\{ID}") }
        );
        let home = Path::new("/Users/me");
        assert_eq!(
            support(Browser::Brave, Platform::MacOs, Some(home), ID),
            Support::File {
                path: home.join(format!("Library/Application Support/BraveSoftware/Brave-Browser/External Extensions/{ID}.json"))
            }
        );
        assert_eq!(support(Browser::Chrome, Platform::Linux, Some(home), ID), Support::None, "no per-user way on Linux");
        assert_eq!(support(Browser::Firefox, Platform::Windows, None, ID), Support::None);
    }

    #[test]
    fn only_real_extension_ids_are_written() {
        assert!(valid_id(ID));
        for bad in ["", "short", &"q".repeat(32), &format!("{}\\x", &ID[..30]), &ID.to_uppercase()] {
            assert!(!valid_id(bad), "{bad:?}");
            assert_eq!(support(Browser::Chrome, Platform::Windows, None, bad), Support::None);
        }
    }

    #[test]
    fn the_file_pointer_is_written_and_removed() {
        let dir = std::env::temp_dir().join(format!("velox-ext-offer-{}", std::process::id()));
        let s = support(Browser::Chrome, Platform::MacOs, Some(&dir), ID);
        assert!(!offered(&s));
        assert!(offer(&s).unwrap());
        assert!(offered(&s));
        let Support::File { path } = &s else { unreachable!() };
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(v["external_update_url"], CHROME_STORE_UPDATE_URL);
        withdraw(&s).unwrap();
        withdraw(&s).unwrap(); // twice is fine
        assert!(!offered(&s));
        let _ = std::fs::remove_dir_all(dir);
        assert!(!offer(&Support::None).unwrap());
    }
}
