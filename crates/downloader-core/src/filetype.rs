//! What kind of file is this? Used to sort downloads into folders (Video, Audio, Documents, ...).
//!
//! The extension decides when it is one we know; otherwise the server's `Content-Type`; otherwise `Other`.
//! Only the *last* extension counts (`a.tar.gz` is a `gz`, so an archive).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Video,
    Audio,
    Image,
    Document,
    Archive,
    Program,
    Other,
}

impl FileKind {
    pub const ALL: [FileKind; 7] =
        [FileKind::Video, FileKind::Audio, FileKind::Image, FileKind::Document, FileKind::Archive, FileKind::Program, FileKind::Other];

    /// The stable key used in settings and in the UI.
    pub fn key(self) -> &'static str {
        match self {
            FileKind::Video => "video",
            FileKind::Audio => "audio",
            FileKind::Image => "image",
            FileKind::Document => "document",
            FileKind::Archive => "archive",
            FileKind::Program => "program",
            FileKind::Other => "other",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    /// The folder this kind goes into unless the user renamed it.
    pub fn default_folder(self) -> &'static str {
        match self {
            FileKind::Video => "Video",
            FileKind::Audio => "Audio",
            FileKind::Image => "Images",
            FileKind::Document => "Documents",
            FileKind::Archive => "Archives",
            FileKind::Program => "Programs",
            FileKind::Other => "Other",
        }
    }
}

const VIDEO: &[&str] =
    &["mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "flv", "mpg", "mpeg", "ts", "m2ts", "mts", "3gp", "ogv", "vob", "divx"];
const AUDIO: &[&str] =
    &["mp3", "flac", "wav", "aac", "m4a", "ogg", "oga", "opus", "wma", "aiff", "aif", "mid", "midi", "amr", "ape", "mka"];
const IMAGE: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "tif", "tiff", "ico", "heic", "heif", "avif", "raw", "cr2", "nef", "arw", "dng",
    "psd",
];
const DOCUMENT: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "odp", "epub", "mobi", "azw3", "djvu",
    "tex", "pages", "numbers", "key", "log", "json", "xml", "html", "htm",
];
const ARCHIVE: &[&str] = &["zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "lz", "lzma", "cab", "z"];
const PROGRAM: &[&str] = &[
    "exe",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "apk",
    "aab",
    "ipa",
    "deb",
    "rpm",
    "pkg",
    "appimage",
    "snap",
    "flatpak",
    "bat",
    "cmd",
    "ps1",
    "sh",
    "jar",
    "run",
    "com",
    "vsix",
    "iso",
    "img",
    "dmg",
    "app",
];

fn by_extension(name: &str) -> Option<FileKind> {
    let ext = name.rsplit_once('.')?.1.trim().to_ascii_lowercase();
    if ext.is_empty() || ext.len() > 12 {
        return None;
    }
    let is = |list: &[&str]| list.contains(&ext.as_str());
    Some(if is(VIDEO) {
        FileKind::Video
    } else if is(AUDIO) {
        FileKind::Audio
    } else if is(IMAGE) {
        FileKind::Image
    } else if is(DOCUMENT) {
        FileKind::Document
    } else if is(ARCHIVE) {
        FileKind::Archive
    } else if is(PROGRAM) {
        FileKind::Program
    } else {
        return None;
    })
}

fn by_content_type(ct: &str) -> Option<FileKind> {
    let ct = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let (main, sub) = ct.split_once('/')?;
    Some(match main {
        "video" => FileKind::Video,
        "audio" => FileKind::Audio,
        "image" => FileKind::Image,
        "text" if matches!(sub, "plain" | "csv" | "markdown" | "rtf" | "html") => FileKind::Document,
        "application" => match sub {
            "pdf" | "msword" | "rtf" | "epub+zip" | "vnd.ms-excel" | "vnd.ms-powerpoint" => FileKind::Document,
            s if s.starts_with("vnd.openxmlformats-officedocument") || s.starts_with("vnd.oasis.opendocument") => FileKind::Document,
            "zip" | "x-zip-compressed" | "x-rar-compressed" | "vnd.rar" | "x-7z-compressed" | "gzip" | "x-gzip" | "x-tar" | "x-bzip2"
            | "x-xz" | "zstd" => FileKind::Archive,
            "x-msdownload"
            | "x-msi"
            | "vnd.microsoft.portable-executable"
            | "vnd.android.package-archive"
            | "x-apple-diskimage"
            | "x-debian-package"
            | "x-rpm"
            | "x-executable"
            | "x-sh"
            | "x-shellscript"
            | "java-archive"
            | "x-iso9660-image" => FileKind::Program,
            _ => return None,
        },
        _ => return None,
    })
}

/// The kind of a file from its final name and, when the name says nothing, the server's `Content-Type`.
pub fn classify(filename: &str, content_type: Option<&str>) -> FileKind {
    by_extension(filename).or_else(|| content_type.and_then(by_content_type)).unwrap_or(FileKind::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_decides_case_insensitively_and_only_the_last_one_counts() {
        for (name, kind) in [
            ("song.MP3", FileKind::Audio),
            ("movie.mkv", FileKind::Video),
            ("a.tar.gz", FileKind::Archive),
            ("report.PDF", FileKind::Document),
            ("photo.jpeg", FileKind::Image),
            ("Setup.exe", FileKind::Program),
            ("ubuntu.iso", FileKind::Program),
            ("notes.txt.exe", FileKind::Program),
            ("weird.xyz", FileKind::Other),
            ("noextension", FileKind::Other),
            ("trailingdot.", FileKind::Other),
            (".hidden", FileKind::Other),
            ("", FileKind::Other),
        ] {
            assert_eq!(classify(name, None), kind, "{name}");
        }
    }

    #[test]
    fn the_content_type_is_used_only_when_the_name_says_nothing() {
        assert_eq!(classify("download", Some("video/mp4")), FileKind::Video);
        assert_eq!(classify("download", Some("Audio/MPEG; charset=x")), FileKind::Audio);
        assert_eq!(classify("download", Some("application/pdf")), FileKind::Document);
        assert_eq!(classify("download", Some("application/zip")), FileKind::Archive);
        assert_eq!(classify("download", Some("application/x-msdownload")), FileKind::Program);
        assert_eq!(classify("download", Some("application/octet-stream")), FileKind::Other);
        assert_eq!(classify("download", Some("nonsense")), FileKind::Other);
        // A known extension wins over a lying content type.
        assert_eq!(classify("clip.mp4", Some("application/pdf")), FileKind::Video);
        assert_eq!(classify("notes.pdf", Some("video/mp4")), FileKind::Document);
    }

    #[test]
    fn every_kind_has_a_distinct_valid_default_folder_and_a_round_trip_key() {
        let mut seen = std::collections::HashSet::new();
        for k in FileKind::ALL {
            assert_eq!(FileKind::parse(k.key()), Some(k));
            assert!(path_safety::check_component(k.default_folder()).is_ok());
            assert!(seen.insert(k.default_folder().to_lowercase()), "duplicate folder {}", k.default_folder());
        }
        assert_eq!(FileKind::parse("nope"), None);
    }

    #[test]
    fn hostile_names_never_panic() {
        for n in ["\u{0}", ".", "..", "a.\u{202e}exe", &"x.".repeat(500), "é.MP3", "a b.mp3 ", "COM1.exe"] {
            let _ = classify(n, Some("\u{0};;;"));
        }
    }
}
