//! Diagnostics: what happened to each download over every attempt (the numbers support asks for first and a
//! short timeline), saved with the download; the application log and the report of Settings > Diagnostics.
//!
//! Everything stays on this computer. Nothing secret goes in: every text passes through
//! [`diagnostics::Redactor`] before it is stored, logged or shown, so links lose their user name, password,
//! query values and fragment, and `Authorization` / `Cookie` values, passwords and tokens are replaced.

use ::diagnostics::{human_bytes, utc_timestamp, Redactor};
use downloader_core::RunEventKind;
use serde::{Deserialize, Serialize};

use super::*;

/// Timeline entries kept per download (the newest).
const MAX_EVENTS: usize = 60;
const DETAILED_KEY: &str = "diagnostics.detailed";

/// Saved per download (table `diagnostics`, JSON), added to after every run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct DiagRecord {
    attempts: u32,
    retries: u32,
    http_status: Option<u16>,
    peak_connections: u32,
    active_ms: u64,
    bytes: u64,
    error_code: Option<String>,
    events: Vec<DiagEventDto>,
}

impl DiagRecord {
    fn push(&mut self, level: &str, text: String, at: i64) {
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(DiagEventDto { at, level: level.to_string(), text });
    }

    fn avg_speed(&self) -> u64 {
        if self.active_ms == 0 {
            0
        } else {
            (self.bytes as u128 * 1000 / self.active_ms as u128).min(u64::MAX as u128) as u64
        }
    }

    /// Add what a run (running or just ended) did.
    fn add_run(&mut self, p: &ProgressHandle, tracker_peak: usize, redactor: &Redactor) {
        self.retries = self.retries.saturating_add(p.retries());
        if let Some(s) = p.http_status() {
            self.http_status = Some(s);
        }
        self.peak_connections = self.peak_connections.max(p.peak_connections().max(tracker_peak) as u32);
        if let Some(t) = p.transfer_started() {
            self.active_ms = self.active_ms.saturating_add(t.elapsed().as_millis() as u64);
        }
        self.bytes = self.bytes.saturating_add(p.received());
        for ev in p.events() {
            let at = ev.at.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
            let (level, text) = describe(&ev.kind);
            self.push(level, redactor.redact(&text).into_owned(), at);
        }
    }
}

fn seconds(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1.0 {
        format!("{} ms", d.as_millis())
    } else {
        format!("{s:.0} s")
    }
}

/// The timeline's words for something the engine reported.
fn describe(kind: &RunEventKind) -> (&'static str, String) {
    match kind {
        RunEventKind::ServerAnswered { status, size, ranges } => (
            "info",
            format!(
                "Server answered {status}: {}, {}",
                size.map_or_else(|| "size unknown".to_string(), human_bytes),
                if *ranges { "parallel download possible" } else { "one connection only (no verified ranges)" }
            ),
        ),
        RunEventKind::Retrying { part, attempt, error, delay } => (
            "warn",
            format!(
                "{} failed (try {attempt}): {error}. Trying again in {}",
                part.map_or_else(|| "Request".to_string(), |p| format!("Part {}", p + 1)),
                seconds(*delay)
            ),
        ),
        RunEventKind::Throttled { connections } => ("warn", format!("The server is limiting connections: using {connections} from now on")),
        RunEventKind::Resuming { from } => ("info", format!("Continuing from {}", human_bytes(*from))),
        RunEventKind::StartingOver { reason } => ("warn", format!("Cannot continue the saved part ({reason}): starting over")),
        RunEventKind::Planned { parallel: true, parts } => ("info", format!("New transfer in {parts} parts over several connections")),
        RunEventKind::Planned { parallel: false, .. } => ("info", "New transfer over one connection".to_string()),
        RunEventKind::Renamed { to } => ("info", format!("The name was taken meanwhile: saved as {to}")),
        RunEventKind::WaitingForServer => {
            ("info", "The server is preparing the file (it answers when it is ready): waiting for it".to_string())
        }
    }
}

