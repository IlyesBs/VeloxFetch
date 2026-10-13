//! Whether a stopped download may continue its saved bytes, from evidence only. Pure functions: the engine asks
//! the server and reads the `.part` file, then calls them.
//!
//! A strong `ETag` equal to the one the download started with proves the same version. Anything weaker (a weak
//! `ETag`, a date, nothing at all) is only a hint, so a few small pieces of the saved bytes are compared with what
//! the server sends at the same places before they are trusted. Any contradiction (another strong or weak `ETag`,
//! another date, other bytes) means another file: the download starts over. Many servers (nginx with compression,
//! Apache, most CDNs) give only weak `ETag`s or none, and their downloads used to start over every time.

use crate::segment::Segment;

/// Bytes compared per sample: enough to tell two files apart, small enough to cost nothing.
pub const SAMPLE_LEN: u64 = 64 * 1024;
/// Samples compared before continuing a download the server cannot prove unchanged.
pub const RESUME_SAMPLES: usize = 4;

/// What the server's answer proves about the saved bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    /// The same strong `ETag`: the same version, byte for byte.
    Proven,
    /// Nothing contradicts it and the server's `ETag` (weak) or date agrees: check samples.
    Likely,
    /// The server gives nothing to compare: only samples can tell.
    Unproven,
}

/// Why the saved bytes were not continued because the file changed (shown in the timeline).
pub const CHANGED: &str = "remote resource changed or cannot be validated (ETag / Last-Modified)";
/// The saved bytes differ from the server's at the same places.
pub const BYTES_DIFFER: &str = "the saved bytes differ from the file on the server";

fn strong(e: Option<&str>) -> Option<&str> {
    e.filter(|e| !e.starts_with("W/") && e.len() > 2)
}

fn opaque(e: &str) -> &str {
    e.strip_prefix("W/").unwrap_or(e)
}

/// Compare what the download knew (`old_*`) with what the server says now. `Err`: the file changed.
pub fn validators(
    old_etag: Option<&str>,
    old_date: Option<&str>,
    new_etag: Option<&str>,
    new_date: Option<&str>,
) -> Result<Proof, &'static str> {
    if let (Some(a), Some(b)) = (strong(old_etag), strong(new_etag)) {
        return if a == b { Ok(Proof::Proven) } else { Err(CHANGED) };
    }
    // Weak comparison (RFC 9110 §8.8.3.2): different tags mean different content, even when one is weak.
    let etags = old_etag.zip(new_etag);
    if etags.is_some_and(|(a, b)| opaque(a) != opaque(b)) {
        return Err(CHANGED);
    }
    match old_date.zip(new_date) {
        Some((a, b)) if a != b => Err(CHANGED),
        Some(_) => Ok(Proof::Likely),
        None if etags.is_some() => Ok(Proof::Likely),
        None => Ok(Proof::Unproven),
    }
}

