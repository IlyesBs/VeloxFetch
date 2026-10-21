//! Download Intelligence: what a link is and how its download will most likely go, worked out before
//! anything is downloaded.
//!
//! Everything here is a pure function of what was *observed*: the link itself, the answer to the one quick
//! check the download makes first anyway (`HEAD`, then `Range: bytes=0-0`, see [`http_client::HttpClient::probe_with`]),
//! the free space of the destination drive and the downloads the app already knows. No score, no guess, no
//! prediction: every label and every warning maps to a fact listed in the result, so the dialog can always say
//! why. The manager side (`manager/intelligence.rs`) gathers the observations; nothing here does I/O.
//!
//! The recommended strategy is only the *start*: the engine's adaptive controller still tunes connections while
//! the download runs ([`downloader_core::planner::connection_limits`] is the same rule the engine uses).

use http_client::ResourceInfo;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::DownloadStatus;

/// What the link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceKind {
    /// A file the server sends as it is.
    File,
    /// The server answered with a web page where a file was expected (often a sign-in or error page).
    WebPage,
    /// An HLS / DASH stream description (`.m3u8`, `.mpd`): the video dialog handles it.
    Stream,
    /// A page of a video site: the video dialog handles it.
    VideoPage,
    /// The server could not be asked.
    Unknown,
}

/// Can a stopped download continue where it stopped?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResumeSupport {
    /// The server proved it sends parts of the file (`206` to `bytes=0-0`), the size is known, and it gives a
    /// validator (`ETag` / `Last-Modified`) so a changed file is noticed instead of being stitched together.
    Supported,
    /// Parts proved and size known, but no validator: resuming works unless the file changes on the server.
    LikelySupported,
    /// The server sent the whole file to a request for one byte, or the size is unknown (the app can only
    /// resume a download whose size it knows).
    NotSupported,
    /// Not enough information (the check failed, or the answer was ambiguous).
    Unknown,
}

/// How the download should start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Strategy {
    /// Several connections (see `connections`); the adaptive controller adjusts them while it runs.
    Parallel,
    /// One connection: the server does not send parts, or the file is too small to split.
    Single,
    /// One connection and no assumptions: size or behaviour unknown, or the server is slow / failing.
    Conservative,
    /// Handled by the video dialog (quality choice).
    Stream,
}

/// Does the link need a sign-in?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Access {
    /// Answered without any credentials.
    Public,
    /// The server asked for a user name and password (`401`), or rejected the session it was given.
    AuthRequired,
    /// The link led to a sign-in page of the website: it works from the browser, signed in.
    BrowserSessionRequired,
    Unknown,
}

/// Room for the file on the destination drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageCheck {
    Ok,
    /// It fits, with less than a tenth of the free space left over.
    Low,
    Insufficient,
    /// The drive's file system (FAT32) cannot hold a file of 4 GiB or more, whatever the free space.
    TooLargeForDrive,
    /// The free space is known but the size is not: the requirement cannot be checked.
    CannotVerify,
    /// The destination drive could not be measured.
    Unavailable,
}

/// Overall look of the download before it starts (a label, not a score).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Health {
    Good,
    /// Something deserves attention (see the warnings) but the download can run.
    Caution,
    /// The download will most likely fail as it is (see the problems).
    Problem,
    /// The server could not be checked.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Info,
    Warning,
    Problem,
}

/// One observation worth telling the user. The UI words it from `code` (and the optional values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FindingCode {
    /// Plain HTTP: what is downloaded can be read or changed on the way.
    Insecure,
    /// An HTTPS link redirected to plain HTTP.
    HttpsDowngrade,
    /// Redirected to another site (`detail` = its host).
    OtherSite,
    AuthRequired,
    SessionRequired,
    /// The browser's session (cookies / sign-in) was sent with the check.
    SessionSent,
    /// `403`: the server refused access (an expired link, a sign-in, a region...).
    AccessRefused,
    /// The server answered with an error (`detail` = `http_404`...).
    ServerError,
    /// No answer at all (`detail` = the error code).
    Unreachable,
    /// The server takes long to answer: it may be preparing the file.
    SlowServer,
    /// The link looks temporary (signed / with an expiry); `at_ms` = when it expires, if it says.
    TemporaryLink,
    ExpiredLink,
    /// A web page came back where a file was expected.
    WebPage,
    /// `bytes` needed, `free` available.
    NoSpace,
    TooLargeForDrive,
    LowSpace,
    /// The size is unknown, so the free space cannot be compared with it.
    StorageUnverified,
    NoResume,
    /// Resume should work, but a change of the file on the server could not be noticed.
    ResumeUnverified,
    UnknownSize,
    /// Already in the list, or downloaded before.
    Duplicate,
    /// A video stream / video page: the video dialog handles it.
    Stream,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub code: FindingCode,
    pub level: Level,
    pub detail: Option<String>,
    pub bytes: Option<u64>,
    pub free: Option<u64>,
    pub at_ms: Option<u64>,
}

impl Finding {
    fn new(code: FindingCode, level: Level) -> Self {
        Self { code, level, detail: None, bytes: None, free: None, at_ms: None }
    }
    fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
}

/// Why two downloads are the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DuplicateReason {
    /// The same link (ignoring the `#fragment`).
    SameLink,
    /// The link leads to where a known download's link is.
    SameFinalLink,
    /// Same site, same strong `ETag` and same size: the same file under another link (signed links...).
    SameFile,
    /// The checksum given for this download is the one a known download has: the same content, whatever the link.
    SameChecksum,
}

/// How sure the match is. No match at all is no [`DuplicateDto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    /// The same link (or redirect target) and the server's answer proves the same version, or the same checksum.
    Exact,
    /// The same link but its version could not be compared, or the same file proven by a strong `ETag` and size.
    Likely,
    /// The same link, but the server now describes another version (another `ETag`, size or date) or the checksum
    /// differs: what was downloaded before may not be what the link gives now.
    Possible,
}

/// The finished file of a completed match, as found on disk (a quick look at its metadata, never its content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileState {
    /// There, with the size it was saved with.
    Present,
    /// Moved or deleted since.
    Missing,
    /// A file is there but its size is not the one saved: changed or replaced since.
    Changed,
    /// Its metadata could not be read (no access, outside the download folders...).
    Unknown,
}

/// The known download this one repeats. A file name alone never makes a duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateDto {
    /// `None`: only in the history (downloaded before, removed from the list since).
    pub id: Option<i64>,
    /// The history line of a match that is only in the history (to open or show its file).
    pub history_id: Option<i64>,
    pub status: Option<DownloadStatus>,
    pub filename: String,
    pub reason: DuplicateReason,
    pub confidence: Confidence,
    /// The server (or the checksum) describes another version than the known download's.
    pub changed: bool,
    /// Completed matches only: whether the finished file is still there.
    pub file: Option<FileState>,
    /// The finished file is there and may be opened from the app (not a program or script).
    pub openable: bool,
}

