//! Videos on web pages (YouTube, Vimeo...) through **yt-dlp** (fetched and kept up to date by VeloxFetch, see `tools.rs`).
//!
//! ```text
//! page link --analyze--> yt-dlp -J (JSON, nothing written) --> qualities
//!           --download-> yt-dlp -f <selector> into a scratch folder --> one published file
//! ```
//!
//! Same stance as `ffmpeg.rs`:
//! * the program is found by *absolute* path (next to the app, then absolute `PATH` entries);
//! * every argument is built here, as an array (no shell); the user's yt-dlp configuration files are
//!   ignored (`--ignore-config`), so nothing on disk can add `--exec` or another output path;
//! * the page link always comes after `--`, so it can never be read as an option;
//! * the output name is fixed (`media.<ext>` inside our own scratch folder); the published name is
//!   made by `sanitize_stem` and placed inside the folder the caller validated;
//! * cookies are never passed (a cookie file or argument would put them on disk or in the process list);
//! * DRM-protected videos and live streams are refused, never half-handled;
//! * the JSON it prints is size-limited, every wait has a time limit, and cancelling kills it.
//!
//! Streaming services that protect every video with DRM (Netflix, Shahid, STC tv...) are recognised
//! by address and refused up front with a clear message (`protected_service`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::download::{confine, publish, Outcome, Phase, Progress, Scratch};
use crate::{
    limits, sanitize_stem, Analysis, MediaError, OutputFormat, Playlist, PlaylistEntry, Result, StreamKind, StreamOption, TrackOption,
};

/// Sites whose video pages VeloxFetch hands to yt-dlp: `(host suffix, path parts that mark a video page)`.
/// An empty path list means any page except the home page. Kept in sync with the extension's
/// `VIDEO_SITES` (a test compares them).
pub const VIDEO_SITES: &[(&str, &[&str])] = &[
    ("youtube.com", &["/watch", "/shorts/", "/embed/", "/live/"]),
    ("youtu.be", &[]),
    ("youtube-nocookie.com", &["/embed/"]),
    ("vimeo.com", &[]),
    ("dailymotion.com", &["/video/"]),
    ("dai.ly", &[]),
    ("twitch.tv", &["/videos/", "/clip/"]),
    ("tiktok.com", &["/video/"]),
    ("x.com", &["/status/"]),
    ("twitter.com", &["/status/"]),
    ("facebook.com", &["/watch", "/videos/", "/reel/"]),
    ("fb.watch", &[]),
    ("instagram.com", &["/p/", "/reel/", "/tv/"]),
    ("reddit.com", &["/comments/"]),
    ("streamable.com", &[]),
    ("rumble.com", &[]),
    ("bilibili.com", &["/video/"]),
    ("soundcloud.com", &[]),
];

/// Playlist pages of those sites (no video on the page itself: the browser's bar never shows there, so the extension
/// does not need them). Their videos are listed and each is downloaded on its own. Kept in sync with the UI's
/// `PLAYLIST_PAGES` (a test compares them). Vimeo showcases and SoundCloud sets are already in `VIDEO_SITES`.
pub const PLAYLIST_PAGES: &[(&str, &[&str])] = &[("youtube.com", &["/playlist"]), ("dailymotion.com", &["/playlist/"])];

/// Services that protect their videos with DRM (Widevine / PlayReady / FairPlay). Nothing can be
/// downloaded from them, by design: VeloxFetch says so instead of failing halfway. Kept in sync with
/// the extension's `PROTECTED_SITES`.
pub const PROTECTED_SITES: &[(&str, &str)] = &[
    ("netflix.com", "Netflix"),
    ("shahid.mbc.net", "Shahid"),
    ("shahid.net", "Shahid"),
    ("stctv.com", "STC tv"),
    ("jawwy.tv", "STC tv"),
    ("primevideo.com", "Prime Video"),
    ("disneyplus.com", "Disney+"),
    ("osnplus.com", "OSN+"),
    ("hulu.com", "Hulu"),
    ("max.com", "Max"),
    ("hbomax.com", "Max"),
    ("tv.apple.com", "Apple TV+"),
    ("starzplay.com", "STARZPLAY"),
    ("tod.tv", "TOD"),
    ("paramountplus.com", "Paramount+"),
    ("peacocktv.com", "Peacock"),
    ("crunchyroll.com", "Crunchyroll"),
    ("watchit.com", "WATCH IT"),
    ("spotify.com", "Spotify"),
];

/// The JSON yt-dlp prints for one video is a few MB at most.
const MAX_JSON_BYTES: usize = 32 * 1024 * 1024;
const ANALYZE_TIMEOUT: Duration = Duration::from_secs(150);
/// No progress line for this long while downloading: yt-dlp is stuck and is stopped.
const STALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Fragments yt-dlp fetches at once (DASH / HLS videos).
const FRAGMENTS: &str = "16";
const PROGRESS_TAG: &str = "VELOXP";

fn host_matches(host: &str, suffix: &str) -> bool {
    host == suffix || host.strip_suffix(suffix).is_some_and(|h| h.ends_with('.'))
}

/// The service's name when `url` belongs to a DRM-only streaming service.
pub fn protected_service(url: &Url) -> Option<&'static str> {
    let host = url.host_str()?.to_ascii_lowercase();
    if host_matches(&host, "amazon.com") && url.path().starts_with("/gp/video") {
        return Some("Prime Video");
    }
    PROTECTED_SITES.iter().find(|(h, _)| host_matches(&host, h)).map(|(_, name)| *name)
}

/// Is `url` a video page of a site handled through yt-dlp?
pub fn is_site_page(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else { return false };
    let path = url.path();
    VIDEO_SITES
        .iter()
        .any(|(h, paths)| host_matches(&host, h) && if paths.is_empty() { path.len() > 1 } else { paths.iter().any(|p| path.contains(p)) })
        || PLAYLIST_PAGES.iter().any(|(h, paths)| host_matches(&host, h) && paths.iter().any(|p| path.starts_with(p)))
}

/// The playlist a video link belongs to (`youtube.com/watch?v=...&list=<id>`), as the playlist's own page. Only the id
/// is taken from the link (letters, digits, `-` and `_`); everything else is dropped.
pub fn playlist_link(url: &Url) -> Option<Url> {
    let host = url.host_str()?.to_ascii_lowercase();
    if !matches!(url.scheme(), "http" | "https") || !(host_matches(&host, "youtube.com") || host_matches(&host, "youtu.be")) {
        return None;
    }
    let id = url.query_pairs().find(|(k, _)| k == "list").map(|(_, v)| v.into_owned())?;
    if !(2..=64).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    Url::parse(&format!("https://www.youtube.com/playlist?list={id}")).ok()
}

/// The qualities offered for every video of a playlist (each video gets the closest it has, never more).
pub const PLAYLIST_QUALITIES: &[&str] = &["best", "h2160", "h1440", "h1080", "h720", "h480", "h360", "audio"];

/// Where yt-dlp is: next to the app first, else the first absolute `PATH` entry that has one.
pub fn find_ytdlp(app_dir: Option<&Path>) -> Option<PathBuf> {
    crate::ffmpeg::find_program(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" }, app_dir)
}

/// A found yt-dlp (and, when available, the ffmpeg it uses to join video and audio, and the JavaScript
/// runtime YouTube's pages need).
#[derive(Debug, Clone)]
pub struct SiteTool {
    program: PathBuf,
    ffmpeg: Option<PathBuf>,
    deno: Option<PathBuf>,
    /// No JavaScript runtime at all (`--no-js-runtimes`): the fallback when Deno hangs or fails on this computer.
    no_js: bool,
    analyze_timeout: Duration,
}

impl SiteTool {
    pub fn new(program: PathBuf, ffmpeg: Option<PathBuf>) -> Self {
        Self { program, ffmpeg, deno: None, no_js: false, analyze_timeout: ANALYZE_TIMEOUT }
    }
    /// Read pages without any JavaScript runtime (fewer YouTube formats, but no dependency on Deno).
    pub fn without_js(mut self) -> Self {
        self.deno = None;
        self.no_js = true;
        self
    }
    /// `--no-js-runtimes` (right after `--ignore-config`) when this tool runs without one.
    fn js_args(&self, mut args: Vec<OsString>) -> Vec<OsString> {
        if self.no_js {
            args.insert(1.min(args.len()), OsString::from("--no-js-runtimes"));
        }
        args
    }
    /// Will yt-dlp run Deno for this page?
    pub fn uses_js(&self) -> bool {
        self.deno.is_some() && !self.no_js
    }
    /// How long reading a page may take before yt-dlp is stopped.
    pub fn with_analyze_timeout(mut self, t: Duration) -> Self {
        self.analyze_timeout = t;
        self
    }
    /// Let yt-dlp use this Deno (an absolute path) to answer the sites' JavaScript challenges (YouTube).
    pub fn with_deno(mut self, deno: Option<PathBuf>) -> Self {
        self.deno = deno.filter(|d| d.is_absolute());
        self
    }
    pub fn program(&self) -> &Path {
        &self.program
    }
}

/// Browser context passed to yt-dlp. Printable ASCII only (anything else is dropped).
#[derive(Debug, Clone, Default)]
pub struct SiteHeaders {
    pub referer: Option<String>,
    pub user_agent: Option<String>,
}

fn header_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 2048 && s.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// Options every run gets, before anything else.
fn base_args(headers: &SiteHeaders, deno: Option<&Path>) -> Vec<OsString> {
    let mut a: Vec<OsString> =
        ["--ignore-config", "--no-playlist", "--no-warnings", "--no-color", "--no-exec", "--no-batch-file", "--socket-timeout", "30"]
            .iter()
            .map(OsString::from)
            .collect();
    if let Some(d) = deno.filter(|d| d.is_absolute()) {
        let mut v = OsString::from("deno:");
        v.push(d.as_os_str());
        a.extend([OsString::from("--js-runtimes"), v]);
    }
    if let Some(r) =
        headers.referer.as_deref().filter(|r| header_ok(r) && Url::parse(r).is_ok_and(|u| matches!(u.scheme(), "http" | "https")))
    {
        a.extend([OsString::from("--referer"), OsString::from(r)]);
    }
    if let Some(ua) = headers.user_agent.as_deref().filter(|u| header_ok(u)) {
        a.extend([OsString::from("--user-agent"), OsString::from(ua)]);
    }
    a
}

/// The page link, checked and always last, after `--`.
fn push_url(a: &mut Vec<OsString>, url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
        return Err(MediaError::site("only plain http and https links can be used"));
    }
    a.push(OsString::from("--"));
    a.push(OsString::from(url.as_str()));
    Ok(())
}

