//! Media engine: HLS (`.m3u8`) and DASH (`.mpd`) streams, and video pages through yt-dlp (`site.rs`).
//!
//! A separate subsystem: it shares nothing with the normal HTTP download engine except the HTTP
//! client, and the normal engine knows nothing about it.
//!
//! ```text
//! manifest URL --analyze--> qualities / audio tracks / subtitles   (nothing is written)
//!              --download-> segments -> scratch files -> one output file
//!                           (optional) ffmpeg remux: separate video + audio -> .mp4 / .mkv / .webm
//! ```
//!
//! Security stance (see `docs/security.md`): a manifest is *untrusted data*.
//! * Every URL in it is checked (http(s) only, no credentials, no https -> http downgrade).
//! * Every count and size has a hard limit (`limits`), enforced while parsing / streaming.
//! * Encrypted / DRM / live streams are refused, never half-handled.
//! * Only names this crate builds (a sanitized stem + fixed suffixes) are ever written, and only
//!   inside the folder the caller already validated.
//! * ffmpeg (optional) is started with an argument *array* (never a shell string), absolute
//!   validated paths, a forced input format (so a "video" that is really a playlist cannot make
//!   ffmpeg open other files or URLs), a timeout, and no stdin.

mod dash;
mod download;
mod ffmpeg;
mod hls;
mod site;

use serde::Serialize;
use url::Url;

pub use download::{analyze, download, sweep_stale_scratch, work_dir_name, DownloadSpec, Outcome, Pacer, Phase, Progress, Selection};
pub use ffmpeg::{find_ffmpeg, Remux};
pub use site::{
    analyze_site, download_site, find_ytdlp, is_site_page, playlist_link, protected_service, SiteHeaders, SiteSpec, SiteTool,
    PLAYLIST_PAGES, PLAYLIST_QUALITIES, PROTECTED_SITES, VIDEO_SITES,
};

/// Hard limits. Every one of them is enforced in Rust whatever the UI says.
pub mod limits {
    /// A manifest / playlist larger than this is refused.
    pub const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
    /// Segments in one track.
    pub const MAX_SEGMENTS: usize = 30_000;
    /// Qualities + audio tracks + subtitle tracks in one manifest.
    pub const MAX_TRACKS: usize = 200;
    /// One segment (or one whole-file track, which is a single "segment").
    pub const MAX_SEGMENT_BYTES: u64 = 32 * 1024 * 1024 * 1024;
    /// Everything one job may download.
    pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024 * 1024;
    /// XML nesting and node count (an MPD is small; a bomb is not).
    pub const MAX_XML_DEPTH: usize = 32;
    pub const MAX_XML_NODES: usize = 100_000;
    /// One URL taken from a manifest.
    pub const MAX_URL_LEN: usize = 4096;
    /// Parallel segment downloads per job.
    pub const SEGMENT_CONCURRENCY: usize = 16;
    /// Tries per segment.
    pub const SEGMENT_TRIES: u32 = 3;
    /// ffmpeg gets this long; then it is killed.
    pub const REMUX_TIMEOUT_SECS: u64 = 2 * 60 * 60;
    /// Videos read from one playlist (the window says when a playlist has more).
    pub const MAX_PLAYLIST_VIDEOS: usize = 500;
}

pub type Result<T> = std::result::Result<T, MediaError>;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// A valid stream this app deliberately does not handle (DRM, live, several periods...).
    #[error("{0}")]
    Unsupported(String),
    #[error("not a valid stream description: {0}")]
    Invalid(String),
    #[error("refused, over a safety limit: {0}")]
    Limit(String),
    #[error("network error: {0}")]
    Http(#[from] http_client::HttpError),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("could not combine the streams: {0}")]
    Remux(String),
    /// A video page that could not be read or downloaded (yt-dlp's or the site's reason, as a sentence).
    #[error("{0}")]
    Site(String),
}

impl MediaError {
    pub(crate) fn invalid(s: impl Into<String>) -> Self {
        Self::Invalid(s.into())
    }
    pub(crate) fn unsupported(s: impl Into<String>) -> Self {
        Self::Unsupported(s.into())
    }
    pub(crate) fn site(s: impl Into<String>) -> Self {
        Self::Site(s.into())
    }
    pub(crate) fn limit(s: impl Into<String>) -> Self {
        Self::Limit(s.into())
    }
}

/// The file type the user wants: a closed set (anything else is refused). Video types keep the
/// picture and sound as they come whenever they fit the container (no quality loss); audio types
/// keep only the sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Mp4,
    Mkv,
    Webm,
    Mp3,
    M4a,
    Opus,
}