/// The file name this download would get is already used in its folder (by a file or by another download).
/// Nothing is ever replaced: the download is saved under the next free name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameConflict {
    /// The name it will most likely be saved as (`name (1).ext`...); the engine decides for good when it starts.
    pub saved_as: Option<String>,
    /// The name is the matched duplicate's own file (the same resource), not some other file of that name.
    pub same_resource: bool,
}

/// The pre-download analysis shown before a download starts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisDto {
    pub kind: ResourceKind,
    pub filename: String,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    /// The link (after redirects) uses HTTPS.
    pub https: bool,
    pub redirected: bool,
    /// Host the link finally led to (never the full address: it may carry a signature).
    pub final_host: Option<String>,
    /// `Accept-Ranges: bytes` (what the server claims).
    pub range_claimed: bool,
    /// `206` to `bytes=0-0` (what the server did).
    pub range_verified: bool,
    pub etag: bool,
    pub last_modified: bool,
    pub resume: ResumeSupport,
    pub strategy: Strategy,
    /// Connections the download starts with (parallel only).
    pub connections: Option<u32>,
    pub access: Access,
    pub temporary: bool,
    pub expires_at_ms: Option<u64>,
    pub storage: StorageCheck,
    pub free_bytes: Option<u64>,
    pub duplicate: Option<DuplicateDto>,
    /// The file name is already taken in the destination folder (a destination conflict, not a duplicate).
    pub name_conflict: Option<NameConflict>,
    pub health: Health,
    /// Most serious first.
    pub findings: Vec<Finding>,
    /// Stable code of what went wrong with the check (`http_404`, `slow`...), when it did.
    pub error_code: Option<String>,
    /// The publisher's checksum was found next to the file (feature #6): the file will be checked against it.
    pub publisher_checksum: Option<PublisherChecksumDto>,
}

/// The publisher's checksum list found for a link: its name (`SHA256SUMS`, never its address) and the digest kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublisherChecksumDto {
    pub list: String,
    /// `SHA-256` / `SHA-512`.
    pub algo: String,
}

/// Why the quick check did not give an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    /// The server answered with this status.
    Status(u16),
    /// No answer in time: the server may be preparing the file.
    Slow,
    /// Any other failure (connection, TLS, refused redirect...), with its error code.
    Other(String),
}

/// The destination drive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskObservation {
    pub free: Option<u64>,
    /// FAT32 (or older FAT): files of 4 GiB and more cannot be written.
    pub fat: bool,
}

/// Everything the analysis is made from.
pub struct Observed<'a> {
    /// The link as it will be downloaded (credentials already taken out).
    pub url: &'a Url,
    pub info: Option<&'a ResourceInfo>,
    pub failure: Option<ProbeFailure>,
    /// Cookies or an `Authorization` header went with the check.
    pub credentials_sent: bool,
    /// The file name the download will get.
    pub filename: String,
    /// The name the link itself suggests (to tell a web page from the expected file).
    pub expected_name: &'a str,
    pub disk: DiskObservation,
    pub duplicate: Option<DuplicateDto>,
    pub name_conflict: Option<NameConflict>,
    pub now_ms: u64,
}

const GIB4: u64 = 4 * 1024 * 1024 * 1024;