pub(crate) fn analyze_args(url: &Url, headers: &SiteHeaders, deno: Option<&Path>) -> Result<Vec<OsString>> {
    let mut a = base_args(headers, deno);
    // `--verbose`: what yt-dlp was doing goes to stderr, so a page that never answers can be explained
    // in the log (see `analyze_site`). stdout stays the JSON alone.
    // A playlist link: only its list (titles, lengths, links), not every video, and at most one more than is
    // kept, to know there are more. A single video is read in full as before (`--flat-playlist` does not change
    // it, and `--no-playlist` keeps `watch?v=...&list=...` to the one video).
    let items = format!("1:{}", limits::MAX_PLAYLIST_VIDEOS + 1);
    a.extend(
        ["--flat-playlist", "--playlist-items", items.as_str(), "--dump-single-json", "--skip-download", "--verbose"].map(OsString::from),
    );
    push_url(&mut a, url)?;
    Ok(a)
}

/// Quality ids are `best`, `audio` or `h<height>`; anything else is refused.
fn check_quality(id: Option<&str>) -> Result<Option<u32>> {
    match id {
        None | Some("best") | Some("audio") => Ok(None),
        Some(s) => s
            .strip_prefix('h')
            .filter(|n| !n.is_empty() && n.len() <= 5 && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<u32>().ok())
            .map(Some)
            .ok_or_else(|| MediaError::site("unknown quality")),
    }
}

/// yt-dlp's format selector for a quality id. Without ffmpeg only formats that already hold both
/// video and audio can be used.
pub(crate) fn selector(quality: Option<&str>, with_ffmpeg: bool) -> Result<String> {
    let height = check_quality(quality)?;
    Ok(match (quality, height, with_ffmpeg) {
        (Some("audio"), _, _) => "ba/b".into(),
        (_, Some(h), true) => format!("bv*[height<={h}]+ba/b[height<={h}]/bv*+ba/b"),
        (_, Some(h), false) => format!("b[height<={h}]/b"),
        (_, None, true) => "bv*+ba/b".into(),
        (_, None, false) => "b".into(),
    })
}

/// The file type a download ends up as: the user's choice, else MP4 for a video and MP3 for "audio".
/// An audio type chosen with a video quality means "only the sound"; a video type chosen with
/// "audio" is ignored (MP3).
pub fn effective_format(quality: Option<&str>, format: Option<OutputFormat>) -> OutputFormat {
    match (quality == Some("audio"), format) {
        (_, Some(f)) if f.is_audio() => f,
        (true, _) => OutputFormat::Mp3,
        (false, Some(f)) => f,
        (false, None) => OutputFormat::Mp4,
    }
}

/// Which of the page's formats to prefer for a file type (yt-dlp's `-S`), so the picture and sound
/// usually go into the file as they are: H.264 + AAC for MP4 (plays everywhere), VP9 + Opus for
/// WebM. The resolution always comes first, so the chosen quality is never lowered for it.
fn preference(format: OutputFormat) -> Option<&'static str> {
    match format {
        OutputFormat::Mp4 => Some("res,fps,vcodec:h264,acodec:aac"),
        OutputFormat::Webm => Some("res,fps,vcodec:vp9,acodec:opus"),
        OutputFormat::M4a => Some("acodec:aac"),
        OutputFormat::Opus => Some("acodec:opus"),
        OutputFormat::Mkv | OutputFormat::Mp3 => None,
    }
}

/// The file in the work folder where ffmpeg writes how far its joining / converting is (`-progress`).
pub(crate) const FFMPEG_PROGRESS_FILE: &str = "ffmpeg-progress.txt";

/// `s` quoted for yt-dlp's own argument splitting (POSIX shell rules): inside single quotes nothing is
/// special, a single quote is closed, escaped and reopened. Backslashes (Windows paths) stay as they are.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Does turning the download into `format` need converting (re-encoding) rather than repackaging?
/// MP3 always does (no site sends MP3); WebM may (picture or sound not in WebM's codecs).
pub(crate) fn converts(format: OutputFormat) -> bool {
    matches!(format, OutputFormat::Mp3 | OutputFormat::Webm)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn download_args(
    url: &Url,
    headers: &SiteHeaders,
    quality: Option<&str>,
    format: OutputFormat,
    scratch: &Path,
    ffmpeg: Option<&Path>,
    deno: Option<&Path>,
    rate: Option<u64>,
) -> Result<Vec<OsString>> {
    let scratch_s = scratch.as_os_str().to_string_lossy();
    if !scratch.is_absolute() || scratch_s.starts_with('-') {
        return Err(MediaError::site("internal error: the working folder is not absolute"));
    }
    check_quality(quality)?;
    let quality = if format.is_audio() { Some("audio") } else { quality };
    let mut a = base_args(headers, deno);
    a.extend(
        [
            "--newline",
            "--no-mtime",
            "--continue",
            "--retries",
            "10",
            "--extractor-retries",
            "3",
            "--fragment-retries",
            "10",
            "--concurrent-fragments",
            FRAGMENTS,
            "--progress-template",
            "download:VELOXP %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(info.duration)s",
            "-f",
        ]
        .map(OsString::from),
    );
    a.push(OsString::from(selector(quality, ffmpeg.is_some())?));
    // The app's speed limits (the user's global limit, the Smart throttle): yt-dlp paces itself.
    if let Some(r) = rate.filter(|r| *r > 0) {
        a.extend([OsString::from("--limit-rate"), OsString::from(r.max(1024).to_string())]);
    }
    if let Some(sort) = preference(format) {
        a.extend([OsString::from("-S"), OsString::from(sort)]);
    }
    if let Some(f) = ffmpeg {
        if !f.is_absolute() {
            return Err(MediaError::site("internal error: a dependency path is not absolute"));
        }
        let ext = format.extension();
        match format {
            // Only the sound, in the chosen type (copied when it already is that type).
            _ if format.is_audio() => a.extend(["--extract-audio", "--audio-format", ext, "--audio-quality", "0"].map(OsString::from)),
            // MP4 and MKV hold any picture and sound: joined and repackaged without re-encoding.
            OutputFormat::Mp4 | OutputFormat::Mkv => a.extend(["--merge-output-format", ext, "--remux-video", ext].map(OsString::from)),
            // WebM holds only VP9 / AV1 + Opus / Vorbis: anything else is converted (rare: WebM is preferred above).
            _ => a.extend(["--merge-output-format", "webm/mkv", "--recode-video", "webm"].map(OsString::from)),
        }
        a.push(OsString::from("--ffmpeg-location"));
        a.push(f.as_os_str().to_os_string());
        // ffmpeg prints nothing yt-dlp passes on while it joins or converts: it writes how far it is to a file
        // in the work folder instead, which the progress reader follows (no more "99 %" for minutes).
        let file = shell_quote(&scratch.join(FFMPEG_PROGRESS_FILE).to_string_lossy());
        for pp in ["Merger", "VideoRemuxer", "VideoConvertor", "ExtractAudio"] {
            a.push(OsString::from("--postprocessor-args"));
            a.push(OsString::from(format!("{pp}+ffmpeg_i1:-progress {file} -nostats")));
        }
    }
    a.extend(["--paths".into(), scratch.as_os_str().to_os_string(), "--output".into(), OsString::from("media.%(ext)s")]);
    push_url(&mut a, url)?;
    Ok(a)
}

/// Fetching only the chosen subtitles (`ids` from the analysis), into `sub.<lang>.<ext>` in the work folder. A run of
/// its own after the video: a language the site refuses then loses only the subtitles, never the video.
pub(crate) fn subtitle_args(
    url: &Url,
    headers: &SiteHeaders,
    ids: &[String],
    scratch: &Path,
    ffmpeg: Option<&Path>,
    deno: Option<&Path>,
) -> Result<Vec<OsString>> {
    let scratch_s = scratch.as_os_str().to_string_lossy();
    if !scratch.is_absolute() || scratch_s.starts_with('-') {
        return Err(MediaError::site("internal error: the working folder is not absolute"));
    }
    let mut langs: Vec<&str> = Vec::new();
    let (mut people, mut site) = (false, false);
    for id in ids {
        let (auto, lang) = subtitle_id(id).ok_or_else(|| MediaError::invalid("unknown subtitle track"))?;
        if auto {
            site = true;
        } else {
            people = true;
        }
        if !langs.contains(&lang) {
            langs.push(lang);
        }
    }
    if langs.is_empty() {
        return Err(MediaError::invalid("no subtitles were chosen"));
    }
    let mut a = base_args(headers, deno);
    // One language the site refuses (YouTube limits its automatic translations) must not stop the others: yt-dlp goes
    // on, and its warnings name what it skipped (`refused_subtitles`).
    a.retain(|x| x != "--no-warnings");
    a.extend(["--skip-download", "--ignore-errors", "--sub-format", "srt/vtt/ass/best"].map(OsString::from));
    if people {
        a.push(OsString::from("--write-subs"));
    }
    if site {
        a.push(OsString::from("--write-auto-subs"));
    }
    a.extend([OsString::from("--sub-langs"), OsString::from(langs.join(","))]);
    if let Some(f) = ffmpeg {
        if !f.is_absolute() {
            return Err(MediaError::site("internal error: a dependency path is not absolute"));
        }
        // SRT: what TVs, phones and every player read.
        a.extend(["--convert-subs", "srt", "--ffmpeg-location"].map(OsString::from));
        a.push(f.as_os_str().to_os_string());
    }
    a.extend([
        "--paths".into(),
        scratch.as_os_str().to_os_string(),
        "--output".into(),
        OsString::from("subtitle:sub.%(ext)s"),
        "--output".into(),
        OsString::from("media.%(ext)s"),
    ]);
    push_url(&mut a, url)?;
    Ok(a)
}

// ---------------------------------------------------------------------------------------------
// Reading yt-dlp's answer

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0)
}

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn has(v: &Value, codec: &str) -> bool {
    text(v, codec).is_some_and(|c| c != "none")
}

fn drm(v: &Value) -> bool {
    v.get("has_drm").and_then(Value::as_bool) == Some(true)
}

fn size_of(f: &Value) -> Option<u64> {
    num(f, "filesize").or_else(|| num(f, "filesize_approx")).map(|n| n as u64)
}