impl OutputFormat {
    pub const VIDEO: [OutputFormat; 3] = [OutputFormat::Mp4, OutputFormat::Mkv, OutputFormat::Webm];
    pub const AUDIO: [OutputFormat; 3] = [OutputFormat::Mp3, OutputFormat::M4a, OutputFormat::Opus];

    /// `mp4`, `mkv`... (case-insensitive); `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        Self::VIDEO.into_iter().chain(Self::AUDIO).find(|f| f.extension().eq_ignore_ascii_case(s.trim()))
    }
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Mp4 => "mp4",
            OutputFormat::Mkv => "mkv",
            OutputFormat::Webm => "webm",
            OutputFormat::Mp3 => "mp3",
            OutputFormat::M4a => "m4a",
            OutputFormat::Opus => "opus",
        }
    }
    pub fn is_audio(self) -> bool {
        Self::AUDIO.contains(&self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Hls,
    Dash,
    /// A video page read by yt-dlp (YouTube...).
    Site,
}

/// What a track's bytes are, decided from the manifest (never from the downloaded bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// MPEG transport stream (HLS `.ts`).
    Ts,
    /// Fragmented MP4 (init segment + `.m4s` / `.mp4`).
    Fmp4,
    Webm,
    /// Raw AAC (ADTS).
    Aac,
    Mp3,
    Vtt,
    Ttml,
}

impl Container {
    /// File extension for a track saved on its own.
    pub fn extension(self, audio_only: bool) -> &'static str {
        match self {
            Container::Ts => "ts",
            Container::Fmp4 if audio_only => "m4a",
            Container::Fmp4 => "mp4",
            Container::Webm => "webm",
            Container::Aac => "aac",
            Container::Mp3 => "mp3",
            Container::Vtt => "vtt",
            Container::Ttml => "ttml",
        }
    }

    /// The demuxer name handed to ffmpeg (`-f`). Forced, so ffmpeg never guesses from content.
    pub(crate) fn ffmpeg_format(self) -> Option<&'static str> {
        match self {
            Container::Ts => Some("mpegts"),
            Container::Fmp4 => Some("mov"),
            Container::Webm => Some("matroska"),
            Container::Aac => Some("aac"),
            Container::Mp3 => Some("mp3"),
            Container::Vtt | Container::Ttml => None,
        }
    }
}

/// One piece of a track: a whole URL, or a byte range of it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    pub url: Url,
    /// Inclusive byte range.
    pub range: Option<(u64, u64)>,
}

/// Everything needed to fetch one track: an optional initialization segment, then the media
/// segments in order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrackPlan {
    pub init: Option<Segment>,
    pub segments: Vec<Segment>,
    pub container: Container,
    pub duration: f64,
    pub discontinuities: u32,
}

/// A quality the user can pick.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamOption {
    pub id: String,
    pub label: String,
    pub bandwidth: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    /// Audio (and subtitle) tracks that belong with this quality.
    pub group: Option<String>,
}

/// An audio or subtitle track the user can add.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackOption {
    pub id: String,
    pub label: String,
    pub language: Option<String>,
    pub group: Option<String>,
    pub default: bool,
}

/// What `analyze` reports. Nothing has been downloaded yet.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub kind: StreamKind,
    /// A file-name suggestion taken from the URL.
    pub title: String,
    pub duration_secs: Option<f64>,
    /// Best quality first.
    pub streams: Vec<StreamOption>,
    pub audio: Vec<TrackOption>,
    pub subtitles: Vec<TrackOption>,
    /// The link is a playlist (YouTube, Vimeo showcase, SoundCloud set...): its videos, each downloaded on its own.
    /// `streams` then holds the qualities offered for all of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist: Option<Playlist>,
    /// The link is one video of a playlist (`watch?v=...&list=...`): the whole playlist can be read instead.
    pub part_of_playlist: bool,
}

/// A playlist read from a page (yt-dlp's flat list: titles and lengths, no video is read yet).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    /// Made safe for a folder name.
    pub title: String,
    /// How many videos the site says the playlist has (may be more than `entries`).
    pub total: Option<u32>,
    /// At most [`limits::MAX_PLAYLIST_VIDEOS`], in the playlist's order.
    pub entries: Vec<PlaylistEntry>,
    /// The playlist has more videos than were read.
    pub truncated: bool,
}