/// Turn observations into the analysis. `plan(size)` is the engine's `(start, ceiling)` connection plan.
pub fn assess(o: Observed<'_>, plan: impl Fn(u64) -> (usize, usize)) -> AnalysisDto {
    let info = o.info;
    let final_url = info.and_then(|i| Url::parse(&i.final_url).ok());
    let shown = final_url.as_ref().unwrap_or(o.url);
    let redirected = info.is_some_and(|i| i.location.is_some()) && final_url.as_ref().is_some_and(|f| f != o.url);
    let content_type = info.and_then(|i| i.content_type.clone());
    let size = info.and_then(|i| i.content_length);
    let mut findings = Vec::new();

    // What it is.
    let stream_type = content_type.as_deref().is_some_and(|t| {
        let t = t.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        matches!(t.as_str(), "application/vnd.apple.mpegurl" | "application/x-mpegurl" | "audio/mpegurl" | "application/dash+xml")
    });
    let kind = if media_engine::is_site_page(o.url) {
        ResourceKind::VideoPage
    } else if crate::is_stream_link(o.url) || stream_type {
        ResourceKind::Stream
    } else if let Some(i) = info {
        if downloader_core::filename::web_page_instead_of_file(i, o.expected_name) {
            ResourceKind::WebPage
        } else {
            ResourceKind::File
        }
    } else {
        ResourceKind::Unknown
    };
    let media = matches!(kind, ResourceKind::Stream | ResourceKind::VideoPage);
    if media {
        findings.push(Finding::new(FindingCode::Stream, Level::Info));
    }

    // Security of the transfer.
    let https = shown.scheme() == "https";
    if o.url.scheme() == "https" && redirected && !https {
        findings.push(Finding::new(FindingCode::HttpsDowngrade, Level::Warning));
    } else if !https {
        findings.push(Finding::new(FindingCode::Insecure, Level::Warning));
    }
    let final_host = shown.host_str().map(str::to_string);
    if redirected && site(o.url) != site(shown) {
        findings.push(Finding::new(FindingCode::OtherSite, Level::Info).detail(final_host.clone().unwrap_or_default()));
    }

    // Sign-in.
    let login_page = kind == ResourceKind::WebPage && redirected && looks_like_sign_in(shown);
    use Access::{AuthRequired, BrowserSessionRequired};
    let access = match (&o.failure, info) {
        (Some(ProbeFailure::Status(401)), _) => AuthRequired,
        (_, Some(_)) if login_page && o.credentials_sent => AuthRequired,
        (_, Some(_)) if login_page => BrowserSessionRequired,
        (None, Some(_)) if kind == ResourceKind::WebPage => Access::Unknown,
        (None, Some(_)) if o.credentials_sent => Access::Unknown,
        (None, Some(_)) => Access::Public,
        _ => Access::Unknown,
    };
    match access {
        AuthRequired => findings.push(Finding::new(FindingCode::AuthRequired, Level::Problem)),
        BrowserSessionRequired => findings.push(Finding::new(FindingCode::SessionRequired, Level::Problem)),
        _ if o.credentials_sent && info.is_some() && kind != ResourceKind::WebPage => {
            findings.push(Finding::new(FindingCode::SessionSent, Level::Info))
        }
        _ => {}
    }
    if kind == ResourceKind::WebPage && !login_page {
        findings.push(Finding::new(FindingCode::WebPage, Level::Problem));
    }

    // The check itself failed.
    let error_code = match &o.failure {
        Some(ProbeFailure::Status(s)) => Some(format!("http_{s}")),
        Some(ProbeFailure::Slow) => Some("slow".into()),
        Some(ProbeFailure::Other(c)) => Some(c.clone()),
        None => None,
    };
    match &o.failure {
        Some(ProbeFailure::Status(401)) | None => {}
        Some(ProbeFailure::Status(403)) => findings.push(Finding::new(FindingCode::AccessRefused, Level::Problem)),
        Some(ProbeFailure::Status(s)) => {
            // Client errors will not go away by themselves; a busy or failing server may.
            let level = if (400..500).contains(s) && !matches!(s, 408 | 425 | 429) { Level::Problem } else { Level::Warning };
            findings.push(Finding::new(FindingCode::ServerError, level).detail(format!("http_{s}")));
        }
        Some(ProbeFailure::Slow) => findings.push(Finding::new(FindingCode::SlowServer, Level::Info)),
        Some(ProbeFailure::Other(c)) => findings.push(Finding::new(FindingCode::Unreachable, Level::Warning).detail(c.clone())),
    }

    // A temporary link.
    let expiry = [Some(o.url), final_url.as_ref()].into_iter().flatten().map(expiring).find(|e| e.temporary);
    let temporary = expiry.is_some();
    let expires_at_ms = expiry.and_then(|e| e.at_ms);
    if let Some(at) = expires_at_ms.filter(|at| *at <= o.now_ms) {
        let mut f = Finding::new(FindingCode::ExpiredLink, Level::Problem);
        f.at_ms = Some(at);
        findings.push(f);
    } else if temporary {
        let mut f = Finding::new(FindingCode::TemporaryLink, Level::Warning);
        f.at_ms = expires_at_ms;
        findings.push(f);
    }

    // Resume.
    let validator = info.is_some_and(|i| i.etag.is_some() || i.last_modified.is_some());
    let resume = match info {
        None => ResumeSupport::Unknown,
        Some(i) if i.range_verified && i.content_length.is_some_and(|s| s > 0) => {
            if validator {
                ResumeSupport::Supported
            } else {
                ResumeSupport::LikelySupported
            }
        }
        // A whole-file answer to a one-byte request, or parts with no total: the engine downloads it in one go.
        Some(i) if i.status == 200 || (i.range_verified && i.content_length.is_none()) => ResumeSupport::NotSupported,
        Some(_) => ResumeSupport::Unknown,
    };
    if !media {
        match resume {
            ResumeSupport::NotSupported => findings.push(Finding::new(FindingCode::NoResume, Level::Info)),
            ResumeSupport::LikelySupported => findings.push(Finding::new(FindingCode::ResumeUnverified, Level::Info)),
            _ => {}
        }
        if info.is_some() && size.is_none() {
            findings.push(Finding::new(FindingCode::UnknownSize, Level::Info));
        }
    }

    // How to start.
    // Anything else (size unknown, a slow or failing server, a web page) starts with one connection and no assumptions.
    let (strategy, connections) = if media {
        (Strategy::Stream, None)
    } else {
        match (info, size) {
            (Some(i), Some(s)) if kind == ResourceKind::File && i.supports_parallel() && s > 0 => {
                let (start, ceiling) = plan(s);
                if ceiling > 1 {
                    (Strategy::Parallel, Some(start as u32))
                } else {
                    (Strategy::Single, None)
                }
            }
            (Some(_), Some(_)) if kind == ResourceKind::File => (Strategy::Single, None),
            _ => (Strategy::Conservative, None),
        }
    };

    // Room on the drive.
    let free_bytes = o.disk.free;
    let storage = match (size.filter(|_| !media), free_bytes) {
        (_, None) => StorageCheck::Unavailable,
        (Some(s), Some(_)) if o.disk.fat && s >= GIB4 => StorageCheck::TooLargeForDrive,
        (Some(s), Some(free)) if s > free => StorageCheck::Insufficient,
        (Some(s), Some(free)) if s > free / 10 * 9 => StorageCheck::Low,
        (Some(_), Some(_)) => StorageCheck::Ok,
        (None, Some(_)) => StorageCheck::CannotVerify,
    };
    let space = |code, level| {
        let mut f = Finding::new(code, level);
        f.bytes = size;
        f.free = free_bytes;
        f
    };
    match storage {
        StorageCheck::Insufficient => findings.push(space(FindingCode::NoSpace, Level::Problem)),
        StorageCheck::TooLargeForDrive => findings.push(space(FindingCode::TooLargeForDrive, Level::Problem)),
        StorageCheck::Low => findings.push(space(FindingCode::LowSpace, Level::Warning)),
        StorageCheck::CannotVerify if !media => findings.push(space(FindingCode::StorageUnverified, Level::Info)),
        _ => {}
    }

    if let Some(d) = &o.duplicate {
        findings.push(Finding::new(FindingCode::Duplicate, Level::Warning).detail(d.filename.clone()));
    }

    findings.sort_by_key(|f| std::cmp::Reverse(f.level));
    let worst = findings.first().map(|f| f.level);
    let health = match worst {
        Some(Level::Problem) => Health::Problem,
        _ if info.is_none() && !media => Health::Unknown,
        Some(Level::Warning) => Health::Caution,
        _ => Health::Good,
    };

    AnalysisDto {
        kind,
        filename: o.filename,
        size: size.filter(|_| !media),
        content_type,
        https,
        redirected,
        final_host,
        range_claimed: info.is_some_and(|i| i.accept_ranges_header),
        range_verified: info.is_some_and(|i| i.range_verified),
        etag: info.is_some_and(|i| i.etag.is_some()),
        last_modified: info.is_some_and(|i| i.last_modified.is_some()),
        resume,
        strategy,
        connections,
        access,
        temporary,
        expires_at_ms,
        storage,
        free_bytes,
        duplicate: o.duplicate,
        name_conflict: o.name_conflict,
        health,
        findings,
        error_code,
        publisher_checksum: None,
    }
}

/// The site part of a host (`dl.example.com` and `www.example.com` are the same site; a CDN is not).
fn site(u: &Url) -> String {
    let h = u.host_str().unwrap_or("").trim_start_matches("www.").to_ascii_lowercase();
    let labels: Vec<&str> = h.split('.').collect();
    if labels.len() <= 2 || h.parse::<std::net::IpAddr>().is_ok() || h.starts_with('[') {
        return h;
    }
    labels[labels.len() - 2..].join(".")
}

/// A sign-in page's address (`/login`, `/signin`, `accounts.` / `login.` hosts...).
fn looks_like_sign_in(u: &Url) -> bool {
    let path = u.path().to_ascii_lowercase();
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    ["login", "signin", "sign-in", "sign_in", "logon", "auth", "sso", "oauth"].iter().any(|w| path.split('/').any(|seg| seg.starts_with(w)))
        || ["login.", "signin.", "accounts.", "auth.", "sso."].iter().any(|p| host.starts_with(p))
}