fn human(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

const DRM_TEXT: &str = "This video is protected with DRM (copy protection), so it cannot be downloaded.";

/// Subtitle file types a track must come in to be offered (anything else, such as a live chat replay, is not a subtitle).
const SUBTITLE_TYPES: &[&str] = &["srt", "vtt", "ass", "ssa", "ttml", "dfxp", "srv1", "srv2", "srv3", "json3"];
/// Subtitle files saved next to the video (what yt-dlp writes, after converting to SRT when it can).
const SUBTITLE_FILES: &[&str] = &["srt", "vtt", "ass", "ssa", "ttml"];
/// At most this many languages are offered (YouTube translates its captions into about 160).
const MAX_SUBTITLE_TRACKS: usize = 400;
/// How long fetching the subtitles may take before yt-dlp is stopped.
const SUBTITLES_TIMEOUT: Duration = Duration::from_secs(150);

/// A language code as the page names it (`en`, `pt-BR`, `zh-Hans`, `en-orig`): letters, digits and inner dashes only, so
/// it is never an option (`-...`), never a pattern, and never yt-dlp's `all`.
fn subtitle_lang(lang: &str) -> bool {
    (1..=15).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !lang.starts_with('-')
        && !lang.ends_with('-')
        && !lang.eq_ignore_ascii_case("all")
}

/// Is `id` a video page's subtitle id (see `subtitle_id`)? Checked before a download starts.
pub fn is_subtitle_id(id: &str) -> bool {
    subtitle_id(id).is_some()
}

/// A subtitle id from the analysis: `m<lang>` (written by the site's people) or `a<lang>` (made by the site: captions
/// from the speech, or their automatic translation). `None` = unknown.
pub(crate) fn subtitle_id(id: &str) -> Option<(bool, &str)> {
    let auto = match id.as_bytes().first()? {
        b'm' => false,
        b'a' => true,
        _ => return None,
    };
    Some((auto, &id[1..])).filter(|(_, lang)| subtitle_lang(lang))
}

/// The subtitles a page offers: those written by people first, then the site's own captions (from the speech, then
/// translated into other languages). A language people wrote is never offered twice.
fn subtitles_from_json(json: &Value) -> Vec<TrackOption> {
    let listed = |key: &str| -> Vec<(String, Option<String>)> {
        let Some(map) = json.get(key).and_then(Value::as_object) else { return Vec::new() };
        map.iter()
            .filter(|(lang, _)| subtitle_lang(lang))
            .filter_map(|(lang, formats)| {
                let formats = formats.as_array()?;
                formats.iter().any(|f| text(f, "ext").is_some_and(|e| SUBTITLE_TYPES.contains(&e))).then(|| {
                    let name =
                        formats.iter().find_map(|f| text(f, "name")).map(|n| n.chars().filter(|c| !c.is_control()).take(60).collect());
                    (lang.clone(), name)
                })
            })
            .collect()
    };
    let video_lang = text(json, "language").unwrap_or("");
    let mut manual = listed("subtitles");
    manual.sort_by(|a, b| a.1.as_deref().unwrap_or(&a.0).cmp(b.1.as_deref().unwrap_or(&b.0)));
    let auto = listed("automatic_captions");
    let has_orig = |lang: &str| auto.iter().any(|(l, _)| l.strip_suffix("-orig") == Some(lang));
    let (mut spoken, mut translated): (Vec<_>, Vec<_>) = auto
        .iter()
        .filter(|(lang, _)| !manual.iter().any(|(m, _)| m == lang.strip_suffix("-orig").unwrap_or(lang)))
        .filter(|(lang, _)| !has_orig(lang)) // `en` beside `en-orig` is the same captions
        .partition(|(lang, _)| lang.ends_with("-orig") || lang == video_lang);
    spoken.sort();
    translated.sort_by(|a, b| a.1.as_deref().unwrap_or(&a.0).cmp(b.1.as_deref().unwrap_or(&b.0)));

    let track = |prefix: &str, (lang, name): &(String, Option<String>), kind: Option<&str>| {
        let base = lang.strip_suffix("-orig").unwrap_or(lang);
        let name = name.as_deref().map(|n| n.trim_end_matches(" (Original)").trim()).filter(|n| !n.is_empty()).unwrap_or(base);
        TrackOption {
            id: format!("{prefix}{lang}"),
            label: kind.map_or_else(|| name.to_string(), |k| format!("{name} · {k}")),
            language: Some(base.to_string()),
            group: None,
            default: false,
        }
    };
    manual
        .iter()
        .map(|m| track("m", m, None))
        .chain(spoken.into_iter().map(|s| track("a", s, Some("auto-generated"))))
        .chain(translated.into_iter().map(|t| track("a", t, Some("auto-translated"))))
        .take(MAX_SUBTITLE_TRACKS)
        .collect()
}

/// What can be downloaded from yt-dlp's JSON for one video.
pub(crate) fn analysis_from_json(json: &Value, with_ffmpeg: bool) -> Result<Analysis> {
    if text(json, "_type").is_some_and(|t| t == "playlist" || t == "multi_video") {
        return playlist_from_json(json);
    }
    if json.get("is_live").and_then(Value::as_bool) == Some(true)
        || text(json, "live_status").is_some_and(|s| s == "is_live" || s == "is_upcoming" || s == "post_live")
    {
        return Err(MediaError::unsupported("live streams cannot be downloaded"));
    }
    let empty = Vec::new();
    let formats = json.get("formats").and_then(Value::as_array).unwrap_or(&empty);
    if formats.len() > 5000 {
        return Err(MediaError::limit("the page lists too many formats"));
    }
    if drm(json) || (!formats.is_empty() && formats.iter().all(drm)) {
        return Err(MediaError::unsupported(DRM_TEXT));
    }
    let usable: Vec<&Value> = formats.iter().filter(|f| !drm(f) && text(f, "protocol") != Some("mhtml")).collect();
    let best_audio = usable.iter().filter(|f| has(f, "acodec") && !has(f, "vcodec")).filter_map(|f| size_of(f)).max();

    // Heights that can be produced: any video with ffmpeg, else only formats with both video and audio.
    struct Height {
        height: u32,
        size: Option<u64>,
        width: Option<u32>,
        codec: Option<String>,
    }
    let mut heights: Vec<Height> = Vec::new();
    for f in &usable {
        if !has(f, "vcodec") || (!with_ffmpeg && !has(f, "acodec")) {
            continue;
        }
        let Some(h) = num(f, "height").map(|h| h as u32).filter(|h| (1..=10_000).contains(h)) else { continue };
        let size = size_of(f).map(|s| if has(f, "acodec") { s } else { s + best_audio.unwrap_or(0) });
        match heights.iter_mut().find(|e| e.height == h) {
            Some(e) => e.size = e.size.max(size),
            None => heights.push(Height {
                height: h,
                size,
                width: num(f, "width").map(|w| w as u32),
                codec: text(f, "vcodec").map(|c| c.split('.').next().unwrap_or(c).chars().take(12).collect()),
            }),
        }
        if heights.len() > 40 {
            break;
        }
    }
    heights.sort_by_key(|e| std::cmp::Reverse(e.height));
    let mut streams: Vec<StreamOption> = heights
        .into_iter()
        .map(|e| StreamOption {
            id: format!("h{}", e.height),
            label: match e.size {
                Some(s) => format!("{}p · about {}", e.height, human(s)),
                None => format!("{}p", e.height),
            },
            bandwidth: None,
            width: e.width,
            height: Some(e.height),
            codecs: e.codec,
            group: None,
        })
        .collect();
    if streams.is_empty() && (json.get("url").is_some() || !usable.is_empty()) {
        streams.push(StreamOption {
            id: "best".into(),
            label: "Best available".into(),
            bandwidth: None,
            width: None,
            height: None,
            codecs: None,
            group: None,
        });
    }
    if usable.iter().any(|f| has(f, "acodec")) || json.get("url").is_some() {
        streams.push(StreamOption {
            id: "audio".into(),
            label: match best_audio {
                Some(s) => format!("Audio only · about {}", human(s)),
                None => "Audio only".into(),
            },
            bandwidth: None,
            width: None,
            height: None,
            codecs: None,
            group: None,
        });
    }
    if streams.is_empty() {
        return Err(MediaError::unsupported("no downloadable video was found on this page"));
    }
    let title = text(json, "title").map(sanitize_stem).unwrap_or_else(|| "video".into());
    Ok(Analysis {
        kind: StreamKind::Site,
        title,
        duration_secs: num(json, "duration"),
        streams,
        audio: Vec::new(),
        subtitles: subtitles_from_json(json),
        playlist: None,
        part_of_playlist: false,
    })
}

/// A playlist from yt-dlp's flat list. Only videos of the sites VeloxFetch reads (`is_site_page`) are kept: a
/// playlist cannot point the app anywhere else, nor at another playlist or a channel.
pub(crate) fn playlist_from_json(json: &Value) -> Result<Analysis> {
    let empty = Vec::new();
    let all = json.get("entries").and_then(Value::as_array).unwrap_or(&empty);
    let mut entries = Vec::new();
    for (i, e) in all.iter().enumerate().take(limits::MAX_PLAYLIST_VIDEOS) {
        let Some(url) =
            text(e, "url").or_else(|| text(e, "webpage_url")).filter(|u| u.len() <= limits::MAX_URL_LEN).and_then(|u| Url::parse(u).ok())
        else {
            continue;
        };
        if text(e, "_type").is_some_and(|t| t == "playlist")
            || !is_site_page(&url)
            || protected_service(&url).is_some()
            || playlist_page(&url)
        {
            continue;
        }
        if !url.username().is_empty() || url.password().is_some() {
            continue;
        }
        let raw_title = text(e, "title").unwrap_or("");
        let gone = raw_title.starts_with('[') && raw_title.ends_with(" video]"); // "[Private video]", "[Deleted video]"
        let closed = text(e, "availability").is_some_and(|a| matches!(a, "private" | "needs_auth" | "premium_only" | "subscriber_only"));
        let live = e.get("is_live").and_then(Value::as_bool) == Some(true)
            || text(e, "live_status").is_some_and(|s| matches!(s, "is_live" | "is_upcoming" | "post_live"));
        let title: String = raw_title.chars().filter(|c| !c.is_control()).take(200).collect::<String>().trim().to_string();
        entries.push(PlaylistEntry {
            index: (i + 1) as u32,
            title: if title.is_empty() { format!("{}", i + 1) } else { title },
            duration_secs: num(e, "duration"),
            available: !(gone || closed || live),
            url,
        });
    }
    if entries.is_empty() {
        return Err(MediaError::unsupported("no downloadable video was found in this playlist"));
    }
    let total = num(json, "playlist_count").map(|n| n.min(u32::MAX as f64) as u32);
    let truncated = all.len() > limits::MAX_PLAYLIST_VIDEOS || total.is_some_and(|t| t as usize > limits::MAX_PLAYLIST_VIDEOS);
    let title = text(json, "title").map(sanitize_stem).filter(|t| !t.is_empty()).unwrap_or_else(|| "playlist".into());
    let streams = PLAYLIST_QUALITIES
        .iter()
        .map(|id| StreamOption {
            id: id.to_string(),
            label: match *id {
                "best" => "Best available".into(),
                "audio" => "Audio only".into(),
                h => format!("{}p", &h[1..]),
            },
            bandwidth: None,
            width: None,
            height: id.strip_prefix('h').and_then(|n| n.parse().ok()),
            codecs: None,
            group: None,
        })
        .collect();
    Ok(Analysis {
        kind: StreamKind::Site,
        title: title.clone(),
        duration_secs: None,
        streams,
        audio: Vec::new(),
        subtitles: Vec::new(),
        playlist: Some(Playlist { title, total, entries, truncated }),
        part_of_playlist: false,
    })
}

/// A link that is itself a playlist or a list of playlists (never downloaded as one video).
fn playlist_page(url: &Url) -> bool {
    let path = url.path();
    path.starts_with("/playlist") || (url.query_pairs().any(|(k, _)| k == "list") && !url.query_pairs().any(|(k, _)| k == "v"))
}

/// A short, single-line reason from yt-dlp's error output (its own `ERROR:` line when there is one).
pub(crate) fn error_from_stderr(stderr: &str) -> MediaError {
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("ERROR:"))
        .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("[debug]")))
        .unwrap_or("")
        .trim();
    let line = line.strip_prefix("ERROR:").unwrap_or(line).trim();
    // yt-dlp prefixes its reason with "[site] video-id: "; the reason alone reads better.
    let line = match line.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((_, rest)) => rest.split_once(": ").map_or(rest, |(_, reason)| reason).trim(),
        None => line,
    };
    let clean: String = line.chars().filter(|c| !c.is_control()).take(300).collect();
    let clean = without_tool_hints(&clean);
    let lower = clean.to_ascii_lowercase();
    if lower.contains("drm") {
        MediaError::unsupported(DRM_TEXT)
    } else if lower.contains("sign in") || lower.contains("login") || (lower.contains("age") && lower.contains("confirm")) {
        MediaError::unsupported(format!("the site wants you to sign in to see this video: {clean}"))
    } else if lower.contains("unsupported url") {
        MediaError::unsupported("no video was found at this address")
    } else if clean.is_empty() {
        MediaError::site("reading the video stopped without a reason")
    } else {
        MediaError::site(format!("The site says: {clean}"))
    }
}

