//! Data types that cross the IPC boundary (UI <-> Rust): commands receive only these structured
//! DTOs and ids, never paths to open, URLs to hand to privileged APIs, or shell strings.
//! Requests reject unknown fields so a compromised or buggy frontend can't smuggle extras in.

use std::path::PathBuf;

use std::sync::Arc;

use downloader_core::{DownloadError, DownloadStatus, Priority, SegmentStatus};
use scheduler::chrono::NaiveDateTime;
pub use scheduler::queue_rules::{ConnectionRule, QueueRules, When as QueueWhen};
pub use scheduler::Schedule;
use serde::{Deserialize, Serialize};
pub use system_integration::{DryRunSystem, NativeSystem, PowerAction, SystemIntegration};

/// Everything the UI is allowed to specify when creating a download.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDownload {
    pub url: String,
    /// Sub-folder *inside* the download root (relative, no `..`). `None` = the root itself.
    pub subdir: Option<String>,
    /// Preferred file name (a bare name, no path). `None` = let the server / URL decide.
    pub filename: Option<String>,
    pub priority: Option<Priority>,
    pub category: Option<String>,
    /// Per-download speed limit in bytes per second.
    pub speed_limit: Option<u64>,
    /// Expected checksum, `sha256:<hex>` / `sha512:<hex>`, from a source you trust.
    pub checksum: Option<String>,
    /// Start immediately (default true). `false` leaves it queued for a later `start_download`.
    pub start: Option<bool>,
    /// A folder the user picked in the native folder dialog (see `pick_folder`): the file goes exactly there.
    /// The UI never sends a path, only this token. Cannot be combined with `subdir`.
    pub folder_token: Option<u64>,
    /// Sent as `Referer` (some servers only serve a file to visitors of their page).
    pub referrer: Option<String>,
    /// A `Cookie` header value (a signed-in session on the site).
    pub cookies: Option<String>,
    /// `User-Agent` for this download only (instead of the one in the network settings).
    pub user_agent: Option<String>,
    /// HTTP authentication. Kept with the download sealed like cookies, never in the link or the logs.
    pub username: Option<String>,
    pub password: Option<String>,
    /// The Smart Download Queue to put it in (`None`: the main queue, or a big-file queue once its size is known).
    pub queue_id: Option<i64>,
    /// Local time `YYYY-MM-DDTHH:MM` to start it at (it is added without starting, and starts on its own then).
    pub start_at: Option<String>,
}

// Hand-written so that logging a request can never print a password typed into the link.
impl std::fmt::Debug for NewDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewDownload")
            .field("url", &secrets::redact_url(&self.url))
            .field("subdir", &self.subdir)
            .field("filename", &self.filename)
            .field("priority", &self.priority)
            .field("category", &self.category)
            .field("start", &self.start)
            .field("queue_id", &self.queue_id)
            .field("start_at", &self.start_at)
            .field("folder_token", &self.folder_token)
            .field("referrer", &self.referrer.as_deref().map(secrets::redact_url))
            .field("cookies", &self.cookies.as_ref().map(|_| "***"))
            .field("user_agent", &self.user_agent)
            .field("username", &self.username.as_ref().map(|_| "***"))
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

/// What a browser knew when it handed a link over. Only the browser bridge supplies this; it is not
/// part of the UI's IPC surface.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserContext {
    /// The page the link was on (sent as `Referer`).
    pub referrer: Option<String>,
    /// A `Cookie` header value for the download URL. Never accepted from a private window.
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    #[serde(default)]
    pub incognito: bool,
}

