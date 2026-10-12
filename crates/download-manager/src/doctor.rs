//! Download Doctor: why a download failed (or is retrying), where the problem is, whether retrying can help, and
//! what the user can do, from what was actually observed.
//!
//! [`diagnose`] is a pure function of an [`Evidence`]: the failure itself (classified from the engine's error,
//! including its source chain, see `manager/doctor.rs`), the attempt's history (retries, dropped connections,
//! throttling, a restart from zero), what happened before (data received, the previous failure), the link (a
//! signed, expiring link) and the computer (online or not, free space). Nothing here retries or changes anything:
//! the engine's retry loop (`downloader_core::retry`) stays the only automatic recovery, and its rules are
//! mirrored, never overridden. Unknown evidence gives `unknown`, never a guess.

use serde::{Deserialize, Serialize};

/// What failed, as observed (before correlation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Failure {
    /// The server's name could not be resolved.
    Dns,
    /// No answer in time.
    Timeout,
    /// The connection was reset or closed mid-way.
    ConnectionReset,
    /// The connection could not be opened (refused, unreachable, proxy...).
    ConnectionFailed,
    /// The secure connection could not be set up (certificate, handshake).
    Tls,
    /// The server answered with this status.
    Http(u16),
    /// Too many redirects, a loop, or a redirect the app refuses (to this computer / network).
    Redirect,
    /// The server stopped honouring byte ranges in the middle of a transfer.
    RangeRejected,
    /// A ranged answer described another version of the file (another strong `ETag` or total size).
    SourceChanged,
    DiskFull,
    /// The folder or file cannot be written (permissions, a file open elsewhere).
    Permission,
    /// The folder is not one the app may write to.
    FolderNotAllowed,
    /// Any other write / read failure on this computer.
    WriteFailed,
    /// A file of that name appeared and cannot be replaced.
    FileExists,
    /// The checksum given by the user does not match.
    Checksum,
    /// Fewer bytes than announced, or a different size at the end.
    Incomplete,
    /// The app's own database (progress, state) could not be written or read.
    Persistence,
    /// A web page came back where the file was expected.
    WebPage,
    /// The request itself is not valid (scheme, address...).
    InvalidRequest,
    ProxyInvalid,
    Cancelled,
    Unknown,
}

/// The diagnosis, after correlating the evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    /// This computer has no route to the internet.
    Offline,
    Dns,
    Timeout,
    ConnectionReset,
    /// Could not connect, while the computer is (or may be) online.
    ConnectionFailed,
    Tls,
    /// 401: a user name and password are needed.
    AuthRequired,
    /// 407.
    ProxyAuth,
    /// 403 with nothing that points to a cause.
    AccessDenied,
    /// 401 / 403 / a sign-in page after data had already arrived with a browser session: the session expired.
    SessionExpired,
    /// A signed link whose stated expiry has passed (or a 403 / 404 / 410 on a signed link).
    LinkExpired,
    /// 404 / 410.
    NotFound,
    /// 408.
    RequestTimeout,
    /// 409.
    Conflict,
    /// 429.
    RateLimited,
    /// 5xx the engine retries (500, 502, 503, 504): temporary on the server's side.
    ServerTemporary,
    /// Other 5xx (501, 505...): the server cannot serve this request.
    ServerError,
    /// Any other HTTP status.
    HttpOther,
    Redirect,
    /// Byte ranges refused mid-transfer, or 416: the saved part cannot be continued as it is.
    RangeFailure,
    DiskFull,
    Permission,
    FolderNotAllowed,
    WriteFailed,
    FileExists,
    Integrity,
    Incomplete,
    Persistence,
    /// A web page instead of the file (no session evidence).
    WebPage,
    InvalidResource,
    ProxyInvalid,
    Cancelled,
    Unknown,
}

