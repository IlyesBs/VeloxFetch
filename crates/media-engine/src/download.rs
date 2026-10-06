//! Fetching manifests, downloading the tracks, and assembling the output files.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use http_client::{HttpClient, RequestExtras};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::ffmpeg::{outputs_to_try, Input, Remux};
use crate::{dash, hls, limits, sanitize_stem, Analysis, Catalog, Container, Entry, MediaError, Result, Segment, Source, TrackPlan};

/// Where a job is, for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Downloading,
    Combining,
    Done,
}

/// Live counters shared with the UI (cheap atomics, read by the progress ticker).
#[derive(Default)]
pub struct Progress {
    pub bytes: AtomicU64,
    pub segments_done: AtomicU64,
    pub segments_total: AtomicU64,
    /// The size of the file being downloaded right now, when known (yt-dlp's current part): lets a player
    /// watch it while it grows.
    pub part_total: AtomicU64,
    phase: AtomicU8,
}

impl Progress {
    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Relaxed) {
            0 => Phase::Preparing,
            1 => Phase::Downloading,
            2 => Phase::Combining,
            _ => Phase::Done,
        }
    }
    pub(crate) fn set(&self, p: Phase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }
}

/// What the user picked. Ids come from `analyze`; anything unknown is refused.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// A quality id (`v0`...). `None` = the best.
    pub stream: Option<String>,
    /// An audio id (`a0`...), `"none"` for no separate audio, `None` = the default for the quality.
    pub audio: Option<String>,
    pub subtitles: Vec<String>,
}

/// Paces what is downloaded: called with each piece's size, it returns once that many bytes may be used
/// (the app's speed limits and Smart throttle). It must finish at once when nothing limits.
pub type Pacer = Arc<dyn Fn(usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Clone)]
pub struct DownloadSpec {
    pub url: Url,
    pub selection: Selection,
    /// Already validated by the caller (inside the download folder).
    pub dest_dir: PathBuf,
    /// When set, `dest_dir` must (still) lie inside this folder, links resolved: checked before the
    /// scratch folder is made and again before every file is published.
    pub sandbox_root: Option<PathBuf>,
    /// Flag every file written as downloaded from the internet (see `path_safety::mark_downloaded`).
    pub mark_of_the_web: bool,
    /// A name without extension; made safe again here.
    pub stem: String,
    /// `None` = ffmpeg is not available or not wanted: tracks are kept as they are.
    pub remux: Option<Remux>,
    /// The job's own work folder (`work_dir_name`, directly inside `dest_dir`), kept when the job is paused or
    /// fails so the next try continues from the pieces already there. `None` = a temporary one.
    pub work_dir: Option<PathBuf>,
    /// The video file type the user wants (MP4 / MKV / WebM; needs `remux`). `None` = MP4, or MKV for
    /// WebM tracks. Tracks that do not fit the chosen type go into MKV, with a note.
    pub format: Option<crate::OutputFormat>,
    /// Sent with every request (`Referer`, cookies) when a browser handed the link over.
    pub extras: RequestExtras,
    /// The app's speed limits; `None` = as fast as the server sends.
    pub pacer: Option<Pacer>,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    /// Everything that was written (the main file first).
    pub files: Vec<PathBuf>,
    /// Plain-language remarks for the user ("ffmpeg not found, audio kept separate"...).
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Manifests

async fn fetch_text(http: &HttpClient, extras: &RequestExtras, url: &Url) -> Result<(String, Url)> {
    let resp = http.get_with(url, extras).await?;
    let final_url = resp.url().clone();
    if resp.content_length().is_some_and(|n| n > limits::MAX_MANIFEST_BYTES as u64) {
        return Err(MediaError::limit("the playlist is larger than 4 MiB"));
    }
    let mut body: Vec<u8> = Vec::new();
    let mut stream = http_client::response_stream(resp);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len() + chunk.len() > limits::MAX_MANIFEST_BYTES {
            return Err(MediaError::limit("the playlist is larger than 4 MiB"));
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(body).map_err(|_| MediaError::invalid("the playlist is not text"))?;
    Ok((text, final_url))
}

async fn load_catalog(http: &HttpClient, extras: &RequestExtras, url: &Url) -> Result<Catalog> {
    let (text, base) = fetch_text(http, extras, url).await?;
    let head = text.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with("#EXTM3U") {
        match hls::parse(&base, &text)? {
            hls::Playlist::Master(m) => Ok(hls::catalog_from_master(&base, m)),
            hls::Playlist::Media(p) => Ok(hls::catalog_from_media(&base, p)),
        }
    } else if head.starts_with('<') && text.contains("<MPD") {
        dash::parse(&base, &text)
    } else {
        Err(MediaError::invalid("this is neither an HLS (.m3u8) nor a DASH (.mpd) stream description"))
    }
}

/// Look at a stream link and say what can be downloaded. Nothing is written.
pub async fn analyze(http: &HttpClient, extras: &RequestExtras, url: &Url) -> Result<Analysis> {
    Ok(load_catalog(http, extras, url).await?.analysis)
}

// ---------------------------------------------------------------------------------------------
// Choosing tracks

fn find<'a, T>(list: &'a [Entry<T>], id: &str, id_of: impl Fn(&T) -> &str, what: &str) -> Result<&'a Entry<T>> {
    list.iter()
        .find(|e| id_of(&e.option) == id)
        .ok_or_else(|| MediaError::invalid(format!("unknown {what} `{}`", id.chars().take(20).collect::<String>())))
}

