//! VeloxFetch download engine. Independent of Tauri/UI: usable from the CLI,
//! tests and (later) the application layer.

pub mod adaptive;
pub mod disk;
pub mod engine;
pub mod error;
pub mod filename;
pub mod filetype;
pub mod integrity;
pub mod limiter;
pub mod planner;
pub mod retry;
pub mod scheduler;
pub mod segment;
pub mod store;

pub use adaptive::{AdaptiveConfig, AdaptiveController, AdaptiveGate, Sample};
pub use engine::{
    numbered_name, part_path, DownloadConfig, DownloadControl, DownloadOutcome, DownloadRequest, Downloader, Placement, ProgressHandle,
    ProgressSnapshot, RunEvent, RunEventKind, StopReason,
};
pub use error::{DownloadError, Result};
pub use filetype::{classify as classify_file, FileKind};
pub use http_client::{BasicAuth, ClientConfig, ResourceInfo};
pub use integrity::{Checksum, HashAlgo};
pub use limiter::{RateLimits, Throttle, TokenBucket};
pub use retry::{classify, ErrorClass, RetryPolicy};
pub use scheduler::{Priority, SlotGuard, SlotScheduler};
pub use segment::{Segment, SegmentStatus};
pub use store::{DownloadRecord, DownloadStatus, DownloadStore};
pub use tokio_util::sync::CancellationToken;
