use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use downloader_core::{
    part_path, DownloadControl, DownloadError, DownloadRecord, DownloadRequest, DownloadStatus, DownloadStore, Downloader, Priority,
    ProgressHandle, RateLimits, SegmentStatus, SlotScheduler,
};
use persistence::SqliteStore;
use tokio::sync::{broadcast, watch};

use crate::api::*;
use crate::validate;

mod desktop;
mod diag;
mod doctor;
mod files;
mod intelligence;
mod media;
mod multinet;
mod network;
mod recovery;
pub(crate) use recovery::{DELAYS as RECOVERY_DELAYS, MAX_TRIES as RECOVERY_MAX_TRIES};
mod pending;
mod preview;
mod profiles;
mod publisher;
mod queue;
mod queues;
mod relink;
mod smart_throttle;
mod storage;
mod sysio;
mod tool_updates;
mod tools;
mod update;
mod usage;

pub use media::is_stream_link;
use pending::PendingList;
pub use preview::{media_type as preview_media_type, PreviewHead, PreviewResponse};
use queue::QueueState;
pub use queues::MAIN_QUEUE;
pub use smart_throttle::{
    find_running as smart_throttle_find_running, is_app_path as smart_throttle_is_app_path, Signals as ThrottleSignals,
    DEFAULT_APPS as SMART_THROTTLE_DEFAULT_APPS, IDLE_AFTER as THROTTLE_IDLE_AFTER,
};
use storage::{Sorter, Storage};
pub use update::UpdatePlan;

/// Time constant of the speed shown for each download: long enough that the short pause between two parts of
/// a connection (a new request) and bursts of buffered data do not make the number jump, short enough that a
/// real change shows within a few seconds.
const SPEED_SMOOTHING_SECS: f64 = 3.0;

/// The speed shown for a download after a tick: smoothed (segments finishing and starting make the raw speed jumpy),
/// but never above a limit it is held to, so a limit just set shows at once instead of the number coming down over
/// several seconds.
fn shown_speed(prev: f64, inst: f64, dt: f64, cap: Option<u64>) -> f64 {
    let w = 1.0 - (-dt / SPEED_SMOOTHING_SECS).exp();
    let s = if prev == 0.0 { inst } else { prev + w * (inst - prev) };
    cap.map_or(s, |c| s.min(c as f64))
}

/// Name of the database sealing key inside the credential store.
pub const SECRET_KEY_NAME: &str = "database-key-v1";
const GLOBAL_LIMIT_KEY: &str = "speed_limit.global";
const CATEGORY_LIMIT_PREFIX: &str = "speed_limit.category.";

/// A running (or queued-for-a-slot) download task.
struct Active {
    control: DownloadControl,
    progress: ProgressHandle,
    done: watch::Receiver<bool>,
}

/// Exponentially smoothed speed for one running download.
struct Tracker {
    last_bytes: u64,
    /// `ProgressHandle::received()` at the previous tick (for the session network total).
    last_received: u64,
    last_at: Instant,
    speed: f64,
    announced_running: bool,
    /// When this run started and how many bytes it already had (for the average speed).
    started: Instant,
    baseline: u64,
    /// Most connections seen open at once (diagnostics).
    peak: usize,
}

/// Cached numbers that are too costly to compute on every tick.
#[derive(Default)]
struct Stats {
    counts: HashMap<String, u64>,
    history_total: u64,
    at: Option<Instant>,
    dirty: bool,
}

#[derive(Default)]
struct DiskStats {
    free: Option<u64>,
    total: Option<u64>,
    folder: Option<u64>,
    disk_at: Option<Instant>,
    folder_at: Option<Instant>,
}

struct Inner {
    cfg: ManagerConfig,
    store: Arc<SqliteStore>,
    downloader: Downloader,
    limits: Arc<RateLimits>,
    scheduler: Arc<SlotScheduler>,
    active: Mutex<HashMap<i64, Active>>,
    trackers: Mutex<HashMap<i64, Tracker>>,
    /// Runs that just ended, until their numbers and timeline are saved (`diag.rs`): the diagnostics never show a
    /// finished download without its last run.
    finishing: Mutex<HashMap<i64, ProgressHandle>>,
    events: broadcast::Sender<ManagerEvent>,
    /// Bytes received from the network since start (sum of per-download progress).
    session_bytes: AtomicU64,
    /// Network usage, saved per day (`usage.rs`).
    usage: usage::Usage,
    /// The publisher's checksum lookups (`publisher.rs`).
    publisher: publisher::Lookups,
    /// Ticks still to send after the last download stopped, so the UI settles on zeros.
    idle_ticks: AtomicU8,
    stats: Mutex<Stats>,
    disk: Mutex<DiskStats>,
    sysio: Arc<sysio::SysIo>,
    queue: Mutex<QueueState>,
    /// The Smart Download Queues (`queues.rs`): several queues with rules, over the one slot scheduler.
    queues: Mutex<queues::Queues>,
    pending: Mutex<PendingList>,
    /// Downloads a new link is being checked for (`relink.rs`).
    relinking: Mutex<relink::Relinking>,
    /// The Add dialog's link check that is running (`intelligence.rs`).
    link_check: Mutex<intelligence::LinkCheck>,
    /// Download Doctor: the newest diagnosis of each failed download (`doctor.rs`), in memory only.
    doctor: Mutex<HashMap<i64, crate::doctor::Diagnosis>>,
    media: Mutex<media::MediaState>,
    /// Smart throttle (`smart_throttle.rs`) and live preview (`preview.rs`): isolated features.
    smart: Mutex<smart_throttle::SmartState>,
    preview: Mutex<preview::PreviewState>,
    /// Download Profiles (`profiles.rs`): presets over the settings above, applied in one place.
    profiles: Mutex<profiles::ProfilesState>,
    /// yt-dlp / Deno / ffmpeg fetched on first use (`tools.rs`).
    tools: tools::ToolsState,
    /// Updates of VeloxFetch itself (`update.rs`).
    update: update::UpdateRuntime,
    /// Replaced when the network settings change (see `network.rs`).
    media_http: std::sync::RwLock<http_client::HttpClient>,
    /// Seals secrets kept in the settings (the proxy password). `None`: they stay in memory only.
    sealer: Option<Arc<secrets::Sealer>>,
    network: Mutex<network::Network>,
    /// Several networks at once (`multinet.rs`).
    multinet: Mutex<multinet::MultiNet>,
    /// Failed downloads tried again on their own (`recovery.rs`), in memory only.
    recovery: Mutex<recovery::Recovery>,
    /// Serialises gate changes (pause/resume queue, schedule boundaries).
    queue_lock: tokio::sync::Mutex<()>,
    /// Serialises Smart throttle's pause policy holding / letting go of the queue.
    game_hold_lock: tokio::sync::Mutex<()>,
    /// Serialises changes to the saved diagnostics of downloads (a run ending while the next one starts).
    diag_lock: tokio::sync::Mutex<()>,
    shutting_down: AtomicBool,
    /// Held while downloads run (and the user wants it): the computer does not sleep on its own.
    awake: Mutex<Option<system_integration::KeepAwake>>,
    /// Keeping awake failed on this system: not tried again until the setting is changed.
    awake_unavailable: AtomicBool,
    /// The app found a tray it can show its icon in.
    tray_available: AtomicBool,
    /// yt-dlp hung or failed with Deno and worked without it on this computer: until the app restarts,
    /// video pages are read and downloaded without a JavaScript runtime.
    js_broken: AtomicBool,
    /// Cookies and credentials are sealed on disk (false: they are kept in memory only).
    secrets_sealed: bool,
    /// The default folder, the type folders and the folder choices (see `storage.rs`).
    storage: Arc<Mutex<Storage>>,
    /// Every folder downloads may be written under; shared with the engine, which checks it before each write.
    roots: Arc<path_safety::AllowedRoots>,
}

