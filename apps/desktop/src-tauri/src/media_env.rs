//! Linux AppImage: the media framework it carries (live preview) keeps its own plugin cache.
//!
//! GStreamer caches what it learned about its plugins in one file per user (`~/.cache/gstreamer-1.0/`). The
//! AppImage's GStreamer and the system's would share that file and overwrite each other's entries (other plugin
//! folders, possibly another GStreamer version). The AppImage's copy gets a cache of its own instead, in the app's
//! cache folder. A cache the user chose (`GST_REGISTRY_1_0` / `GST_REGISTRY`) is kept. Only the AppImage is
//! concerned: the `.deb` / `.rpm` use the system's GStreamer like any other program.
//!
//! The plugins themselves are chosen when the AppImage is built (`scripts/appimage-media.sh`).

use std::ffi::OsString;
use std::path::PathBuf;

/// The cache file the AppImage's GStreamer should use, `None` when nothing is to change.
pub fn appimage_registry(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |k: &str| get(k).is_some_and(|v| !v.is_empty());
    if !set("APPIMAGE") || set("GST_REGISTRY_1_0") || set("GST_REGISTRY") {
        return None;
    }
    let cache = get("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| get("HOME").map(PathBuf::from).filter(|p| p.is_absolute()).map(|h| h.join(".cache")))?;
    Some(cache.join("app.velox.fetch").join("gstreamer-1.0").join(format!("registry.{}.bin", std::env::consts::ARCH)))
}

/// Apply it to this process (inherited by the web view's processes). Called first thing, before any thread.
pub fn prepare() {
    if !cfg!(target_os = "linux") {
        return;
    }
    if let Some(path) = appimage_registry(|k| std::env::var_os(k)) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::env::set_var("GST_REGISTRY_1_0", &path);
    }
}

// Unix paths (an AppImage only exists there; "/home/u" is not an absolute path on Windows).
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn with(vars: &[(&str, &str)]) -> Option<PathBuf> {
        let m: HashMap<String, OsString> = vars.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        appimage_registry(|k| m.get(k).cloned())
    }

    #[test]
    fn the_appimage_gets_its_own_cache() {
        let p = with(&[("APPIMAGE", "/opt/VeloxFetch.AppImage"), ("HOME", "/home/u")]).unwrap();
        assert!(p.starts_with("/home/u/.cache/app.velox.fetch/gstreamer-1.0"), "{p:?}");
        let p = with(&[("APPIMAGE", "/a"), ("HOME", "/home/u"), ("XDG_CACHE_HOME", "/c")]).unwrap();
        assert!(p.starts_with("/c/app.velox.fetch/"), "{p:?}");
        let p = with(&[("APPIMAGE", "/a"), ("HOME", "/home/u"), ("XDG_CACHE_HOME", "rel")]).unwrap();
        assert!(p.starts_with("/home/u/.cache/"), "a relative cache folder is ignored, as the XDG rules say: {p:?}");
    }

    #[test]
    fn nothing_changes_outside_the_appimage_or_when_the_user_chose() {
        assert_eq!(with(&[("HOME", "/home/u")]), None, "installed packages use the system's GStreamer as is");
        assert_eq!(with(&[("APPIMAGE", ""), ("HOME", "/home/u")]), None);
        assert_eq!(with(&[("APPIMAGE", "/a"), ("HOME", "/home/u"), ("GST_REGISTRY_1_0", "/mine.bin")]), None);
        assert_eq!(with(&[("APPIMAGE", "/a"), ("HOME", "/home/u"), ("GST_REGISTRY", "/mine.bin")]), None);
        assert_eq!(with(&[("APPIMAGE", "/a")]), None, "no home folder: GStreamer's own default");
    }
}
