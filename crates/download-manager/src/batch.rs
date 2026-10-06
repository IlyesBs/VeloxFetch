//! Batch downloads: a list of links (a `links.txt`), a pattern (`file_[001-100].zip`), and the
//! clipboard's "this looks like a download" detection.
//!
//! Everything here is pure text handling plus the manager methods that add the result. There is no
//! OS-specific code, so it behaves the same on Windows, macOS and Linux.
//!
//! Security rules (all enforced here, on the Rust side, whatever the UI sends):
//! * a pattern may generate at most [`MAX_GENERATED`] URLs (the product is computed with checked
//!   arithmetic *before* anything is generated), a batch holds at most [`MAX_BATCH`] links and its
//!   text at most [`MAX_TEXT_BYTES`];
//! * every link goes through the normal URL validation (http/https only, host required, no control
//!   characters, bounded length);
//! * duplicates inside the batch and links that are already in the list are skipped;
//! * the destination sub-folder is validated like for any download (no `..`, no absolute paths, no
//!   reserved names, symlinks cannot escape the download folder).

use std::collections::HashSet;

use downloader_core::DownloadStatus;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::api::{DownloadDto, ManagerError, NewDownload};
use crate::manager::DownloadManager;
use crate::validate;

/// URLs one pattern may generate.
pub const MAX_GENERATED: usize = 1000;
/// Links one batch may hold (after expansion).
pub const MAX_BATCH: usize = 1000;
/// Size of the pasted / imported text.
pub const MAX_TEXT_BYTES: usize = 512 * 1024;
/// A link longer than this is refused in a batch (single downloads allow more).
const MAX_LINK_LEN: usize = 2048;
/// Digits in one range bound (`[1-999999999]` at most).
const MAX_RANGE_DIGITS: usize = 9;
/// Ranges in one pattern.
const MAX_RANGES: usize = 3;
/// How many items the preview lists (the counts always cover everything).
const PREVIEW_ITEMS: usize = 300;
/// Most links the clipboard detection reports at once.
const MAX_CLIPBOARD_LINKS: usize = 50;
/// Clipboard text longer than this is never looked at.
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

