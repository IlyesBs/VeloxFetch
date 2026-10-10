//! Dynamic chunk planning: how many connections, and how to split the file.

use crate::segment::Segment;

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

/// Initial connection count from file size; the adaptive controller then doubles it while each
/// doubling still makes the download faster, up to `max_useful_connections`.
///
/// ```text
/// < 1 MB -> 1      1-10 MB -> 4      10-100 MB -> 8      100 MB-1 GB -> 16      > 1 GB -> 32
/// ```
pub fn recommended_connections(size: u64) -> usize {
    match size {
        s if s < MB => 1,
        s if s < 10 * MB => 4,
        s if s < 100 * MB => 8,
        s if s < GB => 16,
        _ => 32,
    }
}

/// Most connections worth trying for a file of this size (the adaptive controller's ceiling).
///
/// ```text
/// < 1 MB -> 1      1-10 MB -> 8      >= 10 MB -> 64 (the user's setting and `min_segment_size` still bound it)
/// ```
pub fn max_useful_connections(size: u64) -> usize {
    match size {
        s if s < MB => 1,
        s if s < 10 * MB => 8,
        _ => MAX_CONNECTIONS,
    }
}

/// The most connections one download ever uses, whatever the settings say.
pub const MAX_CONNECTIONS: usize = 64;

/// `(recommended_start, ceiling)` connection counts: bounded by the caller's limit and by
/// `min_segment_size` (never create tiny segments).
pub fn connection_limits(size: u64, max_connections: usize, min_segment_size: u64) -> (usize, usize) {
    let by_size = (size / min_segment_size.max(1)).max(1) as usize;
    let ceiling = max_useful_connections(size).min(max_connections.max(1)).min(by_size).max(1);
    (recommended_connections(size).min(ceiling), ceiling)
}

/// Cut the file into ~4 segments per possible connection (never smaller than `min_segment_size`),
/// so connections can be added or removed while the download runs and each segment stays a
/// small, independently resumable unit of work.
pub fn plan_segments(size: u64, ceiling: usize, min_segment_size: u64) -> Vec<Segment> {
    let target = (ceiling.max(1) as u64) * 4;
    let seg_size = size.div_ceil(target).max(min_segment_size.max(1));
    split(size, size.div_ceil(seg_size).max(1) as usize)
}

/// Preview order: the first unfinished segment, then the last one (an MP4 often keeps its index at the
/// end, and a player reads it before it can show anything), then the rest from the start. The set of
/// segments is unchanged; only the order in which they get a connection.
pub fn preview_order(mut segments: Vec<Segment>) -> Vec<Segment> {
    segments.sort_by_key(|s| s.start);
    if segments.len() > 2 {
        let last = segments.pop().expect("more than two");
        segments.insert(1, last);
    }
    segments
}