fn host(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| "?".into())
}

fn status_word(s: DownloadStatus) -> String {
    format!("{s:?}").to_lowercase()
}

impl DownloadManager {
    /// Hides secrets (and the home folder) in anything diagnostics store or show.
    pub(super) fn redactor(&self) -> Redactor {
        match &self.inner.cfg.log {
            Some(log) => log.redactor().clone(),
            None => Redactor::with_home(::diagnostics::home_dir().as_deref()),
        }
    }

    async fn load_diag(&self, id: i64) -> DiagRecord {
        let store = self.inner.store.clone();
        let raw = blocking(move || store.diagnostics(id)).await.ok().flatten();
        raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    async fn save_diag(&self, id: i64, d: &DiagRecord) {
        let (store, json) = (self.inner.store.clone(), serde_json::to_string(d).unwrap_or_default());
        if let Err(e) = blocking(move || store.save_diagnostics(id, &json)).await {
            tracing::warn!(id, error = %e, "could not save the diagnostics of a download");
        }
    }

    /// Read, change and save a download's diagnostics, one change at a time.
    async fn update_diag(&self, id: i64, f: impl FnOnce(&mut DiagRecord)) -> DiagRecord {
        let _one_at_a_time = self.inner.diag_lock.lock().await;
        let mut d = self.load_diag(id).await;
        f(&mut d);
        self.save_diag(id, &d).await;
        d
    }

    /// Average speed and time spent transferring over every finished attempt (`(0, 0)` if none), for the
    /// detail view once the live numbers of a run are gone.
    pub(super) async fn stored_average(&self, id: i64) -> (u64, u64) {
        let d = self.load_diag(id).await;
        (d.avg_speed(), d.active_ms / 1000)
    }

    /// A run of `id` starts (first start, resume, retry, the queue starting it again).
    pub(super) async fn note_start(&self, id: i64, previous: DownloadStatus) {
        let d = self
            .update_diag(id, |d| {
                d.attempts = d.attempts.saturating_add(1);
                d.error_code = None;
                let what = match previous {
                    DownloadStatus::Paused => "Resumed",
                    DownloadStatus::Failed => "Retried",
                    _ if d.attempts == 1 => "Started",
                    _ => "Started again",
                };
                d.push("info", format!("{what} (attempt {})", d.attempts), ::diagnostics::now_ms());
            })
            .await;
        tracing::info!(id, attempt = d.attempts, "download started");
    }

    /// A run of `id` ended: add its numbers and timeline, and write the one log line that sums it up
    /// (id, status, error code, retries, HTTP status, connections, average speed).
    pub(super) async fn note_end(&self, id: i64, progress: Option<&ProgressHandle>, tracker_peak: usize, res: &Result<(), DownloadError>) {
        let redactor = self.redactor();
        let (mut run_bytes, mut run_ms) = (0u64, 0u64);
        let (status, d) = {
            let mut status = "completed";
            let d = self
                .update_diag(id, |d| {
                    if let Some(p) = progress {
                        run_bytes = p.received();
                        run_ms = p.transfer_started().map_or(0, |t| t.elapsed().as_millis() as u64);
                        d.add_run(p, tracker_peak, &redactor);
                    }
                    let now = ::diagnostics::now_ms();
                    match res {
                        Ok(()) => {
                            d.error_code = None;
                            let speed =
                                run_bytes.saturating_mul(1000).checked_div(run_ms).map_or_else(|| "-".into(), |v| human_bytes(v) + "/s");
                            d.push("info", format!("Finished: {} received at {speed}", human_bytes(run_bytes)), now);
                        }
                        Err(DownloadError::Paused) => {
                            status = "paused";
                            d.push("info", "Paused (progress kept)".into(), now);
                        }
                        Err(DownloadError::Cancelled) => {
                            status = "cancelled";
                            d.push("info", "Cancelled".into(), now);
                        }
                        Err(e) => {
                            status = "failed";
                            if let Some(s) = e.http_status() {
                                d.http_status = Some(s);
                            }
                            let raw = e.to_string();
                            let code = crate::explain::error_code(&raw);
                            d.push("error", format!("Failed ({code}): {}", redactor.redact(&raw)), now);
                            d.error_code = Some(code);
                        }
                    }
                })
                .await;
            (status, d)
        };
        let run_speed = run_bytes.saturating_mul(1000).checked_div(run_ms).unwrap_or(0);
        let http = d.http_status.map_or_else(|| "-".to_string(), |s| s.to_string());
        let code = d.error_code.clone().unwrap_or_else(|| "-".into());
        if status == "failed" {
            tracing::warn!(
                id,
                status,
                error_code = %code,
                retries = d.retries,
                http_status = %http,
                connections = d.peak_connections,
                avg_speed_bps = run_speed,
                "download ended"
            );
        } else {
            tracing::info!(
                id,
                status,
                error_code = %code,
                retries = d.retries,
                http_status = %http,
                connections = d.peak_connections,
                avg_speed_bps = run_speed,
                "download ended"
            );
        }
    }

    /// Something that happened outside a run (cancelled while paused...).
    pub(super) async fn note_event(&self, id: i64, level: &str, text: &str) {
        let text = self.redactor().redact(text).into_owned();
        self.update_diag(id, |d| d.push(level, text, ::diagnostics::now_ms())).await;
    }

    /// Diagnostics of one download: the saved attempts plus the run in progress, and a text report.
    pub async fn get_download_diagnostics(&self, id: i64) -> Result<DiagnosticsDto, ManagerError> {
        let rec = self.load(id).await?;
        let dto = self.to_dto(&rec);
        let redactor = self.redactor();
        let mut d = self.load_diag(id).await;
        let live = self.active_handle_progress(id);
        if let Some(p) = &live {
            let peak = self.inner.trackers.lock().unwrap().get(&id).map_or(0, |t| t.peak);
            d.add_run(p, peak, &redactor);
        }
        let error_detail = rec.last_error.as_deref().filter(|e| !e.is_empty()).map(|e| redactor.redact(e).into_owned());
        let mut out = DiagnosticsDto {
            id,
            status: rec.status,
            error_code: dto.error_code.clone().or_else(|| d.error_code.clone()),
            error: dto.error.clone(),
            error_detail,
            attempts: d.attempts,
            retries: d.retries,
            http_status: d.http_status,
            connections: dto.connections,
            peak_connections: d.peak_connections.max(dto.connections as u32),
            avg_speed_bps: d.avg_speed(),
            active_secs: d.active_ms / 1000,
            transferred: d.bytes,
            events: d.events,
            report: String::new(),
        };
        out.report = self.download_report(&rec, &dto, &out);
        Ok(out)
    }

    fn download_report(&self, rec: &DownloadRecord, dto: &DownloadDto, d: &DiagnosticsDto) -> String {
        use std::fmt::Write as _;
        let mut r = String::new();
        let _ = writeln!(
            r,
            "VeloxFetch {} - download diagnostics - {}",
            env!("CARGO_PKG_VERSION"),
            utc_timestamp(::diagnostics::now_ms(), false)
        );
        let _ = writeln!(r, "Download #{}: {} ({:?})", d.id, dto.filename, dto.kind);
        let _ = writeln!(r, "Link: {}", rec.url);
        let _ = writeln!(r, "Server: {} - {}", rec.server.as_deref().unwrap_or("?"), rec.content_type.as_deref().unwrap_or("type unknown"));
        let _ = writeln!(r, "Status: {}", status_word(d.status));
        if let Some(code) = &d.error_code {
            let _ = writeln!(r, "Error code: {code}");
        }
        if let Some(e) = &d.error {
            let _ = writeln!(r, "Error: {e}");
        }
        if let Some(e) = &d.error_detail {
            let _ = writeln!(r, "Engine message: {e}");
        }
        let _ = writeln!(r, "HTTP status: {}", d.http_status.map_or_else(|| "-".into(), |s| s.to_string()));
        let _ = writeln!(r, "Attempts: {} - automatic retries: {}", d.attempts, d.retries);
        let mode = if rec.segments.is_empty() {
            "not started"
        } else if rec.resumable {
            "parallel"
        } else {
            "one connection"
        };
        let _ = writeln!(r, "Connections: {} now, {} at most - mode: {mode}", d.connections, d.peak_connections);
        let _ = writeln!(
            r,
            "Average speed: {} over {} s ({} received)",
            if d.avg_speed_bps > 0 { human_bytes(d.avg_speed_bps) + "/s" } else { "-".into() },
            d.active_secs,
            human_bytes(d.transferred)
        );
        let _ = writeln!(
            r,
            "Progress: {} of {} - resumable: {}",
            human_bytes(dto.downloaded),
            dto.total_size.map_or_else(|| "unknown size".into(), human_bytes),
            if rec.resumable { "yes" } else { "no" }
        );
        if !d.events.is_empty() {
            let _ = writeln!(r, "Timeline (UTC):");
            for e in &d.events {
                let _ = writeln!(r, "  {}  {:<5}  {}", utc_timestamp(e.at, false), e.level, e.text);
            }
        }
        // A last pass over the whole text: nothing secret leaves in a report, whatever a field held.
        self.redactor().redact(&r).into_owned()
    }

    // ---------------------------------------------------------------- the application log

    /// The saved "detailed logging" choice, applied at start.
    pub(super) fn apply_saved_logging(&self) {
        if let Some(log) = &self.inner.cfg.log {
            if self.inner.store.get_setting(DETAILED_KEY).ok().flatten().as_deref() == Some("1") {
                log.set_detailed(true);
            }
        }
    }

    /// The newest `limit` lines of the log (only warnings and errors with `problems_only`).
    pub fn get_logs(&self, limit: u32, problems_only: bool) -> LogsDto {
        let Some(log) = &self.inner.cfg.log else {
            return LogsDto { detailed: false, file_logging: false, lines: Vec::new() };
        };
        let limit = limit.clamp(1, 1000) as usize;
        let mut lines = log.recent(if problems_only { 1000 } else { limit });
        if problems_only {
            lines.retain(|l| l.is_problem());
            let skip = lines.len().saturating_sub(limit);
            lines.drain(..skip);
        }
        LogsDto {
            detailed: log.detailed(),
            file_logging: log.writes_files(),
            lines: lines.into_iter().map(|l| LogLineDto { at: l.at, level: l.level, target: l.target, text: l.text }).collect(),
        }
    }

    /// Detailed logging on or off (kept across restarts).
    pub async fn set_detailed_logging(&self, enabled: bool) -> Result<(), ManagerError> {
        let log = self.inner.cfg.log.clone().ok_or_else(|| ManagerError::invalid("logging is not available here"))?;
        let store = self.inner.store.clone();
        blocking(move || store.set_setting(DETAILED_KEY, if enabled { "1" } else { "0" })).await?;
        log.set_detailed(enabled);
        Ok(())
    }

    /// Show the log file in the file manager. The path comes from the log itself, never from the UI.
    pub async fn open_log_folder(&self) -> Result<(), ManagerError> {
        let file = self
            .inner
            .cfg
            .log
            .as_ref()
            .and_then(|l| l.file())
            .ok_or_else(|| ManagerError::invalid("the log is not kept in a file here"))?;
        let shell = self.inner.cfg.shell.clone();
        tokio::task::spawn_blocking(move || shell.0.reveal(&file))
            .await
            .map_err(|e| ManagerError::Internal(e.to_string()))?
            .map_err(|e| ManagerError::Internal(format!("cannot open the file manager: {e}")))
    }

    /// Everything support needs in one text, to copy into a bug report: versions, settings (no passwords,
    /// proxy addresses or links), counts, the latest failures and the recent log. Built here, redacted twice.
    pub async fn diagnostics_report(&self) -> Result<String, ManagerError> {
        use std::fmt::Write as _;
        let mut r = String::new();
        let _ = writeln!(
            r,
            "VeloxFetch {} - diagnostics report - {}",
            env!("CARGO_PKG_VERSION"),
            utc_timestamp(::diagnostics::now_ms(), false)
        );
        let _ = writeln!(r, "System: {} {}", std::env::consts::OS, std::env::consts::ARCH);
        let _ = writeln!(
            r,
            "Cookies and passwords on disk: {}",
            if self.inner.secrets_sealed {
                "sealed with a key from the system's credential store"
            } else {
                "not kept (no credential store)"
            }
        );
        let store = self.inner.store.clone();
        let counts = blocking(move || store.status_counts()).await?;
        let counts: Vec<String> = counts.iter().map(|(s, n)| format!("{n} {s}")).collect();
        let _ = writeln!(r, "Downloads: {}", if counts.is_empty() { "none".into() } else { counts.join(", ") });
        let q = self.get_queue_state().await?;
        let _ = writeln!(
            r,
            "Queue: {} at once, up to {} connections each, paused: {}, schedule: {}",
            q.max_active,
            q.max_connections,
            if q.paused { "yes" } else { "no" },
            if q.schedule.enabled { "on" } else { "off" }
        );
        let limits = self.speed_limits();
        let mut l = vec![format!("global {}", limits.global.map_or_else(|| "none".into(), |v| human_bytes(v) + "/s"))];
        l.extend(limits.by_type.iter().map(|(k, v)| format!("{k} {}/s", human_bytes(*v))));
        let _ = writeln!(r, "Speed limits: {}", l.join(", "));
        let net = self.get_network();
        let mode = match net.proxy_mode {
            ProxyMode::System => "system",
            ProxyMode::None => "none",
            ProxyMode::Manual => "manual",
        };
        let _ = writeln!(
            r,
            "Network: proxy {mode}{}, user agent {}",
            if net.proxy_username.is_some() { " with sign-in" } else { "" },
            if net.user_agent.is_some() { "custom" } else { "default" }
        );
        let _ = writeln!(r, "Sorting by type: {}", if self.get_storage().organize { "on" } else { "off" });
        let (detailed, files) = self.inner.cfg.log.as_ref().map_or((false, false), |l| (l.detailed(), l.writes_files()));
        let _ = writeln!(r, "Detailed logging: {} - log files: {}", if detailed { "on" } else { "off" }, if files { "yes" } else { "no" });

        let failed = self.list_downloads(ListFilter { status: Some(vec![DownloadStatus::Failed]) }).await?;
        let _ = writeln!(r, "\nLatest failures ({} failed in the list):", failed.len());
        if failed.is_empty() {
            let _ = writeln!(r, "  none");
        }
        for f in failed.iter().rev().take(20) {
            let d = self.load_diag(f.id).await;
            let _ = writeln!(
                r,
                "  #{} {} - HTTP {} - {} attempt(s), {} retries - {}",
                f.id,
                f.error_code.as_deref().unwrap_or("-"),
                d.http_status.map_or_else(|| "-".into(), |s| s.to_string()),
                d.attempts,
                d.retries,
                host(&f.url)
            );
        }
        let logs = self.get_logs(200, false);
        let _ = writeln!(r, "\nRecent log (UTC, newest last, {} lines):", logs.lines.len());
        for line in &logs.lines {
            let _ = writeln!(r, "  {} {:<5} {}: {}", utc_timestamp(line.at, true), line.level, line.target, line.text);
        }
        Ok(self.redactor().redact(&r).into_owned())
    }
}
