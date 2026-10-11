//! "Use a new link": a stopped download continued from a fresh link (a signed link that expired, a session that
//! ended, a mirror that went away). The rules here decide, from evidence only, whether the new link gives the
//! *same file*, so the bytes already saved can be kept. Pure functions: the manager (`manager/relink.rs`) asks the
//! server and reads the partial file, then calls them.
//!
//! A matching name never counts, nor a matching size on its own, nor a weak `ETag`. Saved bytes are only reused
//! when the server proves the same version (a strong `ETag` equal to the one the download started with), the
//! size is the same, ranged requests work on the new link and a sample of the saved bytes matches what the new
//! link sends at the same place. Anything less is told to the user, who decides; the saved bytes stay as they are.

use downloader_core::{DownloadRecord, Segment};
use http_client::ResourceInfo;
use serde::Serialize;
use url::Url;

/// Bytes compared per sample: enough to tell two files apart, small enough to cost nothing.
pub const SAMPLE_LEN: u64 = 64 * 1024;
/// Samples compared at most (the start of the saved bytes and the end of the furthest ones).
pub const MAX_SAMPLES: usize = 2;

/// Whether the new link gives the file the download started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// The server proves the same version (same strong `ETag`, same size) and the saved bytes match.
    VerifiedSame,
    /// Nothing contradicts it and the saved bytes match (or nothing is reused), but the server proves nothing.
    LikelySame,
    /// Proven another file: another size, `ETag` or date, other bytes, or a web page.
    DifferentResource,
    /// Not enough evidence either way (the size is not known).
    Unknown,
}

/// The evidence behind a [`Verdict`] or the reason the link cannot be used. One code, translated by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    // ---- the same file
    /// Same strong `ETag` and size (and the saved bytes match).
    EtagMatch,
    /// The saved bytes match what the new link sends, same size; no `ETag` proves the version.
    BytesMatch,
    /// Same size and same `Last-Modified`; no saved bytes to compare.
    DateMatch,
    /// Nothing is saved yet: nothing can be damaged by switching.
    NothingToReuse,
    // ---- another file
    WebPage,
    SizeChanged,
    EtagChanged,
    DateChanged,
    BytesDiffer,
    // ---- not enough evidence
    SizeUnknown,
    /// Only the size matches.
    SizeOnly,
    // ---- the link cannot continue the saved bytes (whatever the file)
    /// The new link does not answer ranged requests correctly: saved bytes cannot be completed from it.
    NoRanges,
    /// The new link gives no strong `ETag` nor `Last-Modified`: a change during the download would go unnoticed.
    NoValidator,
    // ---- nothing was tried
    /// The link (and the browser's session) are the ones the download already uses.
    SameSource,
    /// The new link did not answer (or refused).
    Unreachable,
}

/// What comparing saved bytes with the new link's bytes showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// Not compared (nothing saved, or the size already differs, or ranges do not work).
    NotTaken,
    Matched,
    Differed,
}

/// What the download knew of its file when it stopped.
#[derive(Debug, Clone, Default)]
pub struct Known {
    pub url: String,
    pub size: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
}

impl Known {
    pub fn of(rec: &DownloadRecord) -> Self {
        Self {
            url: rec.url.clone(),
            size: rec.total_size,
            etag: rec.etag.clone(),
            last_modified: rec.last_modified.clone(),
            content_type: rec.content_type.clone(),
        }
    }
}

fn strong(e: Option<&str>) -> Option<&str> {
    e.filter(|e| !e.starts_with("W/") && e.len() > 2)
}

fn html(t: Option<&str>) -> bool {
    t.is_some_and(|t| t.trim_start().to_ascii_lowercase().starts_with("text/html"))
}

fn host(u: &str) -> Option<String> {
    Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase))
}

