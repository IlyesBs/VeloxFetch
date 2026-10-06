//! Showing a downloaded file in the system file manager, and opening it with its usual application.
//!
//! Rules (the caller, `download-manager`, adds its own: the path comes from its database and must lie
//! inside a download folder):
//! * fixed programs, no shell, one path argument that is always absolute (so it can never be read as an option);
//! * [`is_risky_to_open`] files (programs, installers, scripts, shortcuts, web pages) are never *opened*,
//!   only shown in their folder: starting a downloaded program must be the user's own act in the file manager,
//!   where the system's own warnings (Mark of the Web, Gatekeeper) apply.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Platform;

/// Extensions that run code (or hand a link to something that does) when opened.
const RISKY: &[&str] = &[
    // Windows programs, installers and scripts
    "exe",
    "msi",
    "msix",
    "msixbundle",
    "msp",
    "appx",
    "appxbundle",
    "bat",
    "cmd",
    "com",
    "cpl",
    "scr",
    "pif",
    "ps1",
    "psm1",
    "psd1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "hta",
    "lnk",
    "url",
    "scf",
    "inf",
    "reg",
    "msc",
    "application",
    "appref-ms",
    "gadget",
    "settingcontent-ms",
    "library-ms",
    "search-ms",
    "diagcab",
    "xll",
    "chm",
    "sys",
    "dll",
    "ocx",
    "vhd",
    "vhdx",
    "iso",
    "img",
    // cross-platform
    "jar",
    "py",
    "pyw",
    "pl",
    "rb",
    "php",
    "sh",
    "bash",
    "zsh",
    "csh",
    "ksh",
    "fish",
    "command",
    "tool",
    "run",
    "bin",
    "elf",
    // macOS / Linux packages and launchers
    "app",
    "pkg",
    "mpkg",
    "dmg",
    "workflow",
    "scpt",
    "applescript",
    "desktop",
    "appimage",
    "deb",
    "rpm",
    "snap",
    "flatpak",
    "flatpakref",
    "apk",
    "xapk",
    "ipa",
    // web pages and documents that run scripts in a browser
    "html",
    "htm",
    "xhtml",
    "shtml",
    "mht",
    "mhtml",
    "svg",
    "xml",
    "xsl",
];

/// Would opening `path` run code? True for programs, installers, scripts, shortcuts and web pages, and
/// for anything without an extension (on Linux / macOS that is often a program).
pub fn is_risky_to_open(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return true };
    // Windows ignores trailing dots and spaces: "setup.exe." is setup.exe.
    let name = name.trim_end_matches(['.', ' ']);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => RISKY.contains(&ext.to_ascii_lowercase().as_str()),
        _ => true,
    }
}

fn not_absolute() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "not an absolute path")
}

/// Show `path` selected in the file manager (its folder, when the file manager cannot select). A folder is opened.
pub fn reveal(platform: Platform, path: &Path) -> std::io::Result<()> {
    if !path.is_absolute() {
        return Err(not_absolute());
    }
    match platform {
        Platform::Windows => reveal_windows(path),
        // A folder (a download not finished yet) is opened itself, not selected inside its parent.
        Platform::MacOs if path.is_dir() => spawn(Command::new("/usr/bin/open").arg(path)),
        Platform::MacOs => spawn(Command::new("/usr/bin/open").arg("-R").arg(path)),
        Platform::Linux if path.is_dir() => spawn(Command::new("xdg-open").arg(path)),
        Platform::Linux => {
            // The freedesktop file-manager interface selects the file (Nautilus, Dolphin, Nemo, Thunar...);
            // with no such service, open the folder.
            let uri = file_uri(path);
            let shown = Command::new("dbus-send")
                .args(["--session", "--print-reply", "--dest=org.freedesktop.FileManager1", "--type=method_call"])
                .args(["/org/freedesktop/FileManager1", "org.freedesktop.FileManager1.ShowItems"])
                .arg(format!("array:string:{uri}"))
                .arg("string:")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if shown {
                Ok(())
            } else {
                spawn(Command::new("xdg-open").arg(path.parent().unwrap_or(path)))
            }
        }
        Platform::Other => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "unsupported platform")),
    }
}

/// Open `path` with the application the system uses for its type. Refuses [`is_risky_to_open`] files.
pub fn open(platform: Platform, path: &Path) -> std::io::Result<()> {
    if !path.is_absolute() {
        return Err(not_absolute());
    }
    if is_risky_to_open(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "programs, scripts and web pages are not opened from VeloxFetch",
        ));
    }
    match platform {
        Platform::Windows => spawn(Command::new(explorer()).arg(path)),
        Platform::MacOs => spawn(Command::new("/usr/bin/open").arg(path)),
        Platform::Linux => spawn(Command::new("xdg-open").arg(path)),
        Platform::Other => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "unsupported platform")),
    }
}

fn explorer() -> PathBuf {
    std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("explorer.exe")
}

/// The path as Explorer understands it: `\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\share` -> `\\srv\share`, `/` -> `\`.
/// Explorer silently opens a default folder for anything else, instead of the download's own.
pub fn explorer_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().replace('/', "\\");
    let s = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s
    };
    PathBuf::from(s)
}