impl std::fmt::Debug for BrowserContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserContext")
            .field("referrer", &self.referrer.as_deref().map(secrets::redact_url))
            .field("cookies", &self.cookies.as_ref().map(|_| "***"))
            .field("user_agent", &self.user_agent)
            .field("incognito", &self.incognito)
            .finish()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListFilter {
    /// Only downloads in one of these states. `None` = all.
    pub status: Option<Vec<DownloadStatus>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum SpeedTarget {
    Download { id: i64 },
    Global,
    Category { name: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetSpeedLimit {
    pub target: SpeedTarget,
    /// Bytes per second; `None` removes the limit.
    pub bytes_per_sec: Option<u64>,
}

/// A download as shown in the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadDto {
    pub id: i64,
    pub url: String,
    pub filename: String,
    pub destination: String,
    pub status: DownloadStatus,
    pub total_size: Option<u64>,
    pub downloaded: u64,
    /// Current speed in bytes/s (0 unless downloading).
    pub speed_bps: u64,
    pub eta_secs: Option<u64>,
    pub connections: usize,
    pub priority: Priority,
    pub category: Option<String>,
    pub speed_limit: Option<u64>,
    pub resumable: bool,
    /// What went wrong, in plain words (see [`crate::explain`]).
    pub error: Option<String>,
    /// The engine's own message behind `error`, for the details view.
    pub error_detail: Option<String>,
    /// What the user can do about `error`, when only they can fix it.
    pub needs: Option<crate::explain::Needs>,
    /// Short stable name of the error (`http_404`, `disk_full`...), see [`crate::explain::error_code`].
    pub error_code: Option<String>,
    /// What the file is (from its name, else its type): the icon, the type speed limits, the sorting folder.
    pub kind: downloader_core::FileKind,
    /// Queued but not running and not waiting for a slot (e.g. added with "Download later"):
    /// nothing will start it but the user (or the schedule), so the UI offers "Start".
    pub idle: bool,
    /// The Smart Download Queue it is in (1 = the main queue).
    pub queue_id: i64,
    /// Its place in the queue (lower runs first, after priority).
    pub position: i64,
    /// Local time `YYYY-MM-DDTHH:MM` at which it starts on its own.
    pub start_at: Option<String>,
    /// Running, but every connection is waiting to try again after an error.
    pub retrying: bool,
    /// Paused by its queue's rules (a time window ended, the computer is in use...): resumes on its own.
    pub held: bool,
    /// Download Doctor: why it failed (or what the engine is retrying), where the problem is, whether retrying
    /// helps, and what the user can do. `None` when nothing is wrong.
    pub doctor: Option<crate::doctor::Diagnosis>,
    /// Completed, but its file is no longer on disk (deleted or moved by hand): the UI offers "Download again".
    pub file_missing: bool,
}

/// One chunk of a download, for the segment monitor.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentDto {
    pub index: usize,
    pub start: u64,
    /// Inclusive.
    pub end: u64,
    pub downloaded: u64,
    pub status: SegmentStatus,
    pub retries: u32,
}

/// Everything the detail view shows about one download.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadDetailDto {
    pub download: DownloadDto,
    pub segments: Vec<SegmentDto>,
    /// `pending` (not planned yet), `parallel` (ranged) or `sequential` (single connection).
    pub mode: String,
    pub remaining: Option<u64>,
    /// Average speed since this run started (0 when not running).
    pub avg_speed_bps: u64,
    pub elapsed_secs: Option<u64>,
    pub retries: u32,
    pub content_type: Option<String>,
    pub server: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDto {
    pub id: i64,
    pub download_id: Option<i64>,
    pub url: String,
    pub filename: String,
    pub destination: String,
    pub size: Option<u64>,
    pub outcome: String,
    /// Unix seconds.
    pub finished_at: i64,
    pub kind: downloader_core::FileKind,
    /// The file is still where it was saved.
    pub exists: bool,
}

/// A saved file to show in its folder or open: a download of the list, or a line of the history.
/// The path always comes from the database, never from the UI.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FileTarget {
    Download {
        id: i64,
    },
    History {
        id: i64,
    },
    /// A video job (its saved file once finished, else its folder).
    Media {
        id: u64,
    },
}

/// A user name and password for a download whose server asked for one; the download is retried with them.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetCredentials {
    pub id: i64,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for SetCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetCredentials").field("id", &self.id).finish_non_exhaustive()
    }
}

/// How requests reach the internet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// The system's settings (environment variables; the Internet Options on Windows).
    #[default]
    System,
    /// Always connect directly.
    None,
    /// The proxy given in `proxy_url`.
    Manual,
}

/// Network settings as shown in Settings. The proxy password is never sent back, only whether one is saved.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkDto {
    pub proxy_mode: ProxyMode,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_has_password: bool,
    /// The user agent chosen by the user; `None` = the default one.
    pub user_agent: Option<String>,
    pub default_user_agent: String,
    /// The proxy password is sealed on disk (false: it is kept in memory only, until the app closes).
    pub secrets_protected: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetNetwork {
    pub proxy_mode: ProxyMode,
    /// `http://host:port`, `https://...`, `socks5://...` or `socks5h://...` (required for `manual`).
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    /// `None` keeps the saved password, `""` removes it.
    pub proxy_password: Option<String>,
    /// `None` or `""` = the default user agent.
    pub user_agent: Option<String>,
}

impl std::fmt::Debug for SetNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetNetwork")
            .field("proxy_mode", &self.proxy_mode)
            .field("proxy_url", &self.proxy_url.as_deref().map(secrets::redact_url))
            .field("proxy_username", &self.proxy_username.as_ref().map(|_| "***"))
            .field("proxy_password", &self.proxy_password.as_ref().map(|_| "***"))
            .field("user_agent", &self.user_agent)
            .finish()
    }
}

