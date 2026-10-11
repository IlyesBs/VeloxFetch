//! Segment re-verification: a hash of each completed segment, saved with it, so bytes already on disk are checked
//! before they are trusted again after a crash or an outside change of the `.part` file.
//!
//! Lifecycle:
//! * a segment is hashed by the checkpoint that first saves it complete, **after** the `.part` file was synced, in
//!   the same store write as its counters: a saved hash always describes durable bytes;
//! * the `.part` file's stamp (size + modification time) is saved with the hashes. A clean pause takes the last
//!   checkpoint after every writer stopped, so its stamp is exact;
//! * at resume, the same stamp means the file was not touched since: nothing is read. Another stamp (a crash after
//!   more writes, a change from outside) or none: every hashed completed segment is read and hashed again. A
//!   mismatch empties that segment only, so only its range is downloaded again.
//!
//! A segment hash proves the local bytes did not change since they were saved. It does not prove they are what the
//! server meant: that is the job of the source checks (`ETag` / `Last-Modified`, `If-Range`, `Content-Range`) and of
//! a checksum given for the file.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::segment::{Segment, SegmentStatus};

/// Read size while hashing: memory stays bounded whatever the segment's size.
const BUF: usize = 1 << 20;

/// What re-verification found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reverified {
    /// Completed segments read and hashed again.
    pub checked: usize,
    /// Of those, the ones whose bytes no longer match their hash: emptied, to download again.
    pub invalid: usize,
    /// Completed segments without a hash (saved by an older version): kept as they were, unverifiable.
    pub unhashed: usize,
    /// Bytes that will be downloaded again.
    pub invalid_bytes: u64,
}

/// `<len>:<mtime ns>` of the `.part` file, `None` if it cannot be read.
pub fn stamp(part: &Path) -> Option<String> {
    let m = std::fs::metadata(part).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}:{}", m.len(), t.as_nanos()))
}

