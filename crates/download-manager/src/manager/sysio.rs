//! Live network and disk activity for the dashboard's side panels: the whole computer's download and upload
//! speed, and the read / write speed of the disk the download folder is on. Real counters from the operating
//! system (Windows: interface table and `IOCTL_DISK_PERFORMANCE`; Linux: `/proc/net/dev`, `/proc/diskstats`;
//! macOS: interface and IOKit counters), read through `sysinfo`, at most about once a second and never on
//! the async threads.
//!
//! Interfaces without a hardware address (loopback, tunnels, packet-capture filter drivers such as Npcap that
//! mirror a real adapter's counters) are skipped, and adapters are counted once per hardware address, so
//! traffic is not counted twice.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sysinfo::{DiskRefreshKind, Disks, Networks};

/// Bytes per second, `None` until two readings exist (or when the system gives no number).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct IoRates {
    pub net_down: Option<u64>,
    pub net_up: Option<u64>,
    pub disk_read: Option<u64>,
    pub disk_write: Option<u64>,
    /// Number of the reading (the UI adds a point to its charts when it changes).
    pub sample: u64,
}

/// Shortest time between two readings (the dashboard asks ~5 times a second while downloading).
const EVERY: Duration = Duration::from_millis(900);
/// The disk list is read again now and then (a drive plugged in, the folder moved to another one).
const RELIST: Duration = Duration::from_secs(30);

/// Time, received and sent bytes, and the disk's read and written bytes, of one reading.
type Reading = (Instant, u64, u64, Option<(u64, u64)>);

struct Counters {
    nets: Networks,
    disks: Disks,
    listed: Instant,
    /// Totals and time of the previous reading.
    last: Option<Reading>,
}

#[derive(Default)]
pub(super) struct SysIo {
    counters: Mutex<Option<Counters>>,
    rates: Mutex<IoRates>,
    reading: Mutex<Option<Instant>>,
    samples: std::sync::atomic::AtomicU64,
}

impl SysIo {
    /// The latest rates (cheap; never waits for a reading).
    pub(super) fn rates(&self) -> IoRates {
        *self.rates.lock().unwrap()
    }

    /// Start a reading in the background if the last one is old enough. `root`: the download folder.
    pub(super) fn refresh(self: &Arc<Self>, root: PathBuf) {
        {
            let mut at = self.reading.lock().unwrap();
            if at.is_some_and(|t| t.elapsed() < EVERY) {
                return;
            }
            *at = Some(Instant::now());
        }
        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            let rates = me.read(&root);
            *me.rates.lock().unwrap() = rates;
        });
    }

    /// Bytes this computer received so far over its network adapters, read now (`None`: no adapter counted).
    /// Cheap (a counter table, no waiting); Smart throttle measures its own windows with it, on a blocking thread.
    pub(super) fn net_received_total(&self) -> Option<u64> {
        let mut guard = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        let c = guard.get_or_insert_with(Counters::new);
        c.nets.refresh(false);
        let (rx, _, adapters) = net_totals(&c.nets);
        (adapters > 0).then_some(rx)
    }

    fn read(&self, root: &Path) -> IoRates {
        let mut guard = self.counters.lock().unwrap();
        let c = guard.get_or_insert_with(Counters::new);
        let now = Instant::now();
        if now.duration_since(c.listed) > RELIST {
            c.disks.refresh_specifics(true, DiskRefreshKind::nothing().with_io_usage());
            c.nets.refresh(true);
            c.listed = now;
        } else {
            c.disks.refresh_specifics(false, DiskRefreshKind::nothing().with_io_usage());
            c.nets.refresh(false);
        }

        let (rx, tx, _) = net_totals(&c.nets);
        let disk = disk_for(&c.disks, root).map(|d| {
            let u = d.usage();
            (u.total_read_bytes, u.total_written_bytes)
        });

        let sample = c.last.map_or(0, |_| self.samples.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1);
        let rates = match c.last {
            Some((at, prx, ptx, pdisk)) => {
                let secs = now.duration_since(at).as_secs_f64().max(1e-3);
                let rate = |a: u64, b: u64| Some((a.saturating_sub(b) as f64 / secs) as u64);
                let (read, write) = match (disk, pdisk) {
                    (Some((r, w)), Some((pr, pw))) => (rate(r, pr), rate(w, pw)),
                    _ => (None, None),
                };
                IoRates { net_down: rate(rx, prx), net_up: rate(tx, ptx), disk_read: read, disk_write: write, sample }
            }
            None => IoRates::default(),
        };
        c.last = Some((now, rx, tx, disk));
        rates
    }
}

impl Counters {
    fn new() -> Self {
        Counters {
            nets: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_io_usage()),
            listed: Instant::now(),
            last: None,
        }
    }
}

/// Received and sent bytes over the real adapters, each counted once, and how many adapters were counted.
fn net_totals(nets: &Networks) -> (u64, u64, usize) {
    let (mut rx, mut tx) = (0u64, 0u64);
    let mut seen = HashSet::new();
    for n in nets.values() {
        let mac = n.mac_address();
        if mac.is_unspecified() || !seen.insert(mac.0) {
            continue;
        }
        rx = rx.saturating_add(n.total_received());
        tx = tx.saturating_add(n.total_transmitted());
    }
    (rx, tx, seen.len())
}

/// The disk holding `root`: the one whose mount point is the longest prefix of it (or of its nearest existing parent).
pub(super) fn disk_for<'a>(disks: &'a Disks, root: &Path) -> Option<&'a sysinfo::Disk> {
    let root = root.ancestors().find_map(|p| std::fs::canonicalize(p).ok()).unwrap_or_else(|| root.to_path_buf());
    let root = strip_verbatim(&root);
    disks.list().iter().filter(|d| root.starts_with(strip_verbatim(d.mount_point()))).max_by_key(|d| d.mount_point().as_os_str().len())
}

/// `\\?\C:\x` (what `canonicalize` gives on Windows) as `C:\x`, so it compares with mount points.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefix_is_dropped() {
        assert_eq!(strip_verbatim(Path::new(r"\\?\C:\Users\x")), PathBuf::from(r"C:\Users\x"));
        assert_eq!(strip_verbatim(Path::new("/home/x")), PathBuf::from("/home/x"));
    }

    #[tokio::test]
    async fn rates_come_from_two_real_readings() {
        let io = Arc::new(SysIo::default());
        let root = std::env::temp_dir();
        assert_eq!(io.read(&root), IoRates::default(), "one reading gives no rate yet");
        // Some disk and network traffic between the readings.
        let f = root.join(format!("velox-sysio-{}", std::process::id()));
        std::fs::write(&f, vec![7u8; 4 << 20]).unwrap();
        let _ = std::fs::File::open(&f).and_then(|x| x.sync_all());
        std::thread::sleep(Duration::from_millis(300));
        let r = io.read(&root);
        let _ = std::fs::remove_file(&f);
        eprintln!("measured: {r:?}");
        // Machines without a usable interface or disk counter (some CI sandboxes) give None, never a made-up number.
        assert!(r.net_down.is_some() == r.net_up.is_some(), "{r:?}");
        assert!(r.disk_read.is_some() == r.disk_write.is_some(), "{r:?}");
    }
}