/// Where to compare saved bytes, at most `max` windows of [`SAMPLE_LEN`]: the start of the first saved bytes, the
/// end of the furthest ones, then the middle of the largest saved runs. Only inside bytes the download recorded as
/// written (and synced). `(offset, len)`, never overlapping.
pub fn sample_windows(segments: &[Segment], max: usize) -> Vec<(u64, u64)> {
    let done: Vec<(u64, u64)> = segments.iter().filter(|s| s.downloaded > 0).map(|s| (s.start, s.downloaded.min(s.len()))).collect();
    let mut out: Vec<(u64, u64)> = Vec::new();
    let add = |w: (u64, u64), out: &mut Vec<(u64, u64)>| {
        if out.len() < max && w.1 > 0 && !out.iter().any(|&(o, l)| w.0 < o + l && o < w.0 + w.1) {
            out.push(w);
        }
    };
    if let Some(&(start, len)) = done.iter().min_by_key(|(s, _)| *s) {
        add((start, len.min(SAMPLE_LEN)), &mut out);
    }
    if let Some(&(start, len)) = done.iter().max_by_key(|(s, l)| s + l) {
        let w = len.min(SAMPLE_LEN);
        add((start + len - w, w), &mut out);
    }
    let mut largest = done.clone();
    largest.sort_by_key(|&(s, l)| (std::cmp::Reverse(l), s));
    for (start, len) in largest {
        let w = len.min(SAMPLE_LEN);
        add((start + (len - w) / 2, w), &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Proof::*;

    #[test]
    fn a_strong_etag_proves_or_disproves() {
        assert_eq!(validators(Some("\"a\""), None, Some("\"a\""), None), Ok(Proven));
        assert_eq!(validators(Some("\"a\""), Some("d1"), Some("\"b\""), Some("d1")), Err(CHANGED));
    }

    #[test]
    fn a_weak_etag_or_a_date_is_only_a_hint() {
        // The bug this fixes: a weak ETag with the same date used to start over.
        assert_eq!(validators(Some("W/\"a\""), Some("d1"), Some("W/\"a\""), Some("d1")), Ok(Likely));
        assert_eq!(validators(Some("W/\"a\""), None, Some("W/\"a\""), None), Ok(Likely));
        assert_eq!(validators(None, Some("d1"), None, Some("d1")), Ok(Likely));
        // A strong tag served weak (compressed variant), same date.
        assert_eq!(validators(Some("\"a\""), Some("d1"), Some("W/\"a\""), Some("d1")), Ok(Likely));
        // The server stopped sending its ETag: the date still agrees.
        assert_eq!(validators(Some("\"a\""), Some("d1"), None, Some("d1")), Ok(Likely));
    }

    #[test]
    fn any_contradiction_means_another_file() {
        assert_eq!(validators(Some("W/\"a\""), Some("d1"), Some("W/\"b\""), Some("d1")), Err(CHANGED));
        assert_eq!(validators(Some("W/\"a\""), None, Some("\"b\""), None), Err(CHANGED));
        assert_eq!(validators(None, Some("d1"), None, Some("d2")), Err(CHANGED));
        assert_eq!(validators(Some("W/\"a\""), Some("d1"), Some("W/\"a\""), Some("d2")), Err(CHANGED));
    }

    #[test]
    fn nothing_to_compare_needs_samples() {
        assert_eq!(validators(None, None, None, None), Ok(Unproven));
        assert_eq!(validators(Some("W/\"a\""), None, None, Some("d1")), Ok(Unproven));
        assert_eq!(validators(None, Some("d1"), Some("\"x\""), None), Ok(Unproven));
    }

    fn seg(id: usize, start: u64, end: u64, downloaded: u64) -> Segment {
        let mut s = Segment::new(id, start, end);
        s.downloaded = downloaded;
        s
    }

    #[test]
    fn windows_stay_inside_saved_bytes_and_never_overlap() {
        let big = SAMPLE_LEN * 4;
        let segs = [seg(0, 0, big - 1, big), seg(1, big, 2 * big - 1, 100), seg(2, 2 * big, 3 * big - 1, SAMPLE_LEN * 3)];
        // Two windows: the start and the end of the furthest saved bytes.
        assert_eq!(sample_windows(&segs, 2), vec![(0, SAMPLE_LEN), (2 * big + 2 * SAMPLE_LEN, SAMPLE_LEN)]);
        let w = sample_windows(&segs, RESUME_SAMPLES);
        assert_eq!(w.len(), 4);
        for (i, &(o, l)) in w.iter().enumerate() {
            assert!(l > 0 && l <= SAMPLE_LEN);
            assert!(segs.iter().any(|s| o >= s.start && o + l <= s.start + s.downloaded), "window {o}+{l} outside saved bytes");
            for &(o2, l2) in &w[i + 1..] {
                assert!(o + l <= o2 || o2 + l2 <= o, "windows overlap");
            }
        }
        assert!(sample_windows(&[seg(0, 0, 99, 0)], RESUME_SAMPLES).is_empty());
        // A tiny saved part: one window, never two copies of it.
        assert_eq!(sample_windows(&[seg(0, 0, 999, 10)], RESUME_SAMPLES), vec![(0, 10)]);
    }
}