/// Whether the new link gives the file the download started with, and why. `sample`: the saved bytes compared
/// with the new link's. Strongest contradiction first: any proof of another file wins over every match.
pub fn identify(old: &Known, new: &ResourceInfo, new_url: &str, sample: Sample) -> (Verdict, Reason) {
    use Reason::*;
    use Verdict::*;
    if html(new.content_type.as_deref()) && !html(old.content_type.as_deref()) {
        return (DifferentResource, WebPage);
    }
    let sizes = old.size.zip(new.content_length);
    if sizes.is_some_and(|(a, b)| a != b) {
        return (DifferentResource, SizeChanged);
    }
    let etags = strong(old.etag.as_deref()).zip(strong(new.etag.as_deref()));
    if etags.is_some_and(|(a, b)| a != b) {
        return (DifferentResource, EtagChanged);
    }
    let etag_match = etags.is_some();
    let dates = old.last_modified.as_deref().zip(new.last_modified.as_deref());
    // Mirrors copy files at different times: another date only proves something on the same server.
    if !etag_match && dates.is_some_and(|(a, b)| a != b) && host(&old.url).is_some() && host(&old.url) == host(new_url) {
        return (DifferentResource, DateChanged);
    }
    if sample == Sample::Differed {
        return (DifferentResource, BytesDiffer);
    }
    if sizes.is_none() {
        return (Unknown, SizeUnknown);
    }
    if etag_match {
        return (VerifiedSame, EtagMatch);
    }
    if sample == Sample::Matched {
        return (LikelySame, BytesMatch);
    }
    if dates.is_some_and(|(a, b)| a == b) {
        return (LikelySame, DateMatch);
    }
    (Unknown, SizeOnly)
}

/// How far the user said the app may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Switch only on proof (or when nothing saved can be damaged); otherwise just report.
    #[default]
    Auto,
    /// The user saw a "likely the same file" report and chose to continue.
    Confirm,
}

/// What may be done with the new link. `reuse`: saved bytes would be kept (else the download has nothing yet).
/// `ranges`: ranged requests work on the new link. `validator`: it gives a strong `ETag` or a date.
pub fn decide(verdict: Verdict, mode: Mode, reuse: bool, ranges: bool, validator: bool) -> Decision {
    if verdict == Verdict::DifferentResource {
        return Decision { apply: false, can_confirm: false, blocker: None };
    }
    if !reuse {
        // Nothing saved can be damaged: the download simply runs from the new link.
        return Decision { apply: true, can_confirm: false, blocker: None };
    }
    let blocker = if !ranges {
        Some(Reason::NoRanges)
    } else if !validator {
        Some(Reason::NoValidator)
    } else {
        None
    };
    let acceptable = match mode {
        Mode::Auto => verdict == Verdict::VerifiedSame,
        Mode::Confirm => matches!(verdict, Verdict::VerifiedSame | Verdict::LikelySame),
    };
    let can_confirm = blocker.is_none() && verdict == Verdict::LikelySame;
    Decision { apply: acceptable && blocker.is_none(), can_confirm: can_confirm && mode == Mode::Auto, blocker }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub apply: bool,
    /// "Continue anyway" may be offered (likely the same file, the link can continue it).
    pub can_confirm: bool,
    /// Why saved bytes cannot be continued from this link, whatever the file.
    pub blocker: Option<Reason>,
}

/// Where to compare saved bytes: the start of the first saved bytes and the end of the furthest ones, each at
/// most [`SAMPLE_LEN`], only inside bytes the download recorded as written (and synced). `(offset, len)`.
pub fn sample_windows(segments: &[Segment]) -> Vec<(u64, u64)> {
    let done: Vec<(u64, u64)> = segments.iter().filter(|s| s.downloaded > 0).map(|s| (s.start, s.downloaded.min(s.len()))).collect();
    let mut out = Vec::new();
    if let Some(&(start, len)) = done.iter().min_by_key(|(s, _)| *s) {
        out.push((start, len.min(SAMPLE_LEN)));
    }
    if let Some(&(start, len)) = done.iter().max_by_key(|(s, l)| s + l) {
        let w = len.min(SAMPLE_LEN);
        let tail = (start + len - w, w);
        if !out.iter().any(|&(o, l)| tail.0 < o + l && o < tail.0 + tail.1) {
            out.push(tail);
        }
    }
    out.truncate(MAX_SAMPLES);
    out
}

