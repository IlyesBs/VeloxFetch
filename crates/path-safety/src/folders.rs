//! Folders the user may choose to download into.
//!
//! Two rules keep a chosen folder from turning into an attack:
//!
//! * a folder is only ever accepted **after** a native folder dialog (the desktop shell hands it in), never
//!   from text the web view typed, and it must pass [`validate_user_folder`]: it exists, is a real
//!   directory (links are resolved), can be written, is not a drive / file-system root, and is not a place where a
//!   downloaded file would be *run* or would change how the system behaves (program and system folders, the
//!   Startup / autostart / LaunchAgents folders, `.ssh`, the app's own data folder);
//! * everything the engines write is checked against the set of accepted folders ([`AllowedRoots`]) right
//!   before it is written, so a folder swapped for a link afterwards still cannot redirect a download.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::{ensure_within, PathError};

/// The set of folders downloads may be written under. Cheap to clone (shared) and safe to change while
/// downloads run.
#[derive(Debug, Default)]
pub struct AllowedRoots {
    roots: RwLock<Vec<PathBuf>>,
}

impl AllowedRoots {
    pub fn new(roots: Vec<PathBuf>) -> Arc<Self> {
        Arc::new(Self { roots: RwLock::new(roots) })
    }

    pub fn list(&self) -> Vec<PathBuf> {
        self.roots.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn set(&self, roots: Vec<PathBuf>) {
        *self.roots.write().unwrap_or_else(|p| p.into_inner()) = roots;
    }

    pub fn add(&self, root: PathBuf) {
        let mut g = self.roots.write().unwrap_or_else(|p| p.into_inner());
        if !g.contains(&root) {
            g.push(root);
        }
    }

    /// The allowed folder that really contains `path` (symbolic links, junctions and `..` resolved), or an error.
    pub fn check(&self, path: &Path) -> Result<PathBuf, PathError> {
        let roots = self.list();
        let mut last = PathError::Escapes;
        for r in roots {
            match ensure_within(&r, path) {
                Ok(()) => return Ok(r),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

/// Why a folder was refused, in words for the user.
pub type FolderError = String;

/// Remove the `\\?\` prefix Windows adds to canonical paths, so the folder is shown and stored the usual way.
pub fn display_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

/// Accept `path` as a download folder, or say why not. `also_protect` lists more folders that must never be
/// chosen or lie inside a chosen one (the app's own data folder). Returns the canonical folder.
pub fn validate_user_folder(path: &Path, also_protect: &[PathBuf]) -> Result<PathBuf, FolderError> {
    let text = path.to_string_lossy();
    if text.trim().is_empty() || text.contains('\0') || text.len() > 4096 {
        return Err("this is not a folder".into());
    }
    if !path.is_absolute() {
        return Err("the folder must be a full path".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| "that folder does not exist".to_string())?;
    let _ = meta;
    let canon = path.canonicalize().map_err(|_| "that folder cannot be opened".to_string())?;
    if !canon.is_dir() {
        return Err("that is not a folder".into());
    }
    let shown = display_path(&canon);
    if let Some(why) = protected_reason(&shown, also_protect) {
        return Err(format!("that folder is not allowed: {why}"));
    }
    // It must really be writable: try, and clean up.
    let probe = shown.join(format!(".velox-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err("VeloxFetch cannot write to that folder".into()),
        Err(e) => return Err(format!("that folder cannot be written to ({e})")),
    }
    Ok(shown)
}

fn lower(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(any(windows, target_os = "macos")) {
        s.to_lowercase()
    } else {
        s
    }
}

fn inside(candidate: &str, folder: &str) -> bool {
    !folder.is_empty() && (candidate == folder || candidate.starts_with(&format!("{folder}/")))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// `Some(reason)` when downloading into `canon` would be dangerous.
fn protected_reason(canon: &Path, also_protect: &[PathBuf]) -> Option<&'static str> {
    // A file-system or drive root has no name a person would pick on purpose.
    let normal = canon.components().filter(|c| matches!(c, Component::Normal(_))).count();
    if normal == 0 {
        return Some("it is the top of a drive");
    }
    let me = lower(canon);

    // Folders whose name alone is enough, wherever they are.
    const SENSITIVE: [&str; 9] =
        [".ssh", ".gnupg", ".aws", ".kube", "autostart", "launchagents", "launchdaemons", "startupitems", ".config/systemd"];
    let with_slashes = format!("/{me}/").to_lowercase();
    for s in SENSITIVE {
        if with_slashes.contains(&format!("/{}/", s.to_lowercase())) {
            return Some("it is used to sign in or to start programs automatically");
        }
    }
    if with_slashes.contains("/start menu/programs/startup/") || with_slashes.contains("/start menu/programs/startup ") {
        return Some("programs in the Startup folder run every time you sign in");
    }

    // System and program folders.
    let mut system: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["WINDIR", "SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData", "SystemDrive"] {
            if let Some(p) = env_path(var) {
                system.push(p);
            }
        }
        // `SystemDrive` is `C:` (the drive itself, already refused as a root), so only its Windows folder matters.
        system.retain(|p| p.components().filter(|c| matches!(c, Component::Normal(_))).count() > 0);
    } else {
        for p in [
            "/bin",
            "/boot",
            "/dev",
            "/etc",
            "/lib",
            "/lib32",
            "/lib64",
            "/libx32",
            "/proc",
            "/sbin",
            "/sys",
            "/usr",
            "/var/lib",
            "/var/log",
            "/root",
            "/System",
            "/Library",
            "/Applications",
            "/private/etc",
            "/private/var/db",
            "/opt",
            "/snap",
        ] {
            system.push(PathBuf::from(p));
        }
    }
    for s in &system {
        if inside(&me, &lower(s)) {
            return Some("it is a system or program folder");
        }
    }
    for p in also_protect {
        let prot = lower(p);
        if inside(&me, &prot) || inside(&prot, &me) {
            return Some("it holds VeloxFetch's own data");
        }
    }
    // The user's home folder itself and the folder that holds all users' homes.
    for var in ["USERPROFILE", "HOME"] {
        if let Some(h) = env_path(var) {
            if let Some(parent) = h.parent() {
                if lower(parent) == me {
                    return Some("it contains other people's folders");
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-folders-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_ordinary_writable_folder_is_accepted_and_shown_without_the_windows_prefix() {
        let d = tmp("ok");
        let v = validate_user_folder(&d, &[]).unwrap();
        assert!(v.is_dir() && !v.to_string_lossy().starts_with(r"\\?\"), "{v:?}");
        assert!(!v.join(format!(".velox-write-test-{}", std::process::id())).exists(), "the write probe is removed");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nonsense_is_refused_with_a_reason() {
        for bad in ["", "   ", "relative/folder", "..", ".", "C", "/no/such/folder/anywhere-xyz", "\u{0}"] {
            assert!(validate_user_folder(Path::new(bad), &[]).is_err(), "{bad:?}");
        }
        let d = tmp("file");
        let f = d.join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(validate_user_folder(&f, &[]).unwrap_err(), "that is not a folder");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn drive_roots_system_folders_and_startup_folders_are_refused() {
        // A root.
        let root = std::env::temp_dir().ancestors().last().unwrap().to_path_buf();
        assert!(validate_user_folder(&root, &[]).unwrap_err().contains("top of a drive"), "{root:?}");
        // System folders that exist on this machine.
        let candidates: Vec<PathBuf> = if cfg!(windows) {
            ["WINDIR", "ProgramFiles", "ProgramData"].iter().filter_map(|v| env_path(v)).collect()
        } else {
            ["/etc", "/usr", "/bin", "/System", "/Library"].iter().map(PathBuf::from).filter(|p| p.exists()).collect()
        };
        assert!(!candidates.is_empty());
        for c in candidates {
            let e = validate_user_folder(&c, &[]).unwrap_err();
            assert!(e.contains("system or program folder"), "{c:?}: {e}");
            // ... and anything inside them.
            if let Some(inner) =
                std::fs::read_dir(&c).ok().and_then(|mut d| d.find_map(|e| e.ok().filter(|e| e.path().is_dir()).map(|e| e.path())))
            {
                assert!(validate_user_folder(&inner, &[]).is_err(), "{inner:?}");
            }
        }
        // Places that run programs or grant access, wherever they are (built inside a temp folder).
        let base = tmp("sensitive");
        for sub in [
            "AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup",
            "home/.ssh",
            "home/.config/autostart",
            "home/Library/LaunchAgents",
            "home/.gnupg/private-keys-v1.d",
        ] {
            let p = base.join(sub);
            std::fs::create_dir_all(&p).unwrap();
            let e = validate_user_folder(&p, &[]).unwrap_err();
            assert!(e.contains("start programs automatically") || e.contains("sign in"), "{sub}: {e}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_apps_own_data_folder_and_folders_around_it_are_refused() {
        let base = tmp("own");
        let data = base.join("state");
        std::fs::create_dir_all(data.join("deeper")).unwrap();
        let protect = [data.canonicalize().map(|c| display_path(&c)).unwrap()];
        assert!(validate_user_folder(&data, &protect).unwrap_err().contains("own data"));
        assert!(validate_user_folder(&data.join("deeper"), &protect).is_err());
        assert!(validate_user_folder(&base, &protect).is_err(), "a folder that contains it is refused as well");
        let sibling = base.join("downloads");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(validate_user_folder(&sibling, &protect).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_system_folder_is_resolved_and_refused() {
        let base = tmp("link");
        let link = base.join("innocent");
        std::os::unix::fs::symlink("/etc", &link).unwrap();
        assert!(validate_user_folder(&link, &[]).unwrap_err().contains("system"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn allowed_roots_accept_only_what_is_really_inside_and_can_change_while_running() {
        let a = tmp("roots-a");
        let b = tmp("roots-b");
        let roots = AllowedRoots::new(vec![a.clone()]);
        assert_eq!(roots.check(&a.join("x").join("y.bin")).unwrap(), a);
        assert!(roots.check(&b.join("y.bin")).is_err());
        assert!(roots.check(&a.join("..").join(b.file_name().unwrap()).join("y.bin")).is_err(), "`..` does not get out");
        roots.add(b.clone());
        roots.add(b.clone());
        assert_eq!(roots.list().len(), 2, "no duplicates");
        assert_eq!(roots.check(&b.join("y.bin")).unwrap(), b);
        roots.set(vec![b.clone()]);
        assert!(roots.check(&a.join("x.bin")).is_err(), "a folder that was removed from the set is no longer allowed");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