async fn plan_of(http: &HttpClient, extras: &RequestExtras, source: &Source) -> Result<TrackPlan> {
    match source {
        Source::Ready(p) => Ok(p.clone()),
        Source::Playlist(url) => {
            let (text, base) = fetch_text(http, extras, url).await?;
            hls::parse_track(&base, &text)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Scratch space and names

/// A working folder inside the destination folder, removed when dropped (done, failed or cancelled).
/// The folder a job works in. `.1` = kept when the job stops early (a work folder the caller owns: paused or
/// failed jobs continue from it); a folder of our own is removed whatever happens.
pub(crate) struct Scratch(pub(crate) PathBuf, bool);

/// A name for a job's own work folder inside `dest` (see `DownloadSpec::work_dir`): swept like any scratch
/// folder once this app is gone.
pub fn work_dir_name(job: u64) -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!(".velox-media-{}-job{job}-{}", std::process::id(), t % 1_000_000_007)
}

impl Scratch {
    pub(crate) fn create(dest: &Path) -> Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let dir = dest.join(format!(".velox-media-{}-{n}-{}", std::process::id(), t % 1_000_000_007));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir, false))
    }

    /// `work` (the caller's folder, kept between tries) or a fresh one. `work` must be a `.velox-media-` folder
    /// directly inside `dest`: nothing else is ever written to or removed.
    pub(crate) fn open(dest: &Path, work: Option<&Path>) -> Result<Self> {
        let Some(w) = work else { return Self::create(dest) };
        let named = w.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".velox-media-"));
        if w.parent() != Some(dest) || !named {
            return Err(MediaError::invalid("internal error: the work folder is not where it should be"));
        }
        if let Ok(m) = std::fs::symlink_metadata(w) {
            if !m.is_dir() {
                return Err(MediaError::invalid("internal error: the work folder is not a folder"));
            }
        }
        std::fs::create_dir_all(w)?;
        Ok(Self(w.to_path_buf(), true))
    }

    /// The job finished: its working files are not needed any more.
    pub(crate) fn finish(mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        self.1 = true;
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Reserve `stem.ext`, or `stem (1).ext`... The empty file makes the name ours, then a rename
/// replaces it with the real content.
fn reserve(dir: &Path, stem: &str, ext: &str) -> Result<PathBuf> {
    for n in 0..1000u32 {
        let name = if n == 0 { format!("{stem}.{ext}") } else { format!("{stem} ({n}).{ext}") };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(MediaError::invalid("too many files with that name already exist"))
}

pub(crate) fn confine(root: Option<&Path>, dir: &Path) -> Result<()> {
    match root {
        Some(r) => path_safety::ensure_within(r, dir).map_err(|e| MediaError::invalid(e.to_string())),
        None => Ok(()),
    }
}

/// Put the finished file in its place under a name nobody has, without ever leaving an empty placeholder
/// behind: a hard link is created (it fails if the name exists, so nothing is overwritten), then the
/// scratch name is dropped. A crash at any point leaves either nothing or the complete file. Where hard
/// links do not exist (FAT / exFAT drives, some network shares) it falls back to reserving the name.
pub(crate) fn publish(from: &Path, dir: &Path, root: Option<&Path>, stem: &str, ext: &str) -> Result<PathBuf> {
    confine(root, dir)?;
    for n in 0..1000u32 {
        let name = if n == 0 { format!("{stem}.{ext}") } else { format!("{stem} ({n}).{ext}") };
        let target = dir.join(name);
        match std::fs::hard_link(from, &target) {
            Ok(()) => {
                failpoint::hit("media.after_link");
                let _ = std::fs::remove_file(from);
                return Ok(target);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return publish_by_rename(from, dir, stem, ext),
        }
    }
    Err(MediaError::invalid("too many files with that name already exist"))
}

fn publish_by_rename(from: &Path, dir: &Path, stem: &str, ext: &str) -> Result<PathBuf> {
    let target = reserve(dir, stem, ext)?;
    if let Err(e) = std::fs::rename(from, &target) {
        let _ = std::fs::remove_file(&target);
        return Err(e.into());
    }
    Ok(target)
}

/// Remove the working folders (`.velox-media-<pid>-...`) that a crashed or killed process left inside
/// `root` or its sub-folders: folders of other processes untouched for `older_than`. A live process keeps
/// its folder busy, so the age keeps this from ever touching a running job. Returns how many were removed.
pub fn sweep_stale_scratch(root: &Path, older_than: Duration) -> usize {
    fn walk(dir: &Path, depth: usize, older_than: Duration, me: &str, budget: &mut usize, removed: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
            if !meta.is_dir() {
                continue; // files, and links: never followed
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".velox-media-") {
                let mine = name.strip_prefix(".velox-media-").is_some_and(|r| r.starts_with(&format!("{me}-")));
                let idle = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age >= older_than);
                if !mine && idle && std::fs::remove_dir_all(entry.path()).is_ok() {
                    *removed += 1;
                }
            } else if depth > 0 && !name.starts_with('.') {
                walk(&entry.path(), depth - 1, older_than, me, budget, removed);
            }
        }
    }
    let (mut removed, mut budget) = (0, 20_000);
    walk(root, 4, older_than, &std::process::id().to_string(), &mut budget, &mut removed);
    removed
}

// ---------------------------------------------------------------------------------------------
// Downloading

struct Fetcher {
    http: HttpClient,
    extras: RequestExtras,
    progress: Arc<Progress>,
    cancel: CancellationToken,
    pacer: Option<Pacer>,
}

impl Fetcher {
    async fn once(&self, seg: &Segment, path: &Path, got: &mut u64) -> Result<()> {
        let mut file = tokio::fs::File::create(path).await?;
        let mut stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = http_client::Result<bytes::Bytes>> + Send>> = match seg.range {
            Some((s, e)) => Box::pin(self.http.get_range_with(&seg.url, s, e, None, &self.extras).await?.into_stream()),
            None => Box::pin(http_client::response_stream(self.http.get_with(&seg.url, &self.extras).await?)),
        };
        loop {
            let chunk = tokio::select! {
                _ = self.cancel.cancelled() => return Err(MediaError::Cancelled),
                c = stream.next() => c,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk?;
            *got += chunk.len() as u64;
            if *got > limits::MAX_SEGMENT_BYTES {
                return Err(MediaError::limit("one piece of the stream is larger than 32 GiB"));
            }
            if self.progress.bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64 > limits::MAX_TOTAL_BYTES {
                return Err(MediaError::limit("the stream is larger than 64 GiB"));
            }
            file.write_all(&chunk).await?;
            if let Some(pace) = &self.pacer {
                tokio::select! {
                    _ = self.cancel.cancelled() => return Err(MediaError::Cancelled),
                    _ = pace(chunk.len()) => {}
                }
            }
        }
        file.flush().await?;
        if let Some((s, e)) = seg.range {
            if *got != e - s + 1 {
                return Err(MediaError::invalid("the server sent a different amount of data than requested"));
            }
        }
        Ok(())
    }

    /// One segment into `path`, with a few tries for transient network trouble.
    async fn fetch(&self, seg: &Segment, path: &Path) -> Result<u64> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut got = 0u64;
            match self.once(seg, path, &mut got).await {
                Ok(()) => return Ok(got),
                Err(e) => {
                    self.progress.bytes.fetch_sub(got, Ordering::Relaxed);
                    let retry = matches!(&e, MediaError::Http(h) if h.is_retryable()) && attempt < limits::SEGMENT_TRIES;
                    if !retry {
                        return Err(e);
                    }
                    tokio::select! {
                        _ = self.cancel.cancelled() => return Err(MediaError::Cancelled),
                        _ = tokio::time::sleep(Duration::from_millis(500 * attempt as u64)) => {}
                    }
                }
            }
        }
    }
}

