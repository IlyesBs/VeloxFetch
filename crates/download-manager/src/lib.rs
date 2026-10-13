//! Application controller for VeloxFetch: the layer between the UI shell (Tauri) and the
//! download engine.
//!
//! ```text
//! Tauri commands -> DownloadManager (validation, lifecycle, events) -> Downloader (engine)
//! ```
//! The UI only ever passes ids and the DTOs in [`api`]; this crate validates all of it and
//! decides what actually happens on disk and on the network.

pub mod api;
pub mod batch;
pub mod doctor;
pub mod explain;
pub mod intelligence;
pub mod links;
mod manager;
pub mod profiles;
pub mod relink;
pub mod validate;

pub use api::*;
pub use batch::{BatchItem, BatchPreview, BatchRequest, BatchResult, ItemState, MAX_BATCH, MAX_GENERATED};
pub use downloader_core::{DownloadStatus, Priority};
pub use manager::{
    is_stream_link, preview_media_type, smart_throttle_find_running, smart_throttle_is_app_path, DownloadManager, PreviewHead,
    PreviewResponse, ThrottleSignals, UpdatePlan, MAIN_QUEUE, SECRET_KEY_NAME, SMART_THROTTLE_DEFAULT_APPS, THROTTLE_IDLE_AFTER,
};
