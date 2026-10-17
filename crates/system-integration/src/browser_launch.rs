//! Finding the user's browsers and opening their extensions page, on every supported platform.
//!
//! Only fixed, built-in URLs are ever passed to a browser, and every program is started directly
//! with an argument list (no shell), so no text from outside this crate can become a command.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::native_messaging::Browser;
use crate::Platform;

/// The page where the user loads an unpacked / temporary extension.
pub fn extensions_url(browser: Browser) -> &'static str {
    match browser {
        Browser::Chrome => "chrome://extensions",
        Browser::Edge => "edge://extensions",
        Browser::Brave => "brave://extensions",
        Browser::Firefox => "about:debugging#/runtime/this-firefox",
    }
}

/// Map an identifier the OS uses for the default browser (Windows ProgId, Linux `.desktop` file,
/// macOS bundle id) to a browser we know. Other browsers (Vivaldi, Opera, ...) give `None`.
pub fn browser_from_id(id: &str) -> Option<Browser> {
    let l = id.to_ascii_lowercase();
    if l.contains("brave") {
        Some(Browser::Brave)
    } else if l.contains("msedge") || l.contains("microsoft-edge") || l.contains("edgemac") || l.contains("edgehtm") {
        Some(Browser::Edge)
    } else if l.contains("firefox") {
        Some(Browser::Firefox)
    } else if l.contains("chrome") && !l.contains("chromium") {
        Some(Browser::Chrome)
    } else {
        None
    }
}

/// How to start an installed browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launcher {
    Exe(PathBuf),
    /// macOS application name, started with `open -a`.
    MacApp(&'static str),
}

/// The user's default web browser, when it is one of ours.
pub fn default_browser(platform: Platform) -> Option<Browser> {
    match platform {
        Platform::Windows => windows::default_browser_id().as_deref().and_then(browser_from_id),
        Platform::Linux => {
            let out = run_capture("xdg-settings", &["get", "default-web-browser"])?;
            browser_from_id(out.trim())
        }
        Platform::MacOs => {
            let out = run_capture("defaults", &["read", "com.apple.LaunchServices/com.apple.launchservices.secure", "LSHandlers"])?;
            parse_macos_handlers(&out).as_deref().and_then(browser_from_id)
        }
        Platform::Other => None,
    }
}

