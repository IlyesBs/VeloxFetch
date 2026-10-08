//! Download Intelligence, manager side: gathers what the analysis is made from (see `crate::intelligence`
//! for the rules) without changing anything.
//!
//! * the server: the one quick check a download makes first anyway (`probe_quick`: `HEAD`, then
//!   `Range: bytes=0-0`, no retries, bounded by [`ANALYSIS_TIMEOUT`]); never the file itself;
//! * the destination drive: its free space, and (only for files of 4 GiB and more) whether it is FAT32;
//! * the downloads and history the app already has, for duplicates.
//!
//! Nothing is created, written or deleted; secrets the request carries are never part of the result. A check
//! that fails gives an analysis that says so: the download itself stays available.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use http_client::{HttpError, ResourceInfo};
use url::Url;

use super::*;
use crate::intelligence::{self as intel, AnalysisDto, AnalyzeLink, DiskObservation, Known, Observed, ProbeFailure};

/// The longest the quick check may take (a server that prepares the file before answering is reported as slow).
pub(super) const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(20);

/// History rows looked at for duplicates (the most recent ones).
const HISTORY_LOOKBACK: u32 = 500;

/// The check of the Add dialog that is running, if any (a new check, or `url: None`, cancels it).
#[derive(Default)]
pub(super) struct LinkCheck {
    next: u64,
    running: Option<(u64, tokio::task::AbortHandle)>,
}

impl DownloadManager {
    /// Analyse `url` before it is downloaded: one bounded, read-only look at the server, the drive and the
    /// known downloads. Also returns what the server said, for callers that show it (the confirmation dialog).
    pub(super) async fn analyze(
        &self,
        url: &Url,
        headers: &[(String, String)],
        requested: Option<&str>,
    ) -> (Option<ResourceInfo>, AnalysisDto) {
        let expected = link_name(url);
        let media = media_engine::is_site_page(url) || crate::is_stream_link(url);
        let (info, failure) = if media {
            (None, None) // the video dialog reads those; a plain request would only get a page or a playlist
        } else {
            match tokio::time::timeout(ANALYSIS_TIMEOUT, self.inner.downloader.probe_quick(url.as_str(), headers)).await {
                Ok(Ok(i)) => (Some(i), None),
                Ok(Err(e)) => (None, Some(failure_of(&e))),
                Err(_) => (None, Some(ProbeFailure::Slow)),
            }
        };
        let filename = match (requested, &info) {
            (Some(n), _) => n.to_string(),
            (None, Some(i)) => downloader_core::filename::choose_filename(i, url),
            (None, None) => expected.clone(),
        };
        let content_type = info.as_ref().and_then(|i| i.content_type.clone());
        let dir = self.predicted_dir(&filename, content_type.as_deref());
        let size = info.as_ref().and_then(|i| i.content_length);
        let disk = tokio::task::spawn_blocking(move || observe_disk(&dir, size)).await.unwrap_or_default();
        let known = self.known_downloads().await;
        let duplicate = intel::find_duplicate(url, info.as_ref(), &known);
        let credentials_sent = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("cookie") || k.eq_ignore_ascii_case("authorization"));
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let analysis = intel::assess(
            Observed { url, info: info.as_ref(), failure, credentials_sent, filename, expected_name: &expected, disk, duplicate, now_ms },
            |s| self.inner.downloader.connection_plan(s),
        );
        (info, analysis)
    }

    /// Check a link from the Add dialog before adding it. A new check cancels the one still running;
    /// `url: None` only cancels. Read-only, like every analysis.
    pub async fn analyze_link(&self, req: AnalyzeLink) -> Result<Option<AnalysisDto>, ManagerError> {
        let Some(raw) = req.url else {
            if let Some((_, h)) = self.inner.link_check.lock().unwrap().running.take() {
                h.abort();
            }
            return Ok(None);
        };
        let (url, credentials) = validate::split_credentials(validate::url(raw.trim())?);
        let url = crate::links::direct(&url).unwrap_or(url);
        let headers: Vec<(String, String)> = credentials.map(|c| ("Authorization".to_string(), c)).into_iter().collect();
        let this = self.clone();
        let task = tokio::spawn(async move { this.analyze(&url, &headers, None).await.1 });
        let id = {
            let mut c = self.inner.link_check.lock().unwrap();
            if let Some((_, h)) = c.running.take() {
                h.abort();
            }
            c.next += 1;
            c.running = Some((c.next, task.abort_handle()));
            c.next
        };
        let out = task.await;
        {
            let mut c = self.inner.link_check.lock().unwrap();
            if c.running.as_ref().is_some_and(|(r, _)| *r == id) {
                c.running = None;
            }
        }
        match out {
            Ok(a) => Ok(Some(a)),
            Err(e) if e.is_cancelled() => Err(ManagerError::invalid("the check was cancelled")),
            Err(e) => Err(ManagerError::Internal(e.to_string())),
        }
    }

    /// Downloads in the list, then finished files only in the history (for duplicates). Empty if the
    /// database cannot be read: the analysis then simply finds no duplicate.
    async fn known_downloads(&self) -> Vec<Known> {
        let store = self.inner.store.clone();
        let read = tokio::task::spawn_blocking(move || {
            let list = store.list().unwrap_or_default();
            let history = store.history(HISTORY_LOOKBACK).unwrap_or_default();
            (list, history)
        });
        let Ok((list, history)) = read.await else { return Vec::new() };
        let ids: std::collections::HashSet<i64> = list.iter().filter_map(|r| r.id).collect();
        let mut out: Vec<Known> = list
            .into_iter()
            .map(|r| Known { id: r.id, url: r.url, filename: r.filename, status: Some(r.status), etag: r.etag, size: r.total_size })
            .collect();
        out.extend(
            history.into_iter().filter(|h| h.outcome == "completed" && !h.download_id.is_some_and(|id| ids.contains(&id))).map(|h| Known {
                id: None,
                url: h.url,
                filename: h.filename,
                status: None,
                etag: None,
                size: h.size,
            }),
        );
        out
    }
}