/// Everything in `plan` (init first) concatenated into `out`, segments fetched a few at a time but
/// written strictly in order. Subtitles (WebVTT) get their repeated headers removed.
async fn assemble(f: &Arc<Fetcher>, plan: &TrackPlan, scratch: &Path, tag: &str, out: &Path) -> Result<()> {
    let mut all: Vec<Segment> = Vec::with_capacity(plan.segments.len() + 1);
    all.extend(plan.init.clone());
    all.extend(plan.segments.iter().cloned());
    let vtt = plan.container == Container::Vtt;
    if plan.container == Container::Ttml && plan.segments.len() > 1 {
        return Err(MediaError::unsupported("subtitles split in many TTML pieces cannot be joined"));
    }
    // An earlier try (paused, or stopped by an error) may have written the first pieces already: `<tag>.state`
    // says how many and how many bytes. They are kept and the rest continues after them.
    let state = scratch.join(format!("{tag}.state"));
    let resumed = tokio::fs::read_to_string(&state).await.ok().and_then(|s| {
        let (n, len) = s.trim().split_once(' ')?;
        Some((n.parse::<usize>().ok()?, len.parse::<u64>().ok()?))
    });
    let on_disk = tokio::fs::metadata(out).await.map(|m| m.len()).unwrap_or(0);
    let (skip, mut written, mut dest) = match resumed {
        Some((n, len)) if n <= all.len() && on_disk >= len => {
            let d = tokio::fs::OpenOptions::new().write(true).open(out).await?;
            d.set_len(len).await?;
            let mut d = d;
            tokio::io::AsyncSeekExt::seek(&mut d, std::io::SeekFrom::Start(len)).await?;
            f.progress.segments_done.fetch_add(n as u64, Ordering::Relaxed);
            f.progress.bytes.fetch_add(len, Ordering::Relaxed);
            (n, len, d)
        }
        _ => (0, 0, tokio::fs::File::create(out).await?),
    };
    let mut parts = futures_util::stream::iter(all.into_iter().enumerate().skip(skip))
        .map(|(i, seg)| {
            let f = f.clone();
            let path = scratch.join(format!("{tag}-{i}.part"));
            async move { f.fetch(&seg, &path).await.map(|_| (i, path)) }
        })
        .buffered(limits::SEGMENT_CONCURRENCY);
    while let Some(item) = parts.next().await {
        let (i, path) = item?;
        if vtt {
            let raw = tokio::fs::read(&path).await?;
            let body = if i == 0 { &raw[..] } else { strip_vtt_header(&raw) };
            dest.write_all(body).await?;
            written += body.len() as u64;
            if !body.ends_with(b"\n") {
                dest.write_all(b"\n").await?;
                written += 1;
            }
        } else {
            let mut src = tokio::fs::File::open(&path).await?;
            written += tokio::io::copy(&mut src, &mut dest).await?;
        }
        dest.flush().await?;
        tokio::fs::write(&state, format!("{} {written}", i + 1)).await?;
        let _ = tokio::fs::remove_file(&path).await;
        failpoint::hit("media.after_segment");
        f.progress.segments_done.fetch_add(1, Ordering::Relaxed);
    }
    dest.flush().await?;
    Ok(())
}