/// Request headers for the new link. From the browser (it just sent them for that link): used as they are. Typed
/// by the user: the download's own referrer and user agent are kept, but its cookies and sign-in only when the new
/// link is on the same origin (scheme, host, port) — never handed to another server.
pub fn headers_for(
    old_url: &str,
    new_url: &Url,
    old: &[(String, String)],
    from_browser: Option<Vec<(String, String)>>,
) -> Vec<(String, String)> {
    if let Some(h) = from_browser {
        return h;
    }
    let same_origin = Url::parse(old_url).is_ok_and(|o| o.origin() == new_url.origin());
    old.iter()
        .filter(|(k, _)| same_origin || !(k.eq_ignore_ascii_case("cookie") || k.eq_ignore_ascii_case("authorization")))
        .cloned()
        .collect()
}

/// The download already uses this link with these headers: asking it again would fail the same way.
pub fn same_source(old_url: &str, old_headers: &[(String, String)], new_url: &Url, new_headers: &[(String, String)]) -> bool {
    let norm = |u: &Url| crate::batch::normalize(u);
    let mut a: Vec<_> = old_headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.as_str())).collect();
    let mut b: Vec<_> = new_headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.as_str())).collect();
    a.sort();
    b.sort();
    Url::parse(old_url).is_ok_and(|o| norm(&o) == norm(new_url)) && a == b
}

/// Request to continue a stopped download from a new link: typed / pasted by the user (`url`), or the link a
/// browser just handed over for confirmation (`pending`, with the browser's session). Exactly one of the two.
#[derive(Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UseNewLink {
    pub id: i64,
    pub url: Option<String>,
    pub pending: Option<u64>,
    #[serde(default)]
    pub mode: Mode,
}

impl std::fmt::Debug for UseNewLink {
    // A signed link carries its secret in the address: never printed whole.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UseNewLink")
            .field("id", &self.id)
            .field("url", &self.url.as_deref().map(secrets::redact_url))
            .field("pending", &self.pending)
            .field("mode", &self.mode)
            .finish()
    }
}