/// `sha256:<hex>` of `len` bytes at `start` (streamed, bounded memory).
pub fn hash_range(file: &mut std::fs::File, start: u64, len: u64) -> std::io::Result<String> {
    file.seek(SeekFrom::Start(start))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BUF.min(len.max(1) as usize)];
    let mut left = len;
    while left > 0 {
        let n = (left as usize).min(buf.len());
        file.read_exact(&mut buf[..n])?;
        h.update(&buf[..n]);
        left -= n as u64;
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

/// Keep the hash of every segment still complete over the same range (`prev`: as last saved); clear it for any
/// segment that is not complete. A segment whose range changed loses its hash.
pub fn carry_hashes(prev: &[Segment], segs: &mut [Segment]) {
    for s in segs.iter_mut() {
        if !s.is_complete() {
            s.hash = None;
            continue;
        }
        if s.hash.is_none() {
            s.hash =
                prev.iter().find(|p| p.id == s.id && p.start == s.start && p.end == s.end && p.is_complete()).and_then(|p| p.hash.clone());
        }
    }
}

/// Hash the completed segments that have no hash yet. Blocking (call from a blocking thread). A segment that cannot
/// be read keeps no hash (it is re-checked as "unhashed", never taken as verified).
pub fn hash_new(part: &Path, segs: &mut [Segment]) -> std::io::Result<usize> {
    if !segs.iter().any(|s| s.is_complete() && s.hash.is_none()) {
        return Ok(0);
    }
    let mut f = std::fs::File::open(part)?;
    let mut n = 0;
    for s in segs.iter_mut().filter(|s| s.is_complete() && s.hash.is_none()) {
        s.hash = Some(hash_range(&mut f, s.start, s.len())?);
        n += 1;
    }
    Ok(n)
}

/// Read and hash every hashed completed segment again; empty the ones that changed. Blocking.
pub fn reverify(part: &Path, segs: &mut [Segment]) -> std::io::Result<Reverified> {
    let mut r = Reverified::default();
    let mut f = std::fs::File::open(part)?;
    for s in segs.iter_mut().filter(|s| s.is_complete()) {
        let Some(want) = s.hash.clone() else {
            r.unhashed += 1;
            continue;
        };
        r.checked += 1;
        let ok = hash_range(&mut f, s.start, s.len()).is_ok_and(|got| got == want);
        if !ok {
            r.invalid += 1;
            r.invalid_bytes += s.len();
            s.downloaded = 0;
            s.hash = None;
            s.status = SegmentStatus::Pending;
        }
    }
    Ok(r)
}

/// The segments cover `0..total` exactly once, in order, with no gap or overlap (`None` = fine, else why not).
pub fn coverage_problem(segs: &[Segment], total: u64) -> Option<String> {
    let mut next = 0u64;
    for s in segs {
        if s.start != next || s.end < s.start {
            return Some(format!("segment {} starts at {} where {next} was expected", s.id, s.start));
        }
        if s.downloaded > s.len() {
            return Some(format!("segment {} claims more bytes than it holds", s.id));
        }
        next = s.end + 1;
    }
    (next != total).then(|| format!("the segments end at {next}, the file is {total} bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("velox-verify-{}-{name}", std::process::id()));
        std::fs::write(&p, data).unwrap();
        p
    }

    fn segs(n: u64, each: u64) -> Vec<Segment> {
        (0..n)
            .map(|i| Segment {
                downloaded: each,
                status: SegmentStatus::Completed,
                ..Segment::new(i as usize, i * each, (i + 1) * each - 1)
            })
            .collect()
    }

    #[test]
    fn a_changed_segment_is_found_and_only_it_is_emptied() {
        let data: Vec<u8> = (0..4000u32).map(|i| (i * 7) as u8).collect();
        let p = tmp("changed", &data);
        let mut s = segs(4, 1000);
        s[3].downloaded = 400; // not complete: never hashed
        s[3].status = SegmentStatus::Pending;
        assert_eq!(hash_new(&p, &mut s).unwrap(), 3);
        assert!(s[3].hash.is_none());
        assert_eq!(reverify(&p, &mut s.clone()).unwrap(), Reverified { checked: 3, ..Default::default() }, "untouched: all verified");

        let mut bad = data.clone();
        bad[1500] ^= 0xff; // one byte of segment 1
        std::fs::write(&p, &bad).unwrap();
        let r = reverify(&p, &mut s).unwrap();
        assert_eq!((r.checked, r.invalid, r.invalid_bytes), (3, 1, 1000));
        assert_eq!((s[1].downloaded, s[1].status, s[1].hash.is_none()), (0, SegmentStatus::Pending, true));
        assert!(s[0].is_complete() && s[2].is_complete() && s[0].hash.is_some(), "the others are kept");
        assert_eq!(s[3].downloaded, 400, "an incomplete segment is untouched");
    }

    #[test]
    fn segments_from_older_versions_are_counted_not_trusted_as_verified() {
        let p = tmp("legacy", &[1u8; 2000]);
        let mut s = segs(2, 1000);
        let r = reverify(&p, &mut s).unwrap();
        assert_eq!((r.checked, r.unhashed, r.invalid), (0, 2, 0));
    }

    #[test]
    fn hashes_follow_their_exact_range() {
        let mut prev = segs(2, 1000);
        prev[0].hash = Some("sha256:a".into());
        prev[1].hash = Some("sha256:b".into());
        let mut now = segs(2, 1000);
        now[1].end = 1499; // the range changed
        now[1].downloaded = 500;
        carry_hashes(&prev, &mut now);
        assert_eq!(now[0].hash.as_deref(), Some("sha256:a"));
        assert_eq!(now[1].hash, None, "another range: not the same bytes");
        now[0].downloaded = 10; // no longer complete
        carry_hashes(&prev, &mut now);
        assert_eq!(now[0].hash, None);
    }

    #[test]
    fn coverage_without_gaps_or_overlaps() {
        assert_eq!(coverage_problem(&segs(3, 100), 300), None);
        let mut gap = segs(3, 100);
        gap[1].start = 101;
        assert!(coverage_problem(&gap, 300).is_some());
        let mut overlap = segs(3, 100);
        overlap[2].start = 150;
        assert!(coverage_problem(&overlap, 300).is_some());
        assert!(coverage_problem(&segs(3, 100), 400).is_some(), "the end is missing");
        let mut over = segs(2, 100);
        over[0].downloaded = 101;
        assert!(coverage_problem(&over, 200).is_some());
    }

    #[test]
    fn large_segments_are_hashed_with_bounded_memory() {
        // 48 MiB hashed through a 1 MiB buffer; the same answer as hashing it in one go.
        let data = vec![0x5au8; 48 << 20];
        let p = tmp("large", &data);
        let mut f = std::fs::File::open(&p).unwrap();
        let got = hash_range(&mut f, 0, data.len() as u64).unwrap();
        let want: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, format!("sha256:{want}"));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn the_stamp_changes_with_the_file() {
        let p = tmp("stamp", b"abc");
        let a = stamp(&p).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(4).unwrap();
        assert_ne!(stamp(&p).unwrap(), a);
        assert!(stamp(Path::new("/nonexistent/velox.part")).is_none());
    }
}