/// Application controller: owns the lifecycle of every download. The UI layer calls only these
/// methods, with ids and validated DTOs, and never touches the engine, filesystem or network itself.
#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<Inner>,
}

impl DownloadManager {
    /// Open the state database, recover downloads interrupted by a crash, restore saved speed
    /// limits and start the progress ticker.
    pub async fn new(cfg: ManagerConfig) -> Result<Self, ManagerError> {
        tokio::fs::create_dir_all(&cfg.download_root)
            .await
            .map_err(|e| ManagerError::Internal(format!("cannot create download folder: {e}")))?;
        // A crash or a kill leaves the working folder of a stream download behind: clear the old ones.
        let (scratch_root, max_age) = (cfg.download_root.clone(), cfg.media_scratch_max_age);
        let swept = tokio::task::spawn_blocking(move || media_engine::sweep_stale_scratch(&scratch_root, max_age)).await.unwrap_or(0);
        if swept > 0 {
            tracing::info!(folders = swept, "removed working folders of interrupted stream downloads");
        }
        let db_path = cfg.db_path.clone();
        // The key that seals cookies / credentials in the database. The credential store can be slow
        // or missing (no Secret Service on a bare Linux box): never wait long, never fall back to
        // clear text; such secrets then live in memory for the length of the download only.
        let key_store = cfg.key_store.clone();
        let sealer = match tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(move || secrets::Sealer::from_store(key_store.as_ref(), SECRET_KEY_NAME)),
        )
        .await
        {
            Ok(Ok(Ok(s))) => Some(Arc::new(s)),
            other => {
                let why = match other {
                    Ok(Ok(Err(e))) => e.to_string(),
                    _ => "timed out".to_string(),
                };
                tracing::warn!(%why, "secure storage is not available: cookies and credentials are kept in memory only");
                None
            }
        };
        let secrets_sealed = sealer.is_some();
        let settings_sealer = sealer.clone();
        let store = tokio::task::spawn_blocking(move || {
            if let Some(parent) = db_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let s = SqliteStore::open(&db_path)?;
            // Before anything reads a record: with the key, older clear text is sealed; without it, erased.
            s.set_sealer(sealer)?;
            let recovered = s.recover_interrupted()?;
            for r in &recovered {
                tracing::info!(id = r.id, file = %r.filename, action = ?r.action, "recovered interrupted download");
            }
            Ok::<_, DownloadError>(Arc::new(s))
        })
        .await
        .map_err(|e| ManagerError::Internal(e.to_string()))??;