/// The global speed limit and the limits per kind of file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedLimitsDto {
    pub global: Option<u64>,
    /// `video`, `audio`, ... -> bytes per second (only kinds that have a limit).
    pub by_type: std::collections::BTreeMap<String, u64>,
}

/// One entry of a download's timeline (diagnostics).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagEventDto {
    /// Unix milliseconds.
    pub at: i64,
    /// `info`, `warn` or `error`.
    pub level: String,
    /// Already cleaned of secrets.
    pub text: String,
    /// A retry or a throttling notice: the first to go when the timeline is full (milestones stay).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub noise: bool,
}

/// Local diagnostics of one download, over every attempt. Nothing in it is a secret: link credentials, query
/// values, cookies, passwords and tokens are removed before it is built.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsDto {
    pub id: i64,
    pub status: DownloadStatus,
    pub error_code: Option<String>,
    /// In plain words.
    pub error: Option<String>,
    /// The engine's message, redacted.
    pub error_detail: Option<String>,
    /// Times it was started: the first start, resumes and retries.
    pub attempts: u32,
    /// Automatic retries (server checks, connections) over every attempt.
    pub retries: u32,
    /// The last HTTP status a server answered.
    pub http_status: Option<u16>,
    /// Connections open now.
    pub connections: usize,
    /// Most connections open at the same time, over every attempt.
    pub peak_connections: u32,
    /// Bytes received / time spent transferring, over every attempt.
    pub avg_speed_bps: u64,
    pub active_secs: u64,
    /// Bytes received from the network over every attempt (a restart counts again).
    pub transferred: u64,
    pub events: Vec<DiagEventDto>,
    /// Download Doctor's diagnosis of the last failure (with every piece of evidence the failure left), while it is failed.
    pub doctor: Option<crate::doctor::Diagnosis>,
    /// Plain-text summary for a bug report.
    pub report: String,
}

/// A line of the application log (already redacted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLineDto {
    pub at: i64,
    pub level: &'static str,
    pub target: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsDto {
    /// Detailed logging is on.
    pub detailed: bool,
    /// The log is also kept in files (Settings > Diagnostics > Open log folder).
    pub file_logging: bool,
    /// Oldest first.
    pub lines: Vec<LogLineDto>,
}

/// Where a waiting browser download would be saved under another file name.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingFolderQuery {
    pub token: u64,
    pub filename: String,
}

/// Global numbers for the dashboard cards.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsDto {
    /// Combined speed of all running downloads, bytes/s.
    pub speed_bps: u64,
    pub active: u64,
    pub queued: u64,
    pub paused: u64,
    pub failed: u64,
    pub completed: u64,
    /// Everything ever completed plus what is in flight right now.
    pub total_downloaded: u64,
    /// Bytes received from the network since the app started.
    pub session_downloaded: u64,
    pub disk_free: Option<u64>,
    pub disk_total: Option<u64>,
    /// Size of the download folder.
    pub folder_bytes: Option<u64>,
    /// Global speed limit in bytes/s, if one is set.
    pub global_limit: Option<u64>,
    /// The whole computer's network speed, both ways (bytes/s; `None` until measured or where unavailable).
    pub net_down_bps: Option<u64>,
    pub net_up_bps: Option<u64>,
    /// Read / write speed of the disk the download folder is on (bytes/s; `None` likewise).
    pub disk_read_bps: Option<u64>,
    pub disk_write_bps: Option<u64>,
    /// Number of the network / disk reading above (changes about once a second while someone looks).
    pub io_sample: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressDto {
    pub id: i64,
    pub downloaded: u64,
    pub total_size: Option<u64>,
    pub speed_bps: u64,
    pub eta_secs: Option<u64>,
    pub connections: usize,
    /// Every connection is waiting to try again after an error.
    pub retrying: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TickDto {
    pub metrics: MetricsDto,
    pub downloads: Vec<ProgressDto>,
}

/// Pushed to the UI as things happen.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ManagerEvent {
    Added {
        download: DownloadDto,
    },
    /// Status or settings changed.
    Updated {
        download: DownloadDto,
    },
    /// The metrics aggregator's batched update: progress of every running download plus global
    /// metrics, 5-10 times a second (not once per network write).
    Tick {
        tick: TickDto,
    },
    Removed {
        id: i64,
    },
    /// A browser download is waiting for the user's confirmation (or its details were filled in).
    Pending {
        pending: PendingDto,
    },
    /// A waiting browser download was confirmed or dismissed.
    PendingRemoved {
        token: u64,
    },
    /// Queue, schedule or power-action state changed (rare; never sent for ordinary progress).
    Queue {
        queue: QueueStateDto,
    },
    /// The Smart Download Queues changed: a rule, an opening or closing, the network, the order.
    Queues {
        queues: QueuesDto,
    },
    /// A stream (HLS / DASH) job was added, made progress or ended.
    Media {
        job: MediaJobDto,
    },
    /// A finished stream job was removed from the list.
    MediaRemoved {
        id: u64,
    },
    /// The Download Profiles changed (another one selected, one created, changed or deleted).
    Profiles {
        profiles: crate::profiles::ProfilesDto,
    },
}