/// Drop the `WEBVTT` header block (up to the first blank line) of a later WebVTT piece.
fn strip_vtt_header(raw: &[u8]) -> &[u8] {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    if !raw.starts_with(b"WEBVTT") {
        return raw;
    }
    for sep in [&b"\r\n\r\n"[..], b"\n\n"] {
        if let Some(pos) = raw.windows(sep.len()).position(|w| w == sep) {
            return &raw[pos + sep.len()..];
        }
    }
    &[]
}

/// A short, safe tag for a subtitle / audio file name: letters and digits only.
fn tag_of(label: &str, fallback: &str) -> String {
    let t: String = label.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(12).collect();
    if t.is_empty() {
        fallback.to_string()
    } else {
        t
    }
}

/// Download the selected tracks. Cancelling takes effect at once, wherever the work is: while waiting
/// for a slow server's answer, between pieces, or while ffmpeg runs (the futures are dropped, the
/// scratch folder is removed and ffmpeg is killed).
pub async fn download(http: &HttpClient, spec: DownloadSpec, progress: Arc<Progress>, cancel: CancellationToken) -> Result<Outcome> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(MediaError::Cancelled),
        r = download_inner(http, spec, progress, cancel.clone()) => r,
    }
}

async fn download_inner(http: &HttpClient, spec: DownloadSpec, progress: Arc<Progress>, cancel: CancellationToken) -> Result<Outcome> {
    progress.set(Phase::Preparing);
    let catalog = load_catalog(http, &spec.extras, &spec.url).await?;
    let sel = &spec.selection;

    let stream = match &sel.stream {
        Some(id) => find(&catalog.streams, id, |o| &o.id, "quality")?,
        None => catalog.streams.first().ok_or_else(|| MediaError::invalid("the stream lists no video"))?,
    };
    let audio: Option<&Entry<crate::TrackOption>> = match sel.audio.as_deref() {
        Some("none") => None,
        Some(id) => Some(find(&catalog.audio, id, |o| &o.id, "audio track")?),
        None => match &stream.option.group {
            Some(g) => {
                let mut of_group = catalog.audio.iter().filter(|a| a.option.group.as_deref() == Some(g.as_str()));
                let first = of_group.next();
                // The group's default track, else its first.
                first.map(|f| catalog.audio.iter().find(|a| a.option.group.as_deref() == Some(g.as_str()) && a.option.default).unwrap_or(f))
            }
            None => None,
        },
    };
    let mut subs: Vec<&Entry<crate::TrackOption>> = Vec::new();
    for id in &sel.subtitles {
        let e = find(&catalog.subtitles, id, |o| &o.id, "subtitle track")?;
        if !subs.iter().any(|s| s.option.id == e.option.id) {
            subs.push(e);
        }
    }

    let stream_plan = plan_of(http, &spec.extras, &stream.source).await?;
    let audio_plan = match audio {
        Some(a) => Some(plan_of(http, &spec.extras, &a.source).await?),
        None => None,
    };
    let mut sub_plans = Vec::new();
    for s in &subs {
        sub_plans.push(plan_of(http, &spec.extras, &s.source).await?);
    }

    let count = |p: &TrackPlan| p.segments.len() + usize::from(p.init.is_some());
    let total = count(&stream_plan) + audio_plan.as_ref().map_or(0, count) + sub_plans.iter().map(count).sum::<usize>();
    progress.segments_total.store(total as u64, Ordering::Relaxed);
    progress.segments_done.store(0, Ordering::Relaxed);
    progress.bytes.store(0, Ordering::Relaxed);
    progress.set(Phase::Downloading);

    let stem = if spec.stem.trim().is_empty() { catalog.analysis.title.clone() } else { sanitize_stem(&spec.stem) };
    confine(spec.sandbox_root.as_deref(), &spec.dest_dir)?;
    let scratch = Scratch::open(&spec.dest_dir, spec.work_dir.as_deref())?;
    let f = Arc::new(Fetcher {
        http: http.clone(),
        extras: spec.extras.clone(),
        progress: progress.clone(),
        cancel: cancel.clone(),
        pacer: spec.pacer.clone(),
    });

    let video_file = scratch.0.join(format!("video.{}", stream_plan.container.extension(false)));
    assemble(&f, &stream_plan, &scratch.0, "v", &video_file).await?;
    let audio_file = match &audio_plan {
        Some(p) => {
            let out = scratch.0.join(format!("audio.{}", p.container.extension(true)));
            assemble(&f, p, &scratch.0, "a", &out).await?;
            Some(out)
        }
        None => None,
    };

    let mut files: Vec<PathBuf> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // Subtitles are small and independent: a failure loses only that track.
    let mut sub_files = Vec::new();
    for (i, (entry, plan)) in subs.iter().zip(&sub_plans).enumerate() {
        let out = scratch.0.join(format!("sub{i}.{}", plan.container.extension(false)));
        match assemble(&f, plan, &scratch.0, &format!("s{i}"), &out).await {
            Ok(()) => sub_files.push((
                tag_of(entry.option.language.as_deref().unwrap_or(&entry.option.label), &format!("sub{}", i + 1)),
                out,
                plan.container,
            )),
            Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
            Err(e) => notes.push(format!("Subtitles “{}” were skipped: {e}", entry.option.label)),
        }
    }

    // Combine or keep.
    progress.set(Phase::Combining);
    let mut main: Option<PathBuf> = None;
    let video_in = Input { path: &video_file, container: stream_plan.container };
    // A video type the user chose (audio types do not apply to streams).
    let wanted = spec.format.filter(|f| !f.is_audio());
    let natural = stream_plan.container.extension(false);
    let combine = match (&spec.remux, &audio_file, stream_plan.container) {
        // Separate audio always needs combining with the video.
        (Some(r), Some(a), _) => Some((r, Some(Input { path: a, container: audio_plan.as_ref().map_or(Container::Aac, |p| p.container) }))),
        // A transport stream becomes an ordinary .mp4 when ffmpeg is there.
        (Some(r), None, Container::Ts) => Some((r, None)),
        // The user chose another file type than the one the stream comes in.
        (Some(r), None, c) if c.ffmpeg_format().is_some() && wanted.is_some_and(|w| w.extension() != natural) => Some((r, None)),
        _ => None,
    };
    failpoint::hit("media.before_mux");
    if let Some((remux, audio_in)) = combine {
        let mut inputs = vec![video_in];
        inputs.extend(audio_in);
        let mut last_error = None;
        for format in outputs_to_try(&inputs, wanted) {
            let out = scratch.0.join(format!("combined.{}", format.extension()));
            let muxed = remux.mux(Some(video_in), audio_in, &out, format, &cancel).await;
            failpoint::hit("media.after_mux");
            match muxed {
                Ok(()) => {
                    main = Some(publish(&out, &spec.dest_dir, spec.sandbox_root.as_deref(), &stem, format.extension())?);
                    if let Some(w) = wanted.filter(|w| w.extension() != format.extension()) {
                        notes.push(format!(
                            "Saved as .{}: this video's picture or sound cannot go into a .{} file.",
                            format.extension(),
                            w.extension()
                        ));
                    }
                    break;
                }
                Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
                Err(e) => {
                    tracing::warn!(error = %e, format = format.extension(), "remux failed");
                    let _ = std::fs::remove_file(&out);
                    last_error = Some(e);
                }
            }
        }
        if let (None, Some(e)) = (&main, last_error) {
            notes.push(format!("The video and audio could not be combined ({e}); they were saved as separate files."));
        }
    } else if audio_file.is_some() {
        notes.push(
            "The required dependencies are not ready, so the sound is a separate file. Download them in Settings > Required dependencies to have VeloxFetch combine them."
                .into(),
        );
    } else if wanted.is_some_and(|w| w.extension() != natural) {
        notes.push(format!(
            "Saved as .{natural}: changing the file type needs the required dependencies (Settings > Required dependencies)."
        ));
    }
    if main.is_none() {
        main = Some(publish(&video_file, &spec.dest_dir, spec.sandbox_root.as_deref(), &stem, stream_plan.container.extension(false))?);
        if let (Some(a), Some(p)) = (&audio_file, &audio_plan) {
            files.push(publish(a, &spec.dest_dir, spec.sandbox_root.as_deref(), &format!("{stem}.audio"), p.container.extension(true))?);
        }
    }
    let mut ordered = vec![main.expect("set above")];
    ordered.append(&mut files);
    for (tag, path, container) in sub_files {
        ordered.push(publish(&path, &spec.dest_dir, spec.sandbox_root.as_deref(), &format!("{stem}.{tag}"), container.extension(false))?);
    }
    if spec.mark_of_the_web {
        for f in &ordered {
            let _ = path_safety::mark_downloaded(f, spec.url.as_str());
        }
    }
    scratch.finish();
    progress.set(Phase::Done);
    Ok(Outcome { files: ordered, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-me-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_dead_working_folders_are_swept_never_files_links_or_the_running_jobs_own_folder() {
        let root = tmp("sweep");
        let sub = root.join("Videos").join("2024");
        std::fs::create_dir_all(&sub).unwrap();
        let stale = root.join(".velox-media-1-0-1"); // another process
        let stale_deep = sub.join(".velox-media-2-0-2");
        let mine = root.join(format!(".velox-media-{}-0-3", std::process::id()));
        let user_folder = root.join(".velox-media-notes"); // the prefix is what marks a working folder
        let unrelated = root.join("keep-me");
        let file_like = root.join(".velox-media-file");
        for d in [&stale, &stale_deep, &mine, &user_folder, &unrelated] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("x.part"), b"data").unwrap();
        }
        std::fs::write(&file_like, b"a file, not a folder").unwrap();
        // Too young: nothing is touched.
        assert_eq!(sweep_stale_scratch(&root, Duration::from_secs(3600)), 0);
        assert!(stale.exists() && stale_deep.exists());
        // Old enough: the dead ones go, the running job's own folder and everything else stay.
        let n = sweep_stale_scratch(&root, Duration::ZERO);
        assert_eq!(n, 3, "stale, stale_deep and the other scratch-named folder");
        assert!(!stale.exists() && !stale_deep.exists());
        assert!(mine.exists(), "the folder of this very process is never swept");
        assert!(unrelated.exists() && file_like.exists() && sub.exists());
    }

    #[test]
    fn publishing_puts_the_complete_file_in_place_and_never_an_empty_placeholder() {
        let root = tmp("publish-link");
        let src = root.join("scratch.bin");
        std::fs::write(&src, b"complete content").unwrap();
        std::fs::write(root.join("clip.mp4"), b"someone else's").unwrap();
        let out = publish(&src, &root, Some(&root), "clip", "mp4").unwrap();
        assert_eq!(out.file_name().unwrap(), "clip (1).mp4", "an existing name is never overwritten");
        assert_eq!(std::fs::read(&out).unwrap(), b"complete content");
        assert_eq!(std::fs::read(root.join("clip.mp4")).unwrap(), b"someone else's");
        assert!(!src.exists(), "the scratch name is gone");
        let mut names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["clip (1).mp4", "clip.mp4"]);
        // The fallback for drives without hard links behaves the same.
        let src2 = root.join("scratch2.bin");
        std::fs::write(&src2, b"second").unwrap();
        let out2 = publish_by_rename(&src2, &root, "clip", "mp4").unwrap();
        assert_eq!(out2.file_name().unwrap(), "clip (2).mp4");
        assert_eq!(std::fs::read(&out2).unwrap(), b"second");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publishing_stays_inside_the_sandbox_and_never_overwrites() {
        let root = tmp("pub");
        let src = root.join("src.bin");
        std::fs::write(&src, b"video").unwrap();
        let p = publish(&src, &root, Some(&root), "clip", "mp4").unwrap();
        assert_eq!(p.file_name().unwrap(), "clip.mp4");
        std::fs::write(&src, b"video2").unwrap();
        let p2 = publish(&src, &root, Some(&root), "clip", "mp4").unwrap();
        assert_eq!(p2.file_name().unwrap(), "clip (1).mp4");
        assert_eq!(std::fs::read(&p).unwrap(), b"video", "the first file is untouched");

        // A destination outside the sandbox is refused, and nothing is created there.
        let elsewhere = tmp("pub-else");
        std::fs::write(&src, b"video3").unwrap();
        assert!(matches!(publish(&src, &elsewhere, Some(&root), "clip", "mp4"), Err(MediaError::Invalid(_))));
        assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
        assert!(confine(None, &elsewhere).is_ok(), "no sandbox = no check");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn a_link_that_leads_out_of_the_sandbox_is_refused() {
        let root = tmp("pub-link");
        let outside = tmp("pub-link-out");
        let link = root.join("l");
        #[cfg(unix)]
        let ok = std::os::unix::fs::symlink(&outside, &link).is_ok();
        #[cfg(windows)]
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", link.to_str().unwrap(), outside.to_str().unwrap()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            let src = root.join("s.bin");
            std::fs::write(&src, b"x").unwrap();
            assert!(publish(&src, &link, Some(&root), "clip", "mp4").is_err());
            assert!(confine(Some(&root), &link).is_err());
            assert!(std::fs::read_dir(&outside).unwrap().next().is_none(), "nothing written through the link");
            #[cfg(unix)]
            let _ = std::fs::remove_file(&link);
            #[cfg(windows)]
            let _ = std::fs::remove_dir(&link);
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
