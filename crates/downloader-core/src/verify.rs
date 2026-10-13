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

/// Hashes of segments still being downloaded, carried from one checkpoint to the next (in memory, for one run).
///
/// Each checkpoint (after the `.part` file was synced) reads only the bytes a segment received since the previous
/// one and adds them to that segment's running hash; a segment that completes then only reads its last bytes. The
/// result is exactly [`hash_range`] of the whole segment, but the reading is spread over the download instead of
/// done for the last segments, all at once, after the last byte (on a 50 GB file: gigabytes read again at the end).
/// A segment whose range changed, whose progress went back, or any restart (`resets` moved) starts from its first
/// byte again: the running hash is only ever a shortcut, never trusted over the bytes.
///
/// The hashes of finished segments are kept too: the periodic checkpoints save into their own copy of the record, so
/// the final checkpoint used to hash every segment again (the whole file read once more after the last byte). Within a
/// run a finished segment only becomes unfinished through a restart, which empties these.
#[derive(Default)]
pub struct RunningHashes {
    /// `(id, start, end)` -> (bytes hashed from `start`, the hash so far).
    open: std::collections::HashMap<(usize, u64, u64), (u64, Sha256)>,
    /// `(id, start, end)` -> `sha256:<hex>` of a finished segment, made this run.
    done: std::collections::HashMap<(usize, u64, u64), String>,
    /// `ProgressHandle` restarts seen when these were last extended.
    resets: u64,
}

impl RunningHashes {
    /// Forget everything when progress was reset since the last call (a restart can rewrite bytes already hashed).
    pub fn note_resets(&mut self, resets: u64) {
        if resets != self.resets {
            self.open.clear();
            self.done.clear();
            self.resets = resets;
        }
    }

    /// Running hashes of unfinished segments.
    #[cfg(test)]
    pub fn open_count(&self) -> usize {
        self.open.len()
    }
}

/// Read `[from, from + len)` of `file` into `h`.
fn extend(file: &mut std::fs::File, h: &mut Sha256, from: u64, len: u64) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; BUF.min(len.max(1) as usize)];
    let mut left = len;
    while left > 0 {
        let n = (left as usize).min(buf.len());
        file.read_exact(&mut buf[..n])?;
        h.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(())
}