/// What the UI may specify to download a stream (HLS / DASH). Ids come from an earlier analysis;
/// the UI never supplies a segment URL, a path or a command.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaRequest {
    /// The manifest link (`.m3u8` / `.mpd`).
    pub url: String,
    /// Quality id from the analysis. `None` = the best.
    pub stream: Option<String>,
    /// Audio track id, `"none"` for no separate audio, `None` = the default for the quality.
    pub audio: Option<String>,
    #[serde(default)]
    pub subtitles: Vec<String>,
    /// Sub-folder inside the download folder.
    pub subdir: Option<String>,
    /// A name without extension. `None` = taken from the link.
    pub filename: Option<String>,
    /// Join separate video and audio into one file when ffmpeg is available (default true).
    pub combine: Option<bool>,
    /// The file type: `mp4`, `mkv`, `webm` (video) or `mp3`, `m4a`, `opus` (only the sound, video pages).
    /// `None` = MP4 for a video, MP3 for "audio only". Anything else is refused.
    pub format: Option<String>,
    /// A video the browser handed over (see `PendingDto::media`): its link and the browser's headers
    /// are taken from that request, and `url` is ignored.
    pub pending: Option<u64>,
}

/// What `analyze_media` tells the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaAnalysisDto {
    #[serde(flatten)]
    pub analysis: media_engine::Analysis,
    /// ffmpeg was found, so separate video and audio can be joined.
    pub combine_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MediaStatus {
    Preparing,
    Downloading,
    Combining,
    Completed,
    Failed,
    Cancelled,
    /// Stopped by the user; Resume continues from what is already downloaded.
    Paused,
}

impl MediaStatus {
    pub fn is_active(self) -> bool {
        matches!(self, MediaStatus::Preparing | MediaStatus::Downloading | MediaStatus::Combining)
    }
}

/// A stream download as shown in the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaJobDto {
    pub id: u64,
    pub title: String,
    pub host: String,
    pub status: MediaStatus,
    pub bytes: u64,
    pub segments_done: u64,
    pub segments_total: u64,
    /// 0-100 by pieces done, once the total is known.
    pub percent: Option<f64>,
    /// The folder the files are saved in.
    pub folder: String,
    /// File names (not paths) of what was saved; the main file first.
    pub files: Vec<String>,
    pub notes: Vec<String>,
    pub error: Option<String>,
    /// A video page read through yt-dlp (no pieces to count).
    pub site: bool,
    /// Current speed, bytes/s (0 when not downloading).
    pub speed_bps: u64,
    /// Seconds left, estimated from the speed and the share done.
    pub eta_secs: Option<u64>,
    /// Paused or failed with its pieces kept: Resume / Retry continues from them.
    pub resumable: bool,
    /// The video can be watched now (`velox-preview://localhost/m<id>`): the part downloaded so far, or the file.
    pub playable: bool,
    // ---- for the details page (like a file download's) ----
    /// The video page or stream link.
    pub url: String,
    /// The quality chosen (an id from the analysis); `None` = the best.
    pub quality: Option<String>,
    /// The file type asked for (`mp4`, `mp3`...); `None` = the default (MP4, or MP3 for only the sound).
    pub format: Option<String>,
    /// Only the sound was asked for.
    pub audio_only: bool,
    /// Subtitle tracks asked for.
    pub subtitles: usize,
    /// When it was added (ms since 1970).
    pub started_at_ms: u64,
    /// When it last stopped (finished, failed, cancelled or paused); `None` while it runs.
    pub ended_at_ms: Option<u64>,
    /// Time spent running before the current run (ms): with `running_since_ms`, the time it took and the average.
    pub active_ms: u64,
    /// The current run started at (ms since 1970); `None` when not running.
    pub running_since_ms: Option<u64>,
    /// Size of the saved file(s) on disk, once completed.
    pub file_size: Option<u64>,
    /// How many times the app tried again on its own after the site or the network failed part-way.
    pub retries: u32,
}