/// The name the link itself suggests (its last path segment).
fn link_name(url: &Url) -> String {
    let s = downloader_core::filename::sanitize_filename(url.path_segments().and_then(|mut s| s.next_back()).unwrap_or(""));
    if s.is_empty() {
        "download".to_string()
    } else {
        s
    }
}

/// Why the quick check gave no answer. Only a status number or a stable code: never the error text (it can
/// carry the link).
fn failure_of(e: &DownloadError) -> ProbeFailure {
    if let Some(s) = e.http_status() {
        return ProbeFailure::Status(s);
    }
    match e {
        DownloadError::Http(HttpError::SlowToAnswer) => ProbeFailure::Slow,
        DownloadError::Http(h) if h.is_timeout() => ProbeFailure::Slow,
        _ => ProbeFailure::Other(crate::explain::error_code(&e.to_string())),
    }
}

/// Free space of the drive `dir` will be on (through its nearest existing parent), and, for a file of 4 GiB
/// or more, whether that drive is FAT32. The drive list is only read then (it is the one slow part).
fn observe_disk(dir: &Path, size: Option<u64>) -> DiskObservation {
    let Some(on) = nearest_existing(dir) else { return DiskObservation::default() };
    let free = fs4::available_space(on).ok();
    let fat = size.is_some_and(|s| s >= 4 * 1024 * 1024 * 1024) && {
        let disks = sysinfo::Disks::new_with_refreshed_list_specifics(sysinfo::DiskRefreshKind::nothing());
        sysio::disk_for(&disks, on).is_some_and(|d| {
            let fs = d.file_system().to_string_lossy().to_ascii_lowercase();
            matches!(fs.as_str(), "vfat" | "fat" | "fat12" | "fat16" | "fat32" | "msdos")
        })
    };
    DiskObservation { free, fat }
}
