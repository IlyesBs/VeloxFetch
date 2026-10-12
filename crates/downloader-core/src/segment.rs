use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentStatus {
    Pending,
    Downloading,
    Paused,
    Completed,
    Failed,
    Retrying,
    Cancelled,
}

impl SegmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Retrying => "retrying",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "downloading" => Self::Downloading,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "retrying" => Self::Retrying,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// One contiguous byte range of a download. `end` is inclusive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: usize,
    pub start: u64,
    pub end: u64,
    pub downloaded: u64,
    pub status: SegmentStatus,
    #[serde(default)]
    pub retry_count: u32,
    /// `sha256:<hex>` of the segment's bytes as they were on disk (after `fsync`) when its completion was saved.
    /// It detects a later change of those local bytes; it does not prove they are what the server meant. Cleared
    /// whenever the segment is not complete. `None` for segments saved by older versions.
    #[serde(default)]
    pub hash: Option<String>,
}

impl Segment {
    pub fn new(id: usize, start: u64, end: u64) -> Self {
        Self { id, start, end, downloaded: 0, status: SegmentStatus::Pending, retry_count: 0, hash: None }
    }

    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// First byte not yet written.
    pub fn next_offset(&self) -> u64 {
        self.start + self.downloaded
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded >= self.len()
    }
}