/// A link that only works inside the browser: a Microsoft Office Online / Web Apps export (Excel, Word or
/// PowerPoint for the web — `*.officeapps.live.com`, e.g. `XlFileHandler.aspx?...downloadAsCsvEnabled=1`). The
/// file is generated live in the user's Office session and is not stored, so re-fetching the link from outside
/// the browser returns a sign-in / error page: no external downloader can get it. A real SharePoint or OneDrive
/// *file* lives on `*.sharepoint.com` / `*.live.com` and is a plain authenticated download — never flagged here.
pub fn browser_only_link(u: &Url) -> bool {
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    host == "officeapps.live.com" || host.ends_with(".officeapps.live.com")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Expiry {
    pub temporary: bool,
    /// When the link says it stops working (Unix ms), if it says so in a way that can be read.
    pub at_ms: Option<u64>,
}

/// Query names that only signed, short-lived links carry (S3, Google Cloud Storage, CloudFront, Akamai...).
const SIGNED_NAMES: &[&str] = &[
    "x-amz-signature",
    "x-amz-expires",
    "x-amz-credential",
    "x-amz-security-token",
    "x-goog-signature",
    "x-goog-expires",
    "x-goog-credential",
    "googleaccessid",
    "key-pair-id",
    "hdnts",
    "hdnea",
    "__gda__",
];
/// Names that state an expiry.
const EXPIRY_NAMES: &[&str] = &["expires", "expire", "expiry", "expiration", "exp", "validto", "valid_until", "se"];

/// Does the link look temporary? Conservative: only well-known signing parameters, or an expiry next to a
/// signature / token. Signing schemes are never decoded beyond a plain expiry time.
pub fn expiring(u: &Url) -> Expiry {
    let pairs: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.to_ascii_lowercase(), v.into_owned())).collect();
    let has = |n: &str| pairs.iter().any(|(k, _)| k == n);
    let signed = pairs.iter().any(|(k, _)| SIGNED_NAMES.contains(&k.as_str()));
    let expiry_named = pairs.iter().any(|(k, _)| EXPIRY_NAMES.contains(&k.as_str()));
    let secret = pairs.iter().any(|(k, _)| diagnostics::is_secret_name(k) || matches!(k.as_str(), "hash" | "md5" | "hmac" | "st"));
    // CloudFront: Expires + Signature; Azure SAS: se + sig; a generic "expires=...&token=...".
    let temporary = signed || (expiry_named && secret) || (has("policy") && has("signature"));
    if !temporary {
        return Expiry::default();
    }
    let unix = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.parse::<u64>().ok())
            .filter(|s| (1_000_000_000..10_000_000_000).contains(s))
            .map(|s| s * 1000)
    };
    let at_ms = ["expires", "expire", "expiry", "exp", "validto"].iter().find_map(|n| unix(n)).or_else(|| {
        // S3 / GCS: a start time plus a number of seconds.
        let (date, secs) = ["x-amz", "x-goog"].iter().find_map(|p| {
            let d = pairs.iter().find(|(k, _)| *k == format!("{p}-date"))?.1.clone();
            let s = pairs.iter().find(|(k, _)| *k == format!("{p}-expires"))?.1.parse::<u64>().ok()?;
            Some((d, s))
        })?;
        Some(basic_iso_ms(&date)? + secs.min(7 * 24 * 3600) * 1000)
    });
    Expiry { temporary, at_ms }
}

/// `20240131T235959Z` -> Unix ms.
fn basic_iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' || !b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<u64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(4..6)?, n(6..8)?, n(9..11)?, n(11..13)?, n(13..15)?);
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y, m) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400) + h * 3600 + mi * 60 + se) * 1000)
}

/// The link with what must not be shown hidden: the password of `user:pass@` and the value of every query
/// parameter that is a secret (signature, token...) or part of a signed link.
pub fn display_url(u: &Url) -> String {
    let mut out = u.clone();
    if out.password().is_some() {
        let _ = out.set_password(Some(diagnostics::MASK));
    }
    let temporary = expiring(u).temporary;
    if u.query().is_some() {
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| {
                let lower = k.to_ascii_lowercase();
                let hide = diagnostics::is_secret_name(&k)
                    || SIGNED_NAMES.contains(&lower.as_str())
                    || (temporary && !EXPIRY_NAMES.contains(&lower.as_str()) && !lower.ends_with("-date"));
                (k.into_owned(), if hide { diagnostics::MASK.to_string() } else { v.into_owned() })
            })
            .collect();
        if pairs.iter().any(|(_, v)| v == diagnostics::MASK) {
            out.query_pairs_mut().clear().extend_pairs(pairs);
        }
    }
    out.to_string()
}

/// What the app knows of a download, for [`find_duplicate`].
#[derive(Debug, Clone)]
pub struct Known {
    pub id: Option<i64>,
    /// The history line, for one that is only in the history.
    pub history_id: Option<i64>,
    pub url: String,
    pub filename: String,
    pub status: Option<DownloadStatus>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub size: Option<u64>,
    /// The checksum the download was given (normalised `algo:hex`), verified once it completed.
    pub checksum: Option<String>,
    /// Where its file is (or will be): only read by the manager, to look at the finished file. Never sent.
    pub destination: Option<std::path::PathBuf>,
}

/// A strong `ETag` (a weak one, `W/"..."`, only says "about the same", so it proves nothing).
fn strong_etag(e: &str) -> bool {
    !e.starts_with("W/") && e.len() > 2
}

/// Whether the server's answer now describes the known download's version: `Some(true)` proven the same
/// (strong `ETag`, or size and date), `Some(false)` proven another one (another strong `ETag`, size or date),
/// `None` when nothing can be compared.
fn same_version(info: &ResourceInfo, k: &Known) -> Option<bool> {
    let etags = (info.etag.as_deref().filter(|e| strong_etag(e)), k.etag.as_deref().filter(|e| strong_etag(e)));
    if let (Some(a), Some(b)) = etags {
        return Some(a == b);
    }
    let sizes = (info.content_length, k.size);
    if let (Some(a), Some(b)) = sizes {
        if a != b {
            return Some(false);
        }
    }
    let dates = (info.last_modified.as_deref(), k.last_modified.as_deref());
    match (sizes, dates) {
        (_, (Some(a), Some(b))) if a != b => Some(false),
        ((Some(_), Some(_)), (Some(_), Some(_))) => Some(true),
        _ => None,
    }
}

