//! Filesystem safety, shared by every part of VeloxFetch that names or writes a file.
//!
//! One place decides what a safe file name is (so the downloader, the media engine and the input
//! validation can never disagree), whether a path really stays inside the download folder once
//! symbolic links are resolved, and how a file is opened without following a planted link.
//! The rules are the strictest of Windows, macOS and Linux, so a name that is fine here is fine
//! everywhere the file might later be copied to.

mod folders;
pub use folders::{display_path, validate_user_folder, AllowedRoots, FolderError};

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Longest file name we produce or accept, in UTF-8 bytes. Filesystems allow 255 (bytes on
/// Linux/APFS, UTF-16 units on NTFS); the margin leaves room for `.part` and ` (99)` suffixes.
pub const MAX_NAME_BYTES: usize = 200;
/// An extension longer than this is not treated as one when a long name is shortened.
const MAX_EXT_BYTES: usize = 16;

const WINDOWS_RESERVED: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
    "CONIN$",
    "CONOUT$",
];

/// Windows treats `CON`, `con.txt` and `CON .txt` all as the console device.
pub fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ').to_uppercase();
    WINDOWS_RESERVED.contains(&stem.as_str())
}

/// Characters that are invisible or that reorder text: they let `invoice\u{202e}fdp.exe` look like
/// `invoice.pdf`, or make two different names look identical.
fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(c as u32,
            0xad | 0x34f | 0x61c | 0x115f | 0x1160 | 0x17b4 | 0x17b5 | 0x180b..=0x180f
            | 0x200b..=0x200f | 0x2028..=0x202e | 0x2060..=0x206f | 0x3164 | 0xfe00..=0xfe0f
            | 0xfeff | 0xffa0 | 0xfff0..=0xfffb | 0xfffe | 0xffff | 0xe0000..=0xe0fff)
}

/// Not allowed in a name on Windows; `:` also starts an NTFS alternate data stream (`a.txt:evil.exe`).
fn is_illegal(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\')
}

fn trim_edges(s: &str) -> &str {
    s.trim_matches(|c: char| c == '.' || c.is_whitespace())
}

/// Shorten to [`MAX_NAME_BYTES`] on a character boundary, keeping a short extension.
fn limit_len(name: &str) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= MAX_EXT_BYTES + 1 => name.split_at(i),
        _ => (name, ""),
    };
    let mut end = MAX_NAME_BYTES - ext.len();
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ext}", stem[..end].trim_end_matches(|c: char| c == '.' || c.is_whitespace()))
}

/// Reduce an untrusted name (a server's `Content-Disposition`, a URL segment, a manifest title) to
/// a single safe file name. Never fails and never returns an empty string.
pub fn sanitize_filename(raw: &str) -> String {
    // Only the last path component, whichever separator style was used.
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last.chars().filter(|c| !is_invisible(*c)).map(|c| if is_illegal(c) { '_' } else { c }).collect();
    let mut name = limit_len(trim_edges(&cleaned));
    if name.is_empty() {
        return "download".to_string();
    }
    if is_reserved_device_name(&name) {
        name.insert(0, '_');
    }
    name
}

/// Strict check for names that are *chosen by the user or the UI*: reject, never repair.
pub fn check_component(c: &str) -> Result<(), &'static str> {
    if c.is_empty() || c == "." || c == ".." {
        Err("is empty or a relative marker")
    } else if c.len() > MAX_NAME_BYTES {
        Err("is too long")
    } else if c.chars().any(is_invisible) {
        Err("contains control or invisible characters")
    } else if c.chars().any(is_illegal) {
        Err("contains a character that is not allowed in file names")
    } else if c.ends_with('.') || c.ends_with(' ') || c.starts_with(' ') {
        Err("starts with a space or ends with a dot or space")
    } else if is_reserved_device_name(c) {
        Err("is a reserved device name")
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum PathError {
    /// The path leads outside the allowed folder (often through a link).
    Escapes,
    /// The allowed folder itself could not be examined.
    Root(io::Error),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Escapes => f.write_str("path escapes the download folder"),
            PathError::Root(e) => write!(f, "download folder: {e}"),
        }
    }
}

impl std::error::Error for PathError {}

