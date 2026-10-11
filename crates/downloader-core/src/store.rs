//! State-store abstraction. The engine persists progress through this trait so it
//! stays independent of SQLite; the `persistence` crate provides the implementation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::scheduler::Priority;
use crate::segment::Segment;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Verifying => "verifying",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "downloading" => Self::Downloading,
            "paused" => Self::Paused,
            "verifying" => Self::Verifying,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// Everything needed to resume a download after a pause, restart or crash.
///
/// `Debug` is written by hand: the request headers (cookies, credentials) are never printed and
/// the link is shown without its query, so a stray `{:?}` in a log cannot leak them.
#[derive(Clone)]
pub struct DownloadRecord {
    /// `None` until first saved.
    pub id: Option<i64>,
    pub url: String,
    pub filename: String,
    /// Final destination path (the in-progress data lives in `<destination>.part`).
    ///
    /// For a record that has **never started** (see [`DownloadRecord::is_unstarted`]) this is the
    /// target *directory*: the file name is only known after the server has been asked, so the
    /// engine derives it (unless `filename` is set) when the download first runs.
    pub destination: PathBuf,
    pub total_size: Option<u64>,
    pub downloaded: u64,
    pub status: DownloadStatus,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Whether segments can be resumed (server verified to support ranges).
    pub resumable: bool,
    /// Expected checksum (`sha256:<hex>`), if any.
    pub checksum: Option<String>,
    pub last_error: Option<String>,
    pub segments: Vec<Segment>,
    pub priority: Priority,
    pub category: Option<String>,
    /// Per-download speed limit in bytes/s.
    pub speed_limit: Option<u64>,
    /// From the server, for display (`Content-Type`, `Server`).
    pub content_type: Option<String>,
    pub server: Option<String>,
    /// Extra request headers handed over by a browser (`Referer`, `Cookie`, `User-Agent`), so a
    /// resumed download can still authenticate. Cleared when the download completes or is cancelled.
    pub request_headers: Vec<(String, String)>,
    /// The `.part` file's size and modification time (`<len>:<mtime ns>`) when the segment hashes were last saved.
    /// The same stamp at resume: the file was not touched since, the saved segments are trusted as they are.
    /// Another stamp (a crash after more writes, an outside change) or none: completed segments are re-hashed first.
    pub part_stamp: Option<String>,
}

impl std::fmt::Debug for DownloadRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadRecord")
            .field("id", &self.id)
            .field("url", &secrets::redact_url(&self.url))
            .field("filename", &self.filename)
            .field("destination", &self.destination)
            .field("total_size", &self.total_size)
            .field("downloaded", &self.downloaded)
            .field("status", &self.status)
            .field("resumable", &self.resumable)
            .field("segments", &self.segments.len())
            .field("priority", &self.priority)
            .field("category", &self.category)
            .field("request_headers", &format_args!("{} hidden", self.request_headers.len()))
            .finish_non_exhaustive()
    }
}

impl DownloadRecord {
    /// Created (e.g. by the download manager) but no transfer has been planned yet.
    pub fn is_unstarted(&self) -> bool {
        self.segments.is_empty() && self.total_size.is_none() && self.downloaded == 0 && self.status != DownloadStatus::Completed
    }
}

pub trait DownloadStore: Send + Sync {
    fn load(&self, id: i64) -> Result<Option<DownloadRecord>>;
    /// Insert (id `None`) or update the record and its segments atomically. Returns the id.
    fn save(&self, rec: &DownloadRecord) -> Result<i64>;
    /// Record a finished download in history.
    fn complete(&self, id: i64) -> Result<()>;
    /// Is an unfinished download (other than `except`) going to write `destination`? Tells the partial file
    /// of a paused or queued download apart from one a crash left behind with no download to own it.
    fn destination_in_use(&self, destination: &Path, except: Option<i64>) -> Result<bool> {
        let _ = (destination, except);
        Ok(false)
    }
}