        let limits = RateLimits::new();
        let base_global: Option<u64> = store.get_setting(GLOBAL_LIMIT_KEY)?.and_then(|s| s.parse().ok());
        limits.set_global(base_global);
        let profiles = profiles::ProfilesState::load(&store, base_global);
        for (k, v) in store.settings_with_prefix(CATEGORY_LIMIT_PREFIX)? {
            if let (Some(name), Ok(v)) = (k.strip_prefix(CATEGORY_LIMIT_PREFIX), v.parse::<u64>()) {
                limits.set_category(name, Some(v));
            }
        }
        // Saved queue settings (max active, connections, pause, schedule) override the defaults.
        let queue = QueueState::load(&store, &cfg)?;
        let scheduler = SlotScheduler::new(queue.max_active());
        // An urgent download never waits only because less urgent ones hold every slot (one extra slot).
        scheduler.set_express(true);
        let queues = queues::Queues::load(&store);
        // Closed from the start when paused or outside the window, so nothing slips through
        // before the first evaluation.
        scheduler.set_paused(!queue.wants_open((cfg.clock.0)()));
        // Where downloads go, and the folders the engine may write under (checked again before every write).
        let storage = Storage::load(&store, &cfg);
        let mut allowed = storage.allowed_at_start(&store);
        allowed.retain(|p| p.is_absolute());
        let roots = path_safety::AllowedRoots::new(allowed);
        let storage = Arc::new(Mutex::new(storage));
        // The proxy and user agent the user chose. A saved proxy that cannot be used is ignored (logged),
        // never a reason not to start.
        let network = network::Network::load(&store, settings_sealer.as_deref());
        let mut client_cfg = network.client_config(&cfg.client);
        if let Err(e) = http_client::HttpClient::new(&client_cfg) {
            tracing::warn!(error = %e, "the saved network settings cannot be used, using the system's");
            client_cfg = cfg.client.clone();
        }
        let downloader = Downloader::new(&client_cfg, cfg.download.clone())?
            .with_sandbox(roots.clone())
            .with_placement(Arc::new(Sorter(storage.clone())))
            .with_store(store.clone() as Arc<dyn DownloadStore>)
            .with_limits(limits.clone())
            .with_scheduler(scheduler.clone());
        downloader.set_max_connections(queue.max_connections());
        let (events, _) = broadcast::channel(1024);
        let media_http = std::sync::RwLock::new(media::client(&client_cfg)?);
        let smart = smart_throttle::SmartState::load(&store);
        let multinet = multinet::MultiNet::load(&store);
        let inner = Arc::new(Inner {
            cfg,
            store,
            downloader,
            limits,
            scheduler,
            active: Mutex::new(HashMap::new()),
            trackers: Mutex::new(HashMap::new()),
            finishing: Mutex::new(HashMap::new()),
            events,
            session_bytes: AtomicU64::new(0),
            usage: Default::default(),
            publisher: Default::default(),
            idle_ticks: AtomicU8::new(0),
            stats: Mutex::new(Stats { dirty: true, ..Default::default() }),
            disk: Mutex::new(DiskStats::default()),
            sysio: Default::default(),
            queue: Mutex::new(queue),
            queues: Mutex::new(queues),
            pending: Mutex::new(PendingList::default()),
            relinking: Mutex::new(relink::Relinking::default()),
            link_check: Mutex::new(Default::default()),
            doctor: Mutex::new(HashMap::new()),
            media: Mutex::new(Default::default()),
            smart: Mutex::new(smart),
            preview: Mutex::new(Default::default()),
            profiles: Mutex::new(profiles),
            tools: Default::default(),
            update: Default::default(),
            media_http,
            sealer: settings_sealer,
            network: Mutex::new(network),
            multinet: Mutex::new(multinet),
            recovery: Mutex::new(Default::default()),
            queue_lock: tokio::sync::Mutex::new(()),
            game_hold_lock: tokio::sync::Mutex::new(()),
            diag_lock: tokio::sync::Mutex::new(()),
            shutting_down: AtomicBool::new(false),
            awake: Mutex::new(None),
            awake_unavailable: AtomicBool::new(false),
            tray_available: AtomicBool::new(false),
            js_broken: AtomicBool::new(false),
            secrets_sealed,
            storage,
            roots,
        });
        let mgr = Self { inner };
        // The active Download Profile's values over the user's settings, in force before anything starts.
        mgr.apply_profile();
        mgr.apply_saved_logging();
        mgr.resume_game_mode();
        mgr.spawn_ticker();
        // First evaluation happens here, not in the background loop, so "what to do at launch" is
        // decided exactly once and can never race with the first user command.
        mgr.reconcile().await;
        mgr.evaluate_queues().await;
        mgr.spawn_queue_loop();
        // The networks to spread over, read off the start (the queue loop reads them again while it is on).
        if mgr.inner.multinet.lock().unwrap_or_else(|p| p.into_inner()).on {
            mgr.hold_smart_for_networks(true);
            let m = mgr.clone();
            tokio::spawn(async move { m.refresh_networks(true).await });
        }
        mgr.spawn_update_checks();
        mgr.spawn_tool_update_checks();
        Ok(mgr)
    }

    /// The folder every download lands in.
    /// Whether cookies and credentials survive a restart (sealed with a key from the credential
    /// store). When false they are only ever held in memory.
    pub fn secrets_protected(&self) -> bool {
        self.inner.secrets_sealed
    }

    /// Stream of events for the UI.
    pub fn subscribe(&self) -> broadcast::Receiver<ManagerEvent> {
        self.inner.events.subscribe()
    }

    fn emit(&self, ev: ManagerEvent) {
        if !matches!(ev, ManagerEvent::Tick { .. }) {
            self.inner.stats.lock().unwrap().dirty = true; // counts may have changed
                                                           // Send a couple of ticks even if no download is running, so the UI refreshes its numbers.
            self.inner.idle_ticks.fetch_max(2, Ordering::Relaxed);
            if let ManagerEvent::Updated { download } = &ev {
                match download.status {
                    DownloadStatus::Completed => self.inner.disk.lock().unwrap().folder_at = None, // folder size changed
                    DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Verifying => {
                        self.inner.queue.lock().unwrap().note_activity()
                    }
                    _ => {}
                }
            }
        }
        let _ = self.inner.events.send(ev); // no subscribers is fine
    }

    // ---------------------------------------------------------------- commands

    pub async fn create_download(&self, req: NewDownload) -> Result<DownloadDto, ManagerError> {
        self.create_inner(req, Vec::new()).await
    }

    /// Create a download handed over by a browser: like [`create_download`](Self::create_download),
    /// plus the referrer / cookies / user agent the browser had. Used by the browser bridge only.
    pub async fn create_download_from_browser(&self, req: NewDownload, ctx: BrowserContext) -> Result<DownloadDto, ManagerError> {
        let headers = validate::browser_headers(&ctx)?;
        self.create_inner(req, headers).await
    }

    /// Can VeloxFetch fetch a download the browser was starting, on its own? Asked before taking it over from the
    /// browser. Some sites give the file only to the signed-in browser — a SharePoint / OneDrive / Office Online
    /// export, a members-only page — and answer anyone else with a sign-in page, 401 or 403. Taken over, such a
    /// download could only fail; declined, the extension gives it back to the browser, which downloads it with its
    /// own session. One quick check with the headers the browser sent (the same requests the download would
    /// start with). Declined only on that evidence: no answer, a slow server or any other trouble takes the download
    /// as before, and a link the browser cannot hand over is refused later like any other.
    pub async fn browser_can_take_over(&self, req: &NewDownload, ctx: &BrowserContext) -> Result<(), ManagerError> {
        let Ok(url) = validate::url(&req.url) else { return Ok(()) };
        let (url, _) = validate::split_credentials(url);
        let url = crate::links::direct(&url).unwrap_or(url);
        let headers = validate::browser_headers(ctx)?;
        let probe = tokio::time::timeout(Duration::from_secs(15), self.inner.downloader.probe_quick(url.as_str(), &headers)).await;
        let browser_only = match probe {
            Ok(Ok(info)) => {
                // The browser's name for the file (it read the server's answer with the user's sign-in) decides what
                // a file is expected to be; without one, the name the download itself would choose.
                let name = req
                    .filename
                    .as_deref()
                    .map(downloader_core::filename::sanitize_filename)
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| downloader_core::filename::choose_filename(&info, &url));
                downloader_core::filename::web_page_instead_of_file(&info, &name)
            }
            Ok(Err(DownloadError::Http(http_client::HttpError::Status { status: 401 | 403, .. }))) => true,
            _ => false,
        };
        if browser_only {
            tracing::info!(host = url.host_str().unwrap_or("?"), "this file is only given to the signed-in browser: left to the browser");
            return Err(ManagerError::invalid("this file is only given to the signed-in browser: the browser downloads it"));
        }
        Ok(())
    }

    async fn create_inner(&self, req: NewDownload, request_headers: Vec<(String, String)>) -> Result<DownloadDto, ManagerError> {
        let (url, credentials) = validate::split_credentials(validate::url(&req.url)?);
        let url = crate::links::direct(&url).unwrap_or(url);
        let mut request_headers = request_headers;
        // What the user typed in the Add dialog (referrer, cookies, user agent, sign-in) wins over what a browser sent.
        for (k, v) in validate::user_headers(&req)? {
            request_headers.retain(|(name, _)| !name.eq_ignore_ascii_case(&k));
            request_headers.push((k, v));
        }
        if let Some(auth) = credentials.filter(|_| !request_headers.iter().any(|(k, _)| k == "authorization")) {
            // Never in the link: the header travels with the record, sealed on disk, and is dropped when the download ends.
            request_headers.push(("authorization".to_string(), auth));
        }
        let dir = match req.folder_token {
            Some(token) => {
                if req.subdir.as_deref().is_some_and(|s| !s.trim().is_empty()) {
                    return Err(ManagerError::invalid("choose either a folder or a sub-folder, not both"));
                }
                let folder = self.pick_path(token)?;
                self.allow_folder(&folder).await?;
                folder
            }
            None => validate::subdir(&self.download_root(), req.subdir.as_deref())?,
        };
        let mut filename = req.filename.as_deref().map(validate::filename).transpose()?.unwrap_or_default();
        if crate::links::is_google_drive_file(&url) {
            // Always asked: a name typed (or guessed in the browser dialog) does not make a sign-in page the file.
            let real = self.google_drive_filename(&url, &request_headers).await?;
            // `download` is only the placeholder the browser dialog guessed from the address.
            if !real.is_empty() && (filename.is_empty() || filename == "download") {
                filename = real;
            }
        }
        let category = req.category.as_deref().map(validate::category).transpose()?;
        let speed_limit = validate::speed_limit(req.speed_limit)?;
        let checksum = req.checksum.as_deref().map(validate::checksum).transpose()?.map(|c| c.to_string());
        let start_at = self.check_new_placement(&req)?;

        let rec = DownloadRecord {
            id: None,
            url: url.to_string(),
            filename,
            // A never-started record keeps its target *directory* here; the engine appends the
            // file name once the server has been asked (see `DownloadRecord::is_unstarted`).
            destination: dir,
            total_size: None,
            downloaded: 0,
            status: DownloadStatus::Queued,
            etag: None,
            last_modified: None,
            resumable: false,
            checksum,
            last_error: None,
            segments: Vec::new(),
            priority: req.priority.unwrap_or(Priority::Normal),
            category,
            speed_limit,
            content_type: None,
            server: None,
            request_headers,
            part_stamp: None,
            checksum_source: None,
        };
        let id = self.save(&rec).await?;
        self.place_new(id, req.queue_id, start_at).await;
        let dto = self.dto_for(id).await?;
        self.emit(ManagerEvent::Added { download: dto.clone() });
        // With a start time it waits for it (the queue starts it then).
        if req.start.unwrap_or(true) && start_at.is_none() {
            return self.start_download(id).await;
        }
        Ok(dto)
    }

    /// The real name of a Google Drive file, asked before the download is created: Drive answers a
    /// private (or over-quota) file with a sign-in page, which must not be saved as the file. An
    /// empty name (the engine decides later) when Drive cannot be reached right now.
    async fn google_drive_filename(&self, url: &url::Url, headers: &[(String, String)]) -> Result<String, ManagerError> {
        let probe = tokio::time::timeout(Duration::from_secs(20), self.inner.downloader.probe_quick(url.as_str(), headers)).await;
        let Ok(Ok(info)) = probe else { return Ok(String::new()) };
        if let Some(name) = info.content_disposition.as_deref().and_then(http_client::filename_from_content_disposition) {
            return Ok(downloader_core::filename::sanitize_filename(&name));
        }
        let html = info.content_type.as_deref().is_some_and(|t| t.trim_start().to_ascii_lowercase().starts_with("text/html"));
        if html {
            return Err(ManagerError::invalid(
                "Google Drive sent a web page instead of the file: the file is not shared publicly, or it has been downloaded too \
                 many times today. Make it public (\"Anyone with the link\"), or open it in your browser and download it from there \
                 with \"Send cookies\" turned on in the VeloxFetch extension.",
            ));
        }
        Ok(String::new())
    }

    /// Start a queued, paused or failed download (idempotent if it is already running).
    pub async fn start_download(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        self.forget_recovery(id); // the user's try: its own tries count anew if it fails again
        self.user_took_over(id).await;
        let dto = self.start_inner(id).await?;
        self.inner.queue.lock().unwrap().note_user_start();
        Ok(dto)
    }

    /// `start_download` without the user-intent bookkeeping (used when the queue itself resumes work).
    async fn start_inner(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        let mut rec = self.load(id).await?;
        let previous = rec.status;
        // Claim the id atomically so two racing `start` calls can't spawn two tasks.
        let claimed: Result<Option<_>, ManagerError> = {
            use std::collections::hash_map::Entry;
            match self.inner.active.lock().unwrap().entry(id) {
                Entry::Occupied(_) => Ok(None),
                Entry::Vacant(_) if !matches!(rec.status, DownloadStatus::Queued | DownloadStatus::Paused | DownloadStatus::Failed) => {
                    Err(ManagerError::InvalidState { action: "start", status: rec.status })
                }
                Entry::Vacant(slot) => {
                    let control = DownloadControl::new();
                    control.set_preview(self.preview_wanted(id));
                    let progress = ProgressHandle::new();
                    let (tx, rx) = watch::channel(false);
                    slot.insert(Active { control: control.clone(), progress: progress.clone(), done: rx });
                    Ok(Some((control, progress, tx)))
                }
            }
        };
        let Some((control, progress, done_tx)) = claimed? else {
            return self.dto_for(id).await; // already running: idempotent
        };
        // Visible as "queued" until the engine gets a run slot and flips it to "downloading".
        rec.status = DownloadStatus::Queued;
        rec.last_error = None;
        if let Err(e) = self.save(&rec).await {
            self.inner.active.lock().unwrap().remove(&id);
            return Err(e);
        }
        self.policy_for_new_run(id, rec.priority);
        self.look_for_publisher_checksum(id, &rec, &control, &progress);
        let dto = self.dto_for(id).await?;
        self.emit(ManagerEvent::Updated { download: dto.clone() });
        self.note_start(id, previous).await;
        self.update_keep_awake();

        let this = self.clone();
        tokio::spawn(async move {
            // Every line the engine logs for this run starts with `download{id=N}`.
            let req = DownloadRequest::resume(id);
            let run = this.inner.downloader.download(&req, control, progress);
            let res = tracing::Instrument::instrument(run, tracing::info_span!("download", id)).await;
            this.finish(id, res.map(|_| ())).await;
            let _ = done_tx.send(true);
        });
        Ok(dto)
    }

    /// Same as [`start_download`](Self::start_download); named for the UI's "resume" button. Also
    /// starts a download that was added without starting ("Download later").
    pub async fn resume_download(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        let rec = self.load(id).await?;
        if !matches!(rec.status, DownloadStatus::Paused | DownloadStatus::Queued) && !self.is_active(id) {
            return Err(ManagerError::InvalidState { action: "resume", status: rec.status });
        }
        self.start_download(id).await
    }

    /// Pause keeps all progress and the partial file; the download can be resumed later.
    pub async fn pause_download(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        self.inner.queue.lock().unwrap().note_user_pause();
        self.user_took_over(id).await;
        self.forget_held(id).await;
        let was_recovering = self.auto_retry_of(id).is_some();
        self.forget_recovery(id);
        match self.active_handle(id) {
            Some((control, mut done)) => {
                control.pause();
                let _ = tokio::time::timeout(Duration::from_secs(15), done.wait_for(|d| *d)).await;
            }
            None => {
                let mut rec = self.load(id).await?;
                match rec.status {
                    DownloadStatus::Paused => {}
                    // Added without starting: nothing runs, so pausing just keeps the schedule from starting it.
                    // Failed and waiting to be tried again on its own: pausing stops the tries, the bytes stay.
                    DownloadStatus::Queued | DownloadStatus::Failed if rec.status == DownloadStatus::Queued || was_recovering => {
                        rec.status = DownloadStatus::Paused;
                        self.save(&rec).await?;
                        let dto = self.dto_for(id).await?;
                        self.emit(ManagerEvent::Updated { download: dto.clone() });
                        return Ok(dto);
                    }
                    _ => return Err(ManagerError::InvalidState { action: "pause", status: rec.status }),
                }
            }
        }
        self.dto_for(id).await
    }

    /// Cancel discards the partial file for good; the download cannot be resumed.
    pub async fn cancel_download(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        self.forget_recovery(id);
        match self.active_handle(id) {
            Some((control, mut done)) => {
                control.cancel();
                let _ = tokio::time::timeout(Duration::from_secs(15), done.wait_for(|d| *d)).await;
            }
            None => {
                let rec = self.load(id).await?;
                match rec.status {
                    DownloadStatus::Completed | DownloadStatus::Cancelled => {
                        return Err(ManagerError::InvalidState { action: "cancel", status: rec.status })
                    }
                    _ => {
                        self.inner.downloader.cancel(id).await?;
                        self.note_event(id, "info", "Cancelled").await;
                    }
                }
            }
        }
        let dto = self.dto_for(id).await?;
        self.emit(ManagerEvent::Updated { download: dto.clone() });
        Ok(dto)
    }

    /// Retry a failed download (continues from saved segments when the server still allows it).
    pub async fn retry_download(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        let rec = self.load(id).await?;
        if rec.status != DownloadStatus::Failed {
            return Err(ManagerError::InvalidState { action: "retry", status: rec.status });
        }
        self.start_download(id).await
    }

    /// Download a finished, cancelled or failed download again from the start, with the same link, folder and
    /// name (for a file deleted by mistake). Never overwrites: if a file with that name is there, the new copy
    /// gets a free name ("name (1).ext"). Any partial data is discarded; the record (queue, priority, limits)
    /// is kept. A download that is running, paused or waiting is refused: it already continues.
    pub async fn redownload(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        self.forget_recovery(id);
        let mut rec = self.load(id).await?;
        if self.is_active(id) || !matches!(rec.status, DownloadStatus::Completed | DownloadStatus::Cancelled | DownloadStatus::Failed) {
            return Err(ManagerError::InvalidState { action: "download again", status: rec.status });
        }
        let old_dir = if rec.is_unstarted() { Some(rec.destination.clone()) } else { rec.destination.parent().map(Path::to_path_buf) };
        if !rec.is_unstarted() && self.inner.roots.check(&rec.destination).is_ok() {
            let _ = tokio::fs::remove_file(part_path(&rec.destination)).await;
        }
        // The same folder while it is still allowed; else the download folder (the engine creates it if needed).
        rec.destination = old_dir.filter(|d| self.inner.roots.check(d).is_ok()).unwrap_or_else(|| self.download_root());
        rec.total_size = None;
        rec.downloaded = 0;
        rec.segments.clear();
        rec.etag = None;
        rec.last_modified = None;
        rec.resumable = false;
        rec.last_error = None;
        rec.status = DownloadStatus::Queued;
        self.save(&rec).await?;
        self.remember_diagnosis(id, None);
        self.note_event(id, "info", "Downloading again from the start").await;
        self.start_download(id).await
    }

    /// Remove a download from the list. Anything running is cancelled first. The partial `.part`
    /// file is always removed; the finished file is only deleted when `delete_file` is set *and* it
    /// lies inside the download folder.
    pub async fn delete_download(&self, id: i64, delete_file: bool) -> Result<(), ManagerError> {
        self.forget_recovery(id);
        if let Some((control, mut done)) = self.active_handle(id) {
            control.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(15), done.wait_for(|d| *d)).await;
        }
        let rec = self.load(id).await?;
        if !rec.is_unstarted() {
            // Only ever delete inside the download folder, whatever the stored record says. Removing
            // a link deletes the link itself, never what it points to.
            if self.inner.roots.check(&rec.destination).is_ok() {
                let _ = tokio::fs::remove_file(part_path(&rec.destination)).await;
                if delete_file && rec.status == DownloadStatus::Completed {
                    let _ = tokio::fs::remove_file(&rec.destination).await;
                }
            }
        }
        failpoint::hit("delete.after_files");
        let store = self.inner.store.clone();
        blocking(move || store.delete(id)).await?;
        self.inner.trackers.lock().unwrap().remove(&id);
        self.remember_diagnosis(id, None);
        self.forget_placement(id);
        self.emit(ManagerEvent::Removed { id });
        Ok(())
    }

    pub async fn get_download(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        self.dto_for(id).await
    }

    pub async fn list_downloads(&self, filter: ListFilter) -> Result<Vec<DownloadDto>, ManagerError> {
        let store = self.inner.store.clone();
        let records = blocking(move || store.list()).await?;
        Ok(records.iter().filter(|r| filter.status.as_ref().is_none_or(|s| s.contains(&r.status))).map(|r| self.to_dto(r)).collect())
    }

    /// Change priority. Takes effect immediately for downloads waiting for a run slot.
    pub async fn set_priority(&self, id: i64, priority: Priority) -> Result<DownloadDto, ManagerError> {
        let mut rec = self.load(id).await?;
        if matches!(rec.status, DownloadStatus::Completed | DownloadStatus::Cancelled) {
            return Err(ManagerError::InvalidState { action: "re-prioritise", status: rec.status });
        }
        rec.priority = priority;
        self.patch_saved(id, move |r| r.priority = priority).await?;
        if let Some(key) = self.active_handle_progress(id).and_then(|p| p.queue_key()) {
            self.inner.scheduler.set_priority(key, priority);
        }
        if let Some((control, _)) = self.active_handle(id) {
            control.set_priority(priority); // the engine's checkpoints must not overwrite it
        }
        let dto = self.dto_for(id).await?;
        self.emit(ManagerEvent::Updated { download: dto.clone() });
        Ok(dto)
    }

    /// Set (or clear, with `None`) a per-download, per-category or global speed limit.
    pub async fn set_speed_limit(&self, req: SetSpeedLimit) -> Result<(), ManagerError> {
        let limit = validate::speed_limit(req.bytes_per_sec)?;
        match req.target {
            SpeedTarget::Global => {
                // The user's own limit; the active profile may set another one (see `profiles.rs`).
                self.save_setting(GLOBAL_LIMIT_KEY.to_string(), limit).await?;
                self.set_base_global(limit);
            }
            SpeedTarget::Category { name } => {
                let name = validate::category(&name)?;
                self.inner.limits.set_category(&name, limit);
                self.save_setting(format!("{CATEGORY_LIMIT_PREFIX}{name}"), limit).await?;
            }
            SpeedTarget::Download { id } => {
                let rec = self.load(id).await?;
                if matches!(rec.status, DownloadStatus::Completed | DownloadStatus::Cancelled) {
                    return Err(ManagerError::InvalidState { action: "limit", status: rec.status });
                }
                self.patch_saved(id, move |r| r.speed_limit = limit).await?;
                if let Some((control, _)) = self.active_handle(id) {
                    control.set_speed_limit(limit);
                }
                let dto = self.dto_for(id).await?;
                self.emit(ManagerEvent::Updated { download: dto });
            }
        }
        Ok(())
    }

    /// Full picture of one download for the detail view: chunks, server metadata, averages.
    pub async fn get_download_detail(&self, id: i64) -> Result<DownloadDetailDto, ManagerError> {
        let rec = self.load(id).await?;
        let download = self.to_dto(&rec);
        let live = self.active_handle_progress(id).map(|p| p.snapshot()).filter(|s| !s.segments.is_empty());
        let segments: Vec<_> = live.map_or_else(|| rec.segments.clone(), |s| s.segments);
        let (avg_speed_bps, elapsed_secs) = self
            .inner
            .trackers
            .lock()
            .unwrap()
            .get(&id)
            .map(|t| {
                let el = t.started.elapsed();
                let bytes = download.downloaded.saturating_sub(t.baseline);
                ((bytes as f64 / el.as_secs_f64().max(0.5)) as u64, Some(el.as_secs()))
            })
            .unwrap_or((0, None));
        // Finished: the average over every attempt, from the saved diagnostics (the live numbers are gone).
        let (avg_speed_bps, elapsed_secs) = match (avg_speed_bps, elapsed_secs) {
            (0, None) if rec.status == DownloadStatus::Completed => match self.stored_average(id).await {
                (0, _) => (0, None),
                (avg, secs) => (avg, Some(secs)),
            },
            live => live,
        };
        let mode = match (segments.is_empty(), rec.resumable) {
            (true, _) => "pending",
            (false, true) => "parallel",
            (false, false) => "sequential",
        };
        Ok(DownloadDetailDto {
            remaining: download.total_size.map(|t| t.saturating_sub(download.downloaded)),
            retries: segments.iter().map(|g| g.retry_count).sum(),
            segments: segments
                .iter()
                .map(|g| SegmentDto {
                    index: g.id,
                    start: g.start,
                    end: g.end,
                    downloaded: g.downloaded,
                    status: g.status,
                    retries: g.retry_count,
                })
                .collect(),
            mode: mode.to_string(),
            avg_speed_bps,
            elapsed_secs,
            content_type: rec.content_type.clone(),
            server: rec.server.clone(),
            etag: rec.etag.clone(),
            last_modified: rec.last_modified.clone(),
            checksum: rec.checksum.clone(),
            download,
        })
    }

    /// Completed downloads, newest first.
    pub async fn list_history(&self, limit: u32) -> Result<Vec<HistoryDto>, ManagerError> {
        let store = self.inner.store.clone();
        let limit = limit.clamp(1, 500);
        let rows = blocking(move || store.history(limit)).await?;
        Ok(rows
            .into_iter()
            .map(|h| HistoryDto {
                kind: downloader_core::classify_file(&h.filename, None),
                exists: h.destination.is_file(),
                id: h.id,
                download_id: h.download_id,
                url: h.url,
                filename: h.filename,
                destination: h.destination.display().to_string(),
                size: h.size,
                outcome: h.outcome,
                finished_at: h.finished_at,
                verify: h.verified.as_deref().map(|v| VerifyDto::from_source("verified", None, (v != "user").then_some(v))),
            })
            .collect())
    }

    /// Current global metrics (the same numbers the ticks carry).
    pub async fn get_metrics(&self) -> Result<MetricsDto, ManagerError> {
        let (speed, in_flight) = self.live_totals();
        Ok(self.metrics(speed, in_flight, true).await)
    }

    /// Pause everything and wait for it to finish saving. Call before the application exits.
    pub async fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::Relaxed); // no queue/power decisions while exiting
        self.inner.awake.lock().unwrap().take(); // the computer may sleep again
        let running: Vec<_> = {
            let active = self.inner.active.lock().unwrap();
            active.values().map(|a| (a.control.clone(), a.done.clone())).collect()
        };
        for (control, _) in &running {
            control.pause();
        }
        for (_, mut done) in running {
            let _ = tokio::time::timeout(Duration::from_secs(15), done.wait_for(|d| *d)).await;
        }
        self.stop_media().await;
        // What the last seconds received (the downloads just paused, the videos just stopped).
        self.save_usage(true).await;
    }

    // ---------------------------------------------------------------- internals

    fn is_active(&self, id: i64) -> bool {
        self.inner.active.lock().unwrap().contains_key(&id)
    }

    fn active_handle(&self, id: i64) -> Option<(DownloadControl, watch::Receiver<bool>)> {
        self.inner.active.lock().unwrap().get(&id).map(|a| (a.control.clone(), a.done.clone()))
    }

    fn active_handle_progress(&self, id: i64) -> Option<ProgressHandle> {
        self.inner.active.lock().unwrap().get(&id).map(|a| a.progress.clone())
    }

    /// A download task ended: record failures the engine could not persist itself, notify the UI.
    async fn finish(&self, id: i64, res: Result<(), DownloadError>) {
        // Settle the network-bytes total for whatever arrived since the last tick (fast downloads
        // can finish between two ticks and would otherwise never be counted).
        let ended = {
            let mut active = self.inner.active.lock().unwrap();
            let ended = active.remove(&id);
            if let Some(a) = &ended {
                self.inner.finishing.lock().unwrap().insert(id, a.progress.clone());
            }
            ended
        };
        let tracker = self.inner.trackers.lock().unwrap().remove(&id);
        self.forget_publisher_lookup(id);
        let (last, peak) = tracker.map_or((0, 0), |t| (t.last_received, t.peak));
        if let Some(a) = &ended {
            self.count_received(a.progress.received().saturating_sub(last));
        }
        self.note_end(id, ended.as_ref().map(|a| &a.progress), peak, &res).await;
        self.update_keep_awake(); // the computer may sleep again as soon as nothing downloads
        if let Err(e) = &res {
            if !matches!(e, DownloadError::Paused | DownloadError::Cancelled) {
                // Errors before the transfer was planned (e.g. the probe failed) leave the record
                // untouched; make sure it shows as failed.
                let msg = e.to_string();
                let _ = self
                    .patch_saved(id, move |r| {
                        if r.status != DownloadStatus::Failed {
                            r.status = DownloadStatus::Failed;
                            r.last_error = Some(msg);
                        }
                    })
                    .await;
            }
        }
        // A failure the network or the server may get over: tried again on its own (`recovery.rs`).
        match &res {
            Err(e) if !matches!(e, DownloadError::Paused | DownloadError::Cancelled) => self.recover_later(id, &e.to_string()),
            _ => self.recovered(id),
        }
        if let Ok(dto) = self.dto_for(id).await {
            self.emit(ManagerEvent::Updated { download: dto });
        }
    }

    async fn load(&self, id: i64) -> Result<DownloadRecord, ManagerError> {
        let store = self.inner.store.clone();
        blocking(move || store.load(id)).await?.ok_or(ManagerError::NotFound(id))
    }

    async fn save(&self, rec: &DownloadRecord) -> Result<i64, ManagerError> {
        let (store, rec) = (self.inner.store.clone(), rec.clone());
        Ok(blocking(move || store.save(&rec)).await?)
    }

    /// Load, modify and save a record (only for fields the engine is not concurrently writing).
    async fn patch_saved(&self, id: i64, f: impl FnOnce(&mut DownloadRecord) + Send + 'static) -> Result<(), ManagerError> {
        let store = self.inner.store.clone();
        blocking(move || {
            let mut rec = store.load(id)?.ok_or_else(|| DownloadError::InvalidRequest(format!("unknown id {id}")))?;
            f(&mut rec);
            store.save(&rec).map(|_| ())
        })
        .await
        .map_err(|e| match e {
            DownloadError::InvalidRequest(_) => ManagerError::NotFound(id),
            other => other.into(),
        })
    }

    async fn save_setting(&self, key: String, value: Option<u64>) -> Result<(), ManagerError> {
        let store = self.inner.store.clone();
        blocking(move || match value {
            Some(v) => store.set_setting(&key, &v.to_string()),
            None => store.delete_setting(&key),
        })
        .await?;
        Ok(())
    }

    async fn dto_for(&self, id: i64) -> Result<DownloadDto, ManagerError> {
        let rec = self.load(id).await?;
        Ok(self.to_dto(&rec))
    }

    /// Merge the stored record with live progress (bytes, speed, connections).
    fn to_dto(&self, rec: &DownloadRecord) -> DownloadDto {
        let id = rec.id.unwrap_or_default();
        let explained = rec.last_error.as_deref().filter(|e| !e.is_empty()).map(crate::explain::explain);
        let (live, running, retrying, progress) = {
            let active = self.inner.active.lock().unwrap();
            let a = active.get(&id);
            (a.map(|a| a.progress.snapshot()), a.is_some(), a.is_some_and(|a| is_retrying(&a.progress)), a.map(|a| a.progress.clone()))
        };
        let retrying = retrying && rec.status == DownloadStatus::Downloading;
        let place = self.placement_of(id);
        let auto_retry = if rec.status == DownloadStatus::Failed { self.auto_retry_of(id) } else { None };
        let speed = self.inner.trackers.lock().unwrap().get(&id).map_or(0.0, |t| t.speed);
        let (downloaded, total, connections) = match &live {
            Some(s) if !s.segments.is_empty() => {
                (s.downloaded, s.total.or(rec.total_size), s.segments.iter().filter(|g| g.status == SegmentStatus::Downloading).count())
            }
            _ => (rec.downloaded, rec.total_size, 0),
        };
        let speed_bps = if rec.status == DownloadStatus::Downloading { speed as u64 } else { 0 };
        let eta_secs = match total {
            Some(t) if speed_bps > 0 && t > downloaded => Some((t - downloaded) / speed_bps),
            _ => None,
        };
        DownloadDto {
            id,
            url: rec.url.clone(),
            filename: display_name(rec),
            destination: rec.destination.display().to_string(),
            status: rec.status,
            total_size: total,
            downloaded,
            speed_bps,
            eta_secs,
            connections,
            priority: rec.priority,
            category: rec.category.clone(),
            speed_limit: rec.speed_limit,
            resumable: rec.resumable,
            error: explained.as_ref().map(|(text, _)| text.clone()),
            error_detail: rec.last_error.clone(),
            needs: explained.and_then(|(_, needs)| needs),
            error_code: rec.last_error.as_deref().filter(|e| !e.is_empty()).map(crate::explain::error_code),
            kind: downloader_core::classify_file(&display_name(rec), rec.content_type.as_deref()),
            idle: rec.status == DownloadStatus::Queued && !running,
            queue_id: place.queue_id,
            position: place.position,
            start_at: place.start_at,
            retrying,
            held: rec.status == DownloadStatus::Paused && self.held_by_queue(id),
            doctor: self.doctor_for(rec, progress.as_ref(), retrying).map(|mut d| {
                // Tried again on its own: the Doctor says when.
                if let Some(a) = auto_retry.as_ref() {
                    d.retry_at_ms = a.at_ms.or(d.retry_at_ms);
                }
                d
            }),
            // A link counts as present (never followed); only an absent path is "missing".
            file_missing: rec.status == DownloadStatus::Completed && std::fs::symlink_metadata(&rec.destination).is_err(),
            auto_retry,
            verify: self.verify_of(rec, running),
        }
    }

    /// What the card says about the file's integrity (feature #6).
    fn verify_of(&self, rec: &DownloadRecord, running: bool) -> Option<VerifyDto> {
        let own = || VerifyDto::from_source("ready", rec.checksum.as_deref(), None);
        match rec.status {
            DownloadStatus::Completed => {
                rec.checksum.as_ref().map(|c| VerifyDto::from_source("verified", Some(c), rec.checksum_source.as_deref()))
            }
            _ if rec.checksum.is_some() && rec.checksum_source.is_none() => Some(own()),
            _ if running => match self.publisher_lookup_of(rec.id.unwrap_or_default())? {
                downloader_core::PublisherLookup::Looking => {
                    Some(VerifyDto { state: "looking".into(), by: Some("publisher".into()), algo: None, list: None })
                }
                downloader_core::PublisherLookup::Found { checksum, list } => {
                    Some(VerifyDto::from_source("ready", Some(&checksum.to_string()), Some(&format!("publisher:{list}"))))
                }
                downloader_core::PublisherLookup::None => None,
            },
            _ => None,
        }
    }

    /// Metrics aggregator. Rather than forwarding every engine update, sample all running downloads a
    /// few times per second and publish ONE batched `Tick`: progress of every active download plus
    /// the global metrics. The UI therefore renders 5-10 times a second no matter how many network
    /// writes happen, and stays smooth.
    fn spawn_ticker(&self) {
        let weak = Arc::downgrade(&self.inner);
        let every = self.inner.cfg.progress_interval;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let mgr = DownloadManager { inner };
                mgr.tick().await;
            }
        });
    }

    /// Sum of the running downloads' speeds and the bytes they hold right now.
    fn live_totals(&self) -> (u64, u64) {
        let speed = self.inner.trackers.lock().unwrap().values().map(|t| t.speed as u64).sum();
        let in_flight = self.inner.active.lock().unwrap().values().map(|a| a.progress.snapshot().downloaded).sum();
        (speed, in_flight)
    }

    async fn tick(&self) {
        let running: Vec<(i64, ProgressHandle, Option<u64>)> = {
            let active = self.inner.active.lock().unwrap();
            active.iter().map(|(id, a)| (*id, a.progress.clone(), a.control.speed_limit())).collect()
        };
        // The tightest limit every download passes through (its own one is added below; a per-kind one is not known
        // here, so it shows through the smoothing as before).
        let shared_cap = [self.inner.limits.global(), self.inner.limits.smart()].into_iter().flatten().min();
        let mut downloads = Vec::new();
        let (mut speed_total, mut in_flight) = (0u64, 0u64);
        let mut newly_running = Vec::new();
        for (id, progress, own_limit) in running {
            if progress.download_id().is_none() {
                continue; // still waiting for a run slot / being planned
            }
            let snap = progress.snapshot();
            let now = Instant::now();
            let connections = snap.segments.iter().filter(|g| g.status == SegmentStatus::Downloading).count();
            let (speed, first) = {
                let mut trackers = self.inner.trackers.lock().unwrap();
                let t = trackers.entry(id).or_insert(Tracker {
                    last_bytes: snap.downloaded,
                    last_received: 0,
                    last_at: now,
                    speed: 0.0,
                    announced_running: false,
                    started: now,
                    baseline: snap.downloaded,
                    peak: 0,
                });
                t.peak = t.peak.max(connections);
                let dt = now.duration_since(t.last_at).as_secs_f64().max(1e-3);
                let delta = snap.downloaded.saturating_sub(t.last_bytes);
                let received = progress.received();
                self.count_received(received.saturating_sub(t.last_received));
                t.last_received = received;
                let inst = delta as f64 / dt;
                let cap = [own_limit, shared_cap].into_iter().flatten().min();
                t.speed = shown_speed(t.speed, inst, dt, cap);
                (t.last_bytes, t.last_at) = (snap.downloaded, now);
                let first = !t.announced_running;
                t.announced_running = true;
                (t.speed as u64, first)
            };
            if first {
                newly_running.push(id);
            }
            speed_total += speed;
            in_flight += snap.downloaded;
            downloads.push(ProgressDto {
                id,
                downloaded: snap.downloaded,
                total_size: snap.total,
                speed_bps: speed,
                eta_secs: match snap.total {
                    Some(t) if speed > 0 && t > snap.downloaded => Some((t - snap.downloaded) / speed),
                    _ => None,
                },
                connections,
                retrying: is_retrying(&progress),
            });
        }
        for id in newly_running {
            match self.dto_for(id).await {
                // The engine writes "downloading" a moment after the first bytes: until it has, the window would be
                // told "queued" once and never again (a running download shown as waiting). Announced at a later tick.
                Ok(dto) if dto.status == DownloadStatus::Queued => {
                    if let Some(t) = self.inner.trackers.lock().unwrap().get_mut(&id) {
                        t.announced_running = false;
                    }
                }
                Ok(dto) => self.emit(ManagerEvent::Updated { download: dto }),
                Err(_) => {}
            }
        }
        // Go quiet once nothing is running (no download, no video job), after two settling ticks (so the UI
        // shows zeros).
        if downloads.is_empty() && !self.media_busy() {
            if self.inner.idle_ticks.load(Ordering::Relaxed) == 0 {
                return;
            }
            self.inner.idle_ticks.fetch_sub(1, Ordering::Relaxed);
        } else {
            self.inner.idle_ticks.store(2, Ordering::Relaxed);
        }
        let metrics = self.metrics(speed_total, in_flight, false).await;
        self.emit(ManagerEvent::Tick { tick: TickDto { metrics, downloads } });
    }

    /// Assemble the dashboard numbers. Expensive parts are cached and refreshed lazily. `wait_for_disk`:
    /// when the disk numbers are not known yet, wait (briefly) for the first reading instead of answering
    /// without them: an idle app sends no ticks, so a UI that asked once would otherwise never get them.
    async fn metrics(&self, speed_bps: u64, in_flight: u64, wait_for_disk: bool) -> MetricsDto {
        // Status counts + lifetime bytes: refresh when something changed or every 2 s.
        let refresh = {
            let st = self.inner.stats.lock().unwrap();
            st.dirty || st.at.is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        };
        if refresh {
            let store = self.inner.store.clone();
            if let Ok((counts, total)) = blocking(move || Ok((store.status_counts()?, store.history_total_bytes()?))).await {
                let mut st = self.inner.stats.lock().unwrap();
                st.counts = counts.into_iter().collect();
                st.history_total = total;
                st.at = Some(Instant::now());
                st.dirty = false;
            }
        }
        self.save_usage(false).await;
        let usage = self.network_usage_now();
        let reading = self.refresh_disk_in_background();
        let unknown = self.inner.disk.lock().unwrap().total.is_none();
        if let (true, true, Some(reading)) = (wait_for_disk, unknown, reading) {
            let _ = tokio::time::timeout(Duration::from_secs(2), reading).await;
        }
        let (counts, history_total) = {
            let st = self.inner.stats.lock().unwrap();
            (st.counts.clone(), st.history_total)
        };
        let count = |k: &str| counts.get(k).copied().unwrap_or(0);
        // Video jobs (YouTube..., streams) count like downloads on the dashboard.
        let (media_speed, media_running, media_paused, media_held) = self.media_live();
        let root = self.inner.storage.lock().unwrap_or_else(|p| p.into_inner()).root.clone();
        self.inner.sysio.refresh(root);
        let io = self.inner.sysio.rates();
        let networks = self.network_speeds(media_speed, speed_bps + media_speed > 0 || count("downloading") > 0 || media_running > 0);
        let disk = self.inner.disk.lock().unwrap();
        MetricsDto {
            speed_bps: speed_bps + media_speed,
            active: count("downloading") + count("verifying") + media_running,
            queued: count("queued"),
            paused: count("paused") + media_paused,
            failed: count("failed"),
            completed: count("completed"),
            total_downloaded: history_total + in_flight + media_held,
            session_downloaded: self.inner.session_bytes.load(Ordering::Relaxed),
            network_total: usage.total,
            network_today: usage.today,
            network_month: usage.month,
            disk_free: disk.free,
            disk_total: disk.total,
            folder_bytes: disk.folder,
            global_limit: self.inner.limits.global(),
            net_down_bps: io.net_down,
            net_up_bps: io.net_up,
            disk_read_bps: io.disk_read,
            disk_write_bps: io.disk_write,
            io_sample: io.sample,
            networks,
        }
    }

    /// Free/total disk space (every 5 s) and download-folder size (every 20 s), computed off the
    /// async threads so a slow disk can never stall the UI updates. Returns the reading, if one started.
    fn refresh_disk_in_background(&self) -> Option<tokio::task::JoinHandle<()>> {
        let now = Instant::now();
        let (do_disk, do_folder) = {
            let mut d = self.inner.disk.lock().unwrap();
            let due = |at: Option<Instant>, every: u64| at.is_none_or(|t| now.duration_since(t) > Duration::from_secs(every));
            let (a, b) = (due(d.disk_at, 5), due(d.folder_at, 20));
            if a {
                d.disk_at = Some(now);
            }
            if b {
                d.folder_at = Some(now);
            }
            (a, b)
        };
        if !do_disk && !do_folder {
            return None;
        }
        let inner = self.inner.clone();
        Some(tokio::task::spawn_blocking(move || {
            let root = &inner.storage.lock().unwrap_or_else(|p| p.into_inner()).root.clone();
            // The download folder may not exist yet (a fresh install before the first download): measure
            // the drive it will be on, through its nearest existing parent.
            let disk = do_disk.then(|| {
                let on = nearest_existing(root);
                (on.and_then(|p| fs4::available_space(p).ok()), on.and_then(|p| fs4::total_space(p).ok()))
            });
            let folder = do_folder.then(|| dir_size(root));
            let mut d = inner.disk.lock().unwrap();
            if let Some((free, total)) = disk {
                (d.free, d.total) = (free, total);
            }
            if let Some(f) = folder {
                d.folder = f;
            }
        }))
    }
}