/// The known download (or history entry) this link repeats, with how sure that is. Evidence, strongest first:
/// the same checksum (the same content, whatever the link); the same link or redirect target, its version compared
/// with what the server says now (`ETag`, size, date); the same file proven by a strong `ETag` and size on the same
/// site under another link. A file name alone never matches, and a cancelled download never counts. Running or
/// waiting downloads come first, then finished ones, failed ones, then the history; the surer match first among
/// equals. `checksum` is the normalised one given for the new download, if any.
pub fn find_duplicate(url: &Url, info: Option<&ResourceInfo>, checksum: Option<&str>, known: &[Known]) -> Option<DuplicateDto> {
    let link = crate::batch::normalize(url);
    let final_link = info.and_then(|i| Url::parse(&i.final_url).ok()).map(|u| crate::batch::normalize(&u));
    let file_key = info.and_then(|i| {
        let e = i.etag.as_deref().filter(|e| strong_etag(e))?;
        Some((e.to_string(), i.content_length?))
    });
    let rank = |s: Option<DownloadStatus>| match s {
        Some(DownloadStatus::Completed) => 1,
        Some(DownloadStatus::Failed) => 2,
        None => 3,
        Some(_) => 0,
    };
    known
        .iter()
        .filter(|k| k.status != Some(DownloadStatus::Cancelled))
        .filter_map(|k| {
            let ku = Url::parse(&k.url).ok()?;
            let kl = crate::batch::normalize(&ku);
            let sums = checksum.zip(k.checksum.as_deref());
            let link_reason = if kl == link {
                Some(DuplicateReason::SameLink)
            } else if final_link.as_deref() == Some(kl.as_str()) {
                Some(DuplicateReason::SameFinalLink)
            } else {
                None
            };
            let (reason, confidence, changed) = match (sums, link_reason) {
                (Some((a, b)), _) if a.eq_ignore_ascii_case(b) => {
                    (link_reason.unwrap_or(DuplicateReason::SameChecksum), Confidence::Exact, false)
                }
                // The same link, but another content is expected now.
                (Some(_), Some(r)) => (r, Confidence::Possible, true),
                (_, Some(r)) => match info.and_then(|i| same_version(i, k)) {
                    Some(true) => (r, Confidence::Exact, false),
                    Some(false) => (r, Confidence::Possible, true),
                    None => (r, Confidence::Likely, false),
                },
                (_, None) => {
                    let (etag, size) = file_key.as_ref()?;
                    let same = k.etag.as_deref() == Some(etag.as_str()) && k.size == Some(*size) && site(&ku) == site(url);
                    if !same || sums.is_some() {
                        return None; // (another checksum: not the same content, whatever the ETag says)
                    }
                    (DuplicateReason::SameFile, Confidence::Likely, false)
                }
            };
            let dto = DuplicateDto {
                id: k.id,
                history_id: k.history_id,
                status: k.status,
                filename: k.filename.clone(),
                reason,
                confidence,
                changed,
                file: None,
                openable: false,
            };
            Some(((rank(k.status), confidence), dto))
        })
        .min_by_key(|(r, _)| *r)
        .map(|(_, d)| d)
}

/// What a completed match's file looks like now, from its metadata (`None`: it could not be read) and the size
/// it was saved with.
pub fn file_state(meta: Option<std::io::Result<u64>>, saved: Option<u64>) -> FileState {
    match meta {
        None => FileState::Unknown,
        Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => FileState::Missing,
        Some(Err(_)) => FileState::Unknown,
        Some(Ok(len)) if saved.is_some_and(|s| s != len) => FileState::Changed,
        Some(Ok(_)) => FileState::Present,
    }
}

/// Request to look for a duplicate of a link about to be added (the Add bar / dialog). Read-only: no network
/// request, only the downloads and history the app already has and a quick look at a finished file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckDuplicate {
    pub url: String,
    /// The checksum the user gave for the new download, if any.
    pub checksum: Option<String>,
}