/// `path` (which may not exist yet) must really live under `root` once symbolic links, junctions,
/// `..` and case differences are resolved.
pub fn ensure_within(root: &Path, path: &Path) -> Result<(), PathError> {
    let canon_root = root.canonicalize().map_err(PathError::Root)?;
    let mut probe = path;
    // Canonicalize the deepest ancestor that exists; the rest is plain, already-validated names.
    let existing = loop {
        if let Ok(c) = probe.canonicalize() {
            break c;
        }
        match probe.parent() {
            Some(p) => probe = p,
            None => return Err(PathError::Escapes),
        }
    };
    if existing.starts_with(&canon_root) {
        Ok(())
    } else {
        Err(PathError::Escapes)
    }
}

/// Create or truncate `path` for writing, but never through a planted link: a symbolic link at
/// the final component, or (on Unix) a file with several hard links, is refused instead of
/// silently overwriting whatever it points to.
pub fn create_no_follow(path: &Path) -> io::Result<File> {
    refuse_links(path)?;
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    harden(&mut o);
    let f = o.open(path)?;
    Ok(f)
}

/// Open an existing file for writing under the same rule as [`create_no_follow`].
pub fn open_write_no_follow(path: &Path) -> io::Result<File> {
    refuse_links(path)?;
    let mut o = OpenOptions::new();
    o.write(true);
    harden(&mut o);
    o.open(path)
}

fn refuse_links(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "refusing to write through a symbolic link"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if m.is_file() && m.nlink() > 1 {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "refusing to write to a file with several hard links"));
                }
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The address a file came from, made fit to be written into file metadata: no user name or
/// password, no query or fragment (signed links carry a token there), no control characters.
pub fn origin_for_metadata(url: &str) -> String {
    let no_tail = url.split(['?', '#']).next().unwrap_or("");
    let cleaned: String = no_tail.chars().filter(|c| !c.is_control()).collect();
    match cleaned.find("://") {
        Some(i) => {
            let (scheme, rest) = cleaned.split_at(i + 3);
            let end = rest.find('/').unwrap_or(rest.len());
            let host = rest[..end].rsplit('@').next().unwrap_or("");
            format!("{scheme}{host}{}", &rest[end..])
        }
        None => String::new(),
    }
}

/// Tell the operating system this file came from the internet, so it warns before running or
/// opening it (SmartScreen and Office "Protected View" on Windows, Gatekeeper on macOS).
/// Windows: an NTFS `Zone.Identifier` stream with `ZoneId=3`. macOS: the `com.apple.quarantine`
/// attribute. Linux has no equivalent, so nothing is done there. Best effort: file systems
/// without streams or attributes (FAT, some network shares) simply refuse, and that is not an error
/// worth failing a finished download for.
pub fn mark_downloaded(path: &Path, source_url: &str) -> io::Result<()> {
    let origin = origin_for_metadata(source_url);
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let mut body = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
        if !origin.is_empty() {
            body.push_str(&format!("HostUrl={origin}\r\n"));
        }
        std::fs::write(stream, body)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let _ = &origin;
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let value = format!("0081;{secs:x};VeloxFetch;");
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::other("path contains a NUL"))?;
        let name = std::ffi::CString::new("com.apple.quarantine").expect("static name");
        // SAFETY: both strings are NUL-terminated and outlive the call; the value pointer and length match.
        let r = unsafe { libc::setxattr(c_path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0) };
        if r == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (path, origin);
        Ok(())
    }
}

#[cfg(unix)]
fn harden(o: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    // Closes the gap between the check above and the open: a link swapped in now is refused by the kernel.
    o.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(not(unix))]
fn harden(_: &mut OpenOptions) {}