#[cfg(windows)]
fn reveal_windows(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    let path = explorer_path(path);
    // A folder (a download not finished yet): open it.
    if path.is_dir() {
        return spawn(Command::new(explorer()).arg(&path));
    }
    // The shell call selects the file in its own folder, whatever the path looks like (what browsers use).
    if shell_select(&path) {
        return Ok(());
    }
    // Explorer wants `/select,"C:\path with spaces\file"` as one raw argument; a Windows path cannot contain `"`.
    let p = path.to_string_lossy();
    if p.contains('"') {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "unexpected quote in path"));
    }
    spawn(Command::new(explorer()).raw_arg(format!("/select,\"{p}\"")))
}

/// `SHOpenFolderAndSelectItems`: the folder of `path` opens with the file selected.
#[cfg(windows)]
fn shell_select(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the calls; the item list is freed once, after use;
    // COM is released only when this call initialised it.
    unsafe {
        let com = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let item = ILCreateFromPathW(wide.as_ptr());
        let ok = !item.is_null() && SHOpenFolderAndSelectItems(item, 0, std::ptr::null(), 0) >= 0;
        if !item.is_null() {
            ILFree(item);
        }
        if com >= 0 {
            CoUninitialize();
        }
        ok
    }
}

#[cfg(not(windows))]
fn reveal_windows(_: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "not Windows"))
}

/// `file://` URI of an absolute path, every byte outside `A-Z a-z 0-9 - . _ ~ /` percent-encoded (so a `,`
/// cannot split the `dbus-send` array and nothing can be read as another argument).
pub fn file_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Shows / opens files. Replaceable so tests never open windows on the machine they run on.
pub trait FileShell: Send + Sync {
    fn reveal(&self, path: &Path) -> std::io::Result<()>;
    fn open(&self, path: &Path) -> std::io::Result<()>;
}

/// The real file manager and applications.
pub struct SystemShell;

impl FileShell for SystemShell {
    fn reveal(&self, path: &Path) -> std::io::Result<()> {
        reveal(Platform::current(), path)
    }
    fn open(&self, path: &Path) -> std::io::Result<()> {
        open(Platform::current(), path)
    }
}

/// Records what would have been shown or opened, applying the same checks as the real one.
#[derive(Default)]
pub struct DryRunShell {
    pub calls: std::sync::Mutex<Vec<(&'static str, PathBuf)>>,
}

impl FileShell for DryRunShell {
    fn reveal(&self, path: &Path) -> std::io::Result<()> {
        if !path.is_absolute() {
            return Err(not_absolute());
        }
        eprintln!("[dry-run] would show {} in its folder", path.display());
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).push(("reveal", path.to_path_buf()));
        Ok(())
    }
    fn open(&self, path: &Path) -> std::io::Result<()> {
        if !path.is_absolute() {
            return Err(not_absolute());
        }
        if is_risky_to_open(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "programs, scripts and web pages are not opened from VeloxFetch",
            ));
        }
        eprintln!("[dry-run] would open {}", path.display());
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).push(("open", path.to_path_buf()));
        Ok(())
    }
}

fn spawn(cmd: &mut Command) -> std::io::Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_scripts_shortcuts_and_pages_are_never_opened() {
        for name in [
            "setup.exe",
            "SETUP.EXE",
            "a.msi",
            "run.bat",
            "x.cmd",
            "p.ps1",
            "s.vbs",
            "s.js",
            "link.lnk",
            "site.url",
            "a.scr",
            "a.hta",
            "a.jar",
            "a.sh",
            "a.app",
            "a.pkg",
            "a.dmg",
            "a.desktop",
            "a.AppImage",
            "a.deb",
            "page.html",
            "page.htm",
            "icon.svg",
            "setup.exe.",
            "setup.exe ",
            "setup.exe. . ",
            "noextension",
            ".bashrc",
            "a.",
            "a.iso",
        ] {
            assert!(is_risky_to_open(Path::new(name)), "{name}");
        }
        for name in ["movie.mp4", "song.mp3", "photo.JPG", "report.pdf", "sheet.xlsx", "archive.zip", "notes.txt", "book.epub"] {
            assert!(!is_risky_to_open(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn relative_paths_and_risky_files_are_refused_before_anything_runs() {
        assert_eq!(open(Platform::current(), Path::new("movie.mp4")).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(reveal(Platform::current(), Path::new("movie.mp4")).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        let abs = std::env::temp_dir().join("velox-never-run.exe");
        assert_eq!(open(Platform::current(), &abs).unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn explorer_gets_a_path_it_understands() {
        for (given, wanted) in [
            (r"C:\Users\a\Downloads\x.mp4", r"C:\Users\a\Downloads\x.mp4"),
            (r"\\?\D:\Films\x.mp4", r"D:\Films\x.mp4"),
            (r"\\?\UNC\nas\share\x.mp4", r"\\nas\share\x.mp4"),
            ("C:/Users/a/Videos/sub/x.mp4", r"C:\Users\a\Videos\sub\x.mp4"),
            (r"\\nas\share\x.mp4", r"\\nas\share\x.mp4"),
        ] {
            assert_eq!(explorer_path(Path::new(given)), PathBuf::from(wanted), "{given}");
        }
    }

    #[test]
    fn file_uris_escape_everything_that_could_split_an_argument() {
        assert_eq!(file_uri(Path::new("/home/a b/x,y'z\".mp4")), "file:///home/a%20b/x%2Cy%27z%22.mp4");
        assert_eq!(file_uri(Path::new("/tmp/caf\u{e9}")), "file:///tmp/caf%C3%A9");
        assert_eq!(file_uri(Path::new(r"C:\Users\a\f.txt")), "file:///C%3A/Users/a/f.txt");
    }
}