/// `path` itself or its closest parent that exists (`None` if not even the drive / root exists).
fn nearest_existing(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| !p.as_os_str().is_empty() && p.is_dir())
}

/// Total size of the files under `root` (symlinks are not followed; capped so a huge tree can't
/// keep a thread busy for long). `None` if it could not be read.
fn dir_size(root: &Path) -> Option<u64> {
    const MAX_ENTRIES: usize = 50_000;
    if !root.exists() {
        return Some(0); // not created yet: nothing downloaded there
    }
    let (mut total, mut seen) = (0u64, 0usize);
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return Some(total);
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            } else if ft.is_dir() {
                stack.push(entry.path());
            } else if let Ok(m) = entry.metadata() {
                total += m.len();
            }
        }
    }
    Some(total)
}

/// Running, but nothing transfers: every connection (or the first request) waits to try again after an error.
fn is_retrying(p: &ProgressHandle) -> bool {
    let snap = p.snapshot();
    if snap.segments.is_empty() {
        // Still asking the server: retrying if the last thing that happened was a retry.
        return p.download_id().is_some()
            && matches!(p.events().last(), Some(e) if matches!(e.kind, downloader_core::RunEventKind::Retrying { .. }));
    }
    !snap.segments.iter().any(|s| s.status == SegmentStatus::Downloading)
        && snap.segments.iter().any(|s| s.status == SegmentStatus::Retrying)
}