/// Smart throttle: slow downloads down while a game or a call runs (see `manager/smart_throttle.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartThrottleDto {
    pub enabled: bool,
    /// Program names (without `.exe`), compared without case.
    pub apps: Vec<String>,
    /// While one runs, downloads get this share of the best speed seen (5-90 %).
    pub share_percent: u8,
    /// The listed program that is running right now (the cap is on).
    pub active_app: Option<String>,
    pub limit_bps: Option<u64>,
    /// `false`: the running programs could not be listed, so nothing is slowed down.
    pub scan_ok: bool,
    /// Automatic mode: downloads give way while the user uses the internet, and speed up when it is free.
    pub auto: bool,
    /// The least downloads keep, whatever happens (bytes/s): they never stop.
    pub min_bps: u64,
    /// What it is doing now: `off`, `fullIdle`, `fullFree`, `browsing`, `busy`, `app`, `easing`, `unknown`.
    pub reason: String,
    /// The computer's other download traffic (smoothed by nothing: the last reading), when known.
    pub other_bps: Option<u64>,
    /// The user is away from the computer (no input for a while), when the system says.
    pub user_idle: Option<bool>,
    /// The best download speed seen on this connection.
    pub peak_bps: Option<u64>,
    /// The speed the caps are a share of: what downloads got lately while nothing held them back, at most the
    /// best speed seen (the gauge shows the cap against it).
    pub reference_bps: Option<u64>,
    /// What downloads do while a listed program (a game, a call) runs.
    pub game_policy: GamePolicy,
    /// The speed limit of [`GamePolicy::Low`] (bytes/s).
    pub game_limit_bps: u64,
    /// [`GamePolicy::Pause`] holds the queue right now (running downloads paused, none started).
    pub game_hold: bool,
    /// The active Download Profile sets the automatic mode (`true` also turns Smart throttle on); `None`: it leaves
    /// Smart throttle as the user set it (`enabled` / `auto` are always the user's own switches).
    pub profile_adaptive: Option<bool>,
}

/// What downloads do while a listed program (a game, a call) runs. Nothing the user set is changed: the policy is
/// laid over the user's settings while the program runs and lifted when it closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GamePolicy {
    /// The chosen share of the speed downloads had (Smart throttle's mode); the default, as before.
    #[default]
    Adaptive,
    /// A fixed speed limit (`game_limit_bps`).
    Low,
    /// Running downloads pause and no new one starts; they resume when the program closes.
    Pause,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetSmartThrottle {
    pub enabled: Option<bool>,
    pub auto: Option<bool>,
    /// The reserved minimum (bytes/s), 32 KB/s to 1 GB/s.
    pub min_bps: Option<u64>,
    pub apps: Option<Vec<String>>,
    pub share_percent: Option<u8>,
    pub game_policy: Option<GamePolicy>,
    /// The speed limit of [`GamePolicy::Low`] (bytes/s), 32 KB/s to 1 GB/s.
    pub game_limit_bps: Option<u64>,
}

/// How the app behaves on the desktop (see `manager/desktop.rs`): tray, start at login, notifications, and the
/// language of the few texts Rust shows itself (tray menu, notifications).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPrefsDto {
    /// Closing the window keeps VeloxFetch running in the tray (when there is a tray).
    pub close_to_tray: bool,
    /// Started (in the tray) when the user logs in. Read from the system, so it is never shown on when it is not.
    pub start_at_login: bool,
    /// A system notification when a download finishes or fails while the window is not in front.
    pub notify_finished: bool,
    pub language: String,
    /// This desktop shows tray icons (some Linux desktops do not).
    pub tray_available: bool,
    pub start_at_login_available: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetDesktopPrefs {
    pub close_to_tray: Option<bool>,
    pub start_at_login: Option<bool>,
    pub notify_finished: Option<bool>,
    /// A language code like `en`, `fr`, `ar`, `pt-br`.
    pub language: Option<String>,
}

/// What the live preview can play so far (see `manager/preview.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDto {
    pub id: i64,
    pub content_type: String,
    pub total: u64,
    /// Bytes from the start that are on disk (what a player can show without waiting).
    pub ready_from_start: u64,
    /// The start and the end of the file are fetched first.
    pub preview_mode: bool,
    pub complete: bool,
}

/// What kind of video a browser handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PendingMedia {
    /// An HLS / DASH stream description.
    Stream,
    /// A video page (YouTube...), read through yt-dlp.
    Site,
}

