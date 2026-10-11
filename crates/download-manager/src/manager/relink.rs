//! "Use a new link" (see `crate::relink` for the rules): a paused or failed download continued from a fresh link,
//! keeping its saved bytes only when the new link is proven to give the same file.
//!
//! One bounded attempt per request, never a loop: ask the new link once (no retries), compare at most
//! [`relink::MAX_SAMPLES`] small pieces of the saved bytes with it, then either switch and continue, or change
//! nothing and say why. Nothing saved is ever deleted here. The link and headers are written to the record (the
//! headers sealed, like any download's) before the download starts again, so a restart continues from the new
//! link too.

use std::collections::HashSet;

use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::*;
use crate::relink::{self, Known, NewLinkOutcome, Reason, Sample, UseNewLink, Verdict};

/// How long the new link may take to answer the check (and each sample).
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Downloads a new link is being checked for: a second request for the same download is refused meanwhile.
#[derive(Default)]
pub(super) struct Relinking(HashSet<i64>);

/// Takes the download out of [`Relinking`] when the check ends, however it ends.
struct Claim<'a>(&'a Mutex<Relinking>, i64);

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().0.remove(&self.1);
    }
}

impl DownloadManager {
    pub async fn use_new_link(&self, req: UseNewLink) -> Result<NewLinkOutcome, ManagerError> {
        let id = req.id;
        let (url, browser_headers) = match (req.url.as_deref(), req.pending) {
            (Some(u), None) => (u.to_string(), None),
            (None, Some(token)) => {
                let (u, h) = self.pending_source(token)?;
                (u, Some(h))
            }
            _ => return Err(ManagerError::invalid("give either a link or a waiting browser download")),
        };
        let (url, credentials) = validate::split_credentials(validate::url(&url)?);
        let url = crate::links::direct(&url).unwrap_or(url);

        let rec = self.load(id).await?;
        self.check_relinkable(&rec)?;
        if !self.inner.relinking.lock().unwrap().0.insert(id) {
            return Err(ManagerError::invalid("a new link is already being checked for this download"));
        }
        let _claim = Claim(&self.inner.relinking, id);

        let mut headers = relink::headers_for(&rec.url, &url, &rec.request_headers, browser_headers);
        if let Some(auth) = credentials {
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
            headers.push(("authorization".to_string(), auth));
        }
        let reuse = !rec.is_unstarted() && rec.segments.iter().any(|s| s.downloaded > 0);
        let kept_bytes = if reuse { rec.segments.iter().map(|s| s.downloaded.min(s.len())).sum() } else { 0 };
        let outcome = |verdict, reason, http_status| NewLinkOutcome {
            verdict,
            reason,
            applied: false,
            can_confirm: false,
            http_status,
            kept_bytes,
            download: None,
        };

        if relink::same_source(&rec.url, &rec.request_headers, &url, &headers) {
            return self.relink_report(id, outcome(Verdict::Unknown, Reason::SameSource, None)).await;
        }
        let info = match tokio::time::timeout(CHECK_TIMEOUT, self.inner.downloader.probe_quick(url.as_str(), &headers)).await {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => return self.relink_report(id, outcome(Verdict::Unknown, Reason::Unreachable, e.http_status())).await,
            Err(_) => return self.relink_report(id, outcome(Verdict::Unknown, Reason::Unreachable, None)).await,
        };
        let known = Known::of(&rec);
        // Saved bytes are compared only when they could be continued: same size, ranges that work.
        let size_ok = known.size.is_some() && known.size == info.content_length;
        let mut ranges = info.supports_parallel();
        let mut sample = Sample::NotTaken;
        if reuse && size_ok && ranges {
            match self.compare_samples(&rec, url.as_str(), &headers).await {
                Ok(s) => sample = s,
                Err(SampleError::Ranges) => ranges = false,
                Err(SampleError::Refused(status)) => {
                    return self.relink_report(id, outcome(Verdict::Unknown, Reason::Unreachable, status)).await
                }
            }
        }
        let (verdict, evidence) =
            if reuse { relink::identify(&known, &info, url.as_str(), sample) } else { nothing_to_reuse(&known, &info, url.as_str()) };
        let validator = info.etag.as_deref().is_some_and(|e| !e.starts_with("W/")) || info.last_modified.is_some();
        let d = relink::decide(verdict, req.mode, reuse, ranges, validator);
        let reason = if verdict == Verdict::DifferentResource { evidence } else { d.blocker.unwrap_or(evidence) };
        if !d.apply {
            let mut o = outcome(verdict, reason, None);
            o.can_confirm = d.can_confirm;
            return self.relink_report(id, o).await;
        }

        // ---- switch: only if the download is still where it was when the check began
        let (new_url, new_headers, info2) = (url.to_string(), headers, info.clone());
        let before = rec.segments.iter().map(|s| s.downloaded).collect::<Vec<_>>();
        let store = self.inner.store.clone();
        let busy = self.is_active(id);
        let switched = blocking(move || {
            let Some(mut r) = store.load(id)? else { return Ok(false) };
            let same_state = matches!(r.status, DownloadStatus::Paused | DownloadStatus::Failed)
                && r.segments.iter().map(|s| s.downloaded).collect::<Vec<_>>() == before;
            if busy || !same_state {
                return Ok(false);
            }
            r.url = new_url;
            r.request_headers = new_headers;
            if !r.is_unstarted() && reuse {
                // The engine checks these on resume and sends them with every range (`If-Range`): from now on they
                // must be the new link's, which were just proven to describe the same file.
                // (A weak `ETag` is never kept: the engine would take it for a changed file and start over.)
                r.etag = info2.etag.clone().filter(|e| !e.starts_with("W/"));
                r.last_modified = info2.last_modified.clone();
            }
            r.last_error = None;
            store.save(&r).map(|_| true)
        })
        .await?;
        if !switched || self.is_active(id) {
            return Err(ManagerError::invalid("the download changed while the new link was checked: try again"));
        }
        self.remember_diagnosis(id, None);
        let text = match (reuse, verdict) {
            (false, _) => "New link: the download starts from it (nothing was saved yet)".to_string(),
            (true, Verdict::VerifiedSame) => {
                format!("New link: the same file (confirmed by the server and by the saved bytes). Continuing from {kept_bytes} bytes")
            }
            (true, _) => {
                format!("New link: likely the same file (the saved bytes match; chosen by the user). Continuing from {kept_bytes} bytes")
            }
        };
        self.note_event(id, "info", &text).await;
        tracing::info!(id, host = url.host_str().unwrap_or(""), ?verdict, ?evidence, "download continued from a new link");
        if let Some(token) = req.pending {
            let _ = self.dismiss_pending(token); // answered: it continues the download instead of adding one
        }
        let download = self.start_download(id).await?;
        Ok(NewLinkOutcome {
            verdict,
            reason: evidence,
            applied: true,
            can_confirm: false,
            http_status: None,
            kept_bytes,
            download: Some(download),
        })
    }