/// One video of a playlist. Its link stays in Rust: the window chooses videos by `index`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistEntry {
    /// 1-based position in the playlist.
    pub index: u32,
    pub title: String,
    pub duration_secs: Option<f64>,
    /// False for a private, deleted, members-only or live video: shown, but it cannot be downloaded.
    pub available: bool,
    #[serde(skip)]
    pub url: Url,
}

/// How a quality's bytes are obtained.
pub(crate) enum Source {
    /// HLS: the variant playlist still has to be fetched and parsed.
    Playlist(Url),
    Ready(TrackPlan),
}

pub(crate) struct Entry<T> {
    pub option: T,
    pub source: Source,
}

/// A parsed manifest: the public analysis plus how to get every track.
pub(crate) struct Catalog {
    pub analysis: Analysis,
    pub streams: Vec<Entry<StreamOption>>,
    pub audio: Vec<Entry<TrackOption>>,
    pub subtitles: Vec<Entry<TrackOption>>,
}

/// A URL found inside a manifest. Untrusted: http(s) only, no credentials, no downgrade from the
/// manifest's own https, and of sane length.
pub(crate) fn resolve_url(base: &Url, reference: &str) -> Result<Url> {
    let reference = reference.trim();
    if reference.is_empty() || reference.len() > limits::MAX_URL_LEN {
        return Err(MediaError::invalid("a link in the stream description is empty or too long"));
    }
    if reference.chars().any(|c| c.is_control()) {
        return Err(MediaError::invalid("a link in the stream description has control characters"));
    }
    let url = base.join(reference).map_err(|_| MediaError::invalid("a link in the stream description is malformed"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(MediaError::unsupported(format!("the stream uses a `{}` link; only http and https are allowed", url.scheme())));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(MediaError::invalid("a link in the stream description has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(MediaError::unsupported("a link in the stream description contains a user name or password"));
    }
    if base.scheme() == "https" && url.scheme() == "http" {
        return Err(MediaError::unsupported("the stream sends part of the video over plain http from an https page"));
    }
    if http_client::crosses_into_local(base, &url) {
        return Err(MediaError::unsupported("the stream points at an address on this computer or network from a public site"));
    }
    Ok(url)
}

/// Inclusive `(start, end)` from a length and offset, without overflow.
pub(crate) fn byte_range(len: u64, offset: u64) -> Result<(u64, u64)> {
    if len == 0 {
        return Err(MediaError::invalid("a byte range has length 0"));
    }
    let end = offset.checked_add(len - 1).ok_or_else(|| MediaError::invalid("a byte range is too large"))?;
    Ok((offset, end))
}

/// A file-name stem that is safe on every OS: keeps letters, digits and a few punctuation
/// marks, never empty, never longer than 100 characters, never a reserved Windows device name.
pub fn sanitize_stem(raw: &str) -> String {
    let mut s: String =
        raw.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '(' | ')' | '[' | ']') { c } else { '_' }).collect();
    s = s.trim_matches(|c: char| c == '.' || c == ' ' || c == '_').chars().take(100).collect();
    let reserved = path_safety::is_reserved_device_name(&s);
    if s.is_empty() || reserved {
        "video".to_string()
    } else {
        s
    }
}

/// A title from a manifest URL: its last path segment without the extension.
pub(crate) fn title_from_url(u: &Url) -> String {
    let last = u.path_segments().and_then(|mut s| s.rfind(|p| !p.is_empty())).unwrap_or("");
    let decoded = http_client::percent_decode(last).unwrap_or_else(|| last.to_string());
    let stem = decoded.rsplit_once('.').map_or(decoded.as_str(), |(s, _)| s);
    if stem.is_empty() {
        sanitize_stem(u.host_str().unwrap_or("video"))
    } else {
        sanitize_stem(stem)
    }
}

/// Short human text for a resolution / bitrate.
pub(crate) fn quality_label(width: Option<u32>, height: Option<u32>, bandwidth: Option<u64>) -> String {
    let mut parts = Vec::new();
    if let Some(h) = height {
        parts.push(format!("{h}p"));
    } else if let (Some(w), None) = (width, height) {
        parts.push(format!("{w}px wide"));
    }
    if let Some(b) = bandwidth {
        if b >= 1_000_000 {
            parts.push(format!("{:.1} Mbit/s", b as f64 / 1_000_000.0));
        } else if b > 0 {
            parts.push(format!("{} kbit/s", b / 1000));
        }
    }
    if parts.is_empty() {
        "Video".to_string()
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example.com/a/b/master.m3u8").unwrap()
    }

    #[test]
    fn urls_from_a_manifest_are_checked() {
        assert_eq!(resolve_url(&base(), "seg1.ts").unwrap().as_str(), "https://cdn.example.com/a/b/seg1.ts");
        assert_eq!(resolve_url(&base(), "/x/seg.ts?token=1").unwrap().as_str(), "https://cdn.example.com/x/seg.ts?token=1");
        assert_eq!(resolve_url(&base(), "https://other.example.org/s.ts").unwrap().host_str(), Some("other.example.org"));
        for bad in [
            "file:///etc/passwd",
            "ftp://h/x",
            "javascript:alert(1)",
            "data:text/plain,hi",
            "http://h/x.ts",
            "https://user:pw@h/x.ts",
            "",
            "  ",
            "a\nb",
            "https://127.0.0.1/x.ts",
            "https://169.254.169.254/latest",
            "https://nas.local/x.ts",
        ] {
            assert!(resolve_url(&base(), bad).is_err(), "{bad:?} must be refused");
        }
        assert!(resolve_url(&base(), &"a".repeat(limits::MAX_URL_LEN + 1)).is_err());
        // Plain http manifests may use http segments.
        let plain = Url::parse("http://lan/x/m.m3u8").unwrap();
        assert!(resolve_url(&plain, "seg.ts").is_ok());
    }

    #[test]
    fn byte_ranges_do_not_overflow() {
        assert_eq!(byte_range(10, 5).unwrap(), (5, 14));
        assert!(byte_range(0, 0).is_err());
        assert!(byte_range(10, u64::MAX - 3).is_err());
        assert_eq!(byte_range(1, u64::MAX).unwrap(), (u64::MAX, u64::MAX));
    }

    #[test]
    fn stems_are_safe_on_every_os() {
        assert_eq!(sanitize_stem("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_stem("a:b*c?.mp4"), "a_b_c__mp4");
        assert_eq!(sanitize_stem("CON"), "video");
        assert_eq!(sanitize_stem("com1"), "video");
        assert_eq!(sanitize_stem(""), "video");
        assert_eq!(sanitize_stem("..."), "video");
        assert_eq!(sanitize_stem(&"x".repeat(500)).len(), 100);
        assert_eq!(sanitize_stem("My Movie (2024)"), "My Movie (2024)");
        assert!(!sanitize_stem("évènement ✓").contains('/'));
    }

    #[test]
    fn titles_and_labels() {
        assert_eq!(title_from_url(&Url::parse("https://h/path/My%20Show.m3u8?x=1").unwrap()), "My Show");
        assert_eq!(title_from_url(&Url::parse("https://h.example/").unwrap()), "h_example");
        assert_eq!(quality_label(Some(1920), Some(1080), Some(5_000_000)), "1080p · 5.0 Mbit/s");
        assert_eq!(quality_label(None, None, Some(128_000)), "128 kbit/s");
        assert_eq!(quality_label(None, None, None), "Video");
    }
}

#[cfg(test)]
mod robustness {
    use super::*;

    /// Small deterministic generator: the same inputs on every run and every OS, so a failure reproduces.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// Damage `seed` the ways a hostile server or a bit-flip would: cut, repeat, splice, replace.
    fn mutate(rng: &mut Rng, seed: &str, pool: &[&str]) -> String {
        let mut chars: Vec<char> = seed.chars().collect();
        for _ in 0..1 + rng.below(6) {
            if chars.is_empty() {
                break;
            }
            let at = rng.below(chars.len());
            match rng.below(5) {
                0 => chars.truncate(at),
                1 => {
                    let end = (at + 1 + rng.below(40)).min(chars.len());
                    let piece: Vec<char> = chars[at..end].to_vec();
                    for _ in 0..1 + rng.below(30) {
                        for (k, c) in piece.iter().enumerate() {
                            chars.insert(at + k, *c);
                        }
                    }
                }
                2 => {
                    let end = (at + 1 + rng.below(20)).min(chars.len());
                    chars.drain(at..end);
                }
                3 => {
                    let t: Vec<char> = pool[rng.below(pool.len())].chars().collect();
                    for (k, c) in t.iter().enumerate() {
                        chars.insert(at + k, *c);
                    }
                }
                _ => chars[at] = char::from_u32(rng.next() as u32 % 0x3000).unwrap_or('?'),
            }
        }
        chars.into_iter().collect()
    }

    const MASTER: &str = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",URI=\"a/audio.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"aud\"\nv720/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\nv360/index.m3u8\n";
    const MEDIA: &str = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"800@0\"\n#EXTINF:6.0,\n#EXT-X-BYTERANGE:1000@800\nseg1.m4s\n#EXT-X-DISCONTINUITY\n#EXTINF:6.0,\nseg2.m4s\n#EXT-X-ENDLIST\n";
    const MPD: &str = "<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"PT12S\"><Period><AdaptationSet mimeType=\"video/mp4\" contentType=\"video\"><SegmentTemplate initialization=\"i-$RepresentationID$.mp4\" media=\"s-$RepresentationID$-$Number$.m4s\" duration=\"6\" startNumber=\"1\"/><Representation id=\"v1\" bandwidth=\"1000000\" width=\"1280\" height=\"720\" codecs=\"avc1.4d401f\"/></AdaptationSet><AdaptationSet mimeType=\"audio/mp4\" contentType=\"audio\"><SegmentTemplate initialization=\"i-$RepresentationID$.mp4\" media=\"s-$RepresentationID$-$Time$.m4s\"><SegmentTimeline><S t=\"0\" d=\"6\" r=\"-1\"/></SegmentTimeline></SegmentTemplate><Representation id=\"a1\" bandwidth=\"128000\"/></AdaptationSet></Period></MPD>";
    const POOL: &[&str] = &[
        "#EXTINF:-1,",
        "#EXT-X-BYTERANGE:99999999999999999999@0",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"k\"",
        "#EXT-X-MAP:URI=\"\"",
        "\u{0}",
        "\r\n\r\n",
        "<!DOCTYPE x [<!ENTITY a \"aaaa\">]>",
        "<Period>",
        "</MPD>",
        "r=\"-99999999\"",
        "d=\"0\"",
        "$Number%09999999999d$",
        "PT99999999999999S",
        "file:///etc/passwd",
        "http://127.0.0.1/x",
        "\"\"\"",
        "=====",
        "BANDWIDTH=-1",
        "RESOLUTION=99999999999x1",
    ];

    fn base() -> Url {
        Url::parse("https://cdn.example.com/a/master.m3u8").unwrap()
    }

    #[test]
    fn damaged_playlists_and_manifests_are_refused_or_read_but_never_crash() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let started = std::time::Instant::now();
        let (mut ok, mut refused) = (0, 0);
        for i in 0..1500 {
            let seed = [MASTER, MEDIA, MPD][i % 3];
            let text = mutate(&mut rng, seed, POOL);
            // Any answer is fine; a panic or a hang is not.
            let r1 = crate::hls::parse(&base(), &text).is_ok();
            let r2 = crate::hls::parse_track(&base(), &text).is_ok();
            let r3 = crate::dash::parse(&base(), &text).is_ok();
            if r1 || r2 || r3 {
                ok += 1;
            } else {
                refused += 1;
            }
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "parsing damaged input was too slow");
        assert!(ok > 0 && refused > 0, "the generator should produce both readable and refused input ({ok}/{refused})");
    }

    #[test]
    fn random_text_and_bytes_never_crash_the_parsers() {
        let mut rng = Rng(42);
        for _ in 0..800 {
            let len = rng.below(3000);
            let text: String = (0..len).map(|_| char::from_u32(rng.next() as u32 % 0x250).unwrap_or(' ')).collect();
            let _ = crate::hls::parse(&base(), &text);
            let _ = crate::hls::parse_track(&base(), &text);
            let _ = crate::dash::parse(&base(), &text);
            let _ = crate::hls::attributes(&text);
            let _ = crate::dash::iso_duration(&text);
            let _ = crate::resolve_url(&base(), &text);
            let _ = crate::sanitize_stem(&text);
        }
    }

    #[test]
    fn deeply_nested_and_huge_input_stays_bounded() {
        let deep = format!("{}{}", "<a>".repeat(50_000), "</a>".repeat(50_000));
        assert!(crate::dash::parse(&base(), &format!("<MPD>{deep}</MPD>")).is_err());
        let wide = format!("<MPD><Period>{}</Period></MPD>", "<AdaptationSet/>".repeat(200_000));
        assert!(crate::dash::parse(&base(), &wide).is_err());
        let long_line = format!("#EXTM3U\n#EXTINF:1,\n{}\n", "a".repeat(5_000_000));
        let _ = crate::hls::parse_track(&base(), &long_line);
        let many = format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n{}", "#EXTINF:1,\ns.ts\n".repeat(200_000));
        assert!(crate::hls::parse_track(&base(), &many).is_err(), "more segments than the limit is refused");
    }
}
