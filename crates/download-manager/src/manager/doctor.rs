//! Download Doctor, manager side: collects the evidence of a failure (see `crate::doctor` for the rules) from what
//! the app already has (the engine's error and its source chain, the run's events and retry count, the saved
//! diagnostics, the record, the queue's network reading) and keeps the newest diagnosis of each download in memory
//! for the list. No request is made and nothing is retried here: the engine's retry loop stays the only automatic
//! recovery.

use std::error::Error as _;
use std::time::{SystemTime, UNIX_EPOCH};

use downloader_core::RunEventKind;
use http_client::HttpError;

use super::*;
use crate::doctor::{self, Diagnosis, Evidence, Failure};

/// What failed, from the engine's error. Transport errors are read from their *source chain* only (the top message
/// carries the link, which is never looked at or kept).
pub(super) fn failure_of(e: &DownloadError) -> Failure {
    match e {
        DownloadError::Http(h) => match h {
            HttpError::Status { status, .. } => Failure::Http(*status),
            HttpError::SlowToAnswer | HttpError::Stalled => Failure::Timeout,
            HttpError::RangeNotHonored(_) => Failure::RangeRejected,
            HttpError::SourceChanged(_) => Failure::SourceChanged,
            HttpError::InvalidUrl(_) | HttpError::UnsupportedScheme(_) | HttpError::InvalidHeader(_) => Failure::InvalidRequest,
            HttpError::InvalidProxy(_) => Failure::ProxyInvalid,
            HttpError::Transport(t) => transport_failure(t),
        },
        DownloadError::Io(io) => match io.kind() {
            std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded => Failure::DiskFull,
            std::io::ErrorKind::PermissionDenied => Failure::Permission,
            _ => match Failure::from_code(&crate::explain::error_code(&e.to_string())) {
                f @ (Failure::DiskFull | Failure::Permission | Failure::FolderNotAllowed) => f,
                _ => Failure::WriteFailed,
            },
        },
        DownloadError::Cancelled | DownloadError::Paused => Failure::Cancelled,
        DownloadError::FileExists(_) => Failure::FileExists,
        DownloadError::ShortBody { .. } | DownloadError::SizeMismatch { .. } => Failure::Incomplete,
        DownloadError::ChecksumMismatch { .. } => Failure::Checksum,
        DownloadError::Store(_) => Failure::Persistence,
        DownloadError::WebPageInsteadOfFile(_) => Failure::WebPage,
        DownloadError::InvalidRequest(_) | DownloadError::Internal(_) => Failure::from_code(&crate::explain::error_code(&e.to_string())),
    }
}

fn transport_failure(t: &http_client::ReqwestError) -> Failure {
    if t.is_timeout() {
        return Failure::Timeout;
    }
    if t.is_redirect() {
        return Failure::Redirect;
    }
    // The causes below the top error (the top one names the link).
    let mut chain = String::new();
    let mut src = t.source();
    while let Some(s) = src {
        chain.push_str(&s.to_string().to_ascii_lowercase());
        chain.push(' ');
        src = s.source();
    }
    let any = |words: &[&str]| words.iter().any(|w| chain.contains(w));
    // rustls words: "invalid peer certificate: UnknownIssuer", "received corrupt message of type InvalidContentType"
    // (not a TLS server), "received fatal alert: HandshakeFailure".
    if any(&["certificate", "tls", "ssl", "handshake", "alert", "corrupt message", "invalidcontenttype"]) {
        Failure::Tls
    } else if any(&["dns", "failed to lookup", "name or service not known", "no such host", "nodename nor servname", "name resolution"]) {
        Failure::Dns
    } else if any(&["reset", "broken pipe", "connection closed", "aborted", "unexpected eof", "incomplete message", "connection error"]) {
        Failure::ConnectionReset
    } else if any(&["timed out", "timeout"]) {
        Failure::Timeout
    } else if t.is_connect() {
        Failure::ConnectionFailed
    } else if t.is_body() || t.is_decode() {
        Failure::ConnectionReset
    } else if t.is_request() {
        Failure::ConnectionFailed
    } else {
        Failure::Unknown
    }
}