/// Name to show: the chosen file name, else what the URL suggests, else the URL itself.
fn display_name(rec: &DownloadRecord) -> String {
    if !rec.filename.is_empty() {
        return rec.filename.clone();
    }
    url::Url::parse(&rec.url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|mut s| s.rfind(|p| !p.is_empty()).map(str::to_string)))
        .unwrap_or_else(|| rec.url.clone())
}

async fn blocking<T, F>(f: F) -> Result<T, DownloadError>
where
    F: FnOnce() -> Result<T, DownloadError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| DownloadError::Internal(e.to_string()))?
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn is<T: Send + Sync>() {}
    is::<DownloadManager>();
    let _ = Path::new("");
}

#[cfg(test)]
mod speed_tests {
    use super::shown_speed;

    #[test]
    fn a_limit_shows_at_once_and_the_rest_stays_smooth() {
        let mb = 1024.0 * 1024.0;
        // Running at 6 MB/s, then held to 512 KB/s: shown at the limit on the next tick, not seconds later.
        assert_eq!(shown_speed(6.0 * mb, 512.0 * 1024.0, 0.25, Some(512 * 1024)), 512.0 * 1024.0);
        // Without a limit a sudden drop still eases (a pause between two requests does not make it jump).
        let eased = shown_speed(6.0 * mb, 0.0, 0.25, None);
        assert!(eased > 5.0 * mb && eased < 6.0 * mb, "{eased}");
        // A limit above the speed changes nothing.
        assert_eq!(shown_speed(2.0 * mb, 2.0 * mb, 0.25, Some(100 * 1024 * 1024)), 2.0 * mb);
        // The first reading is taken as it is.
        assert_eq!(shown_speed(0.0, 3.0 * mb, 0.25, None), 3.0 * mb);
    }
}