/// Request to check a link from the Add dialog. `url: None` cancels a check still running.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalyzeLink {
    pub url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(f: impl FnOnce(&mut ResourceInfo)) -> ResourceInfo {
        let mut i = ResourceInfo {
            final_url: "https://dl.example.com/a.zip".into(),
            location: None,
            content_length: Some(4 << 30),
            content_type: Some("application/zip".into()),
            content_disposition: None,
            accept_ranges_header: true,
            range_verified: true,
            etag: Some("\"abc\"".into()),
            last_modified: None,
            server: None,
            status: 206,
        };
        f(&mut i);
        i
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn run(u: &Url, i: Option<&ResourceInfo>, failure: Option<ProbeFailure>, free: Option<u64>) -> AnalysisDto {
        assess(
            Observed {
                url: u,
                info: i,
                failure,
                credentials_sent: false,
                filename: "a.zip".into(),
                expected_name: "a.zip",
                disk: DiskObservation { free, fat: false },
                duplicate: None,
                name_conflict: None,
                now_ms: 1_700_000_000_000,
            },
            |s| downloader_core::planner::connection_limits(s, 16, 256 * 1024),
        )
    }

    fn codes(a: &AnalysisDto) -> Vec<FindingCode> {
        a.findings.iter().map(|f| f.code).collect()
    }

    #[test]
    fn ranges_with_a_validator_mean_resume_and_parallel() {
        let u = url("https://dl.example.com/a.zip");
        let a = run(&u, Some(&info(|_| {})), None, Some(1 << 40));
        assert_eq!(a.kind, ResourceKind::File);
        assert_eq!(a.resume, ResumeSupport::Supported);
        assert_eq!(a.strategy, Strategy::Parallel);
        assert_eq!(a.connections, Some(16), "the engine's own start for a 4 GiB file, capped by the setting");
        assert_eq!((a.access, a.storage, a.health), (Access::Public, StorageCheck::Ok, Health::Good));
        assert!(a.https && a.etag && a.range_verified && a.range_claimed);
        assert!(a.findings.is_empty(), "{:?}", a.findings);
    }

    #[test]
    fn ranges_without_a_validator_are_only_likely() {
        let u = url("https://dl.example.com/a.zip");
        let a = run(&u, Some(&info(|i| i.etag = None)), None, Some(1 << 40));
        assert_eq!(a.resume, ResumeSupport::LikelySupported);
        assert!(codes(&a).contains(&FindingCode::ResumeUnverified));
        let a = run(&u, Some(&info(|i| (i.etag, i.last_modified) = (None, Some("Mon".into())))), None, Some(1 << 40));
        assert_eq!(a.resume, ResumeSupport::Supported, "Last-Modified is a validator too");
    }

    #[test]
    fn no_ranges_means_no_resume_and_one_connection() {
        let u = url("https://dl.example.com/a.zip");
        let a = run(&u, Some(&info(|i| (i.range_verified, i.accept_ranges_header, i.status) = (false, false, 200))), None, Some(1 << 40));
        assert_eq!(a.resume, ResumeSupport::NotSupported);
        assert_eq!(a.strategy, Strategy::Single, "never split a file the server sends whole");
        assert_eq!(a.connections, None);
        assert!(codes(&a).contains(&FindingCode::NoResume));
        // Claimed but not honoured: what the server did wins.
        let a = run(&u, Some(&info(|i| (i.range_verified, i.status) = (false, 200))), None, Some(1 << 40));
        assert_eq!(a.resume, ResumeSupport::NotSupported);
        assert!(a.range_claimed && !a.range_verified);
    }

    #[test]
    fn a_small_file_is_not_split() {
        let u = url("https://dl.example.com/a.zip");
        let a = run(&u, Some(&info(|i| i.content_length = Some(100_000))), None, Some(1 << 40));
        assert_eq!((a.strategy, a.resume), (Strategy::Single, ResumeSupport::Supported));
    }

    #[test]
    fn an_unknown_size_is_never_invented() {
        let u = url("https://dl.example.com/a.zip");
        let a = run(&u, Some(&info(|i| (i.content_length, i.range_verified, i.status) = (None, false, 200))), None, Some(1 << 40));
        assert_eq!(a.size, None);
        assert_eq!(a.strategy, Strategy::Conservative);
        assert_eq!(a.storage, StorageCheck::CannotVerify);
        assert!(codes(&a).contains(&FindingCode::UnknownSize) && codes(&a).contains(&FindingCode::StorageUnverified));
        // Parts proved but no total: the engine cannot resume it.
        let a = run(&u, Some(&info(|i| i.content_length = None)), None, Some(1 << 40));
        assert_eq!(a.resume, ResumeSupport::NotSupported);
    }

    #[test]
    fn storage_is_checked_against_the_size() {
        let u = url("https://dl.example.com/a.zip");
        let i = info(|i| i.content_length = Some(1000));
        assert_eq!(run(&u, Some(&i), None, Some(999)).storage, StorageCheck::Insufficient);
        assert_eq!(run(&u, Some(&i), None, Some(999)).health, Health::Problem);
        assert_eq!(run(&u, Some(&i), None, Some(1050)).storage, StorageCheck::Low);
        assert_eq!(run(&u, Some(&i), None, Some(1_000_000)).storage, StorageCheck::Ok);
        assert_eq!(run(&u, Some(&i), None, None).storage, StorageCheck::Unavailable);
        let fat = assess(
            Observed {
                url: &u,
                info: Some(&info(|_| {})),
                failure: None,
                credentials_sent: false,
                filename: "a.zip".into(),
                expected_name: "a.zip",
                disk: DiskObservation { free: Some(1 << 40), fat: true },
                duplicate: None,
                name_conflict: None,
                now_ms: 0,
            },
            |_| (1, 1),
        );
        assert_eq!(fat.storage, StorageCheck::TooLargeForDrive, "4 GiB on FAT32");
    }

    #[test]
    fn plain_http_and_a_downgrade_are_flagged() {
        let u = url("http://dl.example.com/a.zip");
        let a = run(&u, Some(&info(|i| i.final_url = u.to_string())), None, Some(1 << 40));
        assert!(!a.https && codes(&a).contains(&FindingCode::Insecure));
        assert_eq!(a.health, Health::Caution);
        let u = url("https://dl.example.com/a.zip");
        let a = run(
            &u,
            Some(&info(|i| (i.final_url, i.location) = ("http://cdn.other.net/a.zip".into(), Some("x".into())))),
            None,
            Some(1 << 40),
        );
        assert!(a.redirected && !a.https);
        assert!(codes(&a).contains(&FindingCode::HttpsDowngrade) && codes(&a).contains(&FindingCode::OtherSite));
        assert_eq!(a.final_host.as_deref(), Some("cdn.other.net"));
    }

    #[test]
    fn a_redirect_within_the_site_is_not_another_site() {
        let u = url("https://www.example.com/a.zip");
        let a = run(&u, Some(&info(|i| i.location = Some("x".into()))), None, Some(1 << 40));
        assert!(a.redirected && !codes(&a).contains(&FindingCode::OtherSite));
    }

    #[test]
    fn sign_in_is_classified_from_what_came_back() {
        let u = url("https://files.example.com/report.pdf");
        let a = run(&u, None, Some(ProbeFailure::Status(401)), Some(1 << 40));
        assert_eq!((a.access, a.health), (Access::AuthRequired, Health::Problem));
        assert_eq!(a.error_code.as_deref(), Some("http_401"));
        // Sent to the site's sign-in page.
        let login = info(|i| {
            i.final_url = "https://accounts.example.com/login?next=x".into();
            i.location = Some("x".into());
            i.content_type = Some("text/html".into());
        });
        let o = |creds| Observed {
            url: &u,
            info: Some(&login),
            failure: None,
            credentials_sent: creds,
            filename: "report.pdf".into(),
            expected_name: "report.pdf",
            disk: DiskObservation::default(),
            duplicate: None,
            name_conflict: None,
            now_ms: 0,
        };
        assert_eq!(assess(o(false), |_| (1, 1)).access, Access::BrowserSessionRequired);
        assert_eq!(assess(o(true), |_| (1, 1)).access, Access::AuthRequired, "the session it had was not enough");
        // A public file fetched with a browser session: whether the session is needed is not known.
        let mut ok = o(true);
        let plain = info(|_| {});
        ok.info = Some(&plain);
        let a = assess(ok, |_| (1, 1));
        assert_eq!(a.access, Access::Unknown);
        assert!(codes(&a).contains(&FindingCode::SessionSent));
        assert_eq!(run(&u, None, Some(ProbeFailure::Status(403)), None).access, Access::Unknown, "403 alone proves nothing");
    }

    #[test]
    fn a_web_page_instead_of_the_file_is_a_problem() {
        let u = url("https://dl.example.com/a.zip");
        let a = run(&u, Some(&info(|i| i.content_type = Some("text/html; charset=utf-8".into()))), None, Some(1 << 40));
        assert_eq!((a.kind, a.health), (ResourceKind::WebPage, Health::Problem));
        assert!(codes(&a).contains(&FindingCode::WebPage));
        assert_ne!(a.strategy, Strategy::Parallel);
    }

    #[test]
    fn failures_degrade_to_unknown() {
        let u = url("https://dl.example.com/a.zip");
        let a = run(&u, None, Some(ProbeFailure::Other("connection".into())), Some(1 << 40));
        assert_eq!(
            (a.kind, a.resume, a.strategy, a.health),
            (ResourceKind::Unknown, ResumeSupport::Unknown, Strategy::Conservative, Health::Unknown)
        );
        assert_eq!(a.storage, StorageCheck::CannotVerify);
        let a = run(&u, None, Some(ProbeFailure::Status(404)), Some(1 << 40));
        assert_eq!(a.health, Health::Problem);
        assert_eq!(a.findings[0].detail.as_deref(), Some("http_404"));
        let a = run(&u, None, Some(ProbeFailure::Status(503)), Some(1 << 40));
        assert_eq!(a.findings[0].level, Level::Warning, "a busy server may recover");
        let a = run(&u, None, Some(ProbeFailure::Slow), Some(1 << 40));
        assert!(codes(&a).contains(&FindingCode::SlowServer));
        assert_eq!(a.strategy, Strategy::Conservative);
    }

    #[test]
    fn streams_go_to_the_video_dialog() {
        let a = run(&url("https://v.example.com/live/index.m3u8"), None, None, Some(1 << 40));
        assert_eq!((a.kind, a.strategy), (ResourceKind::Stream, Strategy::Stream));
        let a = run(&url("https://v.example.com/x"), Some(&info(|i| i.content_type = Some("application/dash+xml".into()))), None, None);
        assert_eq!(a.kind, ResourceKind::Stream);
        assert_eq!(a.size, None, "a playlist's size is not the video's");
    }

    #[test]
    fn office_online_exports_are_browser_only_but_real_sharepoint_files_are_not() {
        // The user's case: Excel-for-the-web "Download as CSV".
        assert!(browser_only_link(&url("https://frc-excel.officeapps.live.com/x/_layouts/XlFileHandler.aspx?sheetName=DATA&downloadAsCsvEnabled=1&W=1")));
        assert!(browser_only_link(&url("https://officeapps.live.com/p/PptFileHandler.aspx?a=b")));
        // A genuine SharePoint / OneDrive file (a plain authenticated download) must stay downloadable.
        assert!(!browser_only_link(&url("https://contoso.sharepoint.com/sites/x/Shared%20Documents/report.xlsx")));
        assert!(!browser_only_link(&url("https://contoso-my.sharepoint.com/personal/u/_layouts/15/download.aspx?SourceUrl=/a.zip")));
        assert!(!browser_only_link(&url("https://example.com/officeapps.live.com.zip")));
    }

    #[test]
    fn temporary_links_are_detected_conservatively() {
        assert!(expiring(&url("https://b.s3.amazonaws.com/a.zip?X-Amz-Algorithm=A&X-Amz-Signature=s&X-Amz-Expires=60")).temporary);
        assert!(expiring(&url("https://d.cloudfront.net/a.zip?Expires=1700000000&Signature=s&Key-Pair-Id=k")).temporary);
        assert!(expiring(&url("https://a.blob.core.windows.net/c/a.zip?sv=2020&se=2030-01-01&sig=s")).temporary);
        assert!(expiring(&url("https://cdn.example.com/a.zip?expires=1700000000&token=t")).temporary);
        assert!(!expiring(&url("https://example.com/a.zip?expires=tomorrow")).temporary, "an expiry alone is not a signature");
        assert!(!expiring(&url("https://example.com/a.zip?id=3&v=2")).temporary);
        assert!(!expiring(&url("https://example.com/download?file=a.zip")).temporary);
        // A readable expiry.
        assert_eq!(expiring(&url("https://d.cloudfront.net/a?Expires=1700000000&Signature=s")).at_ms, Some(1_700_000_000_000));
        let s3 = expiring(&url("https://b.s3.amazonaws.com/a?X-Amz-Date=20231114T221320Z&X-Amz-Expires=3600&X-Amz-Signature=s"));
        assert_eq!(s3.at_ms, Some(1_700_000_000_000 + 3600 * 1000));
    }

    #[test]
    fn an_expired_link_is_a_problem_and_a_live_one_a_warning() {
        let u = url("https://d.cloudfront.net/a.zip?Expires=1600000000&Signature=s");
        let a = run(&u, None, Some(ProbeFailure::Status(403)), Some(1 << 40));
        assert!(a.temporary && codes(&a).contains(&FindingCode::ExpiredLink));
        let u = url("https://d.cloudfront.net/a.zip?Expires=1800000000&Signature=s");
        let a = run(&u, Some(&info(|_| {})), None, Some(1 << 40));
        assert!(codes(&a).contains(&FindingCode::TemporaryLink));
        assert_eq!(a.health, Health::Caution);
        // Temporary only after the redirect.
        let u = url("https://example.com/get/a.zip");
        let i = info(|i| (i.final_url, i.location) = ("https://b.s3.amazonaws.com/a.zip?X-Amz-Signature=s".into(), Some("x".into())));
        assert!(run(&u, Some(&i), None, Some(1 << 40)).temporary);
    }

    #[test]
    fn secrets_are_hidden_from_the_shown_link() {
        let s = display_url(&url("https://user:pw@d.cloudfront.net/a.zip?Expires=1700000000&Signature=abc&Key-Pair-Id=k"));
        assert!(!s.contains("pw") && !s.contains("abc") && !s.contains("=k"), "{s}");
        assert!(s.contains("Expires=1700000000") && s.contains("/a.zip"), "{s}");
        let s = display_url(&url("https://example.com/a.zip?token=secret&v=2"));
        assert!(!s.contains("secret") && s.contains("v=2"), "{s}");
        assert_eq!(display_url(&url("https://example.com/a.zip?v=2#x")), "https://example.com/a.zip?v=2#x", "nothing to hide: unchanged");
    }

    fn known(id: i64, u: &str, name: &str, status: DownloadStatus, etag: Option<&str>, size: Option<u64>) -> Known {
        Known {
            id: Some(id),
            history_id: None,
            url: u.into(),
            filename: name.into(),
            status: Some(status),
            etag: etag.map(str::to_string),
            last_modified: None,
            size,
            checksum: None,
            destination: None,
        }
    }

    #[test]
    fn duplicates_need_more_than_a_file_name() {
        let u = url("https://dl.example.com/a.zip");
        let list = [known(1, "https://other.example.org/a.zip", "a.zip", DownloadStatus::Completed, None, None)];
        assert_eq!(find_duplicate(&u, None, None, &list), None, "same name, different link");
        let list = [
            known(1, "https://dl.example.com/a.zip#part", "a.zip", DownloadStatus::Completed, None, None),
            known(2, "https://dl.example.com/a.zip", "a.zip", DownloadStatus::Paused, None, None),
            known(3, "https://dl.example.com/a.zip", "a.zip", DownloadStatus::Cancelled, None, None),
        ];
        let d = find_duplicate(&u, None, None, &list).unwrap();
        assert_eq!((d.id, d.reason), (Some(2), DuplicateReason::SameLink), "the paused one comes first, cancelled never");
    }

    #[test]
    fn the_same_file_under_another_link() {
        let u = url("https://dl.example.com/a.zip?sig=2");
        let i = info(|i| i.final_url = "https://dl.example.com/a.zip?sig=2".into());
        let list = [known(5, "https://dl.example.com/a.zip?sig=1", "a.zip", DownloadStatus::Completed, Some("\"abc\""), Some(4 << 30))];
        assert_eq!(find_duplicate(&u, Some(&i), None, &list).unwrap().reason, DuplicateReason::SameFile);
        let other_size = [known(5, "https://dl.example.com/a.zip?sig=1", "a.zip", DownloadStatus::Completed, Some("\"abc\""), Some(1))];
        assert_eq!(find_duplicate(&u, Some(&i), None, &other_size), None);
        let weak = info(|i| i.etag = Some("W/\"abc\"".into()));
        let list_weak = [known(5, "https://dl.example.com/b.zip", "a.zip", DownloadStatus::Completed, Some("W/\"abc\""), Some(4 << 30))];
        assert_eq!(find_duplicate(&u, Some(&weak), None, &list_weak), None, "a weak ETag proves nothing");
        // Redirected to a link the list already has.
        let r = info(|i| i.final_url = "https://cdn.example.net/a.zip".into());
        let list = [known(7, "https://cdn.example.net/a.zip", "a.zip", DownloadStatus::Queued, None, None)];
        assert_eq!(find_duplicate(&url("https://example.com/get"), Some(&r), None, &list).unwrap().reason, DuplicateReason::SameFinalLink);
        // Only in the history.
        let h = [Known {
            id: None,
            history_id: Some(9),
            status: None,
            ..known(0, "https://example.com/get", "a.zip", DownloadStatus::Completed, None, None)
        }];
        let d = find_duplicate(&url("https://example.com/get"), None, None, &h).unwrap();
        assert_eq!((d.id, d.history_id, d.status), (None, Some(9), None));
    }

    #[test]
    fn links_are_normalised_conservatively() {
        let list = [known(1, "https://dl.example.com/a.zip?v=1", "a.zip", DownloadStatus::Completed, None, None)];
        // Host case, default port and a fragment do not change the resource.
        for same in ["https://DL.Example.COM/a.zip?v=1", "https://dl.example.com:443/a.zip?v=1", "https://dl.example.com/a.zip?v=1#top"] {
            let d = find_duplicate(&url(same), None, None, &list).unwrap_or_else(|| panic!("{same}"));
            assert_eq!((d.reason, d.confidence), (DuplicateReason::SameLink, Confidence::Likely), "{same}");
        }
        // Another query, another path, another scheme or port: maybe another resource, never matched by the link.
        for other in [
            "https://dl.example.com/a.zip?v=2",
            "https://dl.example.com/a.zip",
            "https://dl.example.com/b/a.zip?v=1",
            "http://dl.example.com/a.zip?v=1",
            "https://dl.example.com:8443/a.zip?v=1",
            "https://dl.example.com/A.zip?v=1",
        ] {
            assert_eq!(find_duplicate(&url(other), None, None, &list), None, "{other}");
        }
        // A signed link with another signature is not the same link either (only a strong ETag and size prove it).
        let signed = [known(2, "https://dl.example.com/a.zip?X-Amz-Signature=aaa", "a.zip", DownloadStatus::Completed, None, None)];
        assert_eq!(find_duplicate(&url("https://dl.example.com/a.zip?X-Amz-Signature=bbb"), None, None, &signed), None);
        // A malformed stored link is skipped, not an error.
        let bad = [known(3, "not a link", "a.zip", DownloadStatus::Completed, None, None)];
        assert_eq!(find_duplicate(&url("https://dl.example.com/a.zip"), None, None, &bad), None);
    }

    #[test]
    fn the_version_decides_how_sure_a_same_link_match_is() {
        let u = url("https://dl.example.com/a.zip");
        let i = info(|_| {}); // strong "abc", 4 GiB, no date
        let k = |etag: Option<&str>, size: Option<u64>, date: Option<&str>| Known {
            last_modified: date.map(str::to_string),
            ..known(1, "https://dl.example.com/a.zip", "a.zip", DownloadStatus::Completed, etag, size)
        };
        let got = |k: Known, i: &ResourceInfo| {
            let d = find_duplicate(&u, Some(i), None, &[k]).unwrap();
            (d.confidence, d.changed)
        };
        assert_eq!(got(k(Some("\"abc\""), Some(4 << 30), None), &i), (Confidence::Exact, false), "same strong ETag");
        assert_eq!(got(k(Some("\"new\""), Some(4 << 30), None), &i), (Confidence::Possible, true), "the ETag changed");
        assert_eq!(got(k(None, Some(1), None), &i), (Confidence::Possible, true), "the size changed");
        assert_eq!(got(k(None, Some(4 << 30), None), &i), (Confidence::Likely, false), "same size only: not proof");
        assert_eq!(got(k(None, None, None), &i), (Confidence::Likely, false), "nothing known");
        let dated = info(|i| {
            i.etag = None;
            i.last_modified = Some("Tue, 01 Oct 2026 10:00:00 GMT".into());
        });
        let old = Some("Mon, 01 Sep 2026 10:00:00 GMT");
        assert_eq!(got(k(None, Some(4 << 30), old), &dated), (Confidence::Possible, true), "another date");
        let same = Some("Tue, 01 Oct 2026 10:00:00 GMT");
        assert_eq!(got(k(None, Some(4 << 30), same), &dated), (Confidence::Exact, false), "same size and date");
        // Without the server's answer (the Add bar asks nothing), a same-link match is likely, never exact.
        let d = find_duplicate(&u, None, None, &[k(Some("\"abc\""), Some(4 << 30), None)]).unwrap();
        assert_eq!((d.confidence, d.changed), (Confidence::Likely, false));
    }

    #[test]
    fn checksums_prove_the_content() {
        let sum = |s: &str| Known {
            checksum: Some(s.into()),
            ..known(1, "https://a.example.com/x.iso", "x.iso", DownloadStatus::Completed, None, None)
        };
        // The same checksum under a link of another site: the same content.
        let d = find_duplicate(&url("https://mirror.example.net/x.iso"), None, Some("sha256:AB12"), &[sum("sha256:ab12")]).unwrap();
        assert_eq!((d.reason, d.confidence), (DuplicateReason::SameChecksum, Confidence::Exact));
        // The same link but another checksum: another content is expected now.
        let d = find_duplicate(&url("https://a.example.com/x.iso"), None, Some("sha256:ffff"), &[sum("sha256:ab12")]).unwrap();
        assert_eq!((d.reason, d.confidence, d.changed), (DuplicateReason::SameLink, Confidence::Possible, true));
        // Another checksum and another link: nothing, even with the same name.
        assert_eq!(find_duplicate(&url("https://b.example.com/x.iso"), None, Some("sha256:ffff"), &[sum("sha256:ab12")]), None);
    }

    #[test]
    fn running_matches_and_surer_matches_come_first() {
        let u = url("https://dl.example.com/a.zip");
        let i = info(|_| {});
        let list = [
            known(1, "https://dl.example.com/a.zip", "a.zip", DownloadStatus::Completed, Some("\"old\""), None),
            known(2, "https://dl.example.com/a.zip", "a (1).zip", DownloadStatus::Completed, Some("\"abc\""), None),
            known(3, "https://dl.example.com/a.zip", "a (2).zip", DownloadStatus::Failed, None, None),
        ];
        assert_eq!(find_duplicate(&u, Some(&i), None, &list).unwrap().id, Some(2), "the copy of the current version");
        let mut with_active = list.to_vec();
        with_active.push(known(4, "https://dl.example.com/a.zip", "a (3).zip", DownloadStatus::Downloading, None, None));
        assert_eq!(find_duplicate(&u, Some(&i), None, &with_active).unwrap().id, Some(4), "the running one");
        let failed = [known(3, "https://dl.example.com/a.zip", "a.zip", DownloadStatus::Failed, None, None)];
        let d = find_duplicate(&u, None, None, &failed).unwrap();
        assert_eq!((d.status, d.file), (Some(DownloadStatus::Failed), None), "a failed one is told as failed, no file claimed");
    }

    #[test]
    fn a_finished_file_is_told_as_it_is_on_disk() {
        use std::io::{Error, ErrorKind};
        assert_eq!(file_state(Some(Ok(10)), Some(10)), FileState::Present);
        assert_eq!(file_state(Some(Ok(10)), None), FileState::Present);
        assert_eq!(file_state(Some(Ok(9)), Some(10)), FileState::Changed);
        assert_eq!(file_state(Some(Err(Error::from(ErrorKind::NotFound))), Some(10)), FileState::Missing);
        assert_eq!(file_state(Some(Err(Error::from(ErrorKind::PermissionDenied))), Some(10)), FileState::Unknown);
        assert_eq!(file_state(None, Some(10)), FileState::Unknown);
    }
}