/// [`hash_new`], continuing the running hashes: completed segments without a hash get one (reading only what their
/// running hash has not seen), and the running hashes of unfinished segments are brought up to their synced
/// progress. Blocking. A segment that cannot be read keeps no hash (re-checked later as "unhashed").
pub fn hash_progressive(part: &Path, segs: &mut [Segment], running: &mut RunningHashes) -> std::io::Result<usize> {
    let wanted = |s: &Segment| (s.is_complete() && s.hash.is_none()) || (!s.is_complete() && s.downloaded > 0);
    // Only segments of this plan are kept.
    running.open.retain(|k, _| segs.iter().any(|s| (s.id, s.start, s.end) == *k && !(s.is_complete() && s.hash.is_some())));
    running.done.retain(|k, _| segs.iter().any(|s| (s.id, s.start, s.end) == *k && s.is_complete()));
    // Finished this run and hashed then: no need to read them again.
    let mut n = 0;
    for s in segs.iter_mut().filter(|s| s.is_complete() && s.hash.is_none()) {
        if let Some(h) = running.done.get(&(s.id, s.start, s.end)) {
            s.hash = Some(h.clone());
            n += 1;
        }
    }
    if !segs.iter().any(wanted) {
        return Ok(n);
    }
    let mut f = std::fs::File::open(part)?;
    for s in segs.iter_mut().filter(|s| wanted(s)) {
        let key = (s.id, s.start, s.end);
        let upto = if s.is_complete() { s.len() } else { s.downloaded.min(s.len()) };
        let (mut done, mut h) = running.open.remove(&key).unwrap_or_default();
        if done > upto {
            (done, h) = (0, Sha256::new()); // progress went back: from the first byte again
        }
        if let Err(e) = extend(&mut f, &mut h, s.start + done, upto - done) {
            if s.is_complete() {
                return Err(e);
            }
            continue; // an unfinished segment is simply hashed from its start next time
        }
        if s.is_complete() {
            let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
            s.hash = Some(format!("sha256:{hex}"));
            running.done.insert(key, format!("sha256:{hex}"));
            n += 1;
        } else {
            running.open.insert(key, (upto, h));
        }
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

    /// Segment `i` of `segs(n, each)` with `got` bytes received (complete when `got == each`).
    fn at(mut s: Segment, got: u64) -> Segment {
        s.downloaded = got;
        s.status = if got == s.len() { SegmentStatus::Completed } else { SegmentStatus::Downloading };
        s.hash = None;
        s
    }

    #[test]
    fn hashes_continued_checkpoint_by_checkpoint_equal_a_full_read() {
        let data: Vec<u8> = (0..3000u32).map(|i| (i * 13 + 5) as u8).collect();
        let p = tmp("progressive", &data);
        let whole = segs(3, 1000);
        let full = |i: usize| hash_range(&mut std::fs::File::open(&p).unwrap(), whole[i].start, 1000).unwrap();
        let mut running = RunningHashes::default();
        // Three checkpoints: segment 0 grows 300 -> 700 -> done, segment 1 goes 0 -> 999 -> done, segment 2 is done at once.
        for (step, got) in [[300u64, 0, 1000], [700, 999, 1000], [1000, 1000, 1000]].into_iter().enumerate() {
            let mut s: Vec<Segment> = whole.iter().zip(got).map(|(w, g)| at(w.clone(), g)).collect();
            // A hash already saved is carried by the caller (`carry_hashes`): only new ones are made here.
            if step > 0 {
                s[2].hash = Some(full(2));
            }
            hash_progressive(&p, &mut s, &mut running).unwrap();
            for (i, seg) in s.iter().enumerate() {
                if seg.is_complete() {
                    assert_eq!(seg.hash.as_deref(), Some(full(i).as_str()), "step {step} segment {i}");
                } else {
                    assert!(seg.hash.is_none());
                }
            }
        }
        assert_eq!(running.open_count(), 0, "nothing is kept for finished segments");
    }

    #[test]
    fn progress_that_went_back_or_a_restart_hashes_from_the_first_byte_again() {
        let mut data: Vec<u8> = (0..2000u32).map(|i| (i * 3) as u8).collect();
        let p = tmp("back", &data);
        let whole = segs(2, 1000);
        let mut running = RunningHashes::default();
        hash_progressive(&p, &mut [at(whole[0].clone(), 600), at(whole[1].clone(), 0)], &mut running).unwrap();
        // The segment was restarted and its first bytes rewritten with other ones: progress back to 200.
        data[100] ^= 0xff;
        std::fs::write(&p, &data).unwrap();
        hash_progressive(&p, &mut [at(whole[0].clone(), 200)], &mut running).unwrap();
        let mut s = [at(whole[0].clone(), 1000)];
        hash_progressive(&p, &mut s, &mut running).unwrap();
        let want = hash_range(&mut std::fs::File::open(&p).unwrap(), 0, 1000).unwrap();
        assert_eq!(s[0].hash.as_deref(), Some(want.as_str()), "the rewritten byte is in the hash");

        // A restart counted by the progress (it may have gone back and forward again between two checkpoints).
        hash_progressive(&p, &mut [at(whole[1].clone(), 800)], &mut running).unwrap();
        data[1100] ^= 0xff;
        std::fs::write(&p, &data).unwrap();
        running.note_resets(1);
        let mut s = [at(whole[1].clone(), 1000)];
        hash_progressive(&p, &mut s, &mut running).unwrap();
        let want = hash_range(&mut std::fs::File::open(&p).unwrap(), 1000, 1000).unwrap();
        assert_eq!(s[0].hash.as_deref(), Some(want.as_str()));
    }

    #[test]
    fn a_later_checkpoint_on_another_copy_of_the_record_reuses_the_hashes_made_this_run() {
        let data: Vec<u8> = (0..2000u32).map(|i| (i * 17) as u8).collect();
        let p = tmp("reuse", &data);
        let mut running = RunningHashes::default();
        let mut periodic = segs(2, 1000);
        periodic.iter_mut().for_each(|s| s.hash = None);
        assert_eq!(hash_progressive(&p, &mut periodic, &mut running).unwrap(), 2);
        // The bytes change on disk: a read would give another hash, so the same hashes back prove nothing was read.
        std::fs::write(&p, vec![0u8; 2000]).unwrap();
        let mut fin = segs(2, 1000);
        fin.iter_mut().for_each(|s| s.hash = None);
        assert_eq!(hash_progressive(&p, &mut fin, &mut running).unwrap(), 2);
        assert_eq!(fin.iter().map(|s| s.hash.clone()).collect::<Vec<_>>(), periodic.iter().map(|s| s.hash.clone()).collect::<Vec<_>>());
        // After a restart nothing is reused: the bytes are read again.
        running.note_resets(1);
        let mut again = segs(2, 1000);
        again.iter_mut().for_each(|s| s.hash = None);
        hash_progressive(&p, &mut again, &mut running).unwrap();
        let zero = hash_range(&mut std::fs::File::open(&p).unwrap(), 0, 1000).unwrap();
        assert_eq!(again[0].hash.as_deref(), Some(zero.as_str()));
    }

    #[test]
    fn a_segment_whose_range_changed_does_not_reuse_a_running_hash() {
        let data: Vec<u8> = (0..2000u32).map(|i| (i * 11) as u8).collect();
        let p = tmp("replan", &data);
        let mut running = RunningHashes::default();
        hash_progressive(&p, &mut [at(Segment::new(0, 0, 1999), 500)], &mut running).unwrap();
        assert_eq!(running.open_count(), 1);
        // Another plan: segment 0 is now [0, 999].
        let mut s = [at(Segment::new(0, 0, 999), 1000)];
        hash_progressive(&p, &mut s, &mut running).unwrap();
        let want = hash_range(&mut std::fs::File::open(&p).unwrap(), 0, 1000).unwrap();
        assert_eq!(s[0].hash.as_deref(), Some(want.as_str()));
        assert_eq!(running.open_count(), 0);
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