/// Narrow where this process may load code from. Call it first thing in `main` of every program we
/// ship.
///
/// Windows: without this, a DLL that is not one of the program's own imports (a delay-loaded or
/// `LoadLibrary`-ed system library) is searched for in the *current directory* and along `PATH`
/// first, so a file dropped into the folder the program was started from (typically Downloads!)
/// would be loaded instead of the real one. After the call only the application's own folder,
/// System32 and directories added on purpose are searched, and the current directory never is.
/// Other systems have no such search order to fix. Returns whether the restriction is in force.
pub fn harden_process() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn SetDefaultDllDirectories(directory_flags: u32) -> i32;
            fn SetDllDirectoryW(path: *const u16) -> i32;
        }
        const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
        // SAFETY: plain Win32 calls with a constant flag and a pointer to a NUL-terminated empty wide string.
        unsafe {
            // An empty string removes the current directory from the search order.
            let empty = [0u16];
            let a = SetDllDirectoryW(empty.as_ptr());
            let b = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
            a != 0 && b != 0
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod hardening_tests {
    #[test]
    fn hardening_is_applied_on_windows_and_harmless_elsewhere() {
        let ok = super::harden_process();
        assert_eq!(ok, cfg!(windows));
        assert_eq!(super::harden_process(), ok, "calling it twice is fine");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("velox-ps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn traversal_and_separators() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("..\\..\\win\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_filename("a/b\\c/d.zip"), "d.zip");
        assert_eq!(sanitize_filename("/etc/"), "download");
        assert_eq!(sanitize_filename("C:\\Windows\\system32\\x.dll"), "x.dll");
        for s in ["..", ".", "", "...", " . ", "\u{0}", "/", "\\"] {
            assert_eq!(sanitize_filename(s), "download", "{s:?}");
        }
    }

    #[test]
    fn illegal_characters_and_streams() {
        assert_eq!(sanitize_filename("a<b>c:d.txt"), "a_b_c_d.txt");
        assert_eq!(sanitize_filename("we\u{0}ird\n.bin"), "weird.bin");
        assert_eq!(sanitize_filename("report.txt:evil.exe"), "report.txt_evil.exe");
        assert_eq!(sanitize_filename("file.txt::$DATA"), "file.txt__$DATA");
        assert_eq!(sanitize_filename("q?*\"|.bin"), "q____.bin");
    }

    #[test]
    fn reserved_device_names() {
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("nul"), "_nul");
        assert_eq!(sanitize_filename("Com1.tar.gz"), "_Com1.tar.gz");
        assert_eq!(sanitize_filename("lpt9"), "_lpt9");
        assert_eq!(sanitize_filename("COM\u{b9}.txt"), "_COM\u{b9}.txt");
        assert_eq!(sanitize_filename("CONIN$"), "_CONIN$");
        assert_eq!(sanitize_filename("CON .txt"), "_CON .txt");
        // Only the whole stem is reserved.
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
        assert_eq!(sanitize_filename("COM10"), "COM10");
    }

    #[test]
    fn edges_and_unicode_tricks() {
        assert_eq!(sanitize_filename("  name.txt. . "), "name.txt");
        assert_eq!(sanitize_filename(".hidden"), "hidden");
        // Right-to-left override: must not survive to disguise an extension.
        assert_eq!(sanitize_filename("invoice\u{202e}fdp.exe"), "invoicefdp.exe");
        assert_eq!(sanitize_filename("a\u{200b}b\u{200d}c\u{feff}.txt"), "abc.txt");
        assert_eq!(sanitize_filename("x\u{2028}y\u{2029}z.txt"), "xyz.txt");
        assert_eq!(sanitize_filename("tag\u{e0041}.txt"), "tag.txt");
        assert_eq!(sanitize_filename("\u{202e}\u{200b}"), "download");
        // Ordinary non-ASCII names are left alone.
        assert_eq!(sanitize_filename("évènement ✓ 日本語.pdf"), "évènement ✓ 日本語.pdf");
    }

    #[test]
    fn long_names_keep_their_extension_and_stay_valid_utf8() {
        let n = sanitize_filename(&format!("{}.tar.gz", "a".repeat(1000)));
        assert!(n.len() <= MAX_NAME_BYTES && n.ends_with(".gz"), "{}", n.len());
        // 4-byte characters: cut on a boundary, never mid-character, never over the byte limit.
        let n = sanitize_filename(&format!("{}.mp4", "😀".repeat(300)));
        assert!(n.len() <= MAX_NAME_BYTES && n.ends_with(".mp4"));
        assert!(n.starts_with('😀'));
        // No extension at all.
        assert_eq!(sanitize_filename(&"é".repeat(500)).len(), 200);
        // A "long extension" is just part of the name, so the cut may remove it.
        let n = sanitize_filename(&format!("a.{}", "b".repeat(500)));
        assert!(n.len() <= MAX_NAME_BYTES);
        // Truncation must not leave a trailing dot/space (Windows would silently drop it).
        let n = sanitize_filename(&format!("{} .{}", "a".repeat(198), "x".repeat(50)));
        assert!(!n.ends_with('.') && !n.ends_with(' '));
        // Something reserved after shortening is still caught.
        assert!(!is_reserved_device_name(&sanitize_filename("con")) || sanitize_filename("con").starts_with('_'));
    }

    #[test]
    fn sanitizing_is_idempotent_and_output_always_passes_the_strict_check() {
        let samples = [
            "../../x",
            "CON",
            "a:b",
            "  ",
            "trail. ",
            "\u{202e}abc",
            "x".repeat(400).leak(),
            "😀😀.exe",
            "nul.txt",
            "a\u{0}b",
            "COM1 ",
            "..\\..\\CON.txt",
            "file.",
            "lpt1.",
            "é".repeat(150).leak(),
        ];
        for s in samples {
            let once = sanitize_filename(s);
            assert_eq!(sanitize_filename(&once), once, "not idempotent for {s:?}");
            assert!(check_component(&once).is_ok(), "{s:?} -> {once:?} fails the strict check");
        }
    }

    #[test]
    fn strict_check_rejects_instead_of_repairing() {
        assert!(check_component("movie.mkv").is_ok());
        assert!(check_component("évènement ✓.pdf").is_ok());
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a:b",
            "a*",
            "a?",
            "a|b",
            "a<b",
            "a\"b",
            "x.",
            "x ",
            " x",
            "nul",
            "CON.txt",
            "COM\u{b2}",
            "a\u{202e}b",
            "a\u{200b}",
            "a\u{0}",
            "a\nb",
        ] {
            assert!(check_component(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(check_component(&"a".repeat(MAX_NAME_BYTES)).is_ok());
        assert!(check_component(&"a".repeat(MAX_NAME_BYTES + 1)).is_err());
        assert!(check_component(&"é".repeat(101)).is_err(), "length is counted in bytes");
    }

    #[test]
    fn within_accepts_children_and_rejects_the_rest() {
        let r = tmp("within");
        std::fs::create_dir_all(r.join("a/b")).unwrap();
        assert!(ensure_within(&r, &r).is_ok());
        assert!(ensure_within(&r, &r.join("a/b")).is_ok());
        assert!(ensure_within(&r, &r.join("not/yet/created")).is_ok(), "future paths under the root are fine");
        assert!(ensure_within(&r, &r.join("a/../..")).is_err());
        assert!(ensure_within(&r, r.parent().unwrap()).is_err());
        assert!(ensure_within(&r, &std::env::temp_dir().join("velox-ps-elsewhere-never")).is_err());
        assert!(matches!(ensure_within(&r.join("missing-root"), &r), Err(PathError::Root(_))));
        let _ = std::fs::remove_dir_all(&r);
    }

    /// A link to a folder, or a file, that points outside `root`. `None` when the OS refuses
    /// (Windows needs a privilege for file symlinks; junctions for folders always work).
    fn link_dir(link: &Path, target: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J", link.to_str().unwrap(), target.to_str().unwrap()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
    }

    fn link_file(link: &Path, target: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
    }

    #[test]
    fn a_link_out_of_the_root_is_an_escape() {
        let r = tmp("linkdir");
        let outside = tmp("linkdir-outside");
        let link = r.join("l");
        if link_dir(&link, &outside) {
            assert!(ensure_within(&r, &link).is_err());
            assert!(ensure_within(&r, &link.join("deeper/still")).is_err());
            #[cfg(unix)]
            let _ = std::fs::remove_file(&link);
            #[cfg(windows)]
            let _ = std::fs::remove_dir(&link);
        } else {
            eprintln!("skipped: cannot create a folder link here");
        }
        let _ = std::fs::remove_dir_all(&r);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn writing_through_a_file_link_is_refused_and_the_target_is_untouched() {
        let r = tmp("linkfile");
        let victim = r.join("victim.txt");
        std::fs::write(&victim, b"precious").unwrap();
        let link = r.join("evil.part");
        if !link_file(&link, &victim) {
            eprintln!("skipped: cannot create a file symlink here");
            let _ = std::fs::remove_dir_all(&r);
            return;
        }
        assert!(create_no_follow(&link).is_err());
        assert!(open_write_no_follow(&link).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        // A dangling link is refused as well (it would otherwise *create* the target).
        let gone = r.join("gone-target");
        let dangling = r.join("dangling.part");
        if link_file(&dangling, &gone) {
            assert!(create_no_follow(&dangling).is_err());
            assert!(!gone.exists());
        }
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_origin_written_into_metadata_carries_no_secret() {
        assert_eq!(origin_for_metadata("https://user:pw@h.test/a/b.zip?token=SECRET#x"), "https://h.test/a/b.zip");
        assert_eq!(origin_for_metadata("https://h.test/a.zip"), "https://h.test/a.zip");
        assert_eq!(origin_for_metadata("https://h.test"), "https://h.test");
        assert_eq!(origin_for_metadata("https://h.test/a\r\nZoneId=1"), "https://h.test/aZoneId=1", "no line breaks to forge a field");
        assert_eq!(origin_for_metadata("not a url"), "");
    }

    #[cfg(windows)]
    #[test]
    fn a_download_is_marked_as_coming_from_the_internet_on_windows() {
        let r = tmp("motw");
        let f = r.join("setup.exe");
        std::fs::write(&f, b"MZ").unwrap();
        if mark_downloaded(&f, "https://dl.example.com/setup.exe?sig=SECRET").is_err() {
            eprintln!("skipped: this file system has no alternate data streams");
            return;
        }
        let mut stream = f.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let text = std::fs::read_to_string(stream).unwrap();
        assert!(
            text.contains("[ZoneTransfer]") && text.contains("ZoneId=3") && text.contains("HostUrl=https://dl.example.com/setup.exe"),
            "{text}"
        );
        assert!(!text.contains("SECRET"));
        assert_eq!(std::fs::read(&f).unwrap(), b"MZ", "the file itself is untouched");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn ordinary_files_still_work() {
        use std::io::Write;
        let r = tmp("plain");
        let p = r.join("x.part");
        create_no_follow(&p).unwrap().write_all(b"one").unwrap();
        create_no_follow(&p).unwrap().write_all(b"2").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"2", "truncates like File::create");
        open_write_no_follow(&p).unwrap().write_all(b"3").unwrap();
        assert!(open_write_no_follow(&r.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_linked_file_is_refused() {
        let r = tmp("hard");
        let a = r.join("a");
        std::fs::write(&a, b"keep").unwrap();
        std::fs::hard_link(&a, r.join("b")).unwrap();
        assert!(create_no_follow(&r.join("b")).is_err());
        assert_eq!(std::fs::read(&a).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&r);
    }
}

#[cfg(test)]
mod robustness {
    use super::*;

    /// Small deterministic generator: the same inputs on every run and every OS, so a failure reproduces.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    #[allow(dead_code)]
    /// Damage `seed` the ways a hostile server or a bit-flip would: cut, repeat, splice, replace.
    fn mutate(rng: &mut Rng, seed: &str, pool: &[&str]) -> String {
        let mut chars: Vec<char> = seed.chars().collect();
        for _ in 0..1 + rng.below(6) {
            if chars.is_empty() {
                break;
            }
            let at = rng.below(chars.len());
            match rng.below(5) {
                0 => chars.truncate(at),
                1 => {
                    let end = (at + 1 + rng.below(40)).min(chars.len());
                    let piece: Vec<char> = chars[at..end].to_vec();
                    for _ in 0..1 + rng.below(30) {
                        for (k, c) in piece.iter().enumerate() {
                            chars.insert(at + k, *c);
                        }
                    }
                }
                2 => {
                    let end = (at + 1 + rng.below(20)).min(chars.len());
                    chars.drain(at..end);
                }
                3 => {
                    let t: Vec<char> = pool[rng.below(pool.len())].chars().collect();
                    for (k, c) in t.iter().enumerate() {
                        chars.insert(at + k, *c);
                    }
                }
                _ => chars[at] = char::from_u32(rng.next() as u32 % 0x3000).unwrap_or('?'),
            }
        }
        chars.into_iter().collect()
    }

    #[test]
    fn any_text_at_all_becomes_a_safe_name() {
        let pool = [
            "..",
            "/",
            "\\",
            ":",
            "*",
            "?",
            "\"",
            "<",
            ">",
            "|",
            "\u{202e}",
            "\u{200b}",
            "\u{0}",
            " ",
            ".",
            "CON",
            "nul",
            "COM1",
            "\u{e9}",
            "\u{1f600}",
            "\u{feff}",
            "a",
            "b",
            ".exe",
            "\r\n",
            "\u{2028}",
            "aux.txt",
            "\u{e0041}",
            "~",
            "$",
        ];
        let mut rng = Rng(7);
        for _ in 0..20_000 {
            let n = rng.below(60);
            let mut s = String::new();
            for _ in 0..n {
                if rng.below(4) == 0 {
                    s.push(char::from_u32(rng.next() as u32 % 0x11_0000).unwrap_or('x'));
                } else {
                    s.push_str(pool[rng.below(pool.len())]);
                }
            }
            let once = sanitize_filename(&s);
            assert!(check_component(&once).is_ok(), "{s:?} -> {once:?} is not a safe name");
            assert!(once.len() <= MAX_NAME_BYTES && !once.is_empty());
            assert_eq!(sanitize_filename(&once), once, "not idempotent for {s:?}");
            assert!(!once.contains(['/', '\\', ':']) && once != "." && once != "..");
            let _ = origin_for_metadata(&s);
        }
    }
}
