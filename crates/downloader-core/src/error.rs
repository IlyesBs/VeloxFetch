use http_client::HttpError;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("download paused (progress kept, can be resumed)")]
    Paused,
    #[error("download cancelled (partial data discarded)")]
    Cancelled,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("destination already exists: {0}")]
    FileExists(String),
    #[error("segment {index} ended early: got {got} of {expected} bytes")]
    ShortBody { index: usize, got: u64, expected: u64 },
    #[error("size mismatch: expected {expected} bytes, wrote {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("state store error: {0}")]
    Store(String),
    #[error("internal error: {0}")]
    Internal(String),
    /// The server answered with a web page where a file was expected (a sign-in or error page).
    #[error("the server sent a web page instead of the file `{0}`")]
    WebPageInsteadOfFile(String),
}

impl DownloadError {
    /// The HTTP status the server answered with, when that is what went wrong.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            DownloadError::Http(HttpError::Status { status, .. }) => Some(*status),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, DownloadError>;
