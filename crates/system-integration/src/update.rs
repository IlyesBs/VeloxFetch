//! Installing a VeloxFetch update (Phase 19) that the download manager already verified against the signed
//! manifest. Every function here checks the file's exact size and SHA-256 once more, right before it is used:
//!
//! | Installation | What happens |
//! |---|---|
//! | Windows setup (`nsis`) | the installer is opened while VeloxFetch holds the file open with *read-only sharing*: nobody can change, replace or delete it between the check and the start (the user then confirms the system's administrator prompt) |
//! | Linux AppImage | the new AppImage is copied next to the running one *while it is hashed*, checked, made executable, then renamed over it in one step (never half-written); the app starts again |
//! | `.deb` / `.rpm` / macOS `.dmg` | opened with the system's own installer / disk image tool, where the user finishes |
//!
//! Nothing here takes a path or an address from the UI: the caller passes the manager's verified package, and the
//! release page is a constant of the build.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The argument a relaunched copy gets: it waits for the old one to leave before it takes over.
pub const AFTER_UPDATE_ARG: &str = "--after-update";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    /// The new version takes over: the app should quit now.
    Restart,
    /// The system's installer was opened: the user finishes there.
    Opened,
}

fn err(kind: std::io::ErrorKind, msg: &str) -> std::io::Error {
    std::io::Error::new(kind, msg.to_string())
}

fn verify(r: impl Read, size: u64, sha256: &[u8; 32]) -> std::io::Result<()> {
    update_verify::check_reader(r, size, sha256).map_err(|e| err(std::io::ErrorKind::InvalidData, &e.to_string()))
}

/// How this copy of VeloxFetch was installed: `nsis`, `appimage`, `deb`, `rpm`, `dmg`; `None` for a portable
/// copy (it is told about new versions, and the release page opens).
pub fn installation_kind() -> Option<&'static str> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(windows) {
        // The setup puts its uninstaller next to the program; the portable zip has none.
        return exe.parent()?.join("uninstall.exe").is_file().then_some("nsis");
    }
    if cfg!(target_os = "macos") {
        return exe.to_string_lossy().contains(".app/Contents/MacOS/").then_some("dmg");
    }
    if cfg!(target_os = "linux") {
        if appimage_path().is_some() {
            return Some("appimage");
        }
        if exe.starts_with("/usr") {
            if Path::new("/var/lib/dpkg/info/velox-fetch.list").is_file() {
                return Some("deb");
            }
            if Path::new("/usr/bin/rpm").is_file() || Path::new("/bin/rpm").is_file() {
                return Some("rpm");
            }
        }
    }
    None
}

/// The running AppImage file (set by the AppImage runtime): an absolute path to a real file, not a link.
fn appimage_path() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("APPIMAGE")?);
    let meta = std::fs::symlink_metadata(&p).ok()?;
    (p.is_absolute() && meta.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("AppImage"))).then_some(p)
}

/// Install the verified package at `path` (`size` bytes, `sha256`) for installation `kind`.
pub fn install(kind: &str, path: &Path, size: u64, sha256: &[u8; 32]) -> std::io::Result<Installed> {
    if !path.is_absolute() {
        return Err(err(std::io::ErrorKind::InvalidInput, "not an absolute path"));
    }
    match kind {
        "nsis" => run_windows_setup(path, size, sha256).map(|()| Installed::Restart),
        "appimage" => {
            let target = appimage_path().ok_or_else(|| err(std::io::ErrorKind::NotFound, "this copy is not running as an AppImage"))?;
            replace_file(path, &target, size, sha256)?;
            relaunch(&target)?;
            Ok(Installed::Restart)
        }
        "deb" | "rpm" | "dmg" => {
            verify(File::open(path)?, size, sha256)?;
            open_with_system(path)?;
            Ok(Installed::Opened)
        }
        _ => Err(err(std::io::ErrorKind::Unsupported, "this kind of installation cannot be updated from the app")),
    }
}