/// What came of a new link. Nothing is changed unless `applied`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLinkOutcome {
    pub verdict: Verdict,
    /// The evidence, or what stops the link from continuing the saved bytes.
    pub reason: Reason,
    pub applied: bool,
    /// "Continue anyway" can be offered (see [`Decision::can_confirm`]).
    pub can_confirm: bool,
    /// The status the new link answered with, when it refused.
    pub http_status: Option<u16>,
    /// Bytes saved that are kept (whatever the outcome, nothing saved is ever thrown away here).
    pub kept_bytes: u64,
    /// The download as it is now (running from the new link when `applied`).
    pub download: Option<crate::api::DownloadDto>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(f: impl FnOnce(&mut ResourceInfo)) -> ResourceInfo {
        let mut i = ResourceInfo {
            final_url: "https://cdn.example.com/a.iso?sig=new".into(),
            location: None,
            content_length: Some(4 << 30),
            content_type: Some("application/octet-stream".into()),
            content_disposition: None,
            accept_ranges_header: true,
            range_verified: true,
            etag: Some("\"abc\"".into()),
            last_modified: Some("Mon, 01 Jan 2026 00:00:00 GMT".into()),
            server: None,
            status: 206,
        };
        f(&mut i);
        i
    }

    fn known() -> Known {
        Known {
            url: "https://cdn.example.com/a.iso?sig=old".into(),
            size: Some(4 << 30),
            etag: Some("\"abc\"".into()),
            last_modified: Some("Mon, 01 Jan 2026 00:00:00 GMT".into()),
            content_type: Some("application/octet-stream".into()),
        }
    }

    const NEW: &str = "https://cdn.example.com/a.iso?sig=new";
    const MIRROR: &str = "https://mirror.example.org/pub/a.iso";

    #[test]
    fn a_strong_etag_and_size_prove_the_same_file_under_another_link() {
        assert_eq!(identify(&known(), &info(|_| {}), NEW, Sample::Matched), (Verdict::VerifiedSame, Reason::EtagMatch));
        assert_eq!(identify(&known(), &info(|_| {}), MIRROR, Sample::NotTaken), (Verdict::VerifiedSame, Reason::EtagMatch));
    }

    #[test]
    fn any_proof_of_another_file_wins() {
        let k = known();
        assert_eq!(identify(&k, &info(|i| i.content_length = Some(5)), NEW, Sample::NotTaken).1, Reason::SizeChanged);
        assert_eq!(identify(&k, &info(|i| i.etag = Some("\"new\"".into())), NEW, Sample::Matched).1, Reason::EtagChanged);
        // Same name, same size, same ETag — but other bytes: another file.
        assert_eq!(identify(&k, &info(|_| {}), NEW, Sample::Differed), (Verdict::DifferentResource, Reason::BytesDiffer));
        assert_eq!(identify(&k, &info(|i| i.content_type = Some("text/html".into())), NEW, Sample::Matched).1, Reason::WebPage);
    }

    #[test]
    fn a_weak_or_missing_etag_proves_nothing() {
        let k = Known { etag: Some("W/\"abc\"".into()), ..known() };
        assert_eq!(
            identify(&k, &info(|i| i.etag = Some("W/\"abc\"".into())), NEW, Sample::Matched),
            (Verdict::LikelySame, Reason::BytesMatch)
        );
        let none = Known { etag: None, last_modified: None, ..known() };
        assert_eq!(
            identify(&none, &info(|i| (i.etag, i.last_modified) = (None, None)), NEW, Sample::NotTaken),
            (Verdict::Unknown, Reason::SizeOnly)
        );
        assert_eq!(identify(&none, &info(|i| i.etag = None), NEW, Sample::Matched).0, Verdict::LikelySame);
    }

    #[test]
    fn a_date_only_counts_on_the_same_server() {
        let k = Known { etag: None, ..known() };
        let later = info(|i| (i.etag, i.last_modified) = (None, Some("Tue, 02 Jan 2026 00:00:00 GMT".into())));
        assert_eq!(identify(&k, &later, NEW, Sample::Matched).1, Reason::DateChanged);
        assert_eq!(identify(&k, &later, MIRROR, Sample::Matched), (Verdict::LikelySame, Reason::BytesMatch), "a mirror copied it later");
        assert_eq!(identify(&k, &info(|i| i.etag = None), NEW, Sample::NotTaken), (Verdict::LikelySame, Reason::DateMatch));
    }

    #[test]
    fn an_unknown_size_is_unknown() {
        assert_eq!(identify(&known(), &info(|i| i.content_length = None), NEW, Sample::NotTaken), (Verdict::Unknown, Reason::SizeUnknown));
        let k = Known { size: None, ..known() };
        assert_eq!(identify(&k, &info(|_| {}), NEW, Sample::NotTaken).0, Verdict::Unknown);
    }

    #[test]
    fn saved_bytes_are_only_reused_on_proof_or_with_the_users_word() {
        use Verdict::*;
        let d = |v, m, reuse, ranges, val| decide(v, m, reuse, ranges, val);
        assert!(d(VerifiedSame, Mode::Auto, true, true, true).apply);
        let likely = d(LikelySame, Mode::Auto, true, true, true);
        assert!(!likely.apply && likely.can_confirm, "likely: asked first");
        assert!(d(LikelySame, Mode::Confirm, true, true, true).apply);
        for m in [Mode::Auto, Mode::Confirm] {
            assert!(!d(DifferentResource, m, true, true, true).apply);
            assert!(!d(DifferentResource, m, false, true, true).apply, "another file never takes the download's place");
            assert!(!d(Unknown, m, true, true, true).apply);
            assert!(!d(Unknown, m, true, true, true).can_confirm);
        }
        let no_ranges = d(VerifiedSame, Mode::Confirm, true, false, true);
        assert_eq!((no_ranges.apply, no_ranges.can_confirm, no_ranges.blocker), (false, false, Some(Reason::NoRanges)));
        assert_eq!(d(VerifiedSame, Mode::Auto, true, true, false).blocker, Some(Reason::NoValidator));
        // Nothing saved yet: nothing to damage.
        assert!(d(Unknown, Mode::Auto, false, false, false).apply);
    }

    #[test]
    fn samples_are_taken_only_inside_saved_bytes() {
        let seg = |id, start, end, downloaded| Segment { downloaded, ..Segment::new(id, start, end) };
        assert!(sample_windows(&[seg(0, 0, 999, 0)]).is_empty());
        assert_eq!(sample_windows(&[seg(0, 0, 999, 10)]), vec![(0, 10)], "one small piece: one sample");
        let big = SAMPLE_LEN * 4;
        let w = sample_windows(&[seg(0, 0, big - 1, big), seg(1, big, 2 * big - 1, 100), seg(2, 2 * big, 3 * big - 1, SAMPLE_LEN * 3)]);
        assert_eq!(w, vec![(0, SAMPLE_LEN), (2 * big + 2 * SAMPLE_LEN, SAMPLE_LEN)]);
        for (o, l) in w {
            assert!(l <= SAMPLE_LEN && o + l <= 3 * big);
        }
    }

    #[test]
    fn credentials_never_follow_a_link_to_another_server() {
        let old = vec![
            ("cookie".to_string(), "sid=1".to_string()),
            ("authorization".to_string(), "Bearer x".to_string()),
            ("referer".to_string(), "https://site.example.com/".to_string()),
        ];
        let same = Url::parse("https://cdn.example.com/a.iso?sig=2").unwrap();
        assert_eq!(headers_for("https://cdn.example.com/a.iso?sig=1", &same, &old, None).len(), 3);
        let other = Url::parse("https://mirror.example.org/a.iso").unwrap();
        let h = headers_for("https://cdn.example.com/a.iso?sig=1", &other, &old, None);
        assert_eq!(h, vec![("referer".to_string(), "https://site.example.com/".to_string())]);
        let http = Url::parse("http://cdn.example.com/a.iso").unwrap();
        assert_eq!(headers_for("https://cdn.example.com/a.iso", &http, &old, None).len(), 1, "another scheme is another origin");
        let browser = vec![("cookie".to_string(), "sid=fresh".to_string())];
        assert_eq!(headers_for("https://cdn.example.com/a", &other, &old, Some(browser.clone())), browser);
    }

    #[test]
    fn the_same_link_and_session_is_not_a_new_source() {
        let h = vec![("Cookie".to_string(), "sid=1".to_string())];
        let u = Url::parse("https://cdn.example.com/a.iso?sig=1#frag").unwrap();
        assert!(same_source("https://cdn.example.com/a.iso?sig=1", &h, &u, &[("cookie".to_string(), "sid=1".to_string())]));
        assert!(
            !same_source("https://cdn.example.com/a.iso?sig=1", &h, &u, &[("cookie".to_string(), "sid=2".to_string())]),
            "a fresh session is"
        );
        let other = Url::parse("https://cdn.example.com/a.iso?sig=2").unwrap();
        assert!(!same_source("https://cdn.example.com/a.iso?sig=1", &h, &other, &h));
    }
}