/// Where the problem is. The network is only blamed with evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// The server rejected or failed the request.
    Server,
    /// The path between this computer and the server (connection, DNS, proxy).
    Network,
    /// This computer (disk, folder, the app's data).
    ThisComputer,
    /// The link itself (expired, missing, not a file).
    Link,
    /// Sign-in or session.
    Account,
    /// The content received (checksum, size).
    Content,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RetryClass {
    Retryable,
    /// Retrying helps, but only after a wait (`retry_after_secs` when the server said how long).
    RetryableWithDelay,
    /// Retrying fails again until the user does something (sign in, free space, a fresh link...).
    RetryAfterUserAction,
    NotRetryable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Info,
    /// Retrying on its own right now.
    Warning,
    /// Stopped, and Retry can fix it.
    Recoverable,
    /// Stopped until the user acts (or for good).
    Blocking,
    /// The content or the app's data cannot be trusted as it is.
    Critical,
}

/// What the user can do. Only actions the app supports: `retry`, `signIn`, `openFolder` and `cancel` are
/// buttons; the others are advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    Retry,
    /// Retry, but after the wait the server asked for.
    RetryLater,
    /// Retry: the engine checks the server again and starts from zero if the saved part cannot be continued.
    RestartSafely,
    /// Enter a user name and password (the existing sign-in dialog).
    SignIn,
    /// Get the file again from the page, signed in, in the browser.
    ReauthInBrowser,
    /// Get a fresh link from the page and add it.
    NewLink,
    /// Free space on the drive (the button opens the download folder).
    FreeSpace,
    /// Check the internet connection, VPN or proxy.
    CheckConnection,
    /// Check Settings > Network.
    CheckProxy,
    /// Close the program that holds the file, or fix the folder's permissions.
    CheckFolder,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FactorCode {
    /// The engine already retried `n` times in this attempt.
    RepeatedRetries,
    /// `n` connections dropped or timed out and were retried while data still arrived.
    UnstableConnection,
    /// The server limited connections (429 / 503) during this attempt.
    ServerThrottled,
    /// Data had arrived before the failure (the link worked).
    WorkedBefore,
    /// A browser session (cookies) or a sign-in was sent.
    SessionUsed,
    /// The link is signed / temporary.
    TemporaryLink,
    /// The previous attempt failed the same way.
    SameAsBefore,
    /// The computer reports no internet route.
    Offline,
    /// The file changed on the server: the saved part was dropped and the download started over.
    StartedOver,
    /// The server does not support resuming: a retry starts from zero.
    NoResume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Factor {
    pub code: FactorCode,
    pub n: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnosis {
    pub category: Category,
    /// Stable short name (`http_503`, `tls`, `link_expired`...), for logs, support and translations.
    pub code: String,
    pub source: Source,
    pub http_status: Option<u16>,
    pub retry: RetryClass,
    /// The wait the server asked for (`Retry-After`), when it did.
    pub retry_after_secs: Option<u64>,
    /// While the engine retries on its own: when the next try starts (Unix ms).
    pub retry_at_ms: Option<u64>,
    pub severity: Severity,
    /// What else the evidence shows (most telling first).
    pub factors: Vec<Factor>,
    pub actions: Vec<Action>,
    /// Disk full with numbers: bytes needed and free.
    pub needed_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

/// Everything a diagnosis is made from. `Default` is "nothing known".
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    pub failure: Option<Failure>,
    /// `Retry-After` of the last answer.
    pub retry_after_secs: Option<u64>,
    /// Automatic retries in the failed attempt.
    pub run_retries: u32,
    /// Of those, connection drops / timeouts / short bodies.
    pub drops: u32,
    pub throttled: bool,
    pub started_over: bool,
    /// Bytes had been received before the failure (in this attempt or an earlier one).
    pub worked_before: bool,
    /// The previous attempt ended with the same error code.
    pub same_as_before: bool,
    pub session: bool,
    /// The link is signed / temporary; `link_expired`: its stated expiry has passed.
    pub temporary_link: bool,
    pub link_expired: bool,
    pub online: Option<bool>,
    /// The saved transfer can be resumed (ranges verified, size known).
    pub resumable: bool,
    pub downloaded: u64,
    /// "N bytes needed, M available" when the engine said so.
    pub space: Option<(u64, u64)>,
    /// The engine is retrying right now; the next try is at this time.
    pub retry_at_ms: Option<u64>,
}

impl Failure {
    /// The failure an engine error text describes, as far as text can tell (used when the structured error is
    /// gone, e.g. after a restart). The codes are those of [`crate::explain::error_code`].
    pub fn from_code(code: &str) -> Failure {
        if let Some(s) = code.strip_prefix("http_").and_then(|s| s.parse().ok()) {
            return Failure::Http(s);
        }
        match code {
            "dns" => Failure::Dns,
            "timeout" => Failure::Timeout,
            "connection" => Failure::ConnectionFailed,
            "connection_reset" => Failure::ConnectionReset,
            "tls" => Failure::Tls,
            "redirect_refused" | "redirect_loop" => Failure::Redirect,
            "range_not_honored" => Failure::RangeRejected,
            "source_changed" => Failure::SourceChanged,
            "disk_full" => Failure::DiskFull,
            "access_denied" => Failure::Permission,
            "folder_not_allowed" => Failure::FolderNotAllowed,
            "write_failed" => Failure::WriteFailed,
            "file_exists" => Failure::FileExists,
            "checksum_mismatch" => Failure::Checksum,
            "incomplete" => Failure::Incomplete,
            "persistence" => Failure::Persistence,
            "web_page" => Failure::WebPage,
            "invalid_request" => Failure::InvalidRequest,
            "proxy_invalid" => Failure::ProxyInvalid,
            "cancelled" => Failure::Cancelled,
            _ => Failure::Unknown,
        }
    }
}

/// The diagnosis of `e`. See the module documentation.
pub fn diagnose(e: &Evidence) -> Diagnosis {
    use Action::*;
    use Category as C;
    use RetryClass as R;
    use Severity as S;
    use Source as Src;

    let failure = e.failure.unwrap_or(Failure::Unknown);
    let http_status = match failure {
        Failure::Http(s) => Some(s),
        _ => None,
    };
    let offline = e.online == Some(false);
    let session_lost = e.session && e.worked_before;

    let (category, source, retry) = match failure {
        Failure::Dns | Failure::ConnectionFailed | Failure::Timeout if offline => (C::Offline, Src::Network, R::RetryAfterUserAction),
        // The name was not found while the computer is online: a wrong link or a DNS problem; text cannot tell which.
        Failure::Dns => (C::Dns, Src::Network, R::Unknown),
        Failure::Timeout => (C::Timeout, Src::Network, R::Retryable),
        Failure::ConnectionReset => (C::ConnectionReset, Src::Network, R::Retryable),
        Failure::ConnectionFailed => (C::ConnectionFailed, Src::Network, R::Retryable),
        // Certificates do not fix themselves; an interrupted handshake might, but the text rarely says which.
        Failure::Tls => (C::Tls, Src::Server, R::NotRetryable),
        Failure::Http(s) => match s {
            401 if session_lost => (C::SessionExpired, Src::Account, R::RetryAfterUserAction),
            401 => (C::AuthRequired, Src::Account, R::RetryAfterUserAction),
            407 => (C::ProxyAuth, Src::Network, R::RetryAfterUserAction),
            403 | 404 | 410 if e.link_expired => (C::LinkExpired, Src::Link, R::RetryAfterUserAction),
            403 if session_lost => (C::SessionExpired, Src::Account, R::RetryAfterUserAction),
            403 if e.temporary_link && e.worked_before => (C::LinkExpired, Src::Link, R::RetryAfterUserAction),
            403 => (C::AccessDenied, Src::Server, R::RetryAfterUserAction),
            404 | 410 => (C::NotFound, Src::Link, R::NotRetryable),
            408 => (C::RequestTimeout, Src::Server, R::Retryable),
            409 => (C::Conflict, Src::Server, R::Unknown),
            416 => (C::RangeFailure, Src::Server, R::Retryable),
            429 => (C::RateLimited, Src::Server, R::RetryableWithDelay),
            500 | 502 | 503 | 504 => (C::ServerTemporary, Src::Server, R::RetryableWithDelay),
            500..=599 => (C::ServerError, Src::Server, R::NotRetryable),
            _ => (C::HttpOther, Src::Server, R::Unknown),
        },
        Failure::Redirect => (C::Redirect, Src::Link, R::NotRetryable),
        Failure::RangeRejected => (C::RangeFailure, Src::Server, R::Retryable),
        // The saved parts are kept apart from the new version; a retry checks the server and starts over if needed.
        Failure::SourceChanged => (C::Integrity, Src::Content, R::Retryable),
        Failure::DiskFull => (C::DiskFull, Src::ThisComputer, R::RetryAfterUserAction),
        Failure::Permission => (C::Permission, Src::ThisComputer, R::RetryAfterUserAction),
        Failure::FolderNotAllowed => (C::FolderNotAllowed, Src::ThisComputer, R::RetryAfterUserAction),
        Failure::WriteFailed => (C::WriteFailed, Src::ThisComputer, R::Unknown),
        Failure::FileExists => (C::FileExists, Src::ThisComputer, R::RetryAfterUserAction),
        // A second mismatch in a row: the expected checksum (or the server's file) is wrong, retrying will not help.
        Failure::Checksum if e.same_as_before => (C::Integrity, Src::Content, R::NotRetryable),
        Failure::Checksum => (C::Integrity, Src::Content, R::Retryable),
        Failure::Incomplete => (C::Incomplete, Src::Server, R::Retryable),
        Failure::Persistence => (C::Persistence, Src::ThisComputer, R::Unknown),
        Failure::WebPage if session_lost => (C::SessionExpired, Src::Account, R::RetryAfterUserAction),
        Failure::WebPage => (C::WebPage, Src::Link, R::RetryAfterUserAction),
        Failure::InvalidRequest => (C::InvalidResource, Src::Link, R::NotRetryable),
        Failure::ProxyInvalid => (C::ProxyInvalid, Src::Network, R::RetryAfterUserAction),
        Failure::Cancelled => (C::Cancelled, Src::Unknown, R::NotRetryable),
        Failure::Unknown => (C::Unknown, Src::Unknown, R::Unknown),
    };

    // Contributing factors: only what the evidence shows.
    let mut factors = Vec::new();
    let mut add = |code, n: Option<u32>| factors.push(Factor { code, n });
    if offline && category != C::Offline {
        add(FactorCode::Offline, None);
    }
    if e.drops >= 2 && e.worked_before && matches!(source, Src::Network | Src::Server | Src::Unknown) {
        add(FactorCode::UnstableConnection, Some(e.drops));
    }
    if e.throttled {
        add(FactorCode::ServerThrottled, None);
    }
    if e.run_retries >= 3 {
        add(FactorCode::RepeatedRetries, Some(e.run_retries));
    }
    if e.worked_before && matches!(category, C::SessionExpired | C::LinkExpired | C::AccessDenied | C::NotFound) {
        add(FactorCode::WorkedBefore, None);
    }
    if e.session && matches!(source, Src::Account | Src::Link) {
        add(FactorCode::SessionUsed, None);
    }
    if e.temporary_link && category != C::LinkExpired {
        add(FactorCode::TemporaryLink, None);
    }
    if e.same_as_before && category != C::Cancelled {
        add(FactorCode::SameAsBefore, None);
    }
    if e.started_over {
        add(FactorCode::StartedOver, None);
    }
    if !e.resumable && e.downloaded > 0 && matches!(retry, R::Retryable | R::RetryableWithDelay) {
        add(FactorCode::NoResume, None);
    }

    // What the user can do (only what the app supports as a button; the rest is advice).
    let mut actions = match category {
        C::Offline => vec![CheckConnection, Retry],
        C::Dns => vec![CheckConnection, NewLink, Retry],
        C::Timeout | C::ConnectionReset | C::ConnectionFailed | C::RequestTimeout | C::Incomplete => vec![Retry],
        C::Tls => vec![CheckConnection],
        C::AuthRequired => vec![SignIn],
        C::ProxyAuth | C::ProxyInvalid => vec![CheckProxy],
        C::SessionExpired => vec![ReauthInBrowser],
        C::LinkExpired => vec![NewLink],
        C::AccessDenied => vec![ReauthInBrowser, NewLink, SignIn],
        C::NotFound => vec![NewLink],
        C::Conflict | C::HttpOther | C::Unknown | C::WriteFailed | C::Persistence => vec![Retry],
        C::RateLimited | C::ServerTemporary => vec![RetryLater],
        C::ServerError | C::Redirect | C::InvalidResource => vec![NewLink],
        C::RangeFailure => vec![RestartSafely],
        C::DiskFull => vec![FreeSpace, Retry],
        C::Permission | C::FolderNotAllowed | C::FileExists => vec![CheckFolder, Retry],
        C::Integrity if retry == R::Retryable => vec![RestartSafely],
        C::Integrity => vec![NewLink],
        C::WebPage => vec![ReauthInBrowser, NewLink],
        C::Cancelled => vec![],
    };
    if category != C::Cancelled {
        actions.push(Cancel);
    }

    let severity = if e.retry_at_ms.is_some() {
        S::Warning
    } else {
        match (category, retry) {
            (C::Cancelled, _) => S::Info,
            (C::Integrity | C::Persistence, _) => S::Critical,
            (_, R::Retryable | R::RetryableWithDelay) => S::Recoverable,
            (_, R::Unknown) => S::Recoverable,
            _ => S::Blocking,
        }
    };

    let code = match category {
        C::Offline => "offline".to_string(),
        C::SessionExpired => "session_expired".to_string(),
        C::LinkExpired => "link_expired".to_string(),
        C::Tls => "tls".to_string(),
        C::ConnectionReset => "connection_reset".to_string(),
        C::Redirect => "redirect".to_string(),
        C::RangeFailure if http_status.is_none() => "range_not_honored".to_string(),
        C::Integrity if failure == Failure::SourceChanged => "source_changed".to_string(),
        C::Integrity => "checksum_mismatch".to_string(),
        C::Persistence => "persistence".to_string(),
        C::InvalidResource => "invalid_request".to_string(),
        C::WriteFailed => "write_failed".to_string(),
        C::Cancelled => "cancelled".to_string(),
        _ => match failure {
            Failure::Http(s) => format!("http_{s}"),
            f => code_of(f).to_string(),
        },
    };

    Diagnosis {
        category,
        code,
        source,
        http_status,
        retry,
        retry_after_secs: e.retry_after_secs.filter(|_| matches!(retry, R::RetryableWithDelay | R::Retryable)),
        retry_at_ms: e.retry_at_ms,
        severity,
        factors,
        actions,
        needed_bytes: e.space.map(|s| s.0),
        free_bytes: e.space.map(|s| s.1),
    }
}

fn code_of(f: Failure) -> &'static str {
    match f {
        Failure::Dns => "dns",
        Failure::Timeout => "timeout",
        Failure::ConnectionReset => "connection_reset",
        Failure::ConnectionFailed => "connection",
        Failure::Tls => "tls",
        Failure::Http(_) => "http",
        Failure::Redirect => "redirect",
        Failure::RangeRejected => "range_not_honored",
        Failure::SourceChanged => "source_changed",
        Failure::DiskFull => "disk_full",
        Failure::Permission => "access_denied",
        Failure::FolderNotAllowed => "folder_not_allowed",
        Failure::WriteFailed => "write_failed",
        Failure::FileExists => "file_exists",
        Failure::Checksum => "checksum_mismatch",
        Failure::Incomplete => "incomplete",
        Failure::Persistence => "persistence",
        Failure::WebPage => "web_page",
        Failure::InvalidRequest => "invalid_request",
        Failure::ProxyInvalid => "proxy_invalid",
        Failure::Cancelled => "cancelled",
        Failure::Unknown => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(f: Failure) -> Evidence {
        Evidence { failure: Some(f), online: Some(true), resumable: true, ..Default::default() }
    }
    fn has(d: &Diagnosis, c: FactorCode) -> bool {
        d.factors.iter().any(|f| f.code == c)
    }

    #[test]
    fn server_failures_are_the_servers_and_wait_before_retrying() {
        let d = diagnose(&Evidence { run_retries: 5, throttled: true, retry_after_secs: Some(30), ..ev(Failure::Http(503)) });
        assert_eq!((d.category, d.source, d.retry), (Category::ServerTemporary, Source::Server, RetryClass::RetryableWithDelay));
        assert_eq!(d.retry_after_secs, Some(30));
        assert_eq!(d.severity, Severity::Recoverable);
        assert_eq!(d.actions, [Action::RetryLater, Action::Cancel]);
        assert!(has(&d, FactorCode::ServerThrottled) && has(&d, FactorCode::RepeatedRetries));
        assert!(!has(&d, FactorCode::UnstableConnection), "the network is not blamed for a server error");
        let d = diagnose(&Evidence { retry_after_secs: Some(60), ..ev(Failure::Http(429)) });
        assert_eq!((d.category, d.retry, d.retry_after_secs), (Category::RateLimited, RetryClass::RetryableWithDelay, Some(60)));
        assert_eq!(diagnose(&ev(Failure::Http(501))).retry, RetryClass::NotRetryable);
        assert_eq!(diagnose(&ev(Failure::Http(408))).retry, RetryClass::Retryable);
        assert_eq!(diagnose(&ev(Failure::Http(409))).retry, RetryClass::Unknown);
    }

    #[test]
    fn a_missing_file_is_not_retried() {
        let d = diagnose(&ev(Failure::Http(404)));
        assert_eq!(
            (d.category, d.source, d.retry, d.severity),
            (Category::NotFound, Source::Link, RetryClass::NotRetryable, Severity::Blocking)
        );
        assert!(!d.actions.contains(&Action::Retry));
        assert_eq!(d.code, "http_404");
    }

    #[test]
    fn sign_in_and_expired_sessions_and_links() {
        let d = diagnose(&ev(Failure::Http(401)));
        assert_eq!((d.category, d.retry), (Category::AuthRequired, RetryClass::RetryAfterUserAction));
        assert_eq!(d.actions[0], Action::SignIn);
        // 403 with nothing else: access denied, no guess.
        let d = diagnose(&ev(Failure::Http(403)));
        assert_eq!((d.category, d.source), (Category::AccessDenied, Source::Server));
        // 403 after data arrived with a browser session: the session expired.
        let d = diagnose(&Evidence { session: true, worked_before: true, ..ev(Failure::Http(403)) });
        assert_eq!((d.category, d.source, d.code.as_str()), (Category::SessionExpired, Source::Account, "session_expired"));
        assert!(has(&d, FactorCode::SessionUsed));
        assert_eq!(d.actions[0], Action::ReauthInBrowser);
        // A signed link past its stated expiry.
        let d = diagnose(&Evidence { temporary_link: true, link_expired: true, ..ev(Failure::Http(403)) });
        assert_eq!((d.category, d.code.as_str()), (Category::LinkExpired, "link_expired"));
        assert_eq!(d.actions[0], Action::NewLink);
        // A signed link that worked and now gets 403.
        let d = diagnose(&Evidence { temporary_link: true, worked_before: true, ..ev(Failure::Http(403)) });
        assert_eq!(d.category, Category::LinkExpired);
        assert!(has(&d, FactorCode::WorkedBefore));
        // A sign-in page after it worked with a session.
        assert_eq!(diagnose(&Evidence { session: true, worked_before: true, ..ev(Failure::WebPage) }).category, Category::SessionExpired);
        assert_eq!(diagnose(&ev(Failure::WebPage)).category, Category::WebPage);
    }

    #[test]
    fn the_network_is_blamed_only_with_evidence() {
        let d = diagnose(&Evidence { online: Some(false), ..ev(Failure::ConnectionFailed) });
        assert_eq!((d.category, d.source, d.retry), (Category::Offline, Source::Network, RetryClass::RetryAfterUserAction));
        assert_eq!(d.actions[0], Action::CheckConnection);
        let d = diagnose(&ev(Failure::Timeout));
        assert_eq!((d.category, d.retry), (Category::Timeout, RetryClass::Retryable));
        // Resets while data kept arriving: instability contributes.
        let d = diagnose(&Evidence { drops: 4, run_retries: 4, worked_before: true, ..ev(Failure::ConnectionReset) });
        assert!(has(&d, FactorCode::UnstableConnection));
        assert_eq!(d.code, "connection_reset");
        // DNS while online: the link or DNS, unknown which.
        assert_eq!(diagnose(&ev(Failure::Dns)).retry, RetryClass::Unknown);
        assert_eq!(diagnose(&ev(Failure::Tls)).retry, RetryClass::NotRetryable);
        // Unknown online state: never "offline".
        let d = diagnose(&Evidence { online: None, ..ev(Failure::ConnectionFailed) });
        assert_eq!(d.category, Category::ConnectionFailed);
    }

    #[test]
    fn local_problems_are_this_computers_and_are_not_retried_blindly() {
        let d = diagnose(&Evidence { space: Some((8 << 30, 1 << 30)), ..ev(Failure::DiskFull) });
        assert_eq!(
            (d.category, d.source, d.retry, d.severity),
            (Category::DiskFull, Source::ThisComputer, RetryClass::RetryAfterUserAction, Severity::Blocking)
        );
        assert_eq!((d.needed_bytes, d.free_bytes), (Some(8 << 30), Some(1 << 30)));
        assert_eq!(d.actions[0], Action::FreeSpace);
        // Even with many drops before, a disk failure is not a network one.
        let d = diagnose(&Evidence { drops: 5, worked_before: true, ..ev(Failure::Permission) });
        assert_eq!(d.source, Source::ThisComputer);
        assert!(!has(&d, FactorCode::UnstableConnection));
        let d = diagnose(&ev(Failure::Persistence));
        assert_eq!((d.severity, d.source), (Severity::Critical, Source::ThisComputer));
    }

    #[test]
    fn integrity_is_critical_and_a_second_mismatch_stops_retrying() {
        let d = diagnose(&ev(Failure::Checksum));
        assert_eq!((d.category, d.retry, d.severity), (Category::Integrity, RetryClass::Retryable, Severity::Critical));
        assert_eq!(d.actions[0], Action::RestartSafely);
        let d = diagnose(&Evidence { same_as_before: true, ..ev(Failure::Checksum) });
        assert_eq!(d.retry, RetryClass::NotRetryable);
        assert!(has(&d, FactorCode::SameAsBefore));
    }

    #[test]
    fn range_failures_restart_safely() {
        let d = diagnose(&Evidence { downloaded: 1000, resumable: false, ..ev(Failure::RangeRejected) });
        assert_eq!((d.category, d.retry, d.code.as_str()), (Category::RangeFailure, RetryClass::Retryable, "range_not_honored"));
        assert_eq!(d.actions[0], Action::RestartSafely);
        assert!(has(&d, FactorCode::NoResume));
        assert_eq!(diagnose(&ev(Failure::Http(416))).category, Category::RangeFailure);
    }

    #[test]
    fn live_retries_are_a_warning_with_the_next_try() {
        let d = diagnose(&Evidence { retry_at_ms: Some(42), ..ev(Failure::Http(503)) });
        assert_eq!((d.severity, d.retry_at_ms), (Severity::Warning, Some(42)));
    }

    #[test]
    fn unknown_stays_unknown_and_cancel_is_information() {
        let d = diagnose(&Evidence::default());
        assert_eq!((d.category, d.source, d.retry), (Category::Unknown, Source::Unknown, RetryClass::Unknown));
        assert!(d.factors.is_empty());
        let d = diagnose(&ev(Failure::Cancelled));
        assert_eq!((d.severity, d.actions.len()), (Severity::Info, 0));
    }

    #[test]
    fn codes_from_text_round_trip() {
        for (code, f) in [
            ("http_503", Failure::Http(503)),
            ("dns", Failure::Dns),
            ("tls", Failure::Tls),
            ("connection_reset", Failure::ConnectionReset),
            ("disk_full", Failure::DiskFull),
            ("range_not_honored", Failure::RangeRejected),
            ("source_changed", Failure::SourceChanged),
            ("persistence", Failure::Persistence),
            ("redirect_loop", Failure::Redirect),
            ("whatever", Failure::Unknown),
        ] {
            assert_eq!(Failure::from_code(code), f, "{code}");
        }
        // Codes are safe anywhere.
        for f in [Failure::Http(503), Failure::Tls, Failure::Unknown, Failure::DiskFull] {
            let c = diagnose(&ev(f)).code;
            assert!(c.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "{c}");
        }
    }
}