/// From `defaults read ... LSHandlers` output: the bundle id that handles `https`.
pub fn parse_macos_handlers(text: &str) -> Option<String> {
    for block in text.split("},") {
        if !block.contains("LSHandlerURLScheme = https;") {
            continue;
        }
        let key = "LSHandlerRoleAll = ";
        // The last occurrence: an earlier one may belong to a nested "preferred versions" dictionary.
        if let Some(i) = block.rfind(key) {
            let rest = &block[i + key.len()..];
            let end = rest.find(';')?;
            return Some(rest[..end].trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Locate an installed browser.
pub fn find_browser(browser: Browser, platform: Platform) -> Option<Launcher> {
    match platform {
        Platform::Windows => windows::find_exe(browser).map(Launcher::Exe),
        Platform::Linux => {
            // Normal packages, then snap and Flatpak ones (their launchers are not always on the PATH of an app
            // started from the desktop). The Brave line also covers Chromium.
            let names: &[&str] = match browser {
                Browser::Chrome => &["google-chrome", "google-chrome-stable", "com.google.Chrome"],
                Browser::Edge => &["microsoft-edge", "microsoft-edge-stable", "com.microsoft.Edge"],
                Browser::Brave => &["brave-browser", "brave", "com.brave.Browser", "chromium", "chromium-browser", "org.chromium.Chromium"],
                Browser::Firefox => &["firefox", "org.mozilla.firefox"],
            };
            let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
            dirs.extend(["/snap/bin", "/var/lib/flatpak/exports/bin"].map(PathBuf::from));
            dirs.extend(std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/flatpak/exports/bin")));
            let all = std::env::join_paths(dirs).ok()?;
            find_in_path(names, &all).map(Launcher::Exe)
        }
        Platform::MacOs => {
            let app = match browser {
                Browser::Chrome => "Google Chrome",
                Browser::Edge => "Microsoft Edge",
                Browser::Brave => "Brave Browser",
                Browser::Firefox => "Firefox",
            };
            Path::new(&format!("/Applications/{app}.app")).is_dir().then_some(Launcher::MacApp(app))
        }
        Platform::Other => None,
    }
}

/// First file called one of `names` in the `PATH`-style list `path_var`.
pub fn find_in_path(names: &[&str], path_var: &std::ffi::OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        for n in names {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Open the browser on its own extensions page (a new tab if it is already running).
pub fn open_extensions_page(browser: Browser, launcher: &Launcher) -> std::io::Result<()> {
    let mut url = extensions_url(browser);
    // Chromium (on the Brave line) does not know `brave://`.
    if let Launcher::Exe(exe) = launcher {
        if exe.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("chromium")) {
            url = "chrome://extensions";
        }
    }
    match launcher {
        Launcher::Exe(exe) => spawn(Command::new(exe).arg(url)),
        Launcher::MacApp(app) => spawn(Command::new("open").arg("-a").arg(app).arg(url)),
    }
}

/// Open the browser on the extension's page in its store (Chrome Web Store, which Edge and Brave also install
/// from), where the user can add it at once. `id` must be a Chromium extension id.
pub fn open_store_page(launcher: &Launcher, id: &str) -> std::io::Result<()> {
    if !crate::store_extension::valid_id(id) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not an extension id"));
    }
    let url = store_page_url(id);
    match launcher {
        Launcher::Exe(exe) => spawn(Command::new(exe).arg(&url)),
        Launcher::MacApp(app) => spawn(Command::new("open").arg("-a").arg(app).arg(&url)),
    }
}

/// The extension's page in the Chrome Web Store.
pub fn store_page_url(id: &str) -> String {
    format!("https://chromewebstore.google.com/detail/{id}")
}

/// Show a folder in the system file manager.
pub fn open_folder(platform: Platform, dir: &Path) -> std::io::Result<()> {
    match platform {
        Platform::Windows => {
            let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            spawn(Command::new(root.join("explorer.exe")).arg(dir))
        }
        Platform::MacOs => spawn(Command::new("open").arg(dir)),
        Platform::Linux => spawn(Command::new("xdg-open").arg(dir)),
        Platform::Other => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "unsupported platform")),
    }
}

fn spawn(cmd: &mut Command) -> std::io::Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    pub fn default_browser_id() -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice")
            .ok()?
            .get_value::<String, _>("ProgId")
            .ok()
    }

    pub fn find_exe(browser: Browser) -> Option<PathBuf> {
        let (exe, rel) = match browser {
            Browser::Chrome => ("chrome.exe", r"Google\Chrome\Application\chrome.exe"),
            Browser::Edge => ("msedge.exe", r"Microsoft\Edge\Application\msedge.exe"),
            Browser::Brave => ("brave.exe", r"BraveSoftware\Brave-Browser\Application\brave.exe"),
            Browser::Firefox => ("firefox.exe", r"Mozilla Firefox\firefox.exe"),
        };
        let app_paths = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            if let Ok(key) = RegKey::predef(hive).open_subkey(&app_paths) {
                if let Ok(p) = key.get_value::<String, _>("") {
                    let p = PathBuf::from(p.trim_matches('"'));
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let p = PathBuf::from(base).join(rel);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }
}

#[cfg(not(windows))]
mod windows {
    use super::*;
    pub fn default_browser_id() -> Option<String> {
        None
    }
    pub fn find_exe(_: Browser) -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_default_browsers_on_every_platform() {
        for (id, b) in [
            ("ChromeHTML", Browser::Chrome),
            ("MSEdgeHTM", Browser::Edge),
            ("BraveHTML", Browser::Brave),
            ("FirefoxURL-308046B0AF4A39CB", Browser::Firefox),
            ("google-chrome.desktop", Browser::Chrome),
            ("microsoft-edge.desktop", Browser::Edge),
            ("brave-browser.desktop", Browser::Brave),
            ("firefox.desktop", Browser::Firefox),
            ("com.google.chrome", Browser::Chrome),
            ("com.microsoft.edgemac", Browser::Edge),
            ("com.brave.browser", Browser::Brave),
            ("org.mozilla.firefox", Browser::Firefox),
        ] {
            assert_eq!(browser_from_id(id), Some(b), "{id}");
        }
        for other in ["VivaldiHTM", "OperaStable", "chromium.desktop", "org.gnome.Epiphany", ""] {
            assert_eq!(browser_from_id(other), None, "{other}");
        }
    }

    #[test]
    fn reads_the_https_handler_from_macos_output() {
        let text = "(\n{\n LSHandlerContentType = \"public.html\";\n LSHandlerRoleAll = \"com.apple.safari\";\n},\n{\n LSHandlerRoleAll = \"com.brave.Browser\";\n LSHandlerURLScheme = https;\n}\n)";
        assert_eq!(parse_macos_handlers(text).as_deref(), Some("com.brave.Browser"));
        assert_eq!(parse_macos_handlers("()"), None);
    }

    #[test]
    fn only_built_in_pages_are_opened() {
        for b in Browser::ALL {
            let u = extensions_url(b);
            assert!(u.starts_with("chrome://") || u.starts_with("edge://") || u.starts_with("brave://") || u.starts_with("about:"));
        }
    }

    #[test]
    fn finds_programs_on_a_path_list() {
        let dir = std::env::temp_dir().join(format!("velox-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("brave-browser"), b"").unwrap();
        let path = std::env::join_paths([Path::new("/nonexistent-velox"), dir.as_path()]).unwrap();
        assert_eq!(find_in_path(&["firefox", "brave-browser"], &path), Some(dir.join("brave-browser")));
        assert_eq!(find_in_path(&["firefox"], &path), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