    fn check_relinkable(&self, rec: &DownloadRecord) -> Result<(), ManagerError> {
        let id = rec.id.unwrap_or_default();
        if self.is_active(id) || !matches!(rec.status, DownloadStatus::Paused | DownloadStatus::Failed) {
            return Err(ManagerError::InvalidState { action: "use a new link for", status: rec.status });
        }
        Ok(())
    }

    /// An outcome that changes nothing: noted in the download's diagnostics (never the link), then returned.
    async fn relink_report(&self, id: i64, mut o: NewLinkOutcome) -> Result<NewLinkOutcome, ManagerError> {
        let why = match o.reason {
            Reason::SameSource => "it is the link the download already uses",
            Reason::Unreachable => "it did not answer or refused",
            Reason::WebPage => "it gives a web page, not the file",
            Reason::SizeChanged => "it gives a file of another size",
            Reason::EtagChanged | Reason::DateChanged => "the server describes another version",
            Reason::BytesDiffer => "its bytes differ from the saved ones",
            Reason::NoRanges => "it cannot send the missing parts on their own",
            Reason::NoValidator => "it gives no way to notice a change during the download",
            Reason::SizeUnknown | Reason::SizeOnly => "nothing proves it is the same file",
            Reason::BytesMatch | Reason::DateMatch => "it is likely, not proven, to be the same file",
            Reason::EtagMatch | Reason::NothingToReuse => "it was not needed",
        };
        let status = o.http_status.map(|s| format!(" (HTTP {s})")).unwrap_or_default();
        self.note_event(id, "warn", &format!("New link not used: {why}{status}. The saved bytes are kept")).await;
        o.download = self.dto_for(id).await.ok();
        Ok(o)
    }

    /// Compare a few small pieces of the saved bytes with what the new link sends at the same places.
    async fn compare_samples(&self, rec: &DownloadRecord, url: &str, headers: &[(String, String)]) -> Result<Sample, SampleError> {
        let part = downloader_core::part_path(&rec.destination);
        if self.inner.roots.check(&part).is_err() {
            return Err(SampleError::Ranges); // never read outside the download folders
        }
        let windows = relink::sample_windows(&rec.segments);
        if windows.is_empty() {
            return Ok(Sample::NotTaken);
        }
        let Ok(mut file) = tokio::fs::File::open(&part).await else { return Ok(Sample::NotTaken) };
        for (offset, len) in windows {
            let mut saved = vec![0u8; len as usize];
            if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() || file.read_exact(&mut saved).await.is_err() {
                return Ok(Sample::NotTaken);
            }
            let got = tokio::time::timeout(CHECK_TIMEOUT, self.inner.downloader.sample_range(url, headers, offset, len)).await;
            match got {
                Ok(Ok(bytes)) if bytes == saved => {}
                Ok(Ok(_)) => return Ok(Sample::Differed),
                Ok(Err(DownloadError::Http(http_client::HttpError::RangeNotHonored(_)))) => return Err(SampleError::Ranges),
                Ok(Err(e)) => return Err(SampleError::Refused(e.http_status())),
                Err(_) => return Err(SampleError::Refused(None)),
            }
        }
        Ok(Sample::Matched)
    }
}

enum SampleError {
    /// The new link does not answer ranged requests as it claimed.
    Ranges,
    /// It refused or did not answer (status, if any).
    Refused(Option<u16>),
}

/// Nothing saved yet: only proof of another file counts (the record's name and place stay the download's).
fn nothing_to_reuse(known: &Known, info: &http_client::ResourceInfo, url: &str) -> (Verdict, Reason) {
    match relink::identify(known, info, url, Sample::NotTaken) {
        (Verdict::DifferentResource, r) => (Verdict::DifferentResource, r),
        (v, _) => (v, Reason::NothingToReuse),
    }
}