/// The site's reason without the program's own advice (command-line options, its FAQ links, its name),
/// which means nothing to someone using VeloxFetch.
fn without_tool_hints(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    let cut = ["please report", "use --", "see http", "see  http", "yt-dlp", "youtube-dl", " --", "https://github.com"]
        .iter()
        .filter_map(|p| lower.find(p))
        .min()
        .unwrap_or(reason.len());
    reason[..cut].trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '(')).to_string()
}

/// yt-dlp's line when it starts joining, repackaging or converting the downloaded parts.
fn is_finishing_line(line: &str) -> bool {
    let l = line.trim_start();
    ["[Merger]", "[VideoRemuxer]", "[VideoConvertor]", "[ExtractAudio]", "[Fixup"].iter().any(|t| l.starts_with(t))
}

/// `downloaded total estimate` from one progress line (`NA` = unknown).
pub(crate) fn parse_progress(line: &str) -> Option<(u64, Option<u64>)> {
    let rest = line.trim().strip_prefix(PROGRESS_TAG)?.trim();
    let mut it = rest.split_whitespace();
    let n = |s: Option<&str>| s.and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite() && *v >= 0.0).map(|v| v as u64);
    let done = n(it.next())?;
    let total = n(it.next());
    let estimate = n(it.next());
    Some((done, total.or(estimate)))
}

/// The video's length in seconds, the fourth value of a progress line (`NA` = unknown).
pub(crate) fn parse_duration(line: &str) -> Option<f64> {
    let rest = line.trim().strip_prefix(PROGRESS_TAG)?.trim();
    rest.split_whitespace().nth(3)?.parse::<f64>().ok().filter(|d| d.is_finite() && *d > 0.0)
}

/// How far ffmpeg is, in seconds of the result, from its `-progress` file (the last `out_time_us`).
pub(crate) fn ffmpeg_done_secs(text: &str) -> Option<f64> {
    text.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("out_time_us=")?.trim().parse::<i64>().ok())
        .filter(|us| *us >= 0)
        .map(|us| us as f64 / 1_000_000.0)
}

/// Shares of the progress bar (in per-mille). Downloading takes the bar up to `download`; the joining or
/// converting that follows fills the rest from ffmpeg's own progress. Of the download, the picture (the
/// first, big part) counts for `first`, the sound for the rest: they are fetched one after the other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Shares {
    pub download: f64,
    pub first: f64,
}

pub(crate) fn shares(parts: u64, ffmpeg: bool, format: OutputFormat) -> Shares {
    let download = match (ffmpeg, converts(format)) {
        (false, _) => 999.0,
        (true, true) => 850.0,
        (true, false) => 960.0,
    };
    Shares { download, first: if parts > 1 { 0.88 } else { 1.0 } }
}

impl Shares {
    /// Per-mille for `frac` (0..1) of part `part` (0 = the first).
    pub(crate) fn at(&self, part: u64, frac: f64) -> u64 {
        let frac = frac.clamp(0.0, 1.0);
        let share = if part == 0 { self.first * frac } else { self.first + (1.0 - self.first) * frac };
        (share * self.download) as u64
    }
    /// Per-mille while finishing, `frac` (0..1) of ffmpeg's work done.
    pub(crate) fn finishing(&self, frac: f64) -> u64 {
        (self.download + (999.0 - self.download) * frac.clamp(0.0, 1.0)) as u64
    }
}

fn command(program: &Path, args: &[OsString]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    // yt-dlp must not pick up a configuration or plugins through these either.
    cmd.env_remove("YTDLP_CONFIG").env("PYTHONDONTWRITEBYTECODE", "1").env("PYTHONIOENCODING", "utf-8");
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Read at most `max` bytes of a pipe, then keep draining it so the program never blocks.
async fn read_limited<R: tokio::io::AsyncRead + Unpin>(mut r: R, max: usize) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let _ = (&mut r).take(max as u64 + 1).read_to_end(&mut buf).await;
    let over = buf.len() > max;
    buf.truncate(max);
    let mut sink = [0u8; 8192];
    while matches!(r.read(&mut sink).await, Ok(n) if n > 0) {}
    (buf, over)
}

/// The last lines yt-dlp wrote, for the log: control characters removed, long lines cut, and without the
/// line that repeats the whole command line (the page address, referrer and user agent are not needed).
pub(crate) fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<String> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("Command-line config"))
        .map(|l| l.chars().filter(|c| !c.is_control()).take(300).collect())
        .collect();
    let tail = &lines[lines.len().saturating_sub(15)..];
    if tail.is_empty() {
        "(nothing)".to_string()
    } else {
        tail.join(" | ")
    }
}

/// Ask yt-dlp what the page offers. Nothing is written.
pub async fn analyze_site(tool: &SiteTool, url: &Url, headers: &SiteHeaders) -> Result<Analysis> {
    if let Some(name) = protected_service(url) {
        return Err(MediaError::unsupported(format!(
            "{name} protects its videos with DRM (copy protection), so they cannot be downloaded"
        )));
    }
    let args = tool.js_args(analyze_args(url, headers, tool.deno.as_deref())?);
    let mut child =
        command(&tool.program, &args).spawn().map_err(|e| MediaError::site(format!("could not start a required dependency: {e}")))?;
    let out = tokio::spawn(read_limited(child.stdout.take().expect("piped"), MAX_JSON_BYTES));
    let err = tokio::spawn(read_limited(child.stderr.take().expect("piped"), 16 * 1024));
    let status = match tokio::time::timeout(tool.analyze_timeout, child.wait()).await {
        Ok(s) => s?,
        Err(_) => {
            let _ = child.kill().await;
            // What yt-dlp had said so far tells where it was stuck (a network request, the JavaScript
            // runtime...). The pipe ends with the process; don't wait for it longer than a moment.
            let said = tokio::time::timeout(Duration::from_secs(2), err).await.ok().and_then(|r| r.ok()).unwrap_or_default().0;
            tracing::warn!(output = %stderr_tail(&String::from_utf8_lossy(&said)), "yt-dlp did not finish reading the page in time");
            return Err(MediaError::site("the site did not answer in time"));
        }
    };
    let (stdout, over) = out.await.unwrap_or_default();
    let (stderr, _) = err.await.unwrap_or_default();
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        tracing::warn!(output = %stderr_tail(&stderr), "yt-dlp could not read the page");
        return Err(error_from_stderr(&stderr));
    }
    if over {
        return Err(MediaError::limit("the page description is larger than 32 MiB"));
    }
    let json: Value = serde_json::from_slice(&stdout).map_err(|_| MediaError::site("the video page could not be read"))?;
    let mut a = analysis_from_json(&json, tool.ffmpeg.is_some())?;
    a.part_of_playlist = a.playlist.is_none() && playlist_link(url).is_some();
    Ok(a)
}

/// What to download from a page.
#[derive(Clone)]
pub struct SiteSpec {
    pub url: Url,
    /// A quality id from `analyze_site` (`None` = the best).
    pub quality: Option<String>,
    /// The file type (`None` = MP4 for a video, MP3 for "audio"; see `effective_format`).
    pub format: Option<OutputFormat>,
    /// Already validated by the caller.
    pub dest_dir: PathBuf,
    pub sandbox_root: Option<PathBuf>,
    pub mark_of_the_web: bool,
    /// A name without extension (made safe again here); empty = the video's title.
    pub stem: String,
    pub headers: SiteHeaders,
    /// The job's own work folder (see `DownloadSpec::work_dir`): yt-dlp continues its `.part` files from it
    /// after a pause or an error. `None` = a temporary one.
    pub work_dir: Option<PathBuf>,
    /// Bytes per second yt-dlp may use (the app's global limit or the Smart throttle); `None` = no limit.
    pub rate_limit: Option<u64>,
    /// Subtitle ids from `analyze_site`, saved as files next to the video (not with "only the sound").
    pub subtitles: Vec<String>,
}

/// Download the video. Progress is reported as bytes and as per-mille "pieces" (yt-dlp fetches
/// video and audio one after the other; each counts for its share).
pub async fn download_site(tool: &SiteTool, spec: SiteSpec, progress: Arc<Progress>, cancel: CancellationToken) -> Result<Outcome> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(MediaError::Cancelled),
        r = download_site_inner(tool, spec, progress) => r,
    }
}