/// A download a browser handed over that waits for the user's decision (nothing is downloaded and
/// nothing is stored on disk until it is confirmed).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDto {
    pub token: u64,
    pub url: String,
    pub host: String,
    pub filename: String,
    /// Bytes, once the server answered (`None` = unknown or still asking).
    pub size: Option<u64>,
    pub content_type: Option<String>,
    /// The server supports pause/resume and multiple connections.
    pub resumable: Option<bool>,
    /// The folder the file will be saved in.
    pub save_dir: String,
    /// Still asking the server for size and type.
    pub probing: bool,
    pub incognito: bool,
    pub created_at_ms: u64,
    /// Set for a video from the browser's "Download video" bar: the UI opens the video dialog
    /// (quality choice) instead of the file confirmation.
    pub media: Option<PendingMedia>,
    /// Download Intelligence: what the check before the download found (`None` while it runs, and for videos).
    pub analysis: Option<crate::intelligence::AnalysisDto>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmPending {
    pub token: u64,
    /// The user may rename the file in the dialog.
    pub filename: Option<String>,
    /// `false` = add to the list without starting (default `true`).
    pub start: Option<bool>,
    /// Save somewhere else this once: a folder the user picked in the native dialog (see `pick_folder`).
    pub folder_token: Option<u64>,
}

/// A folder the user picked in the native dialog and the app accepted. The token stands for it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedFolder {
    pub token: u64,
    /// For display only.
    pub path: String,
}

/// One kind of file and the folder it is sorted into.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderNameDto {
    pub kind: downloader_core::FileKind,
    pub folder: String,
    pub default_folder: String,
}

/// Where downloads go and how they are sorted.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageDto {
    /// The folder new downloads go to.
    pub root: String,
    /// The folder the app started with (what "reset" returns to).
    pub default_root: String,
    pub is_default: bool,
    /// New files are sorted into a sub-folder by type.
    pub organize: bool,
    pub folders: Vec<FolderNameDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetFolderNames {
    /// Kind (`video`, `audio`, `image`, `document`, `archive`, `program`, `other`) to folder name; empty = default.
    pub names: std::collections::BTreeMap<String, String>,
}

/// Limits the UI may change at runtime. Absent fields are left as they are.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetQueueConfig {
    /// Downloads running at once (1..=32); the rest wait in priority order.
    pub max_active: Option<u32>,
    /// Parallel connections per download (1..=64). Applies to downloads started or resumed afterwards.
    pub max_connections: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PauseQueue {
    /// Also pause the downloads that are running now (they are resumed by `resume_queue`).
    /// `false` lets them finish and only stops new ones from starting.
    pub pause_running: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetPowerAction {
    /// `None` disarms. A power action is one-shot and only ever set by an explicit request.
    pub action: Option<PowerAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerPhase {
    /// Nothing armed (the default).
    Off,
    /// Armed: will count down once every download has finished.
    Armed,
    /// Countdown running; `set_power_action(None)` cancels it.
    Countdown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerStateDto {
    pub action: Option<PowerAction>,
    pub phase: PowerPhase,
    /// Unix time in milliseconds at which the action runs (countdown only).
    pub countdown_ends_at_ms: Option<u64>,
    pub grace_secs: u64,
    /// What this operating system can do.
    pub supported: Vec<PowerAction>,
    pub last_error: Option<String>,
    /// The user wants the computer kept awake while downloads run (on by default).
    pub keep_awake: bool,
    /// It is being kept awake right now.
    pub kept_awake: bool,
    /// This system can do it (false once it failed here).
    pub keep_awake_available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStateDto {
    pub max_active: usize,
    pub max_connections: usize,
    /// The user paused the queue.
    pub paused: bool,
    /// New downloads may start right now (not paused and inside the schedule window).
    pub gate_open: bool,
    /// Downloads holding a run slot / waiting for one.
    pub running: usize,
    pub waiting: usize,
    /// Downloads paused by the queue or schedule that will resume when it reopens.
    pub held: usize,
    /// The queue is held because a listed program runs (Smart throttle's "pause" policy).
    pub game_hold: bool,
    pub schedule: Schedule,
    pub schedule_open: bool,
    /// Local time of the next window boundary, `YYYY-MM-DDTHH:MM`.
    pub next_change: Option<String>,
    pub power: PowerStateDto,
}

/// Why a queue starts nothing right now (`open`: it does).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueGate {
    Open,
    /// The user paused it.
    Paused,
    /// Outside its time window or before its start time (`opens_at` says when it opens).
    Time,
    /// No network.
    Offline,
    /// Not the connection its rules want.
    Connection,
    /// Waiting for the computer to be idle.
    NotIdle,
    /// Other traffic is above its limit.
    Busy,
}

/// An open stretch of time on the queue timeline (local `YYYY-MM-DDTHH:MM`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSpanDto {
    pub from: String,
    pub to: String,
}

/// One Smart Download Queue.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueDto {
    pub id: i64,
    /// Empty for the main queue (the UI names it in the user's language).
    pub name: String,
    pub rules: QueueRules,
    pub paused: bool,
    pub gate: QueueGate,
    /// When a queue closed by time opens next (local `YYYY-MM-DDTHH:MM`).
    pub opens_at: Option<String>,
    /// "Start queue" was pressed: it runs whatever its rules say until it is empty.
    pub forced: bool,
    /// When it may download in the next 24 hours (only for time rules).
    pub spans: Vec<QueueSpanDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuesDto {
    /// In the order the user gave (the first one goes first among equal priorities).
    pub queues: Vec<QueueDto>,
    /// Smart Throttle is slowing downloads right now: Low-priority downloads wait until the line is free.
    pub giving_way: bool,
    /// The computer says how long it has been idle (otherwise "only when idle" is not applied).
    pub idle_known: bool,
    pub online: Option<bool>,
    pub connection: Option<system_integration::ConnectionKind>,
    /// Local time these were worked out at (`YYYY-MM-DDTHH:MM`), the start of the timeline.
    pub now: String,
}

/// Create (`id: None`) or change a queue.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveQueue {
    pub id: Option<i64>,
    pub name: String,
    pub rules: QueueRules,
}

/// Something done to a whole queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum QueueAction {
    /// Start everything in it now, whatever its rules say, until it is empty.
    Start,
    /// Start nothing new, and pause what runs (resumed by Start).
    Pause,
    /// Remove its finished downloads from the list (the files stay).
    ClearCompleted,
    /// Earlier / later in the order of queues.
    MoveUp,
    MoveDown,
    /// Give every unfinished download in it this priority.
    Priority {
        priority: Priority,
    },
}