/// What the user wants added: exactly one of `links` / `pattern`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchRequest {
    /// One link per line; empty lines and lines starting with `#` are ignored.
    pub links: Option<String>,
    /// A link with one to three ranges, e.g. `https://host/file_[001-100].zip` or `.../[a-e].png`.
    pub pattern: Option<String>,
    /// Sub-folder inside the download folder for the whole batch.
    pub subdir: Option<String>,
    /// Start right away (default true); the queue still limits how many run at once.
    pub start: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ItemState {
    /// Will be added.
    Ok,
    /// The same link appears earlier in this batch.
    DuplicateInBatch,
    /// The link is already in the download list.
    AlreadyInList,
    /// Not a usable link.
    Invalid { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchItem {
    /// Line number in the pasted text (1-based), or the position in the generated sequence.
    pub line: u32,
    pub url: String,
    /// File name shown in the preview (the real one is decided by the server when it starts).
    pub filename: String,
    pub state: ItemState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchPreview {
    pub total: usize,
    pub ok: usize,
    pub duplicates: usize,
    pub existing: usize,
    pub invalid: usize,
    /// The first items (all problems are listed before the good ones are cut off).
    pub items: Vec<BatchItem>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchResult {
    pub added: usize,
    pub duplicates: usize,
    pub existing: usize,
    pub invalid: usize,
    /// Links that were valid but could not be added (disk / database problem).
    pub failed: usize,
}

// ----------------------------------------------------------------------------------- parsing

/// The links of a pasted / imported text: (line number, text). Comments and blank lines are dropped.
fn text_lines(text: &str) -> Result<Vec<(u32, &str)>, ManagerError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(ManagerError::invalid(format!("the list is too large (at most {} KiB)", MAX_TEXT_BYTES / 1024)));
    }
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if out.len() >= MAX_BATCH {
            return Err(ManagerError::invalid(format!("too many links in one batch (at most {MAX_BATCH}); split the list")));
        }
        out.push((i as u32 + 1, line));
    }
    Ok(out)
}

/// One range of a pattern.
#[derive(Debug, PartialEq, Eq)]
enum Range {
    /// Numbers `start..=end`, zero-padded to `width` digits.
    Numbers { start: u64, end: u64, width: usize },
    /// Letters `start..=end` (same case).
    Letters { start: u8, end: u8 },
}

impl Range {
    fn len(&self) -> u64 {
        match self {
            Range::Numbers { start, end, .. } => end - start + 1,
            Range::Letters { start, end } => u64::from(end - start) + 1,
        }
    }

    fn nth(&self, n: u64) -> String {
        match self {
            Range::Numbers { start, width, .. } => format!("{:0width$}", start + n, width = *width),
            Range::Letters { start, .. } => char::from(start + n as u8).to_string(),
        }
    }
}

/// `[001-100]` / `[a-e]` at the start of `s` (which begins with `[`): the range and its byte length.
fn parse_range(s: &str) -> Option<Result<(Range, usize), ManagerError>> {
    let close = s.find(']')?;
    let inner = &s[1..close];
    let (a, b) = inner.split_once('-')?;
    let consumed = close + 1;
    let all_digits = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
    if all_digits(a) && all_digits(b) {
        if a.len() > MAX_RANGE_DIGITS || b.len() > MAX_RANGE_DIGITS {
            return Some(Err(ManagerError::invalid(format!("range numbers may have at most {MAX_RANGE_DIGITS} digits"))));
        }
        let (start, end): (u64, u64) = (a.parse().ok()?, b.parse().ok()?);
        if start > end {
            return Some(Err(ManagerError::invalid(format!("the range [{a}-{b}] counts backwards"))));
        }
        // "001" keeps its leading zeros; "1" and "10" do not pad.
        let width = if a.len() > 1 && a.starts_with('0') { a.len() } else { 0 };
        return Some(Ok((Range::Numbers { start, end, width }, consumed)));
    }
    let single = |x: &str| (x.len() == 1 && x.as_bytes()[0].is_ascii_alphabetic()).then(|| x.as_bytes()[0]);
    if let (Some(x), Some(y)) = (single(a), single(b)) {
        if x.is_ascii_lowercase() != y.is_ascii_lowercase() {
            return Some(Err(ManagerError::invalid("a letter range must use one case, like [a-e] or [A-E]")));
        }
        if x > y {
            return Some(Err(ManagerError::invalid(format!("the range [{a}-{b}] counts backwards"))));
        }
        return Some(Ok((Range::Letters { start: x, end: y }, consumed)));
    }
    None // not a range: an ordinary `[` (an IPv6 host, for instance)
}

/// Expand a pattern into its links, in order (the leftmost range changes slowest).
pub fn expand_pattern(pattern: &str) -> Result<Vec<String>, ManagerError> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Err(ManagerError::invalid("the pattern is empty"));
    }
    if pattern.len() > MAX_LINK_LEN || pattern.chars().any(char::is_control) {
        return Err(ManagerError::invalid("the pattern is too long or contains control characters"));
    }
    // Split into literal parts and ranges.
    let mut literals: Vec<String> = vec![String::new()];
    let mut ranges: Vec<Range> = Vec::new();
    let mut rest = pattern;
    while let Some(pos) = rest.find('[') {
        match parse_range(&rest[pos..]) {
            Some(Ok((range, used))) => {
                literals.last_mut().unwrap().push_str(&rest[..pos]);
                if ranges.len() == MAX_RANGES {
                    return Err(ManagerError::invalid(format!("at most {MAX_RANGES} ranges in one pattern")));
                }
                ranges.push(range);
                literals.push(String::new());
                rest = &rest[pos + used..];
            }
            Some(Err(e)) => return Err(e),
            None => {
                literals.last_mut().unwrap().push_str(&rest[..=pos]);
                rest = &rest[pos + 1..];
            }
        }
    }
    literals.last_mut().unwrap().push_str(rest);
    if ranges.is_empty() {
        return Err(ManagerError::invalid("the pattern needs a range such as [001-100] or [a-e]"));
    }
    // The size first, with checked arithmetic, so a hostile pattern can never allocate anything.
    let mut total: u64 = 1;
    for r in &ranges {
        total = total
            .checked_mul(r.len())
            .filter(|t| *t <= MAX_GENERATED as u64)
            .ok_or_else(|| ManagerError::invalid(format!("the pattern would generate more than {MAX_GENERATED} links")))?;
    }
    let mut out = Vec::with_capacity(total as usize);
    for mut n in 0..total {
        // Mixed-radix counter: the last range is the fastest.
        let mut picks = vec![0u64; ranges.len()];
        for (i, r) in ranges.iter().enumerate().rev() {
            picks[i] = n % r.len();
            n /= r.len();
        }
        let mut s = String::with_capacity(pattern.len() + 8);
        for (i, lit) in literals.iter().enumerate() {
            s.push_str(lit);
            if let Some(r) = ranges.get(i) {
                s.push_str(&r.nth(picks[i]));
            }
        }
        out.push(s);
    }
    Ok(out)
}