async fn download_site_inner(tool: &SiteTool, spec: SiteSpec, progress: Arc<Progress>) -> Result<Outcome> {
    progress.set(Phase::Preparing);
    if let Some(name) = protected_service(&spec.url) {
        return Err(MediaError::unsupported(format!(
            "{name} protects its videos with DRM (copy protection), so they cannot be downloaded"
        )));
    }
    // The title comes from the page when the user gave no name.
    let stem =
        if spec.stem.trim().is_empty() { analyze_site(tool, &spec.url, &spec.headers).await?.title } else { sanitize_stem(&spec.stem) };
    confine(spec.sandbox_root.as_deref(), &spec.dest_dir)?;
    let format = effective_format(spec.quality.as_deref(), spec.format);
    if spec.subtitles.len() > 50 || spec.subtitles.iter().any(|id| subtitle_id(id).is_none()) {
        return Err(MediaError::invalid("unknown subtitle track"));
    }
    let scratch = Scratch::open(&spec.dest_dir, spec.work_dir.as_deref())?;
    let args = download_args(
        &spec.url,
        &spec.headers,
        spec.quality.as_deref(),
        format,
        &scratch.0,
        tool.ffmpeg.as_deref(),
        tool.deno.as_deref(),
        spec.rate_limit,
    )
    .map(|a| tool.js_args(a))?;
    let parts: u64 = if tool.ffmpeg.is_some() && !format.is_audio() { 2 } else { 1 };
    let shares = shares(parts, tool.ffmpeg.is_some(), format);
    let ffprogress = scratch.0.join(FFMPEG_PROGRESS_FILE);
    let _ = std::fs::remove_file(&ffprogress); // a file from an earlier try says nothing about this one
    progress.segments_total.store(1000, Ordering::Relaxed);
    progress.segments_done.store(0, Ordering::Relaxed);
    progress.bytes.store(0, Ordering::Relaxed);

    let mut child =
        command(&tool.program, &args).spawn().map_err(|e| MediaError::site(format!("could not start a required dependency: {e}")))?;
    let err = tokio::spawn(read_limited(child.stderr.take().expect("piped"), 16 * 1024));
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    progress.set(Phase::Downloading);
    // `finished` = bytes of the parts already done; a progress line whose count goes down starts the next part.
    let (mut finished, mut current, mut part) = (0u64, 0u64, 0u64);
    // The video's length (from the progress lines), to turn ffmpeg's position into a share.
    let mut duration: Option<f64> = None;
    // Joining / converting prints nothing while ffmpeg works: it gets ffmpeg's own time limit, and its
    // progress file is read twice a second meanwhile.
    let mut finishing = false;
    let mut last_line = tokio::time::Instant::now();
    loop {
        let limit = if finishing { Duration::from_secs(crate::limits::REMUX_TIMEOUT_SECS) } else { STALL_TIMEOUT };
        let line = match tokio::time::timeout(Duration::from_millis(500), lines.next_line()).await {
            Ok(Ok(Some(l))) => l,
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Err(_) => {
                if last_line.elapsed() >= limit {
                    let _ = child.kill().await;
                    return Err(MediaError::site("the download stopped making progress and was stopped"));
                }
                if finishing {
                    let done = tokio::fs::read_to_string(&ffprogress).await.ok().as_deref().and_then(ffmpeg_done_secs);
                    if let (Some(done), Some(total)) = (done, duration) {
                        progress.segments_done.fetch_max(shares.finishing(done / total), Ordering::Relaxed);
                    }
                }
                continue;
            }
        };
        last_line = tokio::time::Instant::now();
        // A part finished in an earlier try (paused, or stopped by an error) is not downloaded again.
        if line.contains("has already been downloaded") {
            part = (part + 1).min(parts.saturating_sub(1));
            current = 0;
            progress.segments_done.fetch_max(shares.at(part, 0.0), Ordering::Relaxed);
            continue;
        }
        if is_finishing_line(&line) {
            finishing = true;
            progress.set(Phase::Combining);
            progress.segments_done.fetch_max(shares.finishing(0.0), Ordering::Relaxed);
            continue;
        }
        let Some((done, total)) = parse_progress(&line) else { continue };
        if duration.is_none() {
            duration = parse_duration(&line);
        }
        if finishing {
            finishing = false;
            progress.set(Phase::Downloading);
        }
        if done < current {
            finished += current;
            part = (part + 1).min(parts.saturating_sub(1));
        }
        current = done;
        let bytes = finished + current;
        if bytes > crate::limits::MAX_TOTAL_BYTES {
            let _ = child.kill().await;
            return Err(MediaError::limit("the video is larger than 64 GiB"));
        }
        progress.bytes.store(bytes, Ordering::Relaxed);
        if let Some(t) = total.filter(|t| *t > 0) {
            progress.part_total.store(t, Ordering::Relaxed);
            let permille = shares.at(part, current as f64 / t as f64);
            progress.segments_done.fetch_max(permille.min(999), Ordering::Relaxed);
        }
    }
    let status = child.wait().await?;
    let (stderr, _) = err.await.unwrap_or_default();
    if !status.success() {
        return Err(error_from_stderr(&String::from_utf8_lossy(&stderr)));
    }
    progress.set(Phase::Combining);
    // The result: the one `media.<ext>` left in our folder (intermediate files are removed by yt-dlp).
    let mut found: Option<(PathBuf, u64)> = None;
    for e in std::fs::read_dir(&scratch.0)?.flatten() {
        let meta = std::fs::symlink_metadata(e.path())?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !meta.is_file() || !name.starts_with("media.") || name.ends_with(".part") || name.ends_with(".ytdl") || name.contains(".temp") {
            continue;
        }
        if found.as_ref().is_none_or(|(_, n)| meta.len() > *n) {
            found = Some((e.path(), meta.len()));
        }
    }
    let (file, _) = found.ok_or_else(|| MediaError::site("the video finished without saving a file"))?;
    let ext: String = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .filter(|e| {
            ["mp4", "mkv", "webm", "m4a", "mp3", "opus", "ogg", "aac", "mov", "flv", "3gp", "wav", "flac", "m4v", "mka", "weba", "ts"]
                .contains(&e.as_str())
        })
        .unwrap_or_else(|| "mp4".into());
    let published = publish(&file, &spec.dest_dir, spec.sandbox_root.as_deref(), &stem, &ext)?;
    let mut notes = Vec::new();
    let mut files = vec![published];
    if !format.is_audio() && !spec.subtitles.is_empty() {
        save_subtitles(tool, &spec, &scratch.0, &mut files, &mut notes).await;
    }
    if spec.mark_of_the_web {
        for f in &files {
            let _ = path_safety::mark_downloaded(f, spec.url.as_str());
        }
    }
    if tool.ffmpeg.is_none() {
        notes.push(
            "The required dependencies are not ready, so the video was saved as the site sends it (no HD join, no file \
             type change). Download them in Settings > Required dependencies."
                .into(),
        );
    } else if ext != format.extension() {
        notes.push(format!("Saved as .{ext}: this video's picture or sound cannot go into a .{} file.", format.extension()));
    }
    scratch.finish();
    progress.segments_done.store(1000, Ordering::Relaxed);
    progress.set(Phase::Done);
    Ok(Outcome { files, notes })
}

/// The languages yt-dlp skipped and why (`WARNING: Unable to download video subtitles for 'fr': HTTP Error 429: ...`).
pub(crate) fn refused_subtitles(stderr: &str) -> Vec<(String, String)> {
    stderr
        .lines()
        .filter_map(|l| l.split_once("Unable to download video subtitles for '")?.1.split_once("': "))
        .filter(|(lang, _)| subtitle_lang(lang))
        .map(|(lang, why)| (lang.to_string(), without_tool_hints(&why.chars().filter(|c| !c.is_control()).take(200).collect::<String>())))
        .collect()
}