/// Split `[0, size)` into `n` contiguous, non-overlapping segments covering every byte.
pub fn split(size: u64, n: usize) -> Vec<Segment> {
    assert!(size > 0 && n > 0);
    let n = (n as u64).min(size);
    let base = size / n;
    let extra = size % n;
    let mut out = Vec::with_capacity(n as usize);
    let mut start = 0;
    for i in 0..n {
        let len = base + u64::from(i < extra);
        out.push(Segment::new(i as usize, start, start + len - 1));
        start += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_order_is_start_end_then_the_rest() {
        let segs = split(1000, 5);
        let ids: Vec<usize> = preview_order(segs.clone()).iter().map(|s| s.id).collect();
        assert_eq!(ids, [0, 4, 1, 2, 3]);
        let mut shuffled = segs.clone();
        shuffled.reverse();
        assert_eq!(preview_order(shuffled).iter().map(|s| s.id).collect::<Vec<_>>(), [0, 4, 1, 2, 3]);
        assert_eq!(preview_order(segs[..2].to_vec()).iter().map(|s| s.id).collect::<Vec<_>>(), [0, 1]);
        assert!(preview_order(Vec::new()).is_empty());
    }

    #[test]
    fn size_table() {
        assert_eq!(recommended_connections(500_000), 1);
        assert_eq!(recommended_connections(5 * MB), 4);
        assert_eq!(recommended_connections(50 * MB), 8);
        assert_eq!(recommended_connections(500 * MB), 16);
        assert_eq!(recommended_connections(5 * GB), 32);
        assert_eq!(recommended_connections(50 * GB), 32);
        assert_eq!(max_useful_connections(5 * MB), 8);
        assert_eq!(max_useful_connections(30 * MB), 64, "the user's 64 is reachable on a mid-size file");
        assert_eq!(max_useful_connections(100 * MB), 64);
        assert_eq!(max_useful_connections(GB), 64);
    }

    #[test]
    fn limits_respect_caps_and_min_segment() {
        assert_eq!(connection_limits(500 * MB, 3, 256 * 1024), (3, 3));
        assert_eq!(connection_limits(600_000, 16, 256 * 1024), (1, 1));
        assert_eq!(connection_limits(50 * MB, 16, 256 * 1024), (8, 16));
        assert_eq!(connection_limits(100 * MB, 64, 256 * 1024), (16, 64), "never stuck at 32 when the setting is 64");
        assert_eq!(connection_limits(12 * MB, 64, 256 * 1024), (8, 48), "bounded by the minimum piece size only");
        assert_eq!(connection_limits(5 * GB, 64, 256 * 1024), (32, 64));
        assert_eq!(connection_limits(100 * GB, 64, 256 * 1024), (32, 64));
        assert_eq!(connection_limits(100 * GB, 128, 256 * 1024), (32, 64), "64 is the hard ceiling");
        assert_eq!(connection_limits(8 * MB, 64, 4 * MB), (2, 2), "never segments under the minimum size");
    }

    #[test]
    fn segments_outnumber_connections_but_stay_large_enough() {
        let segs = plan_segments(100 * MB, 8, 256 * 1024);
        assert!(segs.len() >= 8 * 3 && segs.len() <= 8 * 5, "{}", segs.len());
        assert!(segs.iter().all(|s| s.len() >= 256 * 1024));
        let small = plan_segments(MB, 2, 256 * 1024);
        assert!(small.len() <= 4 && small.iter().all(|s| s.len() >= 256 * 1024 || small.len() == 1));
        assert_eq!(plan_segments(1000, 1, 256 * 1024).len(), 1);
        assert_eq!(segs.iter().map(|s| s.len()).sum::<u64>(), 100 * MB);
    }

    /// A deterministic stream of numbers spread over many orders of magnitude.
    fn numbers(seed: u64) -> impl Iterator<Item = u64> {
        let mut x = seed | 1;
        std::iter::repeat_with(move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        })
    }

    #[test]
    fn any_size_any_settings_gives_a_perfect_partition_with_bounded_connections() {
        let mut n = numbers(0xC0FFEE);
        for _ in 0..20_000 {
            let bits = n.next().unwrap() % 44; // 1 byte .. 16 TiB
            let size = (n.next().unwrap() % (1u64 << bits).max(2)).max(1);
            let max_conn = (n.next().unwrap() % 140) as usize; // includes 0: must be treated as 1
            let min_seg = n.next().unwrap() % (8 * MB); // includes 0
            let (start, ceiling) = connection_limits(size, max_conn, min_seg);
            assert!(start >= 1 && ceiling >= start, "size {size}, max {max_conn}, min {min_seg}: ({start}, {ceiling})");
            assert!(
                ceiling <= max_conn.max(1) && ceiling <= MAX_CONNECTIONS,
                "the caller's limit is a hard ceiling: {ceiling} > {max_conn}"
            );
            if let Some(fit) = size.checked_div(min_seg) {
                assert!(ceiling as u64 <= fit.max(1), "never more connections than minimum-size segments");
            }
            let segs = plan_segments(size, ceiling, min_seg);
            assert!(!segs.is_empty());
            assert_eq!(segs[0].start, 0, "size {size}");
            assert_eq!(segs.last().unwrap().end, size - 1, "size {size}");
            let mut next = 0;
            for (i, s) in segs.iter().enumerate() {
                assert_eq!(s.id, i);
                assert_eq!(s.start, next, "gap or overlap before segment {i} (size {size})");
                assert!(s.end >= s.start, "empty segment {i}");
                assert_eq!(s.downloaded, 0);
                next = s.end + 1;
            }
            assert_eq!(next, size, "the segments must cover exactly the file");
            if min_seg > 0 && segs.len() > 1 {
                // Every segment but the rounding remainder honours the minimum.
                assert!(
                    segs.iter().all(|s| s.len() + 1 >= min_seg.min(size / segs.len() as u64)),
                    "tiny segment for size {size}, min {min_seg}"
                );
            }
        }
    }

    #[test]
    fn resume_offsets_never_leave_their_segment() {
        let mut n = numbers(0xBEEF);
        for _ in 0..20_000 {
            let size = n.next().unwrap() % (1 << 40) + 1;
            let segs = plan_segments(size, (n.next().unwrap() % 16 + 1) as usize, 64 * 1024);
            for mut s in segs {
                s.downloaded = n.next().unwrap() % (s.len() + 1);
                let next = s.next_offset();
                // The range asked from the server after a resume is `next..=end`: valid unless done.
                if s.is_complete() {
                    assert_eq!(next, s.end + 1);
                } else {
                    assert!(next >= s.start && next <= s.end, "resume offset {next} outside {}..={}", s.start, s.end);
                }
            }
        }
    }

    #[test]
    fn split_covers_everything() {
        for (size, n) in [(1u64, 1usize), (10, 3), (1000, 7), (5, 16), (1 << 20, 8)] {
            let segs = split(size, n);
            assert_eq!(segs[0].start, 0);
            assert_eq!(segs.last().unwrap().end, size - 1);
            assert_eq!(segs.iter().map(|s| s.len()).sum::<u64>(), size);
            for w in segs.windows(2) {
                assert_eq!(w[0].end + 1, w[1].start);
            }
        }
    }
}