/// Copy `from` over `target` atomically, checking the bytes as they are copied: what lands is exactly what was
/// checked. The copy is made in the target's folder (same file system), then renamed over it.
pub fn replace_file(from: &Path, target: &Path, size: u64, sha256: &[u8; 32]) -> std::io::Result<()> {
    let dir = target.parent().ok_or_else(|| err(std::io::ErrorKind::InvalidInput, "no folder"))?;
    let name = target.file_name().ok_or_else(|| err(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let tmp = dir.join(format!(".{}.update-{}", name.to_string_lossy(), std::process::id()));
    let result = (|| {
        let src = File::open(from)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o700);
        }
        let mut out = opts.open(&tmp)?;
        // Hash what is written: a reader that tees into the new file.
        struct Tee<'a, R: Read> {
            src: R,
            out: &'a mut File,
            err: Option<std::io::Error>,
        }
        impl<R: Read> Read for Tee<'_, R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.src.read(buf)?;
                if let Err(e) = self.out.write_all(&buf[..n]) {
                    self.err = Some(std::io::Error::new(e.kind(), e.to_string()));
                    return Err(e);
                }
                Ok(n)
            }
        }
        let mut tee = Tee { src: src.take(size + 1), out: &mut out, err: None };
        verify(&mut tee, size, sha256)?;
        if let Some(e) = tee.err {
            return Err(e);
        }
        out.sync_all()?;
        drop(out);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, target)?;
        #[cfg(unix)]
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Start the new copy; it waits for this one to quit (`AFTER_UPDATE_ARG`).
fn relaunch(program: &Path) -> std::io::Result<()> {
    Command::new(program).arg(AFTER_UPDATE_ARG).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

fn open_with_system(path: &Path) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/bin/open");
        c.arg(path);
        c
    } else if cfg!(target_os = "linux") {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    } else {
        return Err(err(std::io::ErrorKind::Unsupported, "unsupported platform"));
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

/// Open the release page (a constant of the build, `https://` only) in the default browser.
pub fn open_release_page(url: &str) -> std::io::Result<()> {
    let safe = url.starts_with("https://")
        && url.len() < 512
        && url.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\'' | b'`' | b'^' | b'&' | b'|' | b'<' | b'>'));
    if !safe {
        return Err(err(std::io::ErrorKind::InvalidInput, "not a release page address"));
    }
    let mut cmd = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut c = Command::new(root.join("explorer.exe"));
        c.arg(url);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/bin/open");
        c.arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

#[cfg(windows)]
fn run_windows_setup(path: &Path, size: u64, sha256: &[u8; 32]) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    const FILE_SHARE_READ: u32 = 0x1;
    const SW_SHOWNORMAL: i32 = 1;
    // Held for the whole check-and-start: others may read the file (the installer itself), never write, rename
    // or delete it.
    let held = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path)?;
    verify(&held, size, sha256)?;
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let file = wide(path.as_os_str());
    let verb = wide(std::ffi::OsStr::new("open"));
    // SAFETY: a zeroed SHELLEXECUTEINFOW is valid (null pointers, no flags); the strings outlive the call.
    let ok = unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        ShellExecuteExW(&mut info)
    };
    let started = if ok != 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) };
    drop(held);
    started
}

#[cfg(not(windows))]
fn run_windows_setup(_path: &Path, _size: u64, _sha256: &[u8; 32]) -> std::io::Result<()> {
    Err(err(std::io::ErrorKind::Unsupported, "a Windows setup is only run on Windows"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-si-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_file_is_replaced_only_by_exactly_the_checked_bytes() {
        let d = temp("replace");
        let (new, target) = (d.join("new.AppImage"), d.join("VeloxFetch.AppImage"));
        std::fs::write(&new, b"new version").unwrap();
        std::fs::write(&target, b"old version").unwrap();
        let sha: [u8; 32] = Sha256::digest(b"new version").into();
        // A wrong checksum or size: the old file stays, and no temporary file is left.
        assert!(replace_file(&new, &target, 11, &[0u8; 32]).is_err());
        assert!(replace_file(&new, &target, 10, &sha).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old version");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2);
        replace_file(&new, &target, 11, &sha).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new version");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn packages_are_checked_before_they_are_opened() {
        let d = temp("open");
        let p = d.join("a.deb");
        std::fs::write(&p, b"package").unwrap();
        // A mismatch never reaches the system installer.
        let e = install("deb", &p, 7, &[1u8; 32]).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        assert!(install("deb", Path::new("relative.deb"), 7, &[1u8; 32]).is_err());
        assert!(install("exe", &p, 7, &[1u8; 32]).is_err());
        if !cfg!(windows) {
            assert_eq!(install("nsis", &p, 7, &[1u8; 32]).unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_a_plain_https_release_page_is_opened() {
        for bad in ["http://example.com", "file:///etc/passwd", "https://a b", "https://x\"y", "https://a&calc", "javascript:alert(1)"] {
            assert!(open_release_page(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_test_run_is_not_an_installed_copy() {
        // Test binaries live in target/: never mistaken for an installation that could replace itself.
        if std::env::var_os("APPIMAGE").is_none() {
            assert_eq!(installation_kind(), None);
        }
    }
}