/// The chosen subtitles, saved next to the video (`files[0]`) as `<video name>.<language>.srt`, in the order chosen.
/// Subtitles are small and independent: when the site refuses some, only they are missing, and a note says so.
async fn save_subtitles(tool: &SiteTool, spec: &SiteSpec, scratch: &Path, files: &mut Vec<PathBuf>, notes: &mut Vec<String>) {
    let args = match subtitle_args(&spec.url, &spec.headers, &spec.subtitles, scratch, tool.ffmpeg.as_deref(), tool.deno.as_deref()) {
        Ok(a) => tool.js_args(a),
        Err(e) => return notes.push(format!("The subtitles were skipped: {e}")),
    };
    let run = async {
        let mut child = command(&tool.program, &args).spawn()?;
        let out = tokio::spawn(read_limited(child.stdout.take().expect("piped"), 64 * 1024));
        let err = tokio::spawn(read_limited(child.stderr.take().expect("piped"), 16 * 1024));
        let status = child.wait().await?;
        let _ = out.await;
        let (stderr, _) = err.await.unwrap_or_default();
        std::io::Result::Ok((status, String::from_utf8_lossy(&stderr).into_owned()))
    };
    let mut refused = Vec::new();
    match tokio::time::timeout(SUBTITLES_TIMEOUT, run).await {
        Ok(Ok((status, stderr))) => {
            refused = refused_subtitles(&stderr);
            if !status.success() && refused.is_empty() {
                tracing::warn!(output = %stderr_tail(&stderr), "yt-dlp could not fetch the subtitles");
                notes.push(format!("Some subtitles could not be downloaded: {}", error_from_stderr(&stderr)));
            }
        }
        Ok(Err(e)) => return notes.push(format!("The subtitles were skipped: could not start a required dependency: {e}")),
        Err(_) => notes.push("Some subtitles could not be downloaded: the site did not answer in time.".into()),
    }
    let stem = files[0].file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "video".into());
    let (mut missing, mut busy): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let mut done: Vec<&str> = Vec::new();
    for (_, lang) in spec.subtitles.iter().filter_map(|id| subtitle_id(id)) {
        if done.contains(&lang) {
            continue;
        }
        done.push(lang);
        let found = SUBTITLE_FILES
            .iter()
            .map(|ext| (scratch.join(format!("sub.{lang}.{ext}")), *ext))
            .find(|(p, _)| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && m.len() > 0));
        let tag = lang.strip_suffix("-orig").unwrap_or(lang);
        match found.map(|(p, ext)| publish(&p, &spec.dest_dir, spec.sandbox_root.as_deref(), &format!("{stem}.{tag}"), ext)) {
            Some(Ok(p)) => files.push(p),
            Some(Err(e)) => notes.push(format!("Subtitles “{tag}” were skipped: {e}")),
            None => match refused.iter().find(|(l, _)| l == lang) {
                Some((_, why)) if why.contains("429") || why.to_ascii_lowercase().contains("too many requests") => busy.push(tag),
                Some((_, why)) => notes.push(format!("Subtitles “{tag}” could not be downloaded: {why}")),
                None => missing.push(tag),
            },
        }
    }
    if !busy.is_empty() {
        notes.push(format!(
            "The site is limiting requests for now, so these subtitles were skipped: {}. They usually work again a little later.",
            busy.join(", ")
        ));
    }
    if !missing.is_empty() {
        notes.push(format!("The site gave no subtitles for: {}.", missing.join(", ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn strings(a: &[OsString]) -> Vec<String> {
        a.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn video_pages_are_recognised_and_other_pages_are_not() {
        for yes in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=x",
            "https://youtube.com/shorts/abc",
            "https://youtu.be/abc",
            "https://vimeo.com/123",
            "https://www.dailymotion.com/video/x8",
            "https://x.com/someone/status/1",
            "https://www.tiktok.com/@a/video/1",
        ] {
            assert!(is_site_page(&u(yes)), "{yes}");
        }
        for no in [
            "https://www.youtube.com/",
            "https://www.youtube.com/feed/subscriptions",
            "https://youtu.be/",
            "https://notyoutube.com/watch?v=x",
            "https://youtube.com.evil.test/watch?v=x",
            "https://example.com/video.mp4",
            "ftp://youtube.com/watch",
        ] {
            assert!(!is_site_page(&u(no)), "{no}");
        }
    }

    #[test]
    fn drm_services_are_named() {
        assert_eq!(protected_service(&u("https://www.netflix.com/watch/1")), Some("Netflix"));
        assert_eq!(protected_service(&u("https://shahid.mbc.net/ar/player/1")), Some("Shahid"));
        assert_eq!(protected_service(&u("https://www.stctv.com/x")), Some("STC tv"));
        assert_eq!(protected_service(&u("https://www.amazon.com/gp/video/detail/1")), Some("Prime Video"));
        assert_eq!(protected_service(&u("https://www.amazon.com/dp/1")), None);
        assert_eq!(protected_service(&u("https://notnetflix.com/")), None);
        assert_eq!(protected_service(&u("https://www.youtube.com/watch?v=1")), None);
    }

    #[test]
    fn arguments_are_fixed_and_the_link_comes_last_after_a_separator() {
        let scratch = if cfg!(windows) { PathBuf::from("C:\\dl\\.velox-media-1") } else { PathBuf::from("/dl/.velox-media-1") };
        let ff = if cfg!(windows) { PathBuf::from("C:\\bin\\ffmpeg.exe") } else { PathBuf::from("/bin/ffmpeg") };
        let headers = SiteHeaders { referer: Some("https://www.youtube.com/".into()), user_agent: Some("Mozilla/5.0".into()) };
        let url = u("https://www.youtube.com/watch?v=--exec&x=;rm -rf /");
        for args in [
            analyze_args(&url, &headers, None).unwrap(),
            download_args(&url, &headers, Some("h720"), OutputFormat::Mp4, &scratch, Some(&ff), None, None).unwrap(),
        ] {
            let s = strings(&args);
            assert_eq!(s[0], "--ignore-config", "the user's yt-dlp configuration is never read");
            assert!(s.contains(&"--no-exec".to_string()) && s.contains(&"--no-playlist".to_string()));
            assert_eq!(s[s.len() - 2], "--");
            assert_eq!(s.last().unwrap(), url.as_str());
            assert!(!s.iter().any(|a| a.starts_with("--cookies")), "cookies are never handed to yt-dlp");
            assert!(!s.iter().any(|a| a == "--exec" || a == "--batch-file" || a == "--load-info-json"));
        }
        let s = strings(&download_args(&url, &headers, Some("h720"), OutputFormat::Mp4, &scratch, Some(&ff), None, None).unwrap());
        assert!(s.windows(2).any(|w| w[0] == "--paths" && w[1] == scratch.to_string_lossy()));
        assert!(s.windows(2).any(|w| w[0] == "--output" && w[1] == "media.%(ext)s"));
        assert!(s.windows(2).any(|w| w[0] == "-f" && w[1].contains("height<=720")));
        // Text that is not a header value is dropped, never passed on.
        let bad = SiteHeaders { referer: Some("javascript:alert(1)".into()), user_agent: Some("a\r\nX-Evil: 1".into()) };
        let s = strings(&analyze_args(&url, &bad, None).unwrap());
        assert!(!s.iter().any(|a| a == "--referer" || a == "--user-agent"));
        assert!(analyze_args(&u("https://user:pw@youtube.com/watch?v=1"), &headers, None).is_err());
        assert!(download_args(&url, &headers, None, OutputFormat::Mp4, Path::new("relative"), None, None, None).is_err());
        // The JavaScript runtime is passed by absolute path only, before the link.
        let deno = if cfg!(windows) { PathBuf::from(r"C:\tools\deno.exe") } else { PathBuf::from("/tools/deno") };
        let s = strings(&analyze_args(&url, &headers, Some(&deno)).unwrap());
        let i = s.iter().position(|x| x == "--js-runtimes").expect("runtime passed");
        assert_eq!(s[i + 1], format!("deno:{}", deno.display()));
        assert!(i < s.iter().position(|x| x == "--").unwrap());
        assert!(!strings(&analyze_args(&url, &headers, Some(Path::new("deno"))).unwrap()).contains(&"--js-runtimes".to_string()));
    }

    #[test]
    fn every_file_type_gets_its_own_arguments() {
        let scratch = if cfg!(windows) { PathBuf::from("C:\\dl\\.velox-media-1") } else { PathBuf::from("/dl/.velox-media-1") };
        let ff = if cfg!(windows) { PathBuf::from("C:\\bin\\ffmpeg.exe") } else { PathBuf::from("/bin/ffmpeg") };
        let url = u("https://www.youtube.com/watch?v=abc");
        let args = |q: Option<&str>, f: OutputFormat, ffmpeg: bool| {
            strings(&download_args(&url, &SiteHeaders::default(), q, f, &scratch, ffmpeg.then_some(ff.as_path()), None, None).unwrap())
        };
        let pair = |s: &[String], k: &str, v: &str| s.windows(2).any(|w| w[0] == k && w[1] == v);

        // MP4: the chosen height first, then H.264 + AAC; joined and repackaged into .mp4 (never .mkv).
        let s = args(Some("h1080"), OutputFormat::Mp4, true);
        assert!(pair(&s, "-S", "res,fps,vcodec:h264,acodec:aac"));
        assert!(pair(&s, "--merge-output-format", "mp4") && pair(&s, "--remux-video", "mp4"));
        assert!(s.iter().any(|a| a.starts_with("bv*[height<=1080]")));
        assert!(!s.iter().any(|a| a.contains("mkv")), "{s:?}");
        // MKV: best picture and sound, any codec.
        let s = args(Some("h720"), OutputFormat::Mkv, true);
        assert!(pair(&s, "--merge-output-format", "mkv") && pair(&s, "--remux-video", "mkv") && !s.contains(&"-S".to_string()));
        // WebM: VP9 + Opus preferred; anything else converted.
        let s = args(None, OutputFormat::Webm, true);
        assert!(pair(&s, "-S", "res,fps,vcodec:vp9,acodec:opus") && pair(&s, "--recode-video", "webm"));
        // Audio types: only the sound, at the best quality, whatever quality was picked.
        for (f, ext) in [(OutputFormat::Mp3, "mp3"), (OutputFormat::M4a, "m4a"), (OutputFormat::Opus, "opus")] {
            let s = args(Some("h720"), f, true);
            assert!(s.contains(&"--extract-audio".to_string()) && pair(&s, "--audio-format", ext), "{s:?}");
            assert!(pair(&s, "-f", "ba/b") && !s.contains(&"--merge-output-format".to_string()));
        }
        // Without ffmpeg nothing can be joined or converted: no ffmpeg options at all.
        let s = args(Some("h720"), OutputFormat::Mp4, false);
        assert!(!s
            .iter()
            .any(|a| a == "--merge-output-format" || a == "--remux-video" || a == "--extract-audio" || a == "--ffmpeg-location"));
        // A bad quality is refused whatever the type.
        assert!(download_args(&url, &SiteHeaders::default(), Some("h1]+x"), OutputFormat::Mp3, &scratch, None, None, None).is_err());
        // The app's speed limit becomes yt-dlp's own; none means no option at all.
        let limited = strings(
            &download_args(&url, &SiteHeaders::default(), None, OutputFormat::Mp4, &scratch, Some(&ff), None, Some(2_000_000)).unwrap(),
        );
        assert!(pair(&limited, "--limit-rate", "2000000"));
        assert!(!args(None, OutputFormat::Mp4, true).contains(&"--limit-rate".to_string()));
        // ffmpeg reports its progress to a file in the work folder, for every joining / converting step.
        let s = args(Some("audio"), OutputFormat::Mp3, true);
        let file = scratch.join(FFMPEG_PROGRESS_FILE).to_string_lossy().into_owned();
        for pp in ["Merger", "VideoRemuxer", "VideoConvertor", "ExtractAudio"] {
            assert!(pair(&s, "--postprocessor-args", &format!("{pp}+ffmpeg_i1:-progress '{file}' -nostats")), "{s:?}");
        }
        assert!(!args(None, OutputFormat::Mp4, false).contains(&"--postprocessor-args".to_string()));
        assert_eq!(shell_quote(r"C:\it's\a"), r"'C:\it'\''s\a'");

        assert_eq!(effective_format(Some("h720"), None), OutputFormat::Mp4);
        assert_eq!(effective_format(Some("audio"), None), OutputFormat::Mp3);
        assert_eq!(effective_format(Some("audio"), Some(OutputFormat::Mkv)), OutputFormat::Mp3);
        assert_eq!(effective_format(Some("h720"), Some(OutputFormat::Opus)), OutputFormat::Opus);
        assert_eq!(effective_format(None, Some(OutputFormat::Webm)), OutputFormat::Webm);
        assert_eq!(OutputFormat::parse("MP4"), Some(OutputFormat::Mp4));
        for bad in ["", "exe", "mp4;", "avi", "../mp4"] {
            assert_eq!(OutputFormat::parse(bad), None, "{bad}");
        }
        assert!(is_finishing_line("[Merger] Merging formats into \"media.mp4\""));
        assert!(is_finishing_line("[ExtractAudio] Destination: media.mp3"));
        assert!(!is_finishing_line("[download] Destination: media.f137.mp4"));
    }

    #[test]
    fn quality_ids_are_a_closed_set() {
        assert_eq!(selector(None, true).unwrap(), "bv*+ba/b");
        assert_eq!(selector(None, false).unwrap(), "b");
        assert_eq!(selector(Some("audio"), false).unwrap(), "ba/b");
        assert_eq!(selector(Some("h1080"), false).unwrap(), "b[height<=1080]/b");
        assert!(selector(Some("h1080"), true).unwrap().starts_with("bv*[height<=1080]+ba"));
        for bad in ["h", "h-1", "h1080]+ba", "best/worst", "h99999999", "x", ""] {
            assert!(selector(Some(bad), true).is_err(), "{bad}");
        }
    }

    fn sample() -> Value {
        serde_json::json!({
            "title": "My: video / test?",
            "duration": 212.5,
            "formats": [
                {"format_id": "sb0", "protocol": "mhtml", "vcodec": "none", "acodec": "none"},
                {"format_id": "140", "vcodec": "none", "acodec": "mp4a.40.2", "filesize": 3_000_000},
                {"format_id": "18", "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "height": 360, "width": 640, "filesize": 10_000_000},
                {"format_id": "137", "vcodec": "avc1.640028", "acodec": "none", "height": 1080, "width": 1920, "filesize": 100_000_000},
                {"format_id": "136", "vcodec": "avc1.4d401f", "acodec": "none", "height": 720, "width": 1280, "filesize_approx": 50_000_000},
                {"format_id": "drm", "vcodec": "avc1", "acodec": "none", "height": 2160, "has_drm": true}
            ]
        })
    }

    #[test]
    fn qualities_come_from_the_formats_best_first() {
        let a = analysis_from_json(&sample(), true).unwrap();
        assert_eq!(a.kind, StreamKind::Site);
        let ids: Vec<&str> = a.streams.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["h1080", "h720", "h360", "audio"], "DRM formats and storyboards are left out");
        assert!(a.streams[0].label.starts_with("1080p · about"));
        assert_eq!(a.duration_secs, Some(212.5));
        assert!(!a.title.contains(['/', ':', '?']));
        // Without ffmpeg only formats with both video and audio are offered.
        let a = analysis_from_json(&sample(), false).unwrap();
        let ids: Vec<&str> = a.streams.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["h360", "audio"]);
    }

    #[test]
    fn drm_live_and_playlists_are_refused() {
        let all_drm = serde_json::json!({"title": "x", "formats": [{"vcodec": "avc1", "acodec": "none", "height": 1080, "has_drm": true}]});
        assert!(matches!(analysis_from_json(&all_drm, true), Err(MediaError::Unsupported(m)) if m.contains("DRM")));
        let live = serde_json::json!({"title": "x", "is_live": true, "formats": []});
        assert!(matches!(analysis_from_json(&live, true), Err(MediaError::Unsupported(m)) if m.contains("live")));
        let list = serde_json::json!({"_type": "playlist", "entries": []});
        assert!(matches!(analysis_from_json(&list, true), Err(MediaError::Unsupported(m)) if m.contains("playlist")), "an empty playlist");
        assert!(analysis_from_json(&serde_json::json!({"title": "x", "formats": []}), true).is_err());
    }

    /// yt-dlp's flat list of a real YouTube playlist (2026-10-17, trimmed), plus the entries a hostile or odd
    /// page could add.
    fn flat_playlist() -> Value {
        serde_json::json!({
            "_type": "playlist", "id": "PLa1F2ddGya_8I8QCMCKlQUOQpgOga2nZ-", "title": "BLENDERHEADS / best: of", "playlist_count": 13,
            "webpage_url": "https://www.youtube.com/playlist?list=PLa1F2ddGya_8I8QCMCKlQUOQpgOga2nZ-",
            "entries": [
                {"_type": "url", "ie_key": "Youtube", "id": "6d5rI5y8hqQ", "url": "https://www.youtube.com/watch?v=6d5rI5y8hqQ",
                 "title": "The end of an era | Blenderheads Ep. 11", "duration": 780, "live_status": null, "availability": null},
                {"_type": "url", "ie_key": "Youtube", "id": "h8rauHRo1CY", "url": "https://www.youtube.com/watch?v=h8rauHRo1CY",
                 "title": "BLENDERHEADS - Ep. 05\u{7}", "duration": 984},
                {"_type": "url", "id": "x", "url": "http://192.168.1.1/admin", "title": "not a video site"},
                {"_type": "url", "id": "y", "url": "file:///etc/passwd", "title": "not a web link"},
                {"_type": "url", "ie_key": "YoutubeTab", "url": "https://www.youtube.com/playlist?list=PLother", "title": "another playlist"},
                {"_type": "playlist", "url": "https://www.youtube.com/watch?v=nested", "title": "nested"},
                {"_type": "url", "url": "https://www.netflix.com/watch/1", "title": "DRM service"},
                {"_type": "url", "url": "https://user:pw@www.youtube.com/watch?v=creds", "title": "credentials"},
                {"_type": "url", "url": "https://www.youtube.com/watch?v=priv", "title": "[Private video]", "duration": null},
                {"_type": "url", "url": "https://www.youtube.com/watch?v=live", "title": "On air", "live_status": "is_live"},
                {"_type": "url", "url": "https://www.youtube.com/watch?v=memb", "title": "Members", "availability": "subscriber_only"},
                {"_type": "url", "url": "https://www.youtube.com/watch?v=notitle"}
            ]
        })
    }

    #[test]
    fn a_playlist_lists_its_videos_of_known_sites_only_never_their_links_to_the_window() {
        let a = analysis_from_json(&flat_playlist(), true).unwrap();
        let p = a.playlist.as_ref().expect("a playlist");
        assert_eq!(a.kind, StreamKind::Site);
        assert_eq!(p.title, "BLENDERHEADS _ best_ of", "safe for a folder name");
        assert_eq!(a.title, p.title);
        assert_eq!((p.total, p.truncated), (Some(13), false));
        let got: Vec<(u32, &str, Option<f64>, bool)> =
            p.entries.iter().map(|e| (e.index, e.title.as_str(), e.duration_secs, e.available)).collect();
        assert_eq!(
            got,
            [
                (1, "The end of an era | Blenderheads Ep. 11", Some(780.0), true),
                (2, "BLENDERHEADS - Ep. 05", Some(984.0), true),
                (9, "[Private video]", None, false),
                (10, "On air", None, false),
                (11, "Members", None, false),
                (12, "12", None, true),
            ],
            "positions are the playlist's; other hosts, other playlists, DRM services and links with credentials are dropped"
        );
        assert_eq!(p.entries[0].url.as_str(), "https://www.youtube.com/watch?v=6d5rI5y8hqQ");
        let shown = serde_json::to_string(&a).unwrap();
        assert!(!shown.contains("watch?v=") && !shown.contains("192.168"), "links never go to the window: {shown}");
        let ids: Vec<&str> = a.streams.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, PLAYLIST_QUALITIES);
        for id in PLAYLIST_QUALITIES {
            assert!(selector(Some(id), true).is_ok() && selector(Some(id), false).is_ok(), "{id} is a quality downloads accept");
        }
        assert!(!a.part_of_playlist);
    }

    #[test]
    fn a_long_playlist_is_cut_at_the_limit_and_says_so() {
        let entries: Vec<Value> = (0..limits::MAX_PLAYLIST_VIDEOS + 1)
            .map(|i| serde_json::json!({"_type": "url", "url": format!("https://www.youtube.com/watch?v=v{i}"), "title": format!("v{i}")}))
            .collect();
        let a = analysis_from_json(&serde_json::json!({"_type": "playlist", "title": "big", "entries": entries}), false).unwrap();
        let p = a.playlist.unwrap();
        assert_eq!(p.entries.len(), limits::MAX_PLAYLIST_VIDEOS);
        assert!(p.truncated);
        assert_eq!(p.entries.last().unwrap().index, limits::MAX_PLAYLIST_VIDEOS as u32);
        let only_hostile = serde_json::json!({"_type": "playlist", "entries": [{"url": "http://127.0.0.1/x"}]});
        assert!(matches!(analysis_from_json(&only_hostile, true), Err(MediaError::Unsupported(_))));
    }

    #[test]
    fn a_video_of_a_playlist_leads_to_the_playlist_page_by_its_id_only() {
        assert_eq!(
            playlist_link(&u("https://www.youtube.com/watch?v=bLojKyJx7fA&list=PLa1F2ddGya_8I8QCMCKlQUOQpgOga2nZ-&index=3&t=10"))
                .unwrap()
                .as_str(),
            "https://www.youtube.com/playlist?list=PLa1F2ddGya_8I8QCMCKlQUOQpgOga2nZ-"
        );
        assert_eq!(
            playlist_link(&u("https://music.youtube.com/watch?v=a&list=RDabc")).unwrap().as_str(),
            "https://www.youtube.com/playlist?list=RDabc"
        );
        assert!(playlist_link(&u("https://youtu.be/abc?list=PL123")).is_some());
        for no in [
            "https://www.youtube.com/watch?v=abc",
            "https://www.youtube.com/watch?v=abc&list=",
            "https://www.youtube.com/watch?v=abc&list=PL%2F..%2Fx",
            "https://www.youtube.com/watch?v=abc&list=a%20b",
            "https://vimeo.com/1?list=PL123",
            "https://evil.test/watch?v=a&list=PL123",
            "ftp://www.youtube.com/watch?v=a&list=PL123",
        ] {
            assert_eq!(playlist_link(&u(no)), None, "{no}");
        }
        assert!(is_site_page(&u("https://www.youtube.com/playlist?list=PL123")), "a playlist page is read through yt-dlp");
        let s = strings(&analyze_args(&u("https://www.youtube.com/playlist?list=PL123"), &SiteHeaders::default(), None).unwrap());
        assert!(s.contains(&"--flat-playlist".to_string()), "only the list, not every video");
        assert!(s.windows(2).any(|w| w[0] == "--playlist-items" && w[1] == "1:501"), "one more than kept, to know there are more");
        assert!(s.contains(&"--no-playlist".to_string()), "a video of a playlist stays one video");
    }

    #[test]
    fn progress_lines_and_errors_are_read() {
        assert_eq!(parse_progress("VELOXP 1024 4096 NA"), Some((1024, Some(4096))));
        assert_eq!(parse_progress("VELOXP 1024 NA 9000.5"), Some((1024, Some(9000))));
        assert_eq!(parse_progress("  VELOXP 5 NA NA"), Some((5, None)));
        assert_eq!(parse_progress("[download] 10%"), None);
        assert_eq!(parse_progress("VELOXP NA NA NA"), None);
        assert_eq!(parse_duration("VELOXP 1024 4096 NA 634"), Some(634.0));
        assert_eq!(parse_duration("VELOXP 1024 4096 NA NA"), None);
        assert_eq!(parse_duration("VELOXP 1024 4096 NA"), None);
        assert_eq!(ffmpeg_done_secs("out_time_us=1000000\nprogress=continue\nout_time_us=19005542\nprogress=end\n"), Some(19.005542));
        assert_eq!(ffmpeg_done_secs("out_time_us=N/A\n"), None);
        assert_eq!(ffmpeg_done_secs(""), None);
    }

    #[test]
    fn the_bar_shares_downloading_and_finishing_by_their_real_weight() {
        // Video + sound into MP4: the picture is most of the bytes, joining is quick.
        let s = shares(2, true, OutputFormat::Mp4);
        assert_eq!(s.at(0, 0.5), 422);
        assert_eq!(s.at(0, 1.0), s.at(1, 0.0));
        assert_eq!(s.at(1, 1.0), 960);
        assert_eq!((s.finishing(0.0), s.finishing(0.5), s.finishing(1.0)), (960, 979, 999));
        // Only the sound into MP3: converting takes a real part of the time, and moves the bar.
        let s = shares(1, true, OutputFormat::Mp3);
        assert_eq!((s.at(0, 1.0), s.finishing(0.5), s.finishing(2.0)), (850, 924, 999));
        // Without ffmpeg the download is all there is.
        assert_eq!(shares(1, false, OutputFormat::Mp4).at(0, 1.0), 999);
        assert!(
            matches!(error_from_stderr("WARNING: x\nERROR: [youtube] abc: This video is DRM protected"), MediaError::Unsupported(m) if m.contains("DRM"))
        );
        assert!(matches!(error_from_stderr("ERROR: Unsupported URL: https://x"), MediaError::Unsupported(_)));
        let e = error_from_stderr("ERROR: boom\u{1b}[31m\r");
        assert!(!e.to_string().contains('\u{1b}'));
        // A plain sentence, without yt-dlp's "[site] id:" prefix or a "stream description" label.
        let e = error_from_stderr("ERROR: [youtube] jNQXAC9IVRc: This video is unavailable");
        assert_eq!(e.to_string(), "The site says: This video is unavailable");
        // With --verbose, a run that stops without an ERROR line is not explained by a debug line.
        assert_eq!(
            error_from_stderr("[debug] Python 3.13\n[debug] exe versions: ffmpeg 7\n").to_string(),
            "reading the video stopped without a reason"
        );
        assert_eq!(error_from_stderr("[debug] x\nsomething broke\n[debug] y").to_string(), "The site says: something broke");
    }

    #[test]
    fn the_log_gets_the_last_lines_yt_dlp_wrote() {
        let many: String = (0..40).map(|i| format!("[debug] line {i}\n")).collect();
        let tail = stderr_tail(&many);
        assert!(tail.starts_with("[debug] line 25") && tail.ends_with("[debug] line 39"), "{tail}");
        assert_eq!(tail.matches(" | ").count(), 14);
        // Not the line repeating the command line, no control characters, long lines cut.
        let t = stderr_tail("[debug] Command-line config: ['--referer', 'https://x/?t=1']\n\u{1b}[0;31mstuck\r\n");
        assert_eq!(t, "[0;31mstuck");
        assert_eq!(stderr_tail(&"x".repeat(1000)).len(), 300);
        assert_eq!(stderr_tail(""), "(nothing)");
        // The analysis asks for the explanation.
        let url = Url::parse("https://www.youtube.com/watch?v=jNQXAC9IVRc").unwrap();
        let args = analyze_args(&url, &SiteHeaders::default(), None).unwrap();
        assert!(args.iter().any(|a| a == "--verbose"));
        assert_eq!(args[args.len() - 2], "--", "the link stays last, after --");
    }

    #[test]
    fn the_programs_own_advice_is_not_shown() {
        let e = error_from_stderr(
            "ERROR: [youtube] abc: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies for the \
             authentication. See  https://github.com/yt-dlp/yt-dlp/wiki/FAQ#how-do-i-pass-cookies-to-yt-dlp  for how to manually pass cookies\n",
        )
        .to_string();
        assert!(e.contains("Sign in to confirm you're not a bot"), "{e}");
        for word in ["--cookies", "yt-dlp", "github"] {
            assert!(!e.to_lowercase().contains(word), "{word} in {e}");
        }
        let e =
            error_from_stderr("ERROR: [generic] x: This video is unavailable. Please report this issue on yt-dlp's tracker\n").to_string();
        assert!(e.ends_with("This video is unavailable") && !e.contains("yt-dlp"), "{e}");
    }

    #[test]
    fn a_pages_subtitles_are_listed_people_first_then_the_sites_own() {
        let mut j = sample();
        j["language"] = "en".into();
        j["subtitles"] = serde_json::json!({
            "fr": [{"ext": "json3", "name": "French"}, {"ext": "vtt", "name": "French"}],
            "ar": [{"ext": "srt", "name": "Arabic"}],
            "live_chat": [{"ext": "json"}],
            "danmaku": [{"ext": "xml"}],
            "-x": [{"ext": "vtt"}], "all": [{"ext": "vtt"}], "en.*": [{"ext": "vtt"}], "a,b": [{"ext": "vtt"}],
            "pt-BR": [{"ext": "vtt", "name": "Portuguese (Brazil)\u{7}"}]
        });
        j["automatic_captions"] = serde_json::json!({
            "en-orig": [{"ext": "vtt", "name": "English (Original)"}],
            "en": [{"ext": "vtt", "name": "English"}],
            "fr": [{"ext": "vtt", "name": "French"}],
            "de": [{"ext": "vtt"}, {"ext": "srt", "name": "German"}],
            "zh-Hans": [{"ext": "vtt", "name": "Chinese (Simplified)"}]
        });
        let a = analysis_from_json(&j, true).unwrap();
        let got: Vec<(&str, &str, Option<&str>)> =
            a.subtitles.iter().map(|s| (s.id.as_str(), s.label.as_str(), s.language.as_deref())).collect();
        assert_eq!(
            got,
            [
                ("mar", "Arabic", Some("ar")),
                ("mfr", "French", Some("fr")),
                ("mpt-BR", "Portuguese (Brazil)", Some("pt-BR")),
                ("aen-orig", "English · auto-generated", Some("en")),
                ("azh-Hans", "Chinese (Simplified) · auto-translated", Some("zh-Hans")),
                ("ade", "German · auto-translated", Some("de")),
            ]
        );
        assert!(a.subtitles.iter().all(|s| is_subtitle_id(&s.id) && s.id.len() <= 16), "every offered id is accepted back");
        assert!(analysis_from_json(&sample(), true).unwrap().subtitles.is_empty(), "a page without subtitles offers none");
    }

    #[test]
    fn subtitle_ids_are_a_language_code_and_nothing_else() {
        assert_eq!(subtitle_id("mpt-BR"), Some((false, "pt-BR")));
        assert_eq!(subtitle_id("aen-orig"), Some((true, "en-orig")));
        for bad in ["", "m", "a", "xen", "m-en", "men-", "mall", "maLL", "men,fr", "men.*", "men orig", "m..", "men/x", "mabcdefghijklmnop"]
        {
            assert!(subtitle_id(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn subtitles_are_fetched_on_their_own_run_with_fixed_arguments() {
        let url = u("https://www.youtube.com/watch?v=abc");
        let scratch = std::env::temp_dir().join("velox-subs-args");
        let ff = std::env::temp_dir().join("ffmpeg");
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let s = strings(
            &subtitle_args(&url, &SiteHeaders::default(), &ids(&["mar", "afr", "mar", "aen-orig"]), &scratch, Some(&ff), None).unwrap(),
        );
        let pair = |s: &[String], k: &str, v: &str| s.windows(2).any(|w| w[0] == k && w[1] == v);
        assert_eq!(s[0], "--ignore-config");
        assert!(pair(&s, "--sub-langs", "ar,fr,en-orig"), "each language once: {s:?}");
        assert!(s.contains(&"--skip-download".into()) && s.contains(&"--write-subs".into()) && s.contains(&"--write-auto-subs".into()));
        assert!(pair(&s, "--convert-subs", "srt") && pair(&s, "--ffmpeg-location", &ff.to_string_lossy()));
        assert!(pair(&s, "--output", "subtitle:sub.%(ext)s") && pair(&s, "--paths", &scratch.to_string_lossy()));
        assert_eq!(&s[s.len() - 2..], ["--", url.as_str()]);
        assert!(!s.iter().any(|a| a.starts_with("--cookies") || a == "--exec" || a == "-f"));
        assert!(s.contains(&"--ignore-errors".into()) && !s.contains(&"--no-warnings".into()), "a refused language never stops the others");

        let s = strings(&subtitle_args(&url, &SiteHeaders::default(), &ids(&["mar"]), &scratch, None, None).unwrap());
        assert!(s.contains(&"--write-subs".into()) && !s.contains(&"--write-auto-subs".into()), "only what was asked for");
        assert!(!s.contains(&"--convert-subs".into()), "without ffmpeg they stay as the site sends them");

        for bad in [ids(&[]), ids(&["mall"]), ids(&["m-en"])] {
            assert!(subtitle_args(&url, &SiteHeaders::default(), &bad, &scratch, None, None).is_err(), "{bad:?}");
        }
        assert!(subtitle_args(&url, &SiteHeaders::default(), &ids(&["mar"]), Path::new("rel"), None, None).is_err());
        assert!(subtitle_args(&url, &SiteHeaders::default(), &ids(&["mar"]), &scratch, Some(Path::new("ffmpeg")), None).is_err());
    }

    #[test]
    fn the_languages_the_site_refused_are_read_from_the_warnings() {
        let stderr = "[info] Writing video subtitles to: x\n\
                      WARNING: Unable to download video subtitles for 'ab': HTTP Error 429: Too Many Requests\n\
                      WARNING: Unable to download video subtitles for 'pt-BR': HTTP Error 404: Not Found\u{1b}[0m\n\
                      WARNING: Unable to download video subtitles for '--exec x': HTTP Error 500\n";
        assert_eq!(
            refused_subtitles(stderr),
            [("ab".to_string(), "HTTP Error 429: Too Many Requests".to_string()), ("pt-BR".into(), "HTTP Error 404: Not Found[0m".into())]
        );
        assert!(refused_subtitles("ERROR: something else").is_empty());
    }
}