/// When one download starts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum StartWhen {
    /// Now, whatever its queue's rules say.
    Now,
    /// Not until the user (or its queue) starts it.
    Later,
    /// At this local time, `YYYY-MM-DDTHH:MM`.
    At { at: String },
}

/// One step in the queue order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueStep {
    Up,
    Down,
    Top,
}

/// Move a download to another queue, change when it starts, or move it up or down.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlaceDownload {
    pub id: i64,
    pub queue_id: Option<i64>,
    pub start: Option<StartWhen>,
    pub step: Option<QueueStep>,
}

/// Source of "now" (local time). Replaceable so scheduling is testable.
#[derive(Clone)]
pub struct Clock(pub Arc<dyn Fn() -> NaiveDateTime + Send + Sync>);

impl Clock {
    pub fn system() -> Self {
        Self(Arc::new(|| scheduler::chrono::Local::now().naive_local()))
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clock")
    }
}

/// The operating system: power actions, files, keeping awake, notifications (see
/// `system_integration::system`). Replaceable so tests never touch the machine they run on.
#[derive(Clone)]
pub struct SystemHandle(pub Arc<dyn SystemIntegration>);

impl SystemHandle {
    /// The machine this program runs on. Only the app passes it: everything else gets the dry run.
    pub fn native() -> Self {
        Self(Arc::new(NativeSystem::new()))
    }
}

