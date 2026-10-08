use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use http_client::{parse_url, ClientConfig, HttpClient, HttpError, RequestExtras, ResourceInfo, Response};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::adaptive::{AdaptiveConfig, AdaptiveController, AdaptiveGate, Sample};
use crate::disk::{self, PumpParams};
use crate::error::{DownloadError, Result};
use crate::filename::{choose_filename, sanitize_filename, web_page_instead_of_file};
use crate::integrity::{self, Checksum};
use crate::limiter::{RateLimits, Throttle, TokenBucket};
use crate::planner;
use crate::retry::{self, RetryPolicy};
use crate::scheduler::{Priority, SlotScheduler};
use crate::segment::{Segment, SegmentStatus};
use crate::store::{DownloadRecord, DownloadStatus, DownloadStore};

#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Upper bound on parallel connections (the planner may use fewer).
    pub max_connections: usize,
    /// Per-worker write buffer. RAM ~= workers * buffer_size * (channel_capacity + 2).
    pub buffer_size: usize,
    pub channel_capacity: usize,
    /// Never create segments smaller than this.
    pub min_segment_size: u64,
    pub retry: RetryPolicy,
    /// How often progress is fsynced and committed to the state store.
    pub checkpoint_interval: Duration,
    /// Adaptive connection count. When disabled, the size-based default is used from the start.
    pub adaptive: AdaptiveConfig,
    /// When set, every file this engine creates or renames must really lie inside this folder
    /// (symbolic links and junctions resolved). Checked again right before each write, so a folder
    /// swapped for a link after the download was queued cannot redirect it.
    pub sandbox_root: Option<PathBuf>,
    /// Flag every finished file as downloaded from the internet (Windows Zone.Identifier, macOS
    /// quarantine) so the OS warns before it is run.
    pub mark_of_the_web: bool,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            max_connections: crate::planner::MAX_CONNECTIONS,
            buffer_size: 64 * 1024,
            channel_capacity: 2,
            min_segment_size: 256 * 1024,
            retry: RetryPolicy::default(),
            checkpoint_interval: Duration::from_millis(500),
            adaptive: AdaptiveConfig::default(),
            sandbox_root: None,
            mark_of_the_web: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub dest_dir: PathBuf,
    /// Overrides the server-provided name (still sanitized).
    pub filename: Option<String>,
    pub overwrite: bool,
    /// Must come from a source more trusted than the file's server (see `integrity`).
    pub expected_checksum: Option<Checksum>,
    /// Resume a stored download (requires a store). `url`/`dest_dir` come from the stored record.
    pub resume_id: Option<i64>,
    /// Which downloads get run slots first (needs a scheduler).
    pub priority: Priority,
    /// Optional category whose shared bandwidth limit applies (see `RateLimits`).
    pub category: Option<String>,
    /// Per-download speed limit in bytes/s. Can be changed later through `DownloadControl`.
    pub speed_limit: Option<u64>,
}

impl DownloadRequest {
    pub fn new(url: impl Into<String>, dest_dir: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            dest_dir: dest_dir.into(),
            filename: None,
            overwrite: false,
            expected_checksum: None,
            resume_id: None,
            priority: Priority::Normal,
            category: None,
            speed_limit: None,
        }
    }

    pub fn resume(id: i64) -> Self {
        Self { resume_id: Some(id), ..Self::new("", "") }
    }
}

#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub id: Option<i64>,
    pub path: PathBuf,
    pub size: u64,
    /// Most connections that were in use at the same time.
    pub connections: usize,
    pub parallel: bool,
    /// Continued from saved segments (false = started fresh, including a restart after the remote changed).
    pub resumed: bool,
    pub checksum_verified: bool,
    pub info: ResourceInfo,
}

#[derive(Debug, Clone)]
pub struct ProgressSnapshot {
    pub total: Option<u64>,
    pub downloaded: u64,
    pub segments: Vec<Segment>,
}

/// Something worth telling about a run, for the download's diagnostics (the manager turns it into text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEventKind {
    /// The server check succeeded.
    ServerAnswered { status: u16, size: Option<u64>, ranges: bool },
    /// A request failed and will be tried again. `part`: the segment, `None` for the server check or a
    /// single-connection transfer.
    Retrying { part: Option<usize>, attempt: u32, error: String, delay: Duration },
    /// The server answered 429 / 503: fewer connections from now on.
    Throttled { connections: usize },
    /// Continuing a saved transfer from this many bytes.
    Resuming { from: u64 },
    /// The saved transfer could not be continued (the file changed, the partial file is missing...).
    StartingOver { reason: String },
    /// A new transfer: over several connections, or one.
    Planned { parallel: bool, parts: usize },
    /// The name was taken while downloading; saved under this one instead.
    Renamed { to: String },
    /// The server did not answer the quick check: it is preparing the file. Waiting for it (one request).
    WaitingForServer,
}

#[derive(Debug, Clone)]
pub struct RunEvent {
    pub at: std::time::SystemTime,
    pub kind: RunEventKind,
}

/// Events kept per run; a flood of retries keeps only the newest.
const MAX_RUN_EVENTS: usize = 40;

struct ProgressInner {
    id: AtomicI64,    // 0 = not stored
    key: AtomicU64,   // scheduler queue key, 0 = none
    total: AtomicU64, // 0 = unknown
    downloaded: AtomicU64,
    /// Every byte written this run (never decreases, unlike `downloaded` which restarts can reduce).
    received: AtomicU64,
    latency_ns: AtomicU64,
    latency_n: AtomicU64,
    segments: Mutex<Vec<Segment>>,
    // ---- diagnostics of this run
    /// Last HTTP status a server answered (0 = none yet).
    http_status: AtomicU16,
    /// Retries of the server check (segment retries are counted on the segments).
    probe_retries: AtomicU32,
    /// Segment retries already counted when this run started (saved by an earlier run).
    retry_baseline: AtomicU32,
    peak_connections: AtomicUsize,
    /// When bytes started to be transferred.
    transfer_started: Mutex<Option<Instant>>,
    events: Mutex<std::collections::VecDeque<RunEvent>>,
}

/// Cheap, cloneable, lock-light view of a running download.
#[derive(Clone)]
pub struct ProgressHandle(Arc<ProgressInner>);

impl Default for ProgressHandle {
    fn default() -> Self {
        Self(Arc::new(ProgressInner {
            id: AtomicI64::new(0),
            key: AtomicU64::new(0),
            total: AtomicU64::new(0),
            downloaded: AtomicU64::new(0),
            received: AtomicU64::new(0),
            latency_ns: AtomicU64::new(0),
            latency_n: AtomicU64::new(0),
            segments: Mutex::new(Vec::new()),
            http_status: AtomicU16::new(0),
            probe_retries: AtomicU32::new(0),
            retry_baseline: AtomicU32::new(0),
            peak_connections: AtomicUsize::new(0),
            transfer_started: Mutex::new(None),
            events: Mutex::new(std::collections::VecDeque::new()),
        }))
    }
}