/// A URL as a key for "is this the same link": scheme and host in lower case, default port and
/// fragment dropped (the `url` crate already does the first two).
pub fn normalize(url: &Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    u.to_string()
}

/// The file name a link would most likely get (the last path segment, decoded and made safe).
fn display_name(url: &Url) -> String {
    let seg = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    if seg.is_empty() {
        return url.host_str().unwrap_or("download").to_string();
    }
    downloader_core::filename::sanitize_filename(&percent_decode(seg))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Classify every candidate link. `existing` are the normalised links already in the list.
fn classify(candidates: Vec<(u32, String)>, existing: &HashSet<String>) -> Vec<BatchItem> {
    let mut seen: HashSet<String> = HashSet::new();
    candidates
        .into_iter()
        .map(|(line, text)| {
            let bad = |reason: &str| BatchItem {
                line,
                url: text.chars().take(200).collect(),
                filename: String::new(),
                state: ItemState::Invalid { reason: reason.into() },
            };
            if text.len() > MAX_LINK_LEN {
                return bad("the link is too long");
            }
            if text.split_whitespace().count() > 1 {
                return bad("more than one thing on the line");
            }
            let url = match validate::url(&text) {
                Ok(u) => u,
                Err(ManagerError::InvalidInput(m)) => return bad(m.trim_start_matches("bad URL: ")),
                Err(_) => return bad("not a valid link"),
            };
            if crate::is_stream_link(&url) {
                return bad("this is a video stream: add it with Stream download instead");
            }
            let key = normalize(&url);
            let state = if !seen.insert(key.clone()) {
                ItemState::DuplicateInBatch
            } else if existing.contains(&key) {
                ItemState::AlreadyInList
            } else {
                ItemState::Ok
            };
            BatchItem { line, filename: display_name(&url), url: url.to_string(), state }
        })
        .collect()
}

// ------------------------------------------------------------------------------- clipboard

/// Extensions that make a copied link "look like a download" (files, not web pages).
const DOWNLOAD_EXTENSIONS: &[&str] = &[
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "iso", "img", "dmg", "pkg", "msi", "msix", "exe", "deb", "rpm", "appimage",
    "apk", "jar", "pdf", "epub", "mobi", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "csv", "mp4", "mkv", "avi", "mov", "webm", "wmv",
    "flv", "m4v", "mp3", "flac", "wav", "m4a", "ogg", "opus", "aac", "torrent", "ttf", "otf", "m3u8", "mpd",
];

/// Download links in copied text, or none. Deliberately strict, because the clipboard often holds
/// things that must never be looked at (passwords, prose): the text must consist *only* of http(s)
/// links, one per line, each ending in a file extension from a fixed list; a link with a user name
/// or password in it is ignored; nothing is kept.
pub fn detect_download_links(text: &str) -> Vec<String> {
    if text.is_empty() || text.len() > MAX_CLIPBOARD_BYTES {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut lines = 0;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        lines += 1;
        if lines > MAX_CLIPBOARD_LINKS || line.len() > MAX_LINK_LEN || line.split_whitespace().count() != 1 {
            return Vec::new(); // prose or something else: not our business
        }
        let Ok(url) = validate::url(line) else { return Vec::new() };
        if !url.username().is_empty() || url.password().is_some() {
            return Vec::new();
        }
        let ext = url.path_segments().and_then(|mut s| s.next_back()).and_then(|f| f.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase());
        if !ext.is_some_and(|e| DOWNLOAD_EXTENSIONS.contains(&e.as_str())) {
            return Vec::new(); // a web page, or a file type we do not recognise
        }
        if seen.insert(normalize(&url)) {
            out.push(url.to_string());
        }
    }
    out
}

// ------------------------------------------------------------------------- manager methods

impl DownloadManager {
    /// The normalised links of everything in the list that is not cancelled.
    async fn known_links(&self) -> Result<HashSet<String>, ManagerError> {
        let all = self.list_downloads(Default::default()).await?;
        Ok(all
            .iter()
            .filter(|d: &&DownloadDto| d.status != DownloadStatus::Cancelled)
            .filter_map(|d| Url::parse(&d.url).ok())
            .map(|u| normalize(&u))
            .collect())
    }

    /// The candidates of a request (validation of the request itself, not of each link).
    fn candidates(req: &BatchRequest) -> Result<Vec<(u32, String)>, ManagerError> {
        match (&req.links, &req.pattern) {
            (Some(text), None) => Ok(text_lines(text)?.into_iter().map(|(n, l)| (n, l.to_string())).collect()),
            (None, Some(p)) => Ok(expand_pattern(p)?.into_iter().enumerate().map(|(i, u)| (i as u32 + 1, u)).collect()),
            _ => Err(ManagerError::invalid("give either a list of links or a pattern")),
        }
    }

    async fn classified(&self, req: &BatchRequest) -> Result<Vec<BatchItem>, ManagerError> {
        // Validate the folder up front: a bad folder must fail the whole batch, not each link.
        validate::subdir(&self.download_root(), req.subdir.as_deref())?;
        let candidates = Self::candidates(req)?;
        if candidates.is_empty() {
            return Err(ManagerError::invalid("there are no links in the list"));
        }
        let existing = self.known_links().await?;
        Ok(classify(candidates, &existing))
    }

    /// What adding this batch would do, without doing anything.
    pub async fn preview_batch(&self, req: BatchRequest) -> Result<BatchPreview, ManagerError> {
        let items = self.classified(&req).await?;
        let count = |f: fn(&ItemState) -> bool| items.iter().filter(|i| f(&i.state)).count();
        let mut preview = BatchPreview {
            total: items.len(),
            ok: count(|s| *s == ItemState::Ok),
            duplicates: count(|s| *s == ItemState::DuplicateInBatch),
            existing: count(|s| *s == ItemState::AlreadyInList),
            invalid: count(|s| matches!(s, ItemState::Invalid { .. })),
            items: Vec::new(),
            truncated: false,
        };
        // Problems first (they need the user's eye), then the links that will be added.
        let (mut shown, rest): (Vec<BatchItem>, Vec<BatchItem>) = items.into_iter().partition(|i| i.state != ItemState::Ok);
        shown.extend(rest);
        preview.truncated = shown.len() > PREVIEW_ITEMS;
        shown.truncate(PREVIEW_ITEMS);
        preview.items = shown;
        Ok(preview)
    }

    /// Add the batch: every good link becomes a download (started unless `start` is false).
    pub async fn add_batch(&self, req: BatchRequest) -> Result<BatchResult, ManagerError> {
        let items = self.classified(&req).await?;
        let mut result = BatchResult { added: 0, duplicates: 0, existing: 0, invalid: 0, failed: 0 };
        for item in items {
            match item.state {
                ItemState::DuplicateInBatch => result.duplicates += 1,
                ItemState::AlreadyInList => result.existing += 1,
                ItemState::Invalid { .. } => result.invalid += 1,
                ItemState::Ok => {
                    let new = NewDownload { url: item.url, subdir: req.subdir.clone(), start: req.start, ..Default::default() };
                    let created = self.create_download(new).await;
                    failpoint::hit("batch.after_item");
                    match created {
                        Ok(_) => result.added += 1,
                        Err(e) => {
                            tracing::warn!(error = %e, "batch: a link could not be added");
                            result.failed += 1;
                        }
                    }
                }
            }
        }
        Ok(result)
    }

    /// Links from the clipboard that are worth offering: download-looking and not in the list yet.
    pub async fn new_download_links(&self, text: &str) -> Vec<String> {
        let found = detect_download_links(text);
        if found.is_empty() {
            return found;
        }
        let Ok(existing) = self.known_links().await else { return found };
        found.into_iter().filter(|l| Url::parse(l).is_ok_and(|u| !existing.contains(&normalize(&u)))).collect()
    }

    /// Put one link (e.g. from the clipboard) in front of the user with the usual details and
    /// Start / Cancel, exactly like a link from the browser.
    pub async fn review_link(&self, url: String) -> Result<crate::api::PendingDto, ManagerError> {
        if validate::url(&url).is_ok_and(|u| crate::is_stream_link(&u)) {
            return Err(ManagerError::invalid("this link is a video stream: use Stream download for it"));
        }
        self.add_pending(NewDownload { url, ..Default::default() }, Default::default()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(p: &str) -> Vec<String> {
        expand_pattern(p).unwrap()
    }

    #[test]
    fn numeric_ranges_keep_their_padding() {
        let v = ok("https://x.test/file_[001-100].zip");
        assert_eq!(v.len(), 100);
        assert_eq!(v[0], "https://x.test/file_001.zip");
        assert_eq!(v[8], "https://x.test/file_009.zip");
        assert_eq!(v[99], "https://x.test/file_100.zip");
        assert_eq!(ok("https://x.test/[1-3].bin"), ["https://x.test/1.bin", "https://x.test/2.bin", "https://x.test/3.bin"]);
        assert_eq!(ok("https://x.test/[8-11]")[3], "https://x.test/11", "no padding without a leading zero");
        assert_eq!(ok("https://x.test/[0-0]"), ["https://x.test/0"]);
    }

    #[test]
    fn letter_and_multiple_ranges_and_order() {
        assert_eq!(ok("https://x.test/[a-c].png"), ["https://x.test/a.png", "https://x.test/b.png", "https://x.test/c.png"]);
        assert_eq!(ok("https://x.test/[A-B]").len(), 2);
        let v = ok("https://x.test/[1-2]/p[01-02].jpg");
        assert_eq!(v, ["https://x.test/1/p01.jpg", "https://x.test/1/p02.jpg", "https://x.test/2/p01.jpg", "https://x.test/2/p02.jpg"]);
    }

    #[test]
    fn an_ordinary_bracket_is_not_a_range() {
        // IPv6 host and a stray bracket next to a real range.
        let v = ok("http://[::1]:8080/f[1-2].bin");
        assert_eq!(v, ["http://[::1]:8080/f1.bin", "http://[::1]:8080/f2.bin"]);
        assert!(expand_pattern("http://[::1]/f.bin").is_err(), "no range at all");
    }

    #[test]
    fn size_limits_are_checked_before_anything_is_generated() {
        assert_eq!(ok("https://x.test/[1-1000]").len(), 1000);
        for bad in [
            "https://x.test/[1-1001]",
            "https://x.test/[1-100]/[1-100]",
            "https://x.test/[1-999999999]",
            "https://x.test/[1-99999999999]",
            "https://x.test/[0-9]/[0-9]/[0-9]/[0-9]",
            "https://x.test/[a-z][a-z][a-z]",
        ] {
            let e = expand_pattern(bad);
            assert!(e.is_err(), "{bad}: {e:?}");
        }
        // Three ranges of 10 = 1000 is still fine.
        assert_eq!(ok("https://x.test/[0-9][0-9][0-9]").len(), 1000);
        // Overflowing products must not panic.
        assert!(expand_pattern("https://x.test/[1-999999999][1-999999999][1-999999999]").is_err());
    }

    #[test]
    fn malformed_patterns_are_refused() {
        for bad in [
            "",
            "   ",
            "https://x.test/nothing.zip",
            "https://x.test/[5-1]",
            "https://x.test/[c-a]",
            "https://x.test/[a-C]",
            "https://x.test/[1-2\u{0}]",
        ] {
            assert!(expand_pattern(bad).is_err(), "{bad:?}");
        }
        assert!(expand_pattern(&format!("https://x.test/{}[1-2]", "a".repeat(3000))).is_err(), "too long");
    }

    #[test]
    fn text_lists_skip_comments_and_blank_lines_and_have_limits() {
        let l = text_lines("\u{feff}# my list\n\nhttps://a.test/1.zip\n  https://a.test/2.zip  \r\n#x\nhttps://a.test/3.zip").unwrap();
        assert_eq!(l.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [3, 4, 6], "original line numbers");
        assert_eq!(l[1].1, "https://a.test/2.zip");
        let many: String = (0..1001).map(|i| format!("https://a.test/{i}\n")).collect();
        assert!(text_lines(&many).is_err(), "more than 1000 links");
        let just_enough: String = (0..1000).map(|i| format!("https://a.test/{i}\n")).collect();
        assert_eq!(text_lines(&just_enough).unwrap().len(), 1000);
        assert!(text_lines(&"x".repeat(MAX_TEXT_BYTES + 1)).is_err(), "text too large");
        assert!(text_lines("").unwrap().is_empty());
    }

    #[test]
    fn links_are_classified_validated_and_deduplicated() {
        let existing: HashSet<String> = [normalize(&Url::parse("https://a.test/old.zip").unwrap())].into();
        let c = |l: &str, n: u32| (n, l.to_string());
        let items = classify(
            vec![
                c("https://A.test/one.zip", 1),
                c("https://a.test/one.zip#frag", 2), // same link (host case, fragment)
                c("https://a.test:443/one.zip", 3),  // default port
                c("https://a.test/old.zip", 4),
                c("javascript:alert(1)", 5),
                c("file:///etc/passwd", 6),
                c("ftp://a.test/x", 7),
                c("https://a.test/two words.zip extra", 8),
                c("not a url", 9),
                c(&format!("https://a.test/{}", "a".repeat(3000)), 10),
                c("https://a.test/two.zip?x=1", 11),
                c("https://a.test/two.zip?x=2", 12), // different query: different link
                c("http://user:pw@a.test/secret.zip", 13),
            ],
            &existing,
        );
        let states: Vec<&ItemState> = items.iter().map(|i| &i.state).collect();
        assert_eq!(states[0], &ItemState::Ok);
        assert_eq!(states[1], &ItemState::DuplicateInBatch);
        assert_eq!(states[2], &ItemState::DuplicateInBatch);
        assert_eq!(states[3], &ItemState::AlreadyInList);
        for (i, state) in states.iter().enumerate().take(10).skip(4) {
            assert!(matches!(state, ItemState::Invalid { .. }), "line {}: {:?}", i + 1, state);
        }
        assert_eq!(states[10], &ItemState::Ok);
        assert_eq!(states[11], &ItemState::Ok);
        assert_eq!(states[12], &ItemState::Ok, "credentials in a URL are allowed by the normal rules");
        assert_eq!(items[0].filename, "one.zip");
        assert_eq!(items[0].url, "https://a.test/one.zip");
    }

    #[test]
    fn preview_names_are_safe() {
        let it = classify(
            vec![
                (1, "https://a.test/a%2F..%2Fb%3A.zip".to_string()),
                (2, "https://a.test/".to_string()),
                (3, "https://a.test/CON.txt".to_string()),
            ],
            &HashSet::new(),
        );
        for i in &it {
            assert!(!i.filename.contains(['/', '\\', ':']), "{:?}", i.filename);
        }
        assert_eq!(it[1].filename, "a.test", "no file name: the host is shown");
    }

    #[test]
    fn clipboard_detection_is_strict() {
        assert_eq!(
            detect_download_links("https://releases.ubuntu.com/24.04/ubuntu-24.04-desktop-amd64.iso"),
            ["https://releases.ubuntu.com/24.04/ubuntu-24.04-desktop-amd64.iso"]
        );
        assert_eq!(
            detect_download_links("  https://a.test/x.ZIP\n\nhttps://a.test/y.mp4\r\nhttps://a.test/x.ZIP  "),
            ["https://a.test/x.ZIP", "https://a.test/y.mp4"],
            "several, trimmed, deduplicated"
        );
        for no in [
            "",
            "hello world",
            "https://google.com/search?q=ubuntu",  // a web page
            "https://a.test/page.html",            // not a download type
            "https://a.test/x.zip and some words", // prose around a link
            "look at https://a.test/x.zip",
            "https://a.test/x.zip\nhello",       // one line is not a link: ignore all
            "https://user:hunter2@a.test/x.zip", // credentials in the link
            "ftp://a.test/x.zip",
            "javascript:alert(1)//x.zip",
            "correct horse battery staple",
            "P@ssw0rd!2024",
            "https://a.test/x.zip https://a.test/y.zip", // two on one line
        ] {
            assert!(detect_download_links(no).is_empty(), "{no:?}");
        }
        let many: String = (0..51).map(|i| format!("https://a.test/{i}.zip\n")).collect();
        assert!(detect_download_links(&many).is_empty(), "more than 50 lines is not a copied link");
        assert!(detect_download_links(&"a".repeat(MAX_CLIPBOARD_BYTES + 1)).is_empty());
    }

    #[test]
    fn percent_decoding_handles_edges() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode(""), "");
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
    fn patterns_and_pasted_text_never_crash_and_never_exceed_their_limits() {
        let pool = [
            "[",
            "]",
            "-",
            "0",
            "1",
            "9",
            "00",
            "999999999",
            "a",
            "z",
            "A-Z",
            "[1-9]",
            "[a-z]",
            "[0-999999999]",
            "://",
            "http://h/",
            "https://h.test/f",
            ".zip",
            "\\",
            "\n",
            "\t",
            "\u{202e}",
            "\u{0}",
            "[1-1000000]",
            "[01-99]",
        ];
        let mut rng = Rng(5);
        let started = std::time::Instant::now();
        for _ in 0..8_000 {
            let mut s = String::new();
            for _ in 0..rng.below(16) {
                s.push_str(pool[rng.below(pool.len())]);
            }
            if let Ok(links) = expand_pattern(&s) {
                assert!(links.len() <= MAX_GENERATED, "{s:?} made {} links", links.len());
            }
            let _ = text_lines(&s);
        }
        // Explosive products are refused before anything is generated.
        assert!(expand_pattern("http://h/[1-999999][1-999999][1-999999].zip").is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }
}