/// What a run's events say: (drops, throttled, started over).
fn run_history(p: &ProgressHandle) -> (u32, bool, bool) {
    let (mut drops, mut throttled, mut over) = (0u32, false, false);
    for ev in p.events() {
        match ev.kind {
            RunEventKind::Retrying { error, .. } => {
                if matches!(crate::explain::error_code(&error).as_str(), "timeout" | "connection" | "incomplete") {
                    drops += 1;
                }
            }
            RunEventKind::Throttled { .. } => throttled = true,
            RunEventKind::StartingOver { .. } => over = true,
            _ => {}
        }
    }
    (drops, throttled, over)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// What the link and the record say (both for a fresh failure and for one only known by its text).
fn record_evidence(rec: &DownloadRecord, ev: &mut Evidence) {
    if let Ok(u) = url::Url::parse(&rec.url) {
        let x = crate::intelligence::expiring(&u);
        ev.temporary_link = x.temporary;
        ev.link_expired = x.at_ms.is_some_and(|at| at <= now_ms());
        ev.session |= !u.username().is_empty();
    }
    ev.session |= rec.request_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("cookie") || k.eq_ignore_ascii_case("authorization"));
    ev.resumable = rec.resumable;
    ev.downloaded = rec.downloaded;
}

impl DownloadManager {
    /// The computer's network state, read now (routing table / adapter: no request is made). Only asked after a
    /// connection failure, so a reading of the moment is worth more than the queue's older one.
    async fn online_now(&self) -> Option<bool> {
        let system = self.inner.cfg.system.0.clone();
        tokio::task::spawn_blocking(move || system.network()).await.ok().and_then(|n| n.online)
    }

    /// The evidence of a run that just failed with `e`. `prior_bytes` / `prior_failure`: from the saved diagnostics.
    pub(super) async fn failure_evidence(&self, id: i64, e: &DownloadError, progress: Option<&ProgressHandle>) -> Evidence {
        let raw = e.to_string();
        let mut ev = Evidence {
            failure: Some(failure_of(e)),
            retry_after_secs: match e {
                DownloadError::Http(HttpError::Status { retry_after: Some(d), .. }) if !d.is_zero() => Some(d.as_secs().max(1)),
                _ => None,
            },
            space: crate::explain::space_numbers(&raw),
            ..Default::default()
        };
        if let Some(p) = progress {
            ev.run_retries = p.retries();
            (ev.drops, ev.throttled, ev.started_over) = run_history(p);
            ev.worked_before = p.received() > 0;
        }
        if let Ok(rec) = self.load(id).await {
            record_evidence(&rec, &mut ev);
            ev.worked_before |= rec.downloaded > 0;
        }
        if matches!(ev.failure, Some(Failure::Dns | Failure::ConnectionFailed | Failure::Timeout)) {
            ev.online = self.online_now().await;
        }
        ev
    }

    /// Diagnosis for the list: the one made when the download failed, or, when the app has restarted since, one
    /// from the saved error text (less evidence, so more often `unknown`). While the engine retries on its own,
    /// what it retries and when.
    pub(super) fn doctor_for(&self, rec: &DownloadRecord, progress: Option<&ProgressHandle>, retrying: bool) -> Option<Diagnosis> {
        let id = rec.id.unwrap_or_default();
        match rec.status {
            DownloadStatus::Failed => {
                if let Some(d) = self.inner.doctor.lock().unwrap().get(&id) {
                    return Some(d.clone());
                }
                let raw = rec.last_error.as_deref().filter(|e| !e.is_empty())?;
                let mut ev = Evidence {
                    failure: Some(Failure::from_code(&crate::explain::error_code(raw))),
                    space: crate::explain::space_numbers(raw),
                    ..Default::default()
                };
                record_evidence(rec, &mut ev);
                Some(doctor::diagnose(&ev))
            }
            DownloadStatus::Downloading if retrying => {
                let p = progress?;
                let (error, at) = p.events().into_iter().rev().find_map(|ev| match ev.kind {
                    RunEventKind::Retrying { error, delay, .. } => Some((
                        error,
                        ev.at.checked_add(delay).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64),
                    )),
                    _ => None,
                })?;
                let mut ev = Evidence {
                    failure: Some(Failure::from_code(&crate::explain::error_code(&error))),
                    retry_at_ms: at,
                    ..Default::default()
                };
                ev.run_retries = p.retries();
                (ev.drops, ev.throttled, ev.started_over) = run_history(p);
                ev.worked_before = p.received() > 0;
                record_evidence(rec, &mut ev);
                Some(doctor::diagnose(&ev))
            }
            _ => None,
        }
    }

    pub(super) fn remember_diagnosis(&self, id: i64, d: Option<Diagnosis>) {
        let mut map = self.inner.doctor.lock().unwrap();
        match d {
            Some(d) => {
                map.insert(id, d);
            }
            None => {
                map.remove(&id);
            }
        }
    }
}