impl ProgressHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store id of the download once it has been saved.
    pub fn download_id(&self) -> Option<i64> {
        Some(self.0.id.load(Ordering::Relaxed)).filter(|&i| i != 0)
    }

    /// Key under which this download waits in the [`SlotScheduler`]; use it with
    /// `SlotScheduler::set_priority` to re-prioritise a queued download.
    pub fn queue_key(&self) -> Option<u64> {
        Some(self.0.key.load(Ordering::Relaxed)).filter(|&k| k != 0)
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        let t = self.0.total.load(Ordering::Relaxed);
        ProgressSnapshot {
            total: (t > 0).then_some(t),
            downloaded: self.0.downloaded.load(Ordering::Relaxed),
            segments: self.0.segments.lock().unwrap().clone(),
        }
    }

    /// Bytes actually received from the network during this run (monotonic).
    pub fn received(&self) -> u64 {
        self.0.received.load(Ordering::Relaxed)
    }

    /// The last HTTP status a server answered during this run.
    pub fn http_status(&self) -> Option<u16> {
        Some(self.0.http_status.load(Ordering::Relaxed)).filter(|&s| s != 0)
    }

    /// Automatic retries during this run (server checks and connections).
    pub fn retries(&self) -> u32 {
        let segments = self.total_retries().saturating_sub(self.0.retry_baseline.load(Ordering::Relaxed));
        segments.saturating_add(self.0.probe_retries.load(Ordering::Relaxed))
    }

    /// Most connections that were open at the same time during this run.
    pub fn peak_connections(&self) -> usize {
        self.0.peak_connections.load(Ordering::Relaxed)
    }

    /// When bytes started to be transferred in this run (`None`: not yet).
    pub fn transfer_started(&self) -> Option<Instant> {
        *self.0.transfer_started.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// What happened during this run so far (the newest [`MAX_RUN_EVENTS`]).
    pub fn events(&self) -> Vec<RunEvent> {
        self.0.events.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect()
    }

    fn note_status(&self, status: u16) {
        self.0.http_status.store(status, Ordering::Relaxed);
    }

    fn note_peak(&self, n: usize) {
        self.0.peak_connections.fetch_max(n, Ordering::Relaxed);
    }

    fn note(&self, kind: RunEventKind) {
        let mut ev = self.0.events.lock().unwrap_or_else(|p| p.into_inner());
        if ev.len() >= MAX_RUN_EVENTS {
            ev.pop_front();
        }
        ev.push_back(RunEvent { at: std::time::SystemTime::now(), kind });
    }

    /// A request failed and is retried: count it, keep the status, tell the timeline.
    fn note_retry(&self, part: Option<usize>, attempt: u32, e: &DownloadError, delay: Duration) {
        if let Some(s) = e.http_status() {
            self.note_status(s);
        }
        self.note(RunEventKind::Retrying { part, attempt, error: e.to_string(), delay });
    }

    fn set_id(&self, id: i64) {
        self.0.id.store(id, Ordering::Relaxed);
    }

    fn set_key(&self, key: u64) {
        self.0.key.store(key, Ordering::Relaxed);
    }

    fn init(&self, total: Option<u64>, segments: Vec<Segment>) {
        self.0.total.store(total.unwrap_or(0), Ordering::Relaxed);
        self.0.downloaded.store(segments.iter().map(|s| s.downloaded).sum(), Ordering::Relaxed);
        self.0.retry_baseline.store(segments.iter().map(|s| s.retry_count).sum(), Ordering::Relaxed);
        *self.0.segments.lock().unwrap() = segments;
        *self.0.transfer_started.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
    }

    fn downloaded_now(&self) -> u64 {
        self.0.downloaded.load(Ordering::Relaxed)
    }

    fn total_retries(&self) -> u32 {
        self.0.segments.lock().unwrap().iter().map(|s| s.retry_count).sum()
    }

    /// Time-to-first-byte of a request (server response time).
    fn record_latency(&self, d: Duration) {
        self.0.latency_ns.fetch_add(d.as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
        self.0.latency_n.fetch_add(1, Ordering::Relaxed);
    }

    /// Mean latency since the previous call.
    fn take_latency(&self) -> Option<Duration> {
        let n = self.0.latency_n.swap(0, Ordering::Relaxed);
        let sum = self.0.latency_ns.swap(0, Ordering::Relaxed);
        (n > 0).then(|| Duration::from_nanos(sum / n))
    }

    fn segment(&self, idx: usize) -> Option<Segment> {
        self.0.segments.lock().unwrap().get(idx).cloned()
    }

    fn add(&self, idx: usize, n: u64) {
        self.0.downloaded.fetch_add(n, Ordering::Relaxed);
        self.0.received.fetch_add(n, Ordering::Relaxed);
        if let Some(s) = self.0.segments.lock().unwrap().get_mut(idx) {
            s.downloaded += n;
        }
    }

    /// Forget a segment's progress (used when a non-resumable transfer restarts).
    fn reset_segment(&self, idx: usize) {
        if let Some(s) = self.0.segments.lock().unwrap().get_mut(idx) {
            self.0.downloaded.fetch_sub(s.downloaded, Ordering::Relaxed);
            s.downloaded = 0;
        }
    }

    fn set_status(&self, idx: usize, st: SegmentStatus) {
        if let Some(s) = self.0.segments.lock().unwrap().get_mut(idx) {
            s.status = st;
        }
    }

    fn bump_retry(&self, idx: usize) {
        if let Some(s) = self.0.segments.lock().unwrap().get_mut(idx) {
            s.retry_count += 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Keep the `.part` file and saved state; the download can be resumed.
    Pause,
    /// Throw the partial data away; the download can not be resumed.
    Cancel,
}

/// Handle to control a running download.
///
/// * [`pause`](Self::pause): `download()` returns `Err(Paused)`, progress is kept, resume later.
/// * [`cancel`](Self::cancel): `download()` returns `Err(Cancelled)`, the `.part` file is deleted and
///   the record is marked `Cancelled`. Cancel wins if both are requested.
/// * [`set_speed_limit`](Self::set_speed_limit): change this download's bandwidth cap while it runs.
#[derive(Clone)]
pub struct DownloadControl {
    stop: CancellationToken,
    reason: Arc<AtomicU8>, // 0 none, 1 pause, 2 cancel
    limiter: Arc<TokenBucket>,
    /// Priority changed while running ([`NO_PRIORITY`] = untouched). The engine folds it into every
    /// checkpoint, so its own copy of the record can't overwrite a change made from outside.
    priority: Arc<AtomicU8>,
    /// Preview mode: fetch the start and the very end first (players need both), then in order.
    preview: Arc<std::sync::atomic::AtomicBool>,
}

const NO_PRIORITY: u8 = u8::MAX;

impl Default for DownloadControl {
    fn default() -> Self {
        Self {
            stop: CancellationToken::new(),
            reason: Arc::new(AtomicU8::new(0)),
            limiter: TokenBucket::new(None),
            priority: Arc::new(AtomicU8::new(NO_PRIORITY)),
            preview: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl DownloadControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pause(&self) {
        let _ = self.reason.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);
        self.stop.cancel();
    }

    pub fn cancel(&self) {
        self.reason.store(2, Ordering::SeqCst);
        self.stop.cancel();
    }

    pub fn stop_reason(&self) -> Option<StopReason> {
        match self.reason.load(Ordering::SeqCst) {
            1 => Some(StopReason::Pause),
            2 => Some(StopReason::Cancel),
            _ => None,
        }
    }

    /// Cap this download's speed in bytes/s (`None` = unlimited). Applies immediately.
    pub fn set_speed_limit(&self, bytes_per_sec: Option<u64>) {
        self.limiter.set_rate(bytes_per_sec);
    }

    pub fn speed_limit(&self) -> Option<u64> {
        self.limiter.rate()
    }

    /// Ask for preview order (see [`planner::preview_order`]); read when the transfer is planned or resumed.
    pub fn set_preview(&self, on: bool) {
        self.preview.store(on, Ordering::Relaxed);
    }

    pub fn preview(&self) -> bool {
        self.preview.load(Ordering::Relaxed)
    }

    /// Record a priority change for a running download so checkpoints keep it.
    pub fn set_priority(&self, p: Priority) {
        self.priority.store(p.as_i64() as u8, Ordering::Relaxed);
    }

    /// The priority set through [`set_priority`](Self::set_priority), if any.
    pub fn priority(&self) -> Option<Priority> {
        match self.priority.load(Ordering::Relaxed) {
            NO_PRIORITY => None,
            v => Some(Priority::from_i64(v as i64)),
        }
    }

    /// Copy values that may change while running (priority, speed limit) into `rec`.
    fn sync_into(&self, rec: &mut DownloadRecord) {
        if let Some(p) = self.priority() {
            rec.priority = p;
        }
        rec.speed_limit = self.speed_limit();
    }

    fn token(&self) -> &CancellationToken {
        &self.stop
    }
}

/// Aborts the wrapped task when dropped (used for the adaptive controller).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Periodic checkpoint task. `finish()` stops it gracefully (letting an in-flight save land before
/// the final state is written); dropping it without `finish()` aborts it, so an aborted download
/// never leaves a checkpointer behind.
struct Checkpointer {
    stop: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Checkpointer {
    async fn finish(mut self) {
        self.stop.cancel();
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}

impl Drop for Checkpointer {
    fn drop(&mut self) {
        if let Some(h) = &self.handle {
            h.abort();
        }
    }
}

/// Decides which folder a file goes into once its final name (and type) is known: the manager uses it
/// to sort downloads into Video / Audio / Documents / ... . `base` is the folder the download was queued
/// for; the answer must be `base` itself or a folder below it.
pub trait Placement: Send + Sync {
    fn folder_for(&self, base: &Path, filename: &str, content_type: Option<&str>) -> PathBuf;
}

#[derive(Clone)]
pub struct Downloader {
    /// Shared by every clone; replaced as a whole when the network settings change (proxy, user agent).
    /// Transfers already running keep the client they started with.
    client: Arc<std::sync::RwLock<HttpClient>>,
    cfg: DownloadConfig,
    /// Live copy of `cfg.max_connections`; changes apply to transfers planned or resumed afterwards.
    max_connections: Arc<AtomicUsize>,
    store: Option<Arc<dyn DownloadStore>>,
    limits: Arc<RateLimits>,
    scheduler: Option<Arc<SlotScheduler>>,
    /// The folders downloads may be written under (shared with the manager; changes apply at once).
    sandbox: Option<Arc<path_safety::AllowedRoots>>,
    placement: Option<Arc<dyn Placement>>,
    /// Files being written by downloads of this downloader (see [`Downloader::claim_name`]).
    claims: Arc<Mutex<std::collections::HashSet<String>>>,
}

/// A file name reserved by a running download; released when the download task ends.
struct NameClaim {
    claims: Arc<Mutex<std::collections::HashSet<String>>>,
    key: String,
}

impl Drop for NameClaim {
    fn drop(&mut self) {
        self.claims.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.key);
    }
}

fn claim_key(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    // Windows and macOS file systems ignore case: `A.zip` and `a.zip` are the same file.
    if cfg!(any(windows, target_os = "macos")) {
        s.to_lowercase()
    } else {
        s
    }
}

/// `report.pdf` -> `report (n).pdf`; a name without an extension gets ` (n)` at the end.
pub fn numbered_name(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

impl Downloader {
    pub fn new(client_cfg: &ClientConfig, cfg: DownloadConfig) -> Result<Self> {
        Ok(Self {
            client: Arc::new(std::sync::RwLock::new(HttpClient::new(client_cfg)?)),
            max_connections: Arc::new(AtomicUsize::new(cfg.max_connections)),
            cfg,
            store: None,
            limits: RateLimits::new(),
            scheduler: None,
            sandbox: None,
            placement: None,
            claims: Arc::default(),
        })
    }

    /// `name` in `dir`, or `name (1)`, `name (2)`...: the first one that no file, no other download (running, or
    /// paused / queued in the store) uses, reserved for the caller until the returned claim is dropped. So a
    /// second download of `setup.exe` becomes `setup (1).exe`, and two downloads of the same name running at
    /// once never share a partial file or overwrite each other. A partial file no download owns (left by a
    /// crash before its record was saved) does not take the name: it is simply started over.
    fn claim_name(&self, dir: &Path, name: &str, me: Option<i64>) -> Result<(PathBuf, String, NameClaim)> {
        let taken = |p: &Path| std::fs::symlink_metadata(p).is_ok();
        for n in 0..10_000u32 {
            let candidate = if n == 0 { name.to_string() } else { numbered_name(name, n) };
            let path = dir.join(&candidate);
            let key = claim_key(&path);
            let mut claims = self.claims.lock().unwrap_or_else(|p| p.into_inner());
            if claims.contains(&key) || taken(&path) {
                continue;
            }
            let owned_elsewhere = match &self.store {
                Some(store) => store.destination_in_use(&path, me)?,
                // Without a store, a partial file may belong to another downloader: never touch it.
                None => taken(&part_path(&path)),
            };
            if owned_elsewhere {
                continue;
            }
            claims.insert(key.clone());
            return Ok((path, candidate, NameClaim { claims: self.claims.clone(), key }));
        }
        Err(DownloadError::FileExists(dir.join(name).display().to_string()))
    }

    /// Reserve the name a stored download already has (so a new download cannot take it while this one's
    /// partial file is briefly absent, e.g. when it restarts from scratch).
    fn claim_existing(&self, path: &Path) -> NameClaim {
        let key = claim_key(path);
        self.claims.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone());
        NameClaim { claims: self.claims.clone(), key }
    }

    /// Persist progress to `store`, enabling pause/resume and crash recovery.
    pub fn with_store(mut self, store: Arc<dyn DownloadStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Share bandwidth limits (global + categories) between several downloaders.
    pub fn with_limits(mut self, limits: Arc<RateLimits>) -> Self {
        self.limits = limits;
        self
    }

    /// Only write under these folders (checked again right before every write). Replaces
    /// `DownloadConfig::sandbox_root` and can change while downloads run.
    pub fn with_sandbox(mut self, roots: Arc<path_safety::AllowedRoots>) -> Self {
        self.sandbox = Some(roots);
        self
    }

    /// Let `placement` choose the sub-folder of each new file (sorting by type).
    pub fn with_placement(mut self, placement: Arc<dyn Placement>) -> Self {
        self.placement = Some(placement);
        self
    }

    /// Use `client` for every request from now on (new network settings). Downloads already
    /// transferring finish with the client they started with; the next start / resume uses this one.
    pub fn set_client(&self, client: HttpClient) {
        *self.client.write().unwrap_or_else(|p| p.into_inner()) = client;
    }

    fn client(&self) -> HttpClient {
        self.client.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn place(&self, base: &Path, name: &str, content_type: Option<&str>) -> PathBuf {
        match &self.placement {
            Some(p) => p.folder_for(base, name, content_type),
            None => base.to_path_buf(),
        }
    }

    /// Limit how many downloads run at once, granting slots by priority.
    pub fn with_scheduler(mut self, scheduler: Arc<SlotScheduler>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Change the per-download connection ceiling. Transfers already running keep the ceiling they
    /// started with; new and resumed transfers use the new value.
    pub fn set_max_connections(&self, n: usize) {
        self.max_connections.store(n.max(1), Ordering::Relaxed);
    }

    pub fn max_connections(&self) -> usize {
        self.max_connections.load(Ordering::Relaxed)
    }

    /// `(start, ceiling)` connections a parallel download of `size` bytes would get now: the same rule the
    /// download uses when it starts (the adaptive controller moves between the two while it runs).
    pub fn connection_plan(&self, size: u64) -> (usize, usize) {
        planner::connection_limits(size, self.max_connections(), self.cfg.min_segment_size)
    }

    pub fn limits(&self) -> &Arc<RateLimits> {
        &self.limits
    }

    pub fn scheduler(&self) -> Option<&Arc<SlotScheduler>> {
        self.scheduler.as_ref()
    }

    pub async fn probe(&self, url: &str) -> Result<ResourceInfo> {
        let url = parse_url(url)?;
        self.probe_with_retry(&url, &CancellationToken::new(), &RequestExtras::default(), None).await
    }

    /// One quick look at a URL with the browser's headers (no retries), to show the user what a
    /// download is before it starts. Same request the download itself would make first.
    pub async fn probe_quick(&self, url: &str, headers: &[(String, String)]) -> Result<ResourceInfo> {
        let url = parse_url(url)?;
        let extras = RequestExtras::from_pairs(headers);
        Ok(self.client().probe_with(&url, &extras).await?)
    }

    async fn probe_with_retry(
        &self,
        url: &Url,
        cancel: &CancellationToken,
        extras: &RequestExtras,
        progress: Option<&ProgressHandle>,
    ) -> Result<ResourceInfo> {
        let res = retry::with_retry(
            &self.cfg.retry,
            cancel,
            |n, e, d| {
                tracing::warn!(attempt = n, error = %e, ?d, "probe failed, retrying");
                if let Some(p) = progress {
                    p.0.probe_retries.fetch_add(1, Ordering::Relaxed);
                    p.note_retry(None, n, e, d);
                }
            },
            |_| async {
                let client = self.client();
                // A server that is slow to answer must not make Pause / Cancel wait for it.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(DownloadError::Cancelled),
                    r = client.probe_with(url, extras) => Ok(r?),
                }
            },
        )
        .await;
        if let Some(p) = progress {
            match &res {
                Ok(info) => {
                    p.note_status(info.status);
                    p.note(RunEventKind::ServerAnswered {
                        status: info.status,
                        size: info.content_length,
                        ranges: info.supports_parallel(),
                    });
                }
                Err(e) => {
                    if let Some(s) = e.http_status() {
                        p.note_status(s);
                    }
                }
            }
        }
        res
    }

    /// Cancel a download that is *not currently running* (paused, failed or interrupted): deletes
    /// its `.part` file and marks it `Cancelled`. For a running download use [`DownloadControl::cancel`].
    pub async fn cancel(&self, id: i64) -> Result<()> {
        let store = self.store.clone().ok_or_else(|| DownloadError::InvalidRequest("cancel by id requires a state store".into()))?;
        let mut rec = blocking({
            let store = store.clone();
            move || store.load(id)
        })
        .await?
        .ok_or_else(|| DownloadError::InvalidRequest(format!("unknown download id {id}")))?;
        if rec.status == DownloadStatus::Completed {
            return Err(DownloadError::InvalidRequest("download already completed".into()));
        }
        self.discard(&mut rec).await;
        Ok(())
    }

    /// Delete partial data and record the download as `Cancelled`.
    async fn discard(&self, rec: &mut DownloadRecord) {
        let _ = tokio::fs::remove_file(part_path(&rec.destination)).await;
        rec.status = DownloadStatus::Cancelled;
        rec.request_headers.clear(); // never keep a browser's cookies for a download that is over
        rec.downloaded = 0;
        rec.resumable = false;
        rec.last_error = None;
        for s in &mut rec.segments {
            s.downloaded = 0;
            s.status = SegmentStatus::Cancelled;
        }
        let _ = self.persist(rec).await;
    }

    /// Turn a stop request into the right terminal state + error (`Paused` or `Cancelled`).
    async fn on_stop(&self, control: &DownloadControl, rec: &mut DownloadRecord) -> DownloadError {
        if control.stop_reason() == Some(StopReason::Cancel) {
            self.discard(rec).await;
            DownloadError::Cancelled
        } else {
            rec.status = DownloadStatus::Paused;
            rec.last_error = None;
            failpoint::hit("pause.before_persist");
            let _ = self.persist(rec).await;
            DownloadError::Paused
        }
    }

    /// Stopped before any transfer began (while queued for a slot or probing).
    async fn stopped_early(&self, control: &DownloadControl, stored: Option<DownloadRecord>) -> DownloadError {
        match stored {
            Some(mut rec) => self.on_stop(control, &mut rec).await,
            None if control.stop_reason() == Some(StopReason::Cancel) => DownloadError::Cancelled,
            None => DownloadError::Paused,
        }
    }

    /// Run (or resume) a download.
    ///
    /// Stopping it through `control` is either a **pause** (`Err(Paused)`: workers flush, state is
    /// checkpointed, resume with `DownloadRequest::resume`) or a **cancel** (`Err(Cancelled)`: the
    /// `.part` file is deleted and the record marked `Cancelled`).
    pub async fn download(&self, req: &DownloadRequest, control: DownloadControl, progress: ProgressHandle) -> Result<DownloadOutcome> {
        let cancel = control.token().clone();

        // ---- load stored state (resume) ------------------------------------------------------
        let stored = match req.resume_id {
            None => None,
            Some(id) => {
                let store = self.store.clone().ok_or_else(|| DownloadError::InvalidRequest("resume requires a state store".into()))?;
                let rec = blocking(move || store.load(id)).await?;
                let rec = rec.ok_or_else(|| DownloadError::InvalidRequest(format!("unknown download id {id}")))?;
                if rec.status == DownloadStatus::Cancelled {
                    return Err(DownloadError::InvalidRequest(format!("download {id} was cancelled; start a new one")));
                }
                Some(rec)
            }
        };
        if stored.is_some() {
            failpoint::hit("resume.after_load");
        }
        let priority = stored.as_ref().map_or(req.priority, |r| r.priority);
        let category = stored.as_ref().map_or(req.category.clone(), |r| r.category.clone());
        if control.speed_limit().is_none() {
            control.set_speed_limit(stored.as_ref().map_or(req.speed_limit, |r| r.speed_limit.or(req.speed_limit)));
        }

        // ---- wait for a run slot (priority order) --------------------------------------------
        let _slot = match &self.scheduler {
            None => None,
            Some(s) => {
                let key = req.resume_id.map_or_else(|| s.next_key(), |i| i as u64);
                progress.set_key(key);
                match s.acquire(key, priority, &cancel).await {
                    Ok(g) => Some(g),
                    Err(_) => return Err(self.stopped_early(&control, stored).await),
                }
            }
        };

        let url = parse_url(stored.as_ref().map_or(&req.url, |r| &r.url))?;
        // What the browser knew (referrer, cookies, user agent), kept with the record so a resumed
        // download authenticates the same way.
        let extras = RequestExtras::from_pairs(stored.as_ref().map_or(&[][..], |r| &r.request_headers[..]));
        // A server that prepares the file before answering (a database export...): its answer to one patient
        // request is saved directly, so the file is prepared once.
        let mut first_answer = None;
        let info = match self.probe_with_retry(&url, &cancel, &extras, Some(&progress)).await {
            Ok(i) => i,
            Err(DownloadError::Cancelled) => return Err(self.stopped_early(&control, stored).await),
            Err(DownloadError::Http(HttpError::SlowToAnswer)) => {
                tracing::info!("the server is slow to answer: waiting for it to prepare the file");
                progress.note(RunEventKind::WaitingForServer);
                match self.wait_for_answer(&url, &cancel, &extras, &progress).await {
                    Ok((info, resp)) => {
                        first_answer = Some(resp);
                        info
                    }
                    Err(DownloadError::Cancelled) => return Err(self.stopped_early(&control, stored).await),
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
        let slow_server = first_answer.is_some();

        // ---- decide: resume or fresh ---------------------------------------------------------
        // The file name this download writes, reserved until it ends.
        let mut _name_claim: Option<NameClaim> = None;
        let (mut rec, resumed) = match stored {
            // Created ahead of time (by the download manager) and never run: plan it now.
            Some(mut rec) if rec.is_unstarted() => {
                let name = if rec.filename.trim().is_empty() { choose_filename(&info, &url) } else { sanitize_filename(&rec.filename) };
                if web_page_instead_of_file(&info, &name) {
                    return Err(DownloadError::WebPageInsteadOfFile(name));
                }
                let dir = self.place(&rec.destination, &name, info.content_type.as_deref());
                self.confine(&dir)?;
                tokio::fs::create_dir_all(&dir).await?;
                let (dest, name) = if req.overwrite {
                    (dir.join(&name), name)
                } else {
                    let (dest, name, claim) = self.claim_name(&dir, &name, rec.id)?;
                    _name_claim = Some(claim);
                    (dest, name)
                };
                self.confine(&dest)?;
                rec.filename = name;
                rec.destination = dest;
                (self.plan_fresh(rec, &info).await?, false)
            }
            Some(rec) => {
                _name_claim = Some(self.claim_existing(&rec.destination));
                match self.check_resumable(&rec, &info).await {
                    Ok(()) => {
                        let mut rec = rec;
                        for s in &mut rec.segments {
                            s.status = if s.is_complete() { SegmentStatus::Completed } else { SegmentStatus::Pending };
                        }
                        (rec, true)
                    }
                    Err(reason) => {
                        tracing::warn!(%reason, "cannot resume, restarting download from scratch");
                        progress.note(RunEventKind::StartingOver { reason: reason.clone() });
                        let _ = tokio::fs::remove_file(part_path(&rec.destination)).await;
                        (self.plan_fresh(rec, &info).await?, false)
                    }
                }
            }
            None => {
                let name = match &req.filename {
                    Some(n) => sanitize_filename(n),
                    None => choose_filename(&info, &url),
                };
                if web_page_instead_of_file(&info, &name) {
                    return Err(DownloadError::WebPageInsteadOfFile(name));
                }
                let dir = self.place(&req.dest_dir, &name, info.content_type.as_deref());
                self.confine(&dir)?;
                tokio::fs::create_dir_all(&dir).await?;
                let (dest, name) = if req.overwrite {
                    (dir.join(&name), name)
                } else {
                    let (dest, name, claim) = self.claim_name(&dir, &name, None)?;
                    _name_claim = Some(claim);
                    (dest, name)
                };
                self.confine(&dest)?;
                let rec = DownloadRecord {
                    id: None,
                    url: url.to_string(),
                    filename: name,
                    destination: dest,
                    total_size: None,
                    downloaded: 0,
                    status: DownloadStatus::Queued,
                    etag: None,
                    last_modified: None,
                    resumable: false,
                    checksum: req.expected_checksum.as_ref().map(|c| c.to_string()),
                    last_error: None,
                    segments: Vec::new(),
                    priority,
                    category: category.clone(),
                    speed_limit: None,
                    content_type: None,
                    server: None,
                    request_headers: Vec::new(),
                };
                (self.plan_fresh(rec, &info).await?, false)
            }
        };
        progress.note(if resumed {
            RunEventKind::Resuming { from: rec.segments.iter().map(|s| s.downloaded).sum() }
        } else {
            RunEventKind::Planned { parallel: rec.resumable, parts: rec.segments.len() }
        });
        // A resumed record's folder may have been swapped for a link since it was written.
        self.confine(&rec.destination)?;
        let expected_checksum = match (&req.expected_checksum, &rec.checksum) {
            (Some(c), _) => Some(c.clone()),
            (None, Some(s)) => Some(Checksum::parse(s).map_err(DownloadError::InvalidRequest)?),
            _ => None,
        };
        let part = part_path(&rec.destination);
        let parallel = rec.resumable;
        // A download with no category of its own shares the limit set for its kind of file ("video", "program", ...).
        let limit_group =
            category.clone().unwrap_or_else(|| crate::filetype::classify(&rec.filename, rec.content_type.as_deref()).key().to_string());
        let throttle = self.limits.throttle_for(Some(&limit_group), control.limiter.clone());

        // ---- run ------------------------------------------------------------------------------
        progress.init(rec.total_size, rec.segments.clone());
        rec.status = DownloadStatus::Downloading;
        rec.last_error = None;
        rec.downloaded = progress.snapshot().downloaded;
        rec.speed_limit = control.speed_limit();
        self.persist(&mut rec).await?;
        if let Some(id) = rec.id {
            progress.set_id(id);
        }

        let checkpointer = self.store.clone().map(|store| {
            let (rec, progress, part, every, control) =
                (rec.clone(), progress.clone(), part.clone(), self.cfg.checkpoint_interval, control.clone());
            let stop = CancellationToken::new();
            let stop2 = stop.clone();
            let handle = tokio::spawn(tracing::Instrument::instrument(
                async move {
                    let mut rec = rec;
                    let mut tick = tokio::time::interval(every);
                    tick.tick().await;
                    loop {
                        tokio::select! {
                            _ = stop2.cancelled() => break,
                            _ = tick.tick() => {}
                        }
                        if let Err(e) = checkpoint(&store, &mut rec, &progress, &part, &control).await {
                            tracing::warn!(error = %e, "checkpoint failed");
                        }
                    }
                },
                tracing::Span::current(),
            ));
            Checkpointer { stop, handle: Some(handle) }
        });

        let total = rec.total_size.unwrap_or(0);
        let run = if parallel {
            self.run_parallel(&url, &info, &part, total, &cancel, &progress, &throttle, &extras, control.preview()).await
        } else {
            // No size announced: the file is made while it is sent (an export, a generated report). Like a browser,
            // wait for such a server as long as it takes, before it answers and between two pieces.
            let patient = slow_server || rec.total_size.is_none();
            self.run_sequential(&url, &part, &cancel, &progress, &throttle, &extras, patient.then(|| first_answer.take())).await.map(|_| 1)
        };
        // Stop periodic checkpoints (letting an in-flight one land) before the final, authoritative one.
        if let Some(c) = checkpointer {
            c.finish().await;
        }

        let cancelled = matches!(run, Err(DownloadError::Cancelled)) && control.stop_reason() == Some(StopReason::Cancel);
        if !cancelled {
            if let Some(store) = self.store.clone() {
                let _ = checkpoint(&store, &mut rec, &progress, &part, &control).await;
            }
        }
        control.sync_into(&mut rec);
        let connections = match run {
            Ok(n) => n,
            Err(DownloadError::Cancelled) => return Err(self.on_stop(&control, &mut rec).await),
            Err(e) => {
                rec.status = DownloadStatus::Failed;
                rec.last_error = Some(e.to_string());
                let _ = self.persist(&mut rec).await;
                return Err(e);
            }
        };
        // Sequential transfers report the byte count they wrote; parallel ones the planned size.
        let size = if parallel { total } else { progress.snapshot().downloaded };

        // ---- verify + finalize ---------------------------------------------------------------
        rec.status = DownloadStatus::Verifying;
        let mut part = part;
        if !req.overwrite && std::fs::symlink_metadata(&rec.destination).is_ok() {
            // A file appeared under this name while downloading (another program, the user): never replace it,
            // take the next free name instead.
            let dir = rec.destination.parent().map(Path::to_path_buf).unwrap_or_default();
            let (alt, alt_name, claim) = self.claim_name(&dir, &rec.filename, rec.id)?;
            self.confine(&alt)?;
            tokio::fs::rename(&part, part_path(&alt)).await?;
            tracing::info!(from = %rec.filename, to = %alt_name, "the file name was taken meanwhile; saving under the next free name");
            progress.note(RunEventKind::Renamed { to: alt_name.clone() });
            rec.destination = alt;
            rec.filename = alt_name;
            part = part_path(&rec.destination);
            _name_claim = Some(claim);
        }
        self.persist(&mut rec).await?;
        let verified = self.finalize(&part, &rec, size, expected_checksum.as_ref()).await;
        if let Err(e) = verified {
            rec.status = DownloadStatus::Failed;
            rec.last_error = Some(e.to_string());
            let _ = self.persist(&mut rec).await;
            return Err(e);
        }
        rec.status = DownloadStatus::Completed;
        rec.request_headers.clear(); // done: the cookies are of no further use
        rec.downloaded = size;
        // The server never said the size: now it is known.
        rec.total_size.get_or_insert(size);
        self.persist(&mut rec).await?;
        if let (Some(store), Some(id)) = (self.store.clone(), rec.id) {
            blocking(move || store.complete(id)).await?;
        }
        Ok(DownloadOutcome {
            id: rec.id,
            path: rec.destination,
            size,
            connections,
            parallel,
            resumed,
            checksum_verified: expected_checksum.is_some(),
            info,
        })
    }

    /// Why a stored download can't be continued (`Ok` = safe to resume).
    async fn check_resumable(&self, rec: &DownloadRecord, info: &ResourceInfo) -> std::result::Result<(), String> {
        if rec.status == DownloadStatus::Completed {
            return Err("already completed".into());
        }
        if !rec.resumable || rec.segments.is_empty() {
            return Err("download is not resumable (sequential transfer)".into());
        }
        if !info.supports_parallel() {
            return Err("server no longer supports verified range requests".into());
        }
        if info.content_length != rec.total_size {
            return Err(format!("size changed: {:?} -> {:?}", rec.total_size, info.content_length));
        }
        // ETag first (strong only), then Last-Modified. No validator at all = cannot prove sameness.
        let same = match (&rec.etag, &info.etag) {
            (Some(a), Some(b)) if !a.starts_with("W/") => a == b,
            (Some(_), _) => false,
            (None, _) => match (&rec.last_modified, &info.last_modified) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
        };
        if !same {
            return Err("remote resource changed or cannot be validated (ETag / Last-Modified)".into());
        }
        let part = part_path(&rec.destination);
        match tokio::fs::metadata(&part).await {
            Ok(m) if Some(m.len()) == rec.total_size => {}
            Ok(m) => return Err(format!(".part size {} != expected {:?}", m.len(), rec.total_size)),
            Err(_) => return Err(".part file missing".into()),
        }
        if rec.segments.iter().any(|s| s.downloaded > s.len()) {
            return Err("stored segment state is inconsistent".into());
        }
        Ok(())
    }

    /// Lay out a brand-new transfer (segments + preallocated `.part`) for `rec`.
    /// Fail unless `path` lies inside the sandbox folder (a no-op when none is configured).
    fn confine(&self, path: &Path) -> Result<()> {
        if let Some(roots) = &self.sandbox {
            return roots.check(path).map(drop).map_err(|e| DownloadError::InvalidRequest(e.to_string()));
        }
        match &self.cfg.sandbox_root {
            Some(root) => path_safety::ensure_within(root, path).map_err(|e| DownloadError::InvalidRequest(e.to_string())),
            None => Ok(()),
        }
    }

    async fn plan_fresh(&self, mut rec: DownloadRecord, info: &ResourceInfo) -> Result<DownloadRecord> {
        let part = part_path(&rec.destination);
        rec.etag = info.etag.clone();
        rec.last_modified = info.last_modified.clone();
        rec.content_type = info.content_type.clone();
        rec.server = info.server.clone();
        rec.total_size = info.content_length;
        rec.downloaded = 0;
        match info.content_length {
            Some(size) if info.supports_parallel() && size > 0 => {
                let (_, ceiling) = planner::connection_limits(size, self.max_connections(), self.cfg.min_segment_size);
                rec.segments = planner::plan_segments(size, ceiling, self.cfg.min_segment_size);
                rec.resumable = true;
                // A server can announce any size. What cannot fit is refused *before* it is reserved,
                // instead of filling the disk (or failing late, after hours of transfer).
                let folder = part.parent().unwrap_or_else(|| Path::new("."));
                if let Ok(free) = fs4::available_space(folder) {
                    if size > free {
                        let msg = format!("not enough free disk space: {size} bytes needed, {free} available");
                        return Err(std::io::Error::new(std::io::ErrorKind::StorageFull, msg).into());
                    }
                }
                disk::create_preallocated(&part, size).await?;
            }
            other => {
                rec.segments = vec![Segment::new(0, 0, other.map_or(0, |s| s.saturating_sub(1)))];
                rec.resumable = false;
            }
        }
        Ok(rec)
    }

    async fn finalize(&self, part: &Path, rec: &DownloadRecord, size: u64, checksum: Option<&Checksum>) -> Result<()> {
        self.confine(&rec.destination)?;
        // The part file must still be the plain file we wrote, not a link put in its place.
        if tokio::fs::symlink_metadata(part).await?.file_type().is_symlink() {
            return Err(DownloadError::InvalidRequest("the partial file was replaced by a link".into()));
        }
        let actual = tokio::fs::metadata(part).await?.len();
        if actual != size {
            return Err(DownloadError::SizeMismatch { expected: size, actual });
        }
        if let Some(c) = checksum {
            if let Err(e) = integrity::verify_file(part, c).await {
                if matches!(e, DownloadError::ChecksumMismatch { .. }) {
                    let _ = tokio::fs::remove_file(part).await; // never leave unverified data lying around
                }
                return Err(e);
            }
        }
        failpoint::hit("finalize.before_rename");
        failpoint::hit_io("finalize.rename")?;
        tokio::fs::rename(part, &rec.destination).await?;
        failpoint::hit("finalize.after_rename");
        if self.cfg.mark_of_the_web {
            if let Err(e) = path_safety::mark_downloaded(&rec.destination, &rec.url) {
                tracing::debug!(error = %e, "could not mark the file as downloaded from the internet");
            }
        }
        Ok(())
    }

    async fn persist(&self, rec: &mut DownloadRecord) -> Result<()> {
        if let Some(store) = self.store.clone() {
            let snapshot = rec.clone();
            rec.id = Some(blocking(move || store.save(&snapshot)).await?);
        }
        Ok(())
    }

    /// Multi-connection transfer. The file is cut into many more segments than connections; a gate
    /// decides how many run at once and an adaptive controller resizes it while the download runs.
    /// Returns the peak number of simultaneous connections.
    #[allow(clippy::too_many_arguments)]
    async fn run_parallel(
        &self,
        url: &Url,
        info: &ResourceInfo,
        part: &Path,
        total: u64,
        cancel: &CancellationToken,
        progress: &ProgressHandle,
        throttle: &Throttle,
        extras: &RequestExtras,
        preview: bool,
    ) -> Result<usize> {
        // A strong ETag (else Last-Modified) makes every ranged request fail loudly if the file changes mid-download.
        let validator = info.etag.clone().filter(|e| !e.starts_with("W/")).or_else(|| info.last_modified.clone());
        let (recommended, ceiling) = planner::connection_limits(total, self.max_connections(), self.cfg.min_segment_size);
        let adaptive = self.cfg.adaptive.enabled && ceiling > 1;
        // Start small (gentle with servers that cap connections per client) and let the controller climb in big
        // steps (4 -> 16 -> 64) while it pays off; without adaptivity, use the size-based default straight away.
        let start = if adaptive { recommended.min(4) } else { recommended }.clamp(1, ceiling);
        let gate = AdaptiveGate::new(start);
        // Shared by the periodic sampler and by workers that see a 429/503 (which react immediately).
        let controller = adaptive.then(|| Arc::new(Mutex::new(AdaptiveController::new(self.cfg.adaptive.clone(), start, ceiling))));
        let _sampler =
            controller.clone().map(|ctl| spawn_sampler(self.cfg.adaptive.interval, ctl, gate.clone(), progress.clone(), throttle.clone()));

        let group = cancel.child_token();
        let mut set = JoinSet::new();
        let pending: Vec<Segment> = progress.snapshot().segments.into_iter().filter(|s| !s.is_complete()).collect();
        // Segments take connection slots in the order they are spawned (the gate is first come, first served).
        for seg in if preview { planner::preview_order(pending) } else { pending } {
            let id = seg.id;
            let (client, url, part, group, progress, validator, policy, gate, throttle, controller, extras) = (
                self.client(),
                url.clone(),
                part.to_path_buf(),
                group.clone(),
                progress.clone(),
                validator.clone(),
                self.cfg.retry.clone(),
                gate.clone(),
                throttle.clone(),
                controller.clone(),
                extras.clone(),
            );
            let (buffer_size, channel_capacity) = (self.cfg.buffer_size, self.cfg.channel_capacity);
            // Spawned tasks do not inherit the caller's span: pass it on, so their log lines name the download.
            set.spawn(tracing::Instrument::instrument(
                async move {
                    let res = retry::with_retry(
                        &policy,
                        &group,
                        |n, e, d| {
                            tracing::warn!(segment = id, attempt = n, error = %e, ?d, "segment failed, retrying");
                            progress.bump_retry(id);
                            progress.set_status(id, SegmentStatus::Retrying);
                            progress.note_retry(Some(id), n, e, d);
                            if retry::is_throttle(e) {
                                // 429 / 503: the server is telling us we use too many connections.
                                if let Some(ctl) = &controller {
                                    if let Some(n) = ctl.lock().unwrap().on_throttled(Instant::now()) {
                                        tracing::info!(connections = n, "server is throttling: reducing connections");
                                        progress.note(RunEventKind::Throttled { connections: n });
                                        gate.set_limit(n);
                                    }
                                }
                            }
                        },
                        |_| {
                            let (client, url, part, group, progress, validator, throttle, gate, extras) =
                                (&client, &url, &part, &group, &progress, &validator, &throttle, &gate, &extras);
                            async move {
                                // A connection slot is held only while a request is actually in flight, never
                                // while backing off between retries: otherwise throttled segments would keep
                                // "occupying" slots and lowering the limit could not take effect.
                                // (Segments start in order; the gate size adapts live.)
                                let _permit = tokio::select! {
                                    _ = group.cancelled() => return Err(DownloadError::Cancelled),
                                    p = gate.acquire() => p,
                                };
                                // Always continue from what is actually on disk for this segment.
                                let cur = progress.segment(id).ok_or_else(|| DownloadError::Internal("missing segment".into()))?;
                                if cur.is_complete() {
                                    return Ok(());
                                }
                                progress.set_status(id, SegmentStatus::Downloading);
                                let from = cur.next_offset();
                                let want = cur.end - from + 1;
                                let sent = Instant::now();
                                let stream = client.get_range_with(url, from, cur.end, validator.as_deref(), extras).await?;
                                progress.record_latency(sent.elapsed());
                                progress.note_status(206); // `get_range_with` accepts nothing else
                                progress.note_peak(gate.peak_active());
                                let got = disk::pump(
                                    stream.into_stream(),
                                    part,
                                    PumpParams {
                                        offset: from,
                                        limit: Some(want),
                                        buffer_size,
                                        channel_capacity,
                                        throttle: Some(throttle.clone()),
                                    },
                                    group,
                                    |n| progress.add(id, n),
                                )
                                .await?;
                                if got < want {
                                    return Err(DownloadError::ShortBody { index: id, got, expected: want });
                                }
                                Ok(())
                            }
                        },
                    )
                    .await;
                    progress.set_status(
                        id,
                        match &res {
                            Ok(()) => SegmentStatus::Completed,
                            Err(DownloadError::Cancelled) => SegmentStatus::Paused,
                            Err(_) => SegmentStatus::Failed,
                        },
                    );
                    res
                },
                tracing::Span::current(),
            ));
        }

        let mut first_err: Option<DownloadError> = None;
        while let Some(joined) = set.join_next().await {
            let res = joined.map_err(|e| DownloadError::Internal(e.to_string())).and_then(|r| r);
            if let Err(e) = res {
                group.cancel(); // stop siblings; they flush and exit
                                // Keep the root cause, not the Cancelled symptom of siblings.
                if first_err.is_none() || matches!(first_err, Some(DownloadError::Cancelled)) {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(DownloadError::Cancelled) | None if cancel.is_cancelled() => Err(DownloadError::Cancelled),
            Some(e) => Err(e),
            None => Ok(gate.peak_active().max(1)),
        }
    }

    /// Single-connection transfer. Not resumable: every attempt restarts from byte 0.
    /// One patient request to a server that prepares the file before answering (see `HttpError::SlowToAnswer`).
    async fn wait_for_answer(
        &self,
        url: &Url,
        cancel: &CancellationToken,
        extras: &RequestExtras,
        progress: &ProgressHandle,
    ) -> Result<(ResourceInfo, Response)> {
        let (info, resp) = retry::with_retry(
            &self.cfg.retry,
            cancel,
            |n, e, d| {
                tracing::warn!(attempt = n, error = %e, ?d, "waiting for the server failed, retrying");
                progress.0.probe_retries.fetch_add(1, Ordering::Relaxed);
                progress.note_retry(None, n, e, d);
            },
            |_| async {
                let client = self.client();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(DownloadError::Cancelled),
                    r = client.get_patient(url, extras) => Ok(r?),
                }
            },
        )
        .await?;
        progress.note_status(info.status);
        progress.note(RunEventKind::ServerAnswered { status: info.status, size: info.content_length, ranges: false });
        Ok((info, resp))
    }

    /// `slow`: the server prepares the file before answering (`Some`, with its first answer if not used yet): every
    /// request waits for it patiently.
    #[allow(clippy::too_many_arguments)]
    async fn run_sequential(
        &self,
        url: &Url,
        part: &Path,
        cancel: &CancellationToken,
        progress: &ProgressHandle,
        throttle: &Throttle,
        extras: &RequestExtras,
        slow: Option<Option<Response>>,
    ) -> Result<u64> {
        let patient = slow.is_some();
        let first = std::sync::Mutex::new(slow.flatten());
        let res = retry::with_retry(
            &self.cfg.retry,
            cancel,
            |n, e, d| {
                tracing::warn!(attempt = n, error = %e, ?d, "transfer failed, restarting");
                progress.bump_retry(0);
                progress.set_status(0, SegmentStatus::Retrying);
                progress.note_retry(None, n, e, d);
            },
            |_| async {
                progress.reset_segment(0);
                progress.set_status(0, SegmentStatus::Downloading);
                let sent = Instant::now();
                let client = self.client();
                let pending = first.lock().unwrap_or_else(|p| p.into_inner()).take();
                let stream = if patient {
                    let resp = match pending {
                        Some(r) => r,
                        None => {
                            let r = tokio::select! {
                                biased;
                                _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                                r = client.get_patient(url, extras) => r?,
                            };
                            r.1
                        }
                    };
                    progress.note_status(resp.status().as_u16());
                    client.patient_stream(resp).boxed()
                } else {
                    let resp = client.get_with(url, extras).await?;
                    progress.note_status(resp.status().as_u16());
                    http_client::response_stream(resp).boxed()
                };
                progress.record_latency(sent.elapsed());
                progress.note_peak(1);
                path_safety::create_no_follow(part)?;
                disk::pump(
                    stream,
                    part,
                    PumpParams {
                        offset: 0,
                        limit: None,
                        buffer_size: self.cfg.buffer_size,
                        channel_capacity: self.cfg.channel_capacity,
                        throttle: Some(throttle.clone()),
                    },
                    cancel,
                    |n| progress.add(0, n),
                )
                .await
            },
        )
        .await;
        progress.set_status(
            0,
            match &res {
                Ok(_) => SegmentStatus::Completed,
                Err(DownloadError::Cancelled) => SegmentStatus::Paused,
                Err(_) => SegmentStatus::Failed,
            },
        );
        res
    }
}

/// Samples throughput / errors / latency every interval and resizes the connection gate.
fn spawn_sampler(
    interval: Duration,
    ctl: Arc<Mutex<AdaptiveController>>,
    gate: Arc<AdaptiveGate>,
    progress: ProgressHandle,
    throttle: Throttle,
) -> AbortOnDrop {
    AbortOnDrop(tokio::spawn(tracing::Instrument::instrument(
        async move {
            let mut tick = tokio::time::interval(interval);
            tick.tick().await;
            let (mut last_at, mut last_bytes, mut last_errors) = (Instant::now(), progress.downloaded_now(), progress.total_retries());
            let mut last_waits = throttle.waits();
            loop {
                tick.tick().await;
                let now = Instant::now();
                let (bytes, errors) = (progress.downloaded_now(), progress.total_retries());
                let secs = now.duration_since(last_at).as_secs_f64().max(1e-3);
                let sample = Sample {
                    throughput: bytes.saturating_sub(last_bytes) as f64 / secs,
                    errors: errors.saturating_sub(last_errors),
                    latency: progress.take_latency(),
                    // A limit made a connection wait (a shared one holds each download far below its value), or
                    // the speed runs at the tightest limit.
                    limited: throttle.waits() > last_waits || throttle.is_holding(bytes.saturating_sub(last_bytes) as f64 / secs),
                };
                last_waits = throttle.waits();
                (last_at, last_bytes, last_errors) = (now, bytes, errors);
                let decision = ctl.lock().unwrap().observe(sample);
                if let Some(n) = decision {
                    tracing::debug!(connections = n, throughput = sample.throughput, errors = sample.errors, "adaptive resize");
                    gate.set_limit(n);
                }
            }
        },
        tracing::Span::current(),
    )))
}

/// Durable checkpoint. Order matters for crash safety: snapshot the counters, fsync the
/// data those counters describe, and only then commit the counters. A crash can therefore
/// lose progress, but the database never claims bytes the disk doesn't have.
async fn checkpoint(
    store: &Arc<dyn DownloadStore>,
    rec: &mut DownloadRecord,
    progress: &ProgressHandle,
    part: &Path,
    control: &DownloadControl,
) -> Result<()> {
    let snap = progress.snapshot();
    if let Ok(mut f) = tokio::fs::OpenOptions::new().write(true).open(part).await {
        f.flush().await?;
        failpoint::hit_io("disk.fsync")?;
        f.sync_data().await?;
    }
    failpoint::hit("checkpoint.after_fsync"); // data is durable, the counters are not yet
    rec.downloaded = snap.downloaded;
    rec.segments = snap.segments;
    control.sync_into(rec);
    let (store, snapshot) = (store.clone(), rec.clone());
    rec.id = Some(blocking(move || store.save(&snapshot)).await?);
    Ok(())
}

async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| DownloadError::Internal(e.to_string()))?
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}