impl std::fmt::Debug for SystemHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SystemHandle")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("download {0} not found")]
    NotFound(i64),
    #[error("cannot {action} a download that is {status:?}")]
    InvalidState { action: &'static str, status: DownloadStatus },
    #[error(transparent)]
    Engine(#[from] DownloadError),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ManagerError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }
}

/// Error shape sent to the UI: a stable machine-readable code plus a message.
/// Deliberately terse: no internal paths, no stack traces.
#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

impl From<ManagerError> for ApiError {
    fn from(e: ManagerError) -> Self {
        let code = match &e {
            ManagerError::InvalidInput(_) => "invalid_input",
            ManagerError::NotFound(_) => "not_found",
            ManagerError::InvalidState { .. } => "invalid_state",
            ManagerError::Engine(_) => "engine",
            ManagerError::Internal(_) => "internal",
        };
        Self { code, message: e.to_string() }
    }
}

/// Base addresses of the video tools' official downloads. `None` = never fetch that tool.
#[derive(Debug, Clone, Default)]
pub struct ToolSources {
    /// yt-dlp's release folder: `<base>/<file>` and `<base>/SHA2-256SUMS`.
    pub ytdlp: Option<String>,
    /// Deno's download server: `<base>/release-latest.txt`, `<base>/release/<v>/<zip>` and `.sha256sum`.
    pub deno: Option<String>,
    /// yt-dlp's ffmpeg builds (Windows): `<base>/ffmpeg-master-latest-<win64|winarm64>-gpl.zip` and
    /// `<base>/checksums.sha256`.
    pub ffmpeg: Option<String>,
    /// Tried when `ffmpeg` fails (Windows x64): `<base>/ffmpeg-release-essentials.zip` and `.sha256`.
    pub ffmpeg_fallback: Option<String>,
    /// macOS and Linux builds: `<base>/<macos|linux>/<amd64|arm64>/<release|snapshot>/ffmpeg.zip`, which
    /// redirects to the current build; its checksum is `<that address>.sha256`.
    pub ffmpeg_unix: Option<String>,
}

/// One video tool (see `ToolSources`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub installed: bool,
    pub fetching: bool,
    /// VeloxFetch can fetch it on this computer.
    pub can_fetch: bool,
    /// Why the last attempt failed.
    pub error: Option<String>,
    /// While fetching: bytes received so far and the file's size (0 = not known yet).
    pub received: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoToolsDto {
    pub ytdlp: ToolStatus,
    pub deno: ToolStatus,
    pub ffmpeg: ToolStatus,
}

#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// SQLite state file.
    pub db_path: PathBuf,
    /// Every download lands inside this directory (the UI can only choose sub-folders of it).
    pub download_root: PathBuf,
    /// Downloads running at once; the rest wait in priority order.
    pub max_concurrent_downloads: usize,
    pub download: downloader_core::DownloadConfig,
    pub client: downloader_core::ClientConfig,
    /// How often the aggregated `Tick` is emitted (default 200 ms = 5 per second).
    pub progress_interval: std::time::Duration,
    /// How often the queue re-evaluates the schedule and the power trigger.
    pub queue_poll: std::time::Duration,
    /// Visible, cancellable countdown before a power action runs.
    pub power_grace: std::time::Duration,
    pub clock: Clock,
    /// The operating system. A dry run unless the app passes [`SystemHandle::native`].
    pub system: SystemHandle,
    /// Use this ffmpeg instead of looking for one (tests only; the desktop app honours it in debug
    /// builds only).
    pub ffmpeg_path: Option<PathBuf>,
    /// Use this yt-dlp instead of looking for one (tests only, like `ffmpeg_path`).
    pub ytdlp_path: Option<PathBuf>,
    /// Where VeloxFetch may fetch yt-dlp, Deno and ffmpeg from when a video page needs them (see
    /// `manager/tools.rs`). Nothing by default: the desktop app passes `ToolSources::official()`.
    pub tools: ToolSources,
    /// Where fetched tools are kept (`None`: `tools` next to the database).
    pub tools_dir: Option<PathBuf>,
    /// Tools the installer put next to the app (read-only for the app: used when the user has no copy of
    /// their own; a refresh goes to `tools_dir`).
    pub shared_tools_dir: Option<PathBuf>,
    /// How long yt-dlp may read a page with Deno before the page is read again without it (default 45 s).
    pub site_js_timeout: std::time::Duration,
    /// Where the key that seals cookies and credentials on disk is kept. The desktop app passes the
    /// operating system's credential store; the default keeps it in memory only (nothing on disk
    /// can then be opened after a restart, which is what tests want).
    pub key_store: Arc<dyn secrets::KeyStore>,
    /// Working folders of stream downloads that a crashed process left behind are removed at start-up
    /// once they have been untouched this long (a running job keeps its folder busy).
    pub media_scratch_max_age: std::time::Duration,
    /// Sort new downloads into Video / Audio / Documents / ... folders unless the user decided otherwise
    /// (the setting stored in the database wins once the user has chosen).
    pub organize_default: bool,
    /// The application log (lines for Settings > Diagnostics, detailed logging switch, the log folder).
    /// `None`: no log (tests, the command-line tool) and the diagnostics commands say so.
    pub log: Option<diagnostics::LogHandle>,
    /// The program to start at login (the app itself; an AppImage file). `None`: starting at login is not offered.
    pub app_launcher: Option<PathBuf>,
}

impl ManagerConfig {
    pub fn new(db_path: impl Into<PathBuf>, download_root: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            download_root: download_root.into(),
            max_concurrent_downloads: 3,
            download: Default::default(),
            client: Default::default(),
            progress_interval: std::time::Duration::from_millis(200),
            queue_poll: std::time::Duration::from_secs(1),
            power_grace: std::time::Duration::from_secs(60),
            clock: Clock::system(),
            system: SystemHandle(Arc::new(DryRunSystem::new())),
            ffmpeg_path: None,
            ytdlp_path: None,
            tools: ToolSources::default(),
            tools_dir: None,
            shared_tools_dir: None,
            site_js_timeout: std::time::Duration::from_secs(45),
            key_store: Arc::new(secrets::MemoryKeyStore::default()),
            media_scratch_max_age: std::time::Duration::from_secs(15 * 60),
            organize_default: false,
            log: None,
            app_launcher: None,
        }
    }
}
