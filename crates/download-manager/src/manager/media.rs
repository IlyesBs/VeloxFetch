//! Stream downloads (HLS / DASH) and video pages (YouTube... through yt-dlp). A separate subsystem: its own job list and its own events. It
//! shares nothing with the normal download queue, database or engine, so the ordinary HTTP downloader cannot
//! be affected by it, except the speed limits: the user's global limit and the Smart throttle apply to video
//! jobs too (`media_pacer`, `site_rate`). Jobs live in memory; a finished file stays on disk.

use super::tools::Tool;
use std::path::{Path, PathBuf};

use http_client::{HttpClient, RequestExtras};
use media_engine::{DownloadSpec, OutputFormat, Phase, Progress, Remux, Selection, SiteHeaders, SiteSpec, SiteTool};
use tokio_util::sync::CancellationToken;

use super::*;

/// Stream jobs running at once.
const MAX_ACTIVE_JOBS: usize = 3;
/// Jobs kept in the list; the oldest finished ones are dropped first.
const MAX_JOBS: usize = 100;
/// Looking at a manifest must not hang.
const ANALYZE_TIMEOUT: Duration = Duration::from_secs(60);
/// yt-dlp has its own limit (90 s); this one only guards against a program that ignores it.
const SITE_ANALYZE_TIMEOUT: Duration = Duration::from_secs(180);
/// Playlists read lately, kept for `start_playlist` (the newest first), and for how long.
const KEPT_PLAYLISTS: usize = 8;
const PLAYLIST_TTL: Duration = Duration::from_secs(30 * 60);

/// A playlist `analyze_media` read: the videos' links stay here, the window chooses them by position.
struct KeptPlaylist {
    key: String,
    at: Instant,
    playlist: media_engine::Playlist,
    /// The browser's headers it was read with (referrer, user agent), for its videos too.
    headers: Vec<(String, String)>,
}

/// What a job runs: a stream description, or a video page through yt-dlp.
#[derive(Clone)]
enum Work {
    /// `bool`: ffmpeg is being fetched; the job waits for it to join the tracks.
    Stream(DownloadSpec, bool),
    /// `wait_ffmpeg`: ffmpeg is being fetched; the job waits for it before it starts.
    Site(SiteTool, SiteSpec, bool),
}

fn site_headers(headers: &[(String, String)]) -> SiteHeaders {
    let get = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
    SiteHeaders { referer: get("referer"), user_agent: get("user-agent") }
}

fn drm_refusal(url: &url::Url) -> Result<(), ManagerError> {
    match media_engine::protected_service(url) {
        Some(name) => {
            Err(ManagerError::invalid(format!("{name} protects its videos with DRM (copy protection), so they cannot be downloaded")))
        }
        None => Ok(()),
    }
}

struct Job {
    dto: MediaJobDto,
    cancel: CancellationToken,
    /// What to run on Resume / Retry (`None` once the job is over for good).
    work: Option<Work>,
    /// The job's work folder (pieces kept while paused or failed).
    work_dir: Option<PathBuf>,
    progress: Arc<Progress>,
    /// The user paused it: the stop that follows is a pause, not a cancel.
    pausing: bool,
    /// The files saved (main first) once completed.
    saved: Vec<PathBuf>,
    /// The page or stream link (for the history).
    page: String,
}

#[derive(Default)]
pub(super) struct MediaState {
    next: u64,
    jobs: Vec<Job>,
    /// Video jobs Smart throttle's pause policy paused (a game runs), to resume when it closes. In memory, like the jobs.
    game_held: Vec<u64>,
    /// The pause policy holds video downloads: queued ones do not start meanwhile.
    held: bool,
    playlists: Vec<KeptPlaylist>,
    next_playlist: u64,
}

pub(super) fn client(cfg: &http_client::ClientConfig) -> Result<HttpClient, ManagerError> {
    HttpClient::new(cfg).map_err(|e| ManagerError::Internal(format!("cannot create the stream client: {e}")))
}

/// A short stable name for a video job's failure, for the UI's texts (the message itself stays as the detail).
pub(crate) fn media_error_code(e: &media_engine::MediaError) -> String {
    use media_engine::MediaError as E;
    let lower = e.to_string().to_ascii_lowercase();
    let page = |fallback: &str| -> String {
        if lower.contains("drm") || lower.contains("protected") || lower.contains("encrypted") {
            "media_drm"
        } else if lower.contains("live") {
            "media_live"
        } else if lower.contains("playlist or a channel") {
            "media_playlist"
        } else if lower.contains("sign in") {
            "media_sign_in"
        } else if lower.contains("no downloadable video") || lower.contains("no video was found") {
            "media_no_video"
        } else {
            fallback
        }
        .to_string()
    };
    match e {
        E::Unsupported(_) => page("media_unsupported"),
        E::Site(_) => page("media_site"),
        E::Invalid(_) => "media_invalid".into(),
        E::Limit(_) => "media_limit".into(),
        E::Remux(_) => "media_remux".into(),
        E::Cancelled => "cancelled".into(),
        E::Http(_) | E::Io(_) => match crate::explain::error_code(&e.to_string()) {
            c if c == "other" => if matches!(e, E::Io(_)) { "media_file" } else { "media_network" }.to_string(),
            c => c,
        },
    }
}

/// `.m3u8` / `.mpd` links are streams, not files.
pub fn is_stream_link(url: &url::Url) -> bool {
    url.path_segments()
        .and_then(|mut s| s.next_back())
        .and_then(|f| f.rsplit_once('.'))
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case("m3u8") || e.eq_ignore_ascii_case("mpd"))
}

fn strip_known_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if ["mp4", "ts", "mkv", "webm", "m4a", "aac", "mp3", "opus", "mov", "m3u8", "mpd"]
                .contains(&ext.to_ascii_lowercase().as_str()) =>
        {
            stem
        }
        _ => name,
    }
}

impl DownloadManager {
    pub(super) fn media_client(&self) -> HttpClient {
        self.inner.media_http.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Is ffmpeg available right now? (Looked up each time, so installing it needs no restart.) The one
    /// VeloxFetch fetched for itself first, then one next to the app or on `PATH`.
    pub(super) fn ffmpeg(&self) -> Option<Remux> {
        if let Some(p) = &self.inner.cfg.ffmpeg_path {
            return p.is_file().then(|| Remux::new(p.clone()));
        }
        if let Some(p) = self.installed_tool(Tool::Ffmpeg) {
            return Some(Remux::new(p));
        }
        let app_dir: Option<PathBuf> = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from));
        media_engine::find_ffmpeg(app_dir.as_deref()).map(Remux::new)
    }

    /// Is yt-dlp available right now? (Looked up each time, like ffmpeg.) With the Deno VeloxFetch
    /// fetched, when there is one (YouTube's pages need a JavaScript runtime).
    pub(super) fn ytdlp(&self) -> Option<SiteTool> {
        let program = match &self.inner.cfg.ytdlp_path {
            Some(p) => p.is_file().then(|| p.clone())?,
            None => match self.installed_tool(Tool::YtDlp) {
                Some(p) => p,
                None => {
                    let app_dir: Option<PathBuf> = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from));
                    media_engine::find_ytdlp(app_dir.as_deref())?
                }
            },
        };
        Some(self.with_js(SiteTool::new(program, self.ffmpeg().map(|r| r.program().to_path_buf()))))
    }

    /// Deno for the sites' JavaScript, unless it already failed on this computer (then none at all).
    fn with_js(&self, tool: SiteTool) -> SiteTool {
        if self.inner.js_broken.load(Ordering::Relaxed) {
            tool.without_js()
        } else {
            tool.with_deno(self.installed_tool(Tool::Deno))
        }
    }

    /// yt-dlp for a video page, fetching what is missing on first use: yt-dlp and Deno are waited for
    /// (the page cannot be read without them); ffmpeg (large) is fetched in the background. Returns the
    /// tool and whether ffmpeg is still on its way.
    async fn site_tool(&self) -> Result<(SiteTool, bool), ManagerError> {
        let need_ytdlp = self.ytdlp().is_none();
        let need_deno = self.installed_tool(Tool::Deno).is_none() && self.can_fetch(Tool::Deno);
        if self.ffmpeg().is_none() {
            self.fetch_in_background(Tool::Ffmpeg, false);
        }
        let ytdlp = async {
            if need_ytdlp {
                self.ensure_tool(Tool::YtDlp, false).await;
            }
        };
        let deno = async {
            if need_deno {
                self.ensure_tool(Tool::Deno, false).await;
            }
        };
        tokio::join!(ytdlp, deno);
        if self.tool_stale(Tool::YtDlp) {
            // An old yt-dlp is not replaced by itself any more: its update is looked for, and offered.
            self.check_tool_updates_soon();
        }
        let tool = self.ytdlp().ok_or_else(|| ManagerError::invalid(self.no_ytdlp_message()))?;
        let ffmpeg_coming = self.ffmpeg().is_none() && self.tool_installing(Tool::Ffmpeg);
        Ok((tool, ffmpeg_coming))
    }

    /// The link to look at and the headers to send: from a waiting browser video, or the typed link.
    fn media_source(&self, url: &str, pending: Option<u64>) -> Result<(url::Url, Vec<(String, String)>), ManagerError> {
        match pending {
            Some(token) => self.pending_media(token),
            None => Ok((validate::url_without_credentials(url)?, Vec::new())),
        }
    }

    /// Any stream job still working or waiting its turn (counts as "busy" for the power trigger).
    pub(super) fn media_busy(&self) -> bool {
        self.inner.media.lock().unwrap().jobs.iter().any(|j| j.dto.status.is_active() || j.dto.status == MediaStatus::Queued)
    }

    /// Look at a stream link: which qualities, audio tracks and subtitles it offers. Nothing is
    /// downloaded or written.
    pub async fn analyze_media(&self, url: String) -> Result<MediaAnalysisDto, ManagerError> {
        self.analyze_media_for(url, None).await
    }

    /// `analyze_media`, for a typed link or (with `pending`) a video the browser handed over. Video pages
    /// (YouTube...) are read through yt-dlp; DRM services are refused before anything is asked.
    pub async fn analyze_media_for(&self, url: String, pending: Option<u64>) -> Result<MediaAnalysisDto, ManagerError> {
        self.analyze_media_with(url, pending, false).await
    }

    /// `analyze_media_for`; with `whole_playlist`, the playlist the video link belongs to (`watch?v=...&list=...`)
    /// is read instead of the video. A playlist's videos are kept for `start_playlist`.
    pub async fn analyze_media_with(
        &self,
        url: String,
        pending: Option<u64>,
        whole_playlist: bool,
    ) -> Result<MediaAnalysisDto, ManagerError> {
        let (mut url, headers) = self.media_source(&url, pending)?;
        if whole_playlist {
            url = media_engine::playlist_link(&url).ok_or_else(|| ManagerError::invalid("this link is not part of a playlist"))?;
        }
        drm_refusal(&url)?;
        let analysis = if media_engine::is_site_page(&url) {
            let (mut tool, ffmpeg_coming) = self.site_tool().await?;
            if ffmpeg_coming {
                // Offer the HD qualities now: the download waits for ffmpeg.
                tool = self.with_js(SiteTool::new(tool.program().to_path_buf(), Some(self.tools_dir().join(Tool::Ffmpeg.file_name()))));
            }
            // Reading a page fails now and then (the site is slow, a check times out): try once more. When the error
            // says the site changed, an update of yt-dlp is looked for and offered (the user accepts it or not).
            //
            // Deno (YouTube's JavaScript challenges) hangs or fails on some computers while yt-dlp itself works:
            // then the page is read again without any JavaScript runtime (fewer formats, but the video downloads),
            // and the rest of this session does the same. The user never has to fix their computer.
            let mut tries = 0;
            let mut dropped_js = false;
            loop {
                tries += 1;
                let attempt = if tool.uses_js() { tool.clone().with_analyze_timeout(self.inner.cfg.site_js_timeout) } else { tool.clone() };
                let r = tokio::time::timeout(SITE_ANALYZE_TIMEOUT, media_engine::analyze_site(&attempt, &url, &site_headers(&headers)))
                    .await
                    .unwrap_or_else(|_| Err(media_engine::MediaError::Site("the site did not answer in time".into())));
                match r {
                    Ok(a) => {
                        if dropped_js {
                            tracing::warn!("video pages work without the JavaScript runtime here: not using it until restart");
                            self.inner.js_broken.store(true, Ordering::Relaxed);
                        }
                        break a;
                    }
                    Err(e) if tool.uses_js() && worth_retrying(&e) => {
                        tracing::warn!(error = %e, "reading the video page with the JavaScript runtime failed; trying without it");
                        tool = tool.without_js();
                        dropped_js = true;
                        tries -= 1; // the usual retry is still there for the page itself
                    }
                    Err(e) if tries < 2 && worth_retrying(&e) => {
                        tracing::warn!(error = %e, "reading the video page failed; trying again");
                        if looks_outdated(&e) {
                            self.check_tool_updates_soon();
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    Err(e) => return Err(ManagerError::invalid(e.to_string())),
                }
            }
        } else {
            if self.ffmpeg().is_none() {
                // Joining video and audio needs it: start getting it while the user chooses.
                self.fetch_in_background(Tool::Ffmpeg, false);
            }
            let extras = RequestExtras::from_pairs(&headers);
            tokio::time::timeout(ANALYZE_TIMEOUT, media_engine::analyze(&self.media_client(), &extras, &url))
                .await
                .map_err(|_| ManagerError::invalid("the stream did not answer in time"))?
                .map_err(|e| ManagerError::invalid(e.to_string()))?
        };
        let combine_available = self.ffmpeg().is_some() || self.tool_installing(Tool::Ffmpeg) || self.can_fetch(Tool::Ffmpeg);
        let playlist_key = analysis.playlist.as_ref().map(|p| self.keep_playlist(p.clone(), headers));
        Ok(MediaAnalysisDto { analysis, combine_available, playlist_key })
    }

    pub async fn start_media(&self, req: MediaRequest) -> Result<MediaJobDto, ManagerError> {
        validate::remember_needs_folder(req.remember_for, req.folder_token)?;
        let (url, headers) = self.media_source(&req.url, req.pending)?;
        drm_refusal(&url)?;
        let site = media_engine::is_site_page(&url);
        let tool = if site { Some(self.site_tool().await?) } else { None };
        // Ids are letters and digits; a video page's subtitle id also has the dashes of its language (`mpt-BR`).
        for id in req.stream.iter().chain(req.audio.iter()).chain(req.subtitles.iter()) {
            if id.is_empty() || id.len() > 16 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || id.starts_with('-') {
                return Err(ManagerError::invalid("unknown track"));
            }
        }
        if req.stream.iter().chain(req.audio.iter()).any(|id| id.contains('-'))
            || (site && !req.subtitles.iter().all(|id| media_engine::is_subtitle_id(id)))
            || (!site && req.subtitles.iter().any(|id| id.contains('-')))
        {
            return Err(ManagerError::invalid("unknown track"));
        }
        if req.subtitles.len() > 50 {
            return Err(ManagerError::invalid("too many subtitle tracks"));
        }
        let format = match req.format.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
            None => None,
            Some(f) => Some(OutputFormat::parse(f).ok_or_else(|| ManagerError::invalid("unknown file type"))?),
        };
        if !site && format.is_some_and(OutputFormat::is_audio) {
            return Err(ManagerError::invalid("a stream is saved as a video: choose MP4, MKV or WebM"));
        }
        // Joining (and changing the file type) needs ffmpeg: a stream waits for it when it is on its way.
        let combine = req.combine.unwrap_or(true);
        let stream_waits = !site && combine && self.ffmpeg().is_none() && self.can_fetch(Tool::Ffmpeg);
        if stream_waits {
            self.fetch_in_background(Tool::Ffmpeg, false);
        }
        let stem = match req.filename.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(n) => {
                let n = strip_known_extension(n);
                validate::filename(n)?;
                n.to_string()
            }
            None => String::new(), // the engine takes the title from the link
        };
        let named = req.subdir.as_deref().filter(|s| !s.trim().is_empty());
        // Only the sound is saved as audio; anything else is a video (for the folder remembered for each).
        let kind = if format.is_some_and(OutputFormat::is_audio) || req.stream.as_deref() == Some("audio") {
            downloader_core::FileKind::Audio
        } else {
            downloader_core::FileKind::Video
        };
        if req.folder_token.is_some() && named.is_some() {
            return Err(ManagerError::invalid("choose either a folder or a sub-folder, not both"));
        }
        let picked = req.folder_token.map(|token| self.pick_path(token)).transpose()?;
        let remembered = if picked.is_none() && named.is_none() { self.remembered_folder(kind) } else { None };
        let (root, dest_dir) = match (&picked, remembered) {
            // A folder the user picked: the video goes exactly there, and the job is confined to it.
            (Some(folder), _) => {
                self.allow_folder(folder).await?;
                (folder.clone(), folder.clone())
            }
            // The folder remembered for videos (or for sound): exactly there, confined to it.
            (None, Some(folder)) => (folder.clone(), folder),
            (None, None) => {
                let root = self.download_root();
                // Sorting by type: a stream is video, unless the user named a sub-folder.
                let sorted = named.is_none().then(|| self.sorted_subfolder(downloader_core::FileKind::Video)).flatten();
                let dest_dir = validate::subdir(&root, named.or(sorted.as_deref()))?;
                (root, dest_dir)
            }
        };
        tokio::fs::create_dir_all(&dest_dir).await.map_err(|e| ManagerError::Internal(format!("cannot create the folder: {e}")))?;
        validate::ensure_within(&root, &dest_dir)?;

        let title = if stem.is_empty() && site {
            "video".to_string()
        } else if stem.is_empty() {
            media_engine::sanitize_stem(url.path_segments().and_then(|mut s| s.next_back()).unwrap_or(""))
        } else {
            stem.clone()
        };
        let page = url.to_string();
        // A video page's subtitles go with the picture: none with only the sound.
        let site_subtitles = if format.is_some_and(OutputFormat::is_audio) || req.stream.as_deref() == Some("audio") {
            Vec::new()
        } else {
            req.subtitles.clone()
        };
        let (dto, work_dir) = {
            let mut m = self.inner.media.lock().unwrap();
            m.next += 1;
            let id = m.next;
            // The job's own work folder: kept while it is paused or failed, so Resume / Retry continues.
            let work_dir = dest_dir.join(media_engine::work_dir_name(id));
            let dto = MediaJobDto {
                id,
                title: strip_known_extension(&title).to_string(),
                host: url.host_str().unwrap_or("").to_string(),
                status: MediaStatus::Preparing,
                bytes: 0,
                segments_done: 0,
                segments_total: 0,
                percent: None,
                folder: dest_dir.to_string_lossy().into_owned(),
                files: Vec::new(),
                notes: Vec::new(),
                error: None,
                error_code: None,
                site,
                speed_bps: 0,
                eta_secs: None,
                resumable: false,
                playable: false,
                url: page.clone(),
                quality: req.stream.clone(),
                format: format.map(|f| f.extension().to_string()),
                audio_only: format.is_some_and(OutputFormat::is_audio),
                subtitles: if site { site_subtitles.len() } else { req.subtitles.len() },
                started_at_ms: now_ms(),
                ended_at_ms: None,
                active_ms: 0,
                running_since_ms: Some(now_ms()),
                file_size: None,
                retries: 0,
                playlist: None,
            };
            (dto, work_dir)
        };
        let work = match tool {
            Some((tool, wait_ffmpeg)) => Work::Site(
                tool,
                SiteSpec {
                    url,
                    quality: req.stream,
                    format,
                    dest_dir,
                    sandbox_root: Some(root.clone()),
                    mark_of_the_web: self.inner.cfg.download.mark_of_the_web,
                    stem,
                    headers: site_headers(&headers),
                    work_dir: Some(work_dir.clone()),
                    rate_limit: None, // set for each run (run_once): the limits can change meanwhile
                    subtitles: site_subtitles,
                },
                wait_ffmpeg,
            ),
            None => Work::Stream(
                DownloadSpec {
                    url,
                    selection: Selection { stream: req.stream, audio: req.audio, subtitles: req.subtitles },
                    dest_dir,
                    stem,
                    sandbox_root: Some(root.clone()),
                    mark_of_the_web: self.inner.cfg.download.mark_of_the_web,
                    remux: if combine { self.ffmpeg() } else { None },
                    work_dir: Some(work_dir.clone()),
                    format,
                    extras: RequestExtras::from_pairs(&headers),
                    pacer: Some(self.media_pacer()),
                },
                stream_waits,
            ),
        };
        let dto = self.add_job(dto, work, work_dir, page);
        if let (Some(remember), Some(folder)) = (req.remember_for, picked) {
            self.remember_for_kind(remember, &folder).await;
        }
        // The browser's request is answered: it leaves the waiting list.
        if let Some(token) = req.pending {
            let _ = self.take_pending(token);
        }
        self.start_queued();
        Ok(self.with_job(dto.id, |j| j.dto.clone()).unwrap_or(dto))
    }

    /// Put a new job in the list, queued (`start_queued` starts it when there is a free place).
    fn add_job(&self, mut dto: MediaJobDto, work: Work, work_dir: PathBuf, page: String) -> MediaJobDto {
        dto.status = MediaStatus::Queued;
        dto.running_since_ms = None;
        {
            let mut m = self.inner.media.lock().unwrap();
            m.jobs.push(Job {
                dto: dto.clone(),
                cancel: CancellationToken::new(),
                work: Some(work),
                work_dir: Some(work_dir),
                progress: Arc::new(Progress::default()),
                pausing: false,
                saved: Vec::new(),
                page,
            });
            // Keep the list bounded: drop the oldest finished jobs (never one waiting, paused or running).
            while m.jobs.len() > MAX_JOBS {
                match m
                    .jobs
                    .iter()
                    .position(|j| !j.dto.status.is_active() && !matches!(j.dto.status, MediaStatus::Paused | MediaStatus::Queued))
                {
                    Some(i) => {
                        let old = m.jobs.remove(i);
                        remove_work_dir(&old);
                    }
                    None => break,
                }
            }
        }
        self.emit(ManagerEvent::Media { job: dto.clone() });
        dto
    }

    /// Start queued jobs, oldest first, while fewer than [`MAX_ACTIVE_JOBS`] run (none while the pause policy holds
    /// video downloads). Called whenever a job is added or one stops.
    pub(super) fn start_queued(&self) {
        let started: Vec<(MediaJobDto, CancellationToken)> = {
            let mut m = self.inner.media.lock().unwrap();
            if m.held {
                return;
            }
            let free = MAX_ACTIVE_JOBS.saturating_sub(m.jobs.iter().filter(|j| j.dto.status.is_active()).count());
            let now = now_ms();
            let mut out = Vec::new();
            for j in m.jobs.iter_mut().filter(|j| j.dto.status == MediaStatus::Queued).take(free) {
                j.cancel = CancellationToken::new();
                j.pausing = false;
                j.dto.status = MediaStatus::Preparing;
                track_time(&mut j.dto, now);
                out.push((j.dto.clone(), j.cancel.clone()));
            }
            out
        };
        for (dto, cancel) in started {
            self.emit(ManagerEvent::Media { job: dto.clone() });
            let mgr = self.clone();
            tokio::spawn(async move { mgr.run_media(dto.id, cancel).await });
        }
    }

    /// Keep a playlist just read; its key for `start_playlist`.
    fn keep_playlist(&self, playlist: media_engine::Playlist, headers: Vec<(String, String)>) -> String {
        let mut m = self.inner.media.lock().unwrap();
        m.next_playlist += 1;
        let key = format!("p{}", m.next_playlist);
        m.playlists.retain(|p| p.at.elapsed() < PLAYLIST_TTL);
        m.playlists.insert(0, KeptPlaylist { key: key.clone(), at: Instant::now(), playlist, headers });
        m.playlists.truncate(KEPT_PLAYLISTS);
        key
    }

    /// Download the chosen videos of a playlist `analyze_media` read: one video job each, in the playlist's order, in
    /// one folder (the playlist's name), named by position. A few run at once; the others wait their turn.
    pub async fn start_playlist(&self, req: PlaylistRequest) -> Result<Vec<MediaJobDto>, ManagerError> {
        validate::remember_needs_folder(req.remember_for, req.folder_token)?;
        let (playlist, headers) = {
            let m = self.inner.media.lock().unwrap();
            let kept = m
                .playlists
                .iter()
                .find(|p| p.key == req.key && p.at.elapsed() < PLAYLIST_TTL)
                .ok_or_else(|| ManagerError::invalid("this playlist is no longer known: read the link again"))?;
            (kept.playlist.clone(), kept.headers.clone())
        };
        if req.items.is_empty() {
            return Err(ManagerError::invalid("choose at least one video"));
        }
        if req.items.len() > media_engine::limits::MAX_PLAYLIST_VIDEOS {
            return Err(ManagerError::invalid("too many videos"));
        }
        let mut items = req.items.clone();
        items.sort_unstable();
        items.dedup();
        if items.len() != req.items.len() {
            return Err(ManagerError::invalid("a video was chosen twice"));
        }
        let mut chosen = Vec::with_capacity(items.len());
        for i in &items {
            let e = playlist.entries.iter().find(|e| e.index == *i).ok_or_else(|| ManagerError::invalid("unknown video"))?;
            if !e.available {
                return Err(ManagerError::invalid("this video cannot be downloaded (private, members only or live)"));
            }
            chosen.push(e.clone());
        }
        let quality = req.quality.clone().filter(|q| q != "best");
        if let Some(q) = &quality {
            if !media_engine::PLAYLIST_QUALITIES.contains(&q.as_str()) {
                return Err(ManagerError::invalid("unknown quality"));
            }
        }
        let format = match req.format.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
            None => None,
            Some(f) => Some(OutputFormat::parse(f).ok_or_else(|| ManagerError::invalid("unknown file type"))?),
        };
        let format = format.or_else(|| (quality.as_deref() == Some("audio")).then_some(OutputFormat::Mp3));

        // The folder: the user's, else the playlist's name inside the videos folder; both inside the folder the
        // user picked, when there is one (the jobs are then confined to it).
        let picked = match req.folder_token {
            Some(token) => {
                let folder = self.pick_path(token)?;
                self.allow_folder(&folder).await?;
                Some(folder)
            }
            None => None,
        };
        // Else the folder remembered for videos (or for sound), when there is one: the playlist's folder goes inside.
        let kind =
            if format.is_some_and(OutputFormat::is_audio) { downloader_core::FileKind::Audio } else { downloader_core::FileKind::Video };
        let remembered = if picked.is_none() { self.remembered_folder(kind) } else { None };
        let chosen_base = picked.clone().or(remembered);
        let root = chosen_base.clone().unwrap_or_else(|| self.download_root());
        let named = match req.subdir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => {
                let mut name = media_engine::sanitize_stem(&playlist.title);
                if validate::filename(&name).is_err() {
                    name = "playlist".into();
                }
                match chosen_base.is_none().then(|| self.sorted_subfolder(downloader_core::FileKind::Video)).flatten() {
                    Some(v) => format!("{v}/{name}"),
                    None => name,
                }
            }
        };
        let dest_dir = validate::subdir(&root, Some(&named))?;
        tokio::fs::create_dir_all(&dest_dir).await.map_err(|e| ManagerError::Internal(format!("cannot create the folder: {e}")))?;
        validate::ensure_within(&root, &dest_dir)?;

        let (tool, wait_ffmpeg) = self.site_tool().await?;
        let width = items.last().map_or(1, |n| n.to_string().len()).max(2);
        let count = chosen.len() as u32;
        let mut jobs = Vec::with_capacity(chosen.len());
        for e in chosen {
            drm_refusal(&e.url)?;
            // "07 - Title": the playlist's order in any file list. A title that is not a safe name keeps the number.
            let mut stem = format!("{:0width$} - {}", e.index, media_engine::sanitize_stem(&e.title));
            stem = stem.chars().take(150).collect::<String>().trim_end().to_string();
            if validate::filename(&stem).is_err() {
                stem = format!("{:0width$}", e.index);
            }
            let id = {
                let mut m = self.inner.media.lock().unwrap();
                m.next += 1;
                m.next
            };
            let work_dir = dest_dir.join(media_engine::work_dir_name(id));
            let dto = MediaJobDto {
                id,
                title: stem.clone(),
                host: e.url.host_str().unwrap_or("").to_string(),
                status: MediaStatus::Queued,
                bytes: 0,
                segments_done: 0,
                segments_total: 0,
                percent: None,
                folder: dest_dir.to_string_lossy().into_owned(),
                files: Vec::new(),
                notes: Vec::new(),
                error: None,
                error_code: None,
                site: true,
                speed_bps: 0,
                eta_secs: None,
                resumable: false,
                playable: false,
                url: e.url.to_string(),
                quality: quality.clone(),
                format: format.map(|f| f.extension().to_string()),
                audio_only: format.is_some_and(OutputFormat::is_audio),
                subtitles: 0,
                started_at_ms: now_ms(),
                ended_at_ms: None,
                active_ms: 0,
                running_since_ms: None,
                file_size: None,
                retries: 0,
                playlist: Some(PlaylistTag { title: playlist.title.clone(), index: e.index, count }),
            };
            let work = Work::Site(
                tool.clone(),
                SiteSpec {
                    url: e.url.clone(),
                    quality: quality.clone(),
                    format,
                    dest_dir: dest_dir.clone(),
                    sandbox_root: Some(root.clone()),
                    mark_of_the_web: self.inner.cfg.download.mark_of_the_web,
                    stem,
                    headers: site_headers(&headers),
                    work_dir: Some(work_dir.clone()),
                    rate_limit: None,
                    subtitles: Vec::new(),
                },
                wait_ffmpeg,
            );
            jobs.push(self.add_job(dto, work, work_dir, e.url.to_string()));
        }
        if let Some(token) = req.pending {
            let _ = self.take_pending(token);
        }
        tracing::info!(videos = jobs.len(), "playlist videos added");
        self.start_queued();
        if let (Some(remember), Some(folder)) = (req.remember_for, picked) {
            self.remember_for_kind(remember, &folder).await;
        }
        Ok(jobs.into_iter().map(|d| self.with_job(d.id, |j| j.dto.clone()).unwrap_or(d)).collect())
    }

    /// Run a job (again, after a pause or a failure: from what is already in its work folder). Tries up to
    /// [`TRIES`] times on its own when the site or the network fails part-way.
    async fn run_media(&self, id: u64, cancel: CancellationToken) {
        let Some((mut work, progress)) = ({
            let m = self.inner.media.lock().unwrap();
            m.jobs.iter().find(|j| j.dto.id == id).and_then(|j| Some((j.work.clone()?, j.progress.clone())))
        }) else {
            return;
        };
        if let Work::Site(tool, _, wait_ffmpeg @ true) = &mut work {
            // HD needs ffmpeg, which is still being fetched (once): wait for it, then use whatever there is.
            self.media_update(id, |d| d.notes = vec!["Downloading required dependencies for HD (only the first time)…".into()]);
            tokio::select! {
                _ = self.ensure_tool(Tool::Ffmpeg, false) => {}
                _ = cancel.cancelled() => {}
            }
            if let Some(fresh) = self.ytdlp() {
                *tool = fresh;
            }
            *wait_ffmpeg = false;
            self.media_update(id, |d| d.notes.clear());
        }
        if let Work::Stream(spec, wait_ffmpeg @ true) = &mut work {
            // Joining needs ffmpeg, which is being fetched (once): wait for it.
            self.media_update(id, |d| {
                d.notes = vec!["Downloading required dependencies to join video and sound (only the first time)…".into()]
            });
            tokio::select! {
                _ = self.ensure_tool(Tool::Ffmpeg, false) => {}
                _ = cancel.cancelled() => {}
            }
            spec.remux = self.ffmpeg();
            *wait_ffmpeg = false;
            self.media_update(id, |d| d.notes.clear());
        }
        // What the next run starts from (ffmpeg found, a refreshed yt-dlp).
        self.with_job(id, |j| j.work = Some(work.clone()));

        let mut refreshed = false;
        let mut attempt = 1u32;
        let result = loop {
            if cancel.is_cancelled() {
                break Err(media_engine::MediaError::Cancelled);
            }
            let r = self.run_once(id, work.clone(), &progress, &cancel).await;
            match r {
                Err(e) if attempt < TRIES && worth_retrying(&e) && !cancel.is_cancelled() => {
                    tracing::warn!(id, attempt, error = %e, "video download failed; trying again");
                    // Sites change: an old yt-dlp is a common reason. Its update is looked for and offered (never
                    // downloaded without the user); one the user accepted meanwhile is used for the next try.
                    if let Work::Site(tool, _, _) = &mut work {
                        if !refreshed && looks_outdated(&e) {
                            refreshed = true;
                            self.check_tool_updates_soon();
                            if let Some(fresh) = self.ytdlp() {
                                *tool = fresh;
                            }
                        }
                    }
                    self.media_update(id, |d| {
                        d.retries += 1;
                        d.status = MediaStatus::Preparing;
                        d.speed_bps = 0;
                        d.notes = vec![format!("It stopped ({e}). Trying again ({} of {TRIES})…", attempt + 1)];
                    });
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(3 * attempt as u64)) => {}
                        _ = cancel.cancelled() => {}
                    }
                    attempt += 1;
                }
                other => break other,
            }
        };

        let pausing = self.with_job(id, |j| j.pausing).unwrap_or(false);
        let mut saved = None;
        self.media_update(id, |d| {
            Self::apply_progress(d, &progress);
            d.speed_bps = 0;
            d.eta_secs = None;
            match &result {
                Ok(o) => {
                    d.status = MediaStatus::Completed;
                    d.percent = Some(100.0);
                    d.files = o.files.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
                    // The page's title, once known (a job started without a name shows "video" until then).
                    if let Some(stem) = o.files.first().and_then(|p| p.file_stem()) {
                        d.title = stem.to_string_lossy().into_owned();
                    }
                    d.notes = o.notes.clone();
                    d.error = None;
                    d.error_code = None;
                    d.resumable = false;
                    d.playable = o.files.first().is_some_and(|f| super::preview::media_type(&f.to_string_lossy()).is_some());
                    d.file_size = Some(o.files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum());
                    saved = Some(o.files.clone());
                }
                Err(media_engine::MediaError::Cancelled) if pausing => {
                    d.status = MediaStatus::Paused;
                    d.notes.clear();
                    d.resumable = true;
                }
                Err(media_engine::MediaError::Cancelled) => {
                    d.status = MediaStatus::Cancelled;
                    d.resumable = false;
                    d.playable = false;
                }
                Err(e) => {
                    tracing::warn!(id, error = %e, "video download failed");
                    d.status = MediaStatus::Failed;
                    d.notes.clear();
                    d.error = Some(e.to_string());
                    d.error_code = Some(media_error_code(e));
                    // Only what can work again offers Retry (DRM, live streams, a refused link would not).
                    d.resumable = worth_retrying(e);
                }
            }
        });
        let status = self.with_job(id, |j| j.dto.status);
        match (status, saved) {
            (Some(MediaStatus::Completed), Some(files)) => {
                let entry = self.with_job(id, |j| {
                    j.work = None;
                    j.work_dir = None;
                    j.saved = files.clone();
                    (j.page.clone(), j.dto.bytes)
                });
                // The finished video goes into the history, and counts in "Total downloaded".
                if let (Some((page, bytes)), Some(main)) = (entry, files.first()) {
                    let size = std::fs::metadata(main).map(|m| m.len()).unwrap_or(bytes);
                    let name = main.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let (store, main) = (self.inner.store.clone(), main.clone());
                    let _ = blocking(move || store.add_history(&page, &name, &main, size).map(|_| ())).await;
                    self.inner.stats.lock().unwrap().dirty = true;
                }
            }
            // Over for good (cancelled, or a failure Retry could not fix): its pieces go.
            (Some(MediaStatus::Cancelled), _) | (Some(MediaStatus::Failed), _) => {
                self.with_job(id, |j| {
                    if j.dto.status == MediaStatus::Cancelled || !j.dto.resumable {
                        remove_work_dir(j);
                        j.work = None;
                        j.work_dir = None;
                    }
                });
            }
            _ => {}
        }
        // Its place is free: the next queued video starts.
        self.start_queued();
    }

    /// Stream jobs pace every piece through the app's shared limits (the user's global limit and the
    /// Smart throttle), exactly like ordinary downloads.
    fn media_pacer(&self) -> media_engine::Pacer {
        let throttle = self.inner.limits.throttle_for(None, downloader_core::limiter::TokenBucket::new(None));
        // Cancelling is the job's business (the fetcher stops waiting on it); this token is never cancelled.
        let never = CancellationToken::new();
        Arc::new(move |n| {
            let (t, c) = (throttle.clone(), never.clone());
            Box::pin(async move {
                let _ = t.take(n, &c).await;
            })
        })
    }

    /// The speed one yt-dlp job may use: the tightest of the user's global limit and the Smart throttle's
    /// cap, shared with the other video-page jobs that are running. `None` = no limit.
    fn site_rate(&self) -> Option<u64> {
        let limits = &self.inner.limits;
        let cap = match (limits.global(), limits.smart()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }?;
        let sites = {
            let m = self.inner.media.lock().unwrap();
            m.jobs.iter().filter(|j| j.dto.site && j.dto.status.is_active()).count().max(1) as u64
        };
        Some((cap / sites).max(16 * 1024))
    }

    /// One run of the engine, with the job's numbers (speed, share done) kept up to date for the UI.
    /// yt-dlp is told its speed limit when it starts; when the limits change a lot while it downloads (the
    /// Smart throttle turns on or off, the user changes the global limit), it is restarted with the new one
    /// and continues from its partial files.
    async fn run_once(
        &self,
        id: u64,
        work: Work,
        progress: &Arc<Progress>,
        cancel: &CancellationToken,
    ) -> Result<media_engine::Outcome, media_engine::MediaError> {
        loop {
            let mut work = work.clone();
            let rate = if let Work::Site(_, spec, _) = &mut work {
                spec.rate_limit = self.site_rate();
                Some(spec.rate_limit)
            } else {
                None
            };
            let run_cancel = cancel.child_token();
            let mut restarting = false;
            let mut differs = 0u32;
            let r = self
                .run_watched(id, work, progress, &run_cancel, |me| {
                    // Restarting yt-dlp costs seconds (it asks the site again), so only a limit that really changed
                    // and stays changed restarts it, and only while it downloads (joining and converting are not
                    // network work). See `limit_needs_restart`.
                    let Some(started) = rate else { return false };
                    if progress.phase() != Phase::Downloading {
                        return false;
                    }
                    differs = if limit_needs_restart(started, me.site_rate()) { differs + 1 } else { 0 };
                    let changed = differs >= RESTART_CONFIRM;
                    if changed {
                        restarting = true;
                        run_cancel.cancel();
                    }
                    changed
                })
                .await;
            match r {
                Err(media_engine::MediaError::Cancelled) if restarting && !cancel.is_cancelled() => {
                    tracing::info!(id, "the speed limits changed; restarting yt-dlp with the new limit");
                    continue;
                }
                other => return other,
            }
        }
    }

    async fn run_watched(
        &self,
        id: u64,
        work: Work,
        progress: &Arc<Progress>,
        cancel: &CancellationToken,
        mut limits_changed: impl FnMut(&Self) -> bool,
    ) -> Result<media_engine::Outcome, media_engine::MediaError> {
        let http = self.media_client();
        let (p, c) = (progress.clone(), cancel.clone());
        let mut run = Box::pin(async move {
            match work {
                Work::Stream(spec, _) => media_engine::download(&http, spec, p, c).await,
                Work::Site(tool, spec, _) => media_engine::download_site(&tool, spec, p, c).await,
            }
        });
        let mut tick = tokio::time::interval(Duration::from_millis(400));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let (mut last_bytes, mut last_at, mut speed) = (None::<u64>, Instant::now(), 0f64);
        // The limits are looked at every 2 s (5 ticks), not on every tick.
        let mut ticks = 0u32;
        loop {
            tokio::select! {
                r = &mut run => break r,
                _ = tick.tick() => {
                    ticks = ticks.wrapping_add(1);
                    if ticks.is_multiple_of(5) && limits_changed(self) {
                        continue;
                    }
                    let bytes = progress.bytes.load(std::sync::atomic::Ordering::Relaxed);
                    let now = Instant::now();
                    if let Some(before) = last_bytes {
                        let delta = bytes.saturating_sub(before);
                        // Bytes received (the dashboard's network usage, saved per day).
                        self.count_received(delta);
                        let inst = delta as f64 / now.duration_since(last_at).as_secs_f64().max(1e-3);
                        speed = if speed == 0.0 { inst } else { 0.7 * speed + 0.3 * inst };
                    }
                    (last_bytes, last_at) = (Some(bytes), now);
                    let playable = self.media_preview_source(id).is_some();
                    self.media_update(id, |d| {
                        Self::apply_progress(d, progress);
                        d.notes.retain(|n| !n.starts_with("It stopped"));
                        d.speed_bps = if d.status == MediaStatus::Downloading { speed as u64 } else { 0 };
                        d.eta_secs = match d.percent {
                            Some(pct) if d.speed_bps > 0 && pct > 0.5 && pct < 100.0 => {
                                let total = d.bytes as f64 * 100.0 / pct;
                                Some(((total - d.bytes as f64).max(0.0) / d.speed_bps as f64) as u64)
                            }
                            _ => None,
                        };
                        d.playable = playable;
                    });
                }
            }
        }
    }

    /// Read (or change) one job under the lock.
    fn with_job<T>(&self, id: u64, f: impl FnOnce(&mut Job) -> T) -> Option<T> {
        let mut m = self.inner.media.lock().unwrap();
        m.jobs.iter_mut().find(|j| j.dto.id == id).map(f)
    }

    fn apply_progress(d: &mut MediaJobDto, p: &Progress) {
        use std::sync::atomic::Ordering::Relaxed;
        d.bytes = p.bytes.load(Relaxed);
        d.segments_done = p.segments_done.load(Relaxed);
        d.segments_total = p.segments_total.load(Relaxed);
        d.percent = (d.segments_total > 0).then(|| (d.segments_done as f64 / d.segments_total as f64 * 100.0).min(100.0));
        if d.site {
            // yt-dlp reports a share of the work, not pieces: only the percentage means something.
            d.segments_done = 0;
            d.segments_total = 0;
        }
        if d.status.is_active() {
            d.status = match p.phase() {
                Phase::Preparing => MediaStatus::Preparing,
                Phase::Downloading => MediaStatus::Downloading,
                Phase::Combining | Phase::Done => MediaStatus::Combining,
            };
        }
    }

    /// Change one job and tell the UI, only if something is different.
    fn media_update(&self, id: u64, f: impl FnOnce(&mut MediaJobDto)) {
        let changed = {
            let mut m = self.inner.media.lock().unwrap();
            m.jobs.iter_mut().find(|j| j.dto.id == id).and_then(|j| {
                let before = j.dto.clone();
                f(&mut j.dto);
                track_time(&mut j.dto, now_ms());
                (j.dto != before).then(|| j.dto.clone())
            })
        };
        if let Some(job) = changed {
            self.emit(ManagerEvent::Media { job });
        }
    }

    pub fn list_media(&self) -> Vec<MediaJobDto> {
        self.inner.media.lock().unwrap().jobs.iter().map(|j| j.dto.clone()).collect()
    }

    /// Video jobs for the dashboard: (combined speed, running, paused, bytes held by unfinished jobs).
    pub(super) fn media_live(&self) -> (u64, u64, u64, u64) {
        let m = self.inner.media.lock().unwrap();
        let active = || m.jobs.iter().filter(|j| j.dto.status.is_active());
        let speed = active().map(|j| j.dto.speed_bps).sum();
        let running = active().count() as u64;
        let held = active().map(|j| j.dto.bytes).sum();
        let paused = m.jobs.iter().filter(|j| j.dto.status == MediaStatus::Paused).count() as u64;
        (speed, running, paused, held)
    }

    /// Pause a running video job: it stops now and keeps what it downloaded; Resume continues from there. A queued
    /// one is set aside (it does not start on its own any more).
    pub async fn pause_media(&self, id: u64) -> Result<(), ManagerError> {
        enum Act {
            Stop(CancellationToken),
            Aside(Box<MediaJobDto>),
        }
        let act = self
            .with_job(id, |j| {
                if j.dto.status == MediaStatus::Queued {
                    j.dto.status = MediaStatus::Paused;
                    j.dto.resumable = true;
                    return Ok(Act::Aside(Box::new(j.dto.clone())));
                }
                if !j.dto.status.is_active() {
                    return Err(ManagerError::invalid("only a running video download can be paused"));
                }
                j.pausing = true;
                Ok(Act::Stop(j.cancel.clone()))
            })
            .ok_or_else(|| ManagerError::invalid("no such video download"))??;
        match act {
            Act::Stop(token) => token.cancel(),
            Act::Aside(job) => self.emit(ManagerEvent::Media { job: *job }),
        }
        Ok(())
    }

    /// Smart throttle's pause policy (a game starts): pause the running video jobs and remember them. Waits until they
    /// have stopped (at most 15 s each), so letting go right after finds them paused.
    pub(super) async fn hold_media_for_game(&self) {
        let ids: Vec<u64> = {
            let mut m = self.inner.media.lock().unwrap();
            m.held = true; // queued videos wait too
            m.jobs.iter().filter(|j| j.dto.status.is_active()).map(|j| j.dto.id).collect()
        };
        let mut paused = Vec::new();
        for id in ids {
            if self.pause_media(id).await.is_ok() {
                paused.push(id);
            }
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(15) && self.media_statuses(&paused).iter().any(|s| s.is_some_and(MediaStatus::is_active)) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut m = self.inner.media.lock().unwrap();
        for id in paused {
            if !m.game_held.contains(&id) {
                m.game_held.push(id);
            }
        }
    }

    /// The game closed: resume the video jobs the pause policy paused (only those still paused).
    pub(super) async fn release_media_for_game(&self) {
        let ids = {
            let mut m = self.inner.media.lock().unwrap();
            m.held = false;
            std::mem::take(&mut m.game_held)
        };
        for (id, status) in ids.iter().zip(self.media_statuses(&ids)) {
            if status == Some(MediaStatus::Paused) {
                // All back in line first, then started in order: those it paused before the ones that were waiting.
                if let Err(e) = self.requeue_media(*id) {
                    tracing::warn!(id, error = %e, "could not resume a video download after the game");
                }
            }
        }
        self.start_queued();
    }

    fn media_statuses(&self, ids: &[u64]) -> Vec<Option<MediaStatus>> {
        let m = self.inner.media.lock().unwrap();
        ids.iter().map(|id| m.jobs.iter().find(|j| j.dto.id == *id).map(|j| j.dto.status)).collect()
    }

    /// Resume a paused video job, or try a failed one again, from what is already downloaded. The user's choice: a
    /// job the pause policy paused is theirs from now on.
    pub async fn resume_media(&self, id: u64) -> Result<MediaJobDto, ManagerError> {
        self.inner.media.lock().unwrap().game_held.retain(|h| *h != id);
        self.resume_media_inner(id).await
    }

    /// Back in the queue: it starts at once when there is a free place, else in its turn.
    async fn resume_media_inner(&self, id: u64) -> Result<MediaJobDto, ManagerError> {
        let dto = self.requeue_media(id)?;
        self.start_queued();
        Ok(self.with_job(id, |j| j.dto.clone()).unwrap_or(dto))
    }

    /// A paused or failed job back in the queue (not started here).
    fn requeue_media(&self, id: u64) -> Result<MediaJobDto, ManagerError> {
        let dto = {
            let mut m = self.inner.media.lock().unwrap();
            let j = m.jobs.iter_mut().find(|j| j.dto.id == id).ok_or_else(|| ManagerError::invalid("no such video download"))?;
            if j.dto.status.is_active() || j.dto.status == MediaStatus::Queued {
                return Ok(j.dto.clone());
            }
            if !j.dto.resumable || j.work.is_none() {
                return Err(ManagerError::invalid("this video download cannot be continued; start it again"));
            }
            j.pausing = false;
            j.dto.status = MediaStatus::Queued;
            j.dto.error = None;
            j.dto.error_code = None;
            j.dto.notes.clear();
            j.dto.resumable = false;
            j.dto.clone()
        };
        self.emit(ManagerEvent::Media { job: dto.clone() });
        Ok(dto)
    }

    /// Stop a video job for good: its partial files are removed. Finished jobs are unaffected.
    pub async fn cancel_media(&self, id: u64) -> Result<(), ManagerError> {
        enum Act {
            Stop(CancellationToken),
            Drop(Box<MediaJobDto>),
            Nothing,
        }
        let act = self
            .with_job(id, |j| {
                if j.dto.status.is_active() {
                    j.pausing = false;
                    Act::Stop(j.cancel.clone())
                } else if matches!(j.dto.status, MediaStatus::Paused | MediaStatus::Failed | MediaStatus::Queued) {
                    remove_work_dir(j);
                    j.work = None;
                    j.work_dir = None;
                    j.dto.status = MediaStatus::Cancelled;
                    j.dto.resumable = false;
                    j.dto.playable = false;
                    track_time(&mut j.dto, now_ms());
                    Act::Drop(Box::new(j.dto.clone()))
                } else {
                    Act::Nothing
                }
            })
            .ok_or_else(|| ManagerError::invalid("no such video download"))?;
        match act {
            Act::Stop(t) => t.cancel(),
            Act::Drop(job) => self.emit(ManagerEvent::Media { job: *job }),
            Act::Nothing => {}
        }
        Ok(())
    }

    /// Remove a finished job from the list (a paused or failed job's pieces go). The saved files stay, unless
    /// `delete_files` is set: then they are deleted too, but only those inside a download folder.
    pub async fn clear_media(&self, id: u64, delete_files: bool) -> Result<(), ManagerError> {
        let mut m = self.inner.media.lock().unwrap();
        match m.jobs.iter().position(|j| j.dto.id == id) {
            Some(i) if !m.jobs[i].dto.status.is_active() => {
                let old = m.jobs.remove(i);
                remove_work_dir(&old);
                drop(m);
                if delete_files && old.dto.status == MediaStatus::Completed {
                    for f in old.saved.iter().filter(|f| self.inner.roots.check(f).is_ok()) {
                        let _ = std::fs::remove_file(f);
                    }
                }
                self.emit(ManagerEvent::MediaRemoved { id });
                Ok(())
            }
            Some(_) => Err(ManagerError::invalid("cancel it first")),
            None => Err(ManagerError::invalid("no such video download")),
        }
    }

    /// The saved file of a finished job, else the folder it is saved in (for "Show in folder" / "Open").
    pub(super) fn media_target(&self, id: u64) -> Result<(PathBuf, bool), ManagerError> {
        let m = self.inner.media.lock().unwrap();
        let j = m.jobs.iter().find(|j| j.dto.id == id).ok_or_else(|| ManagerError::invalid("no such video download"))?;
        Ok(match j.saved.first() {
            Some(f) if j.dto.status == MediaStatus::Completed => (f.clone(), true),
            _ => (PathBuf::from(&j.dto.folder), false),
        })
    }

    /// What a player can be given for a job right now: the finished file (whole), or the video being written
    /// (`(path, known size or None, still growing)`).
    pub(super) fn media_preview_source(&self, id: u64) -> Option<(PathBuf, Option<u64>, bool)> {
        let (saved, work_dir, active, part_total) = {
            let m = self.inner.media.lock().unwrap();
            let j = m.jobs.iter().find(|j| j.dto.id == id)?;
            let part_total = j.progress.part_total.load(std::sync::atomic::Ordering::Relaxed);
            (
                (j.dto.status == MediaStatus::Completed).then(|| j.saved.first().cloned()).flatten(),
                j.work_dir.clone(),
                j.dto.status.is_active(),
                part_total,
            )
        };
        if let Some(f) = saved {
            let len = std::fs::metadata(&f).ok()?.len();
            return Some((f, Some(len), false));
        }
        let (path, part_file) = growing_video(&work_dir?)?;
        // yt-dlp marks an unfinished file with `.part`; a stream's video file grows until the job ends.
        let yt_part = path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("media."));
        let done = if yt_part { !part_file } else { !active };
        let len = std::fs::metadata(&path).ok()?.len();
        let total = if done { Some(len) } else { (part_total >= len && part_total > 0).then_some(part_total) };
        Some((path, total, active && !done))
    }

    /// App is closing: stop every video job; their pieces are removed (jobs are not kept across a restart).
    pub(super) async fn stop_media(&self) {
        let tokens: Vec<_> =
            self.inner.media.lock().unwrap().jobs.iter().filter(|j| j.dto.status.is_active()).map(|j| j.cancel.clone()).collect();
        for t in &tokens {
            t.cancel();
        }
        if !tokens.is_empty() {
            let t = Instant::now();
            while self.media_busy() && t.elapsed() < Duration::from_secs(5) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        for j in self.inner.media.lock().unwrap().jobs.iter() {
            remove_work_dir(j);
        }
    }
}

/// How many times a job runs before it is shown as failed.
const TRIES: u32 = 3;

/// Limit checks in a row (2 s apart) that must find the limit changed before yt-dlp is restarted: a cap that
/// moves for a moment, or steps back up, does not stop the video.
const RESTART_CONFIRM: u32 = 2;

/// Whether yt-dlp, started with the limit `started`, must be restarted for the limit `now`: one appears or goes,
/// or it is at least twice (or half) what it was. Smaller moves (Smart throttle easing back up by steps) are
/// not worth the seconds a restart costs.
fn limit_needs_restart(started: Option<u64>, now: Option<u64>) -> bool {
    match (started, now) {
        (None, None) => false,
        (Some(a), Some(b)) => a.max(b) >= a.min(b).saturating_mul(2),
        _ => true,
    }
}

/// A failure that may well not happen again (the network, the site, yt-dlp): try again. DRM, live streams,
/// sign-in pages, limits and refused links stay failed.
fn worth_retrying(e: &media_engine::MediaError) -> bool {
    use media_engine::MediaError as E;
    match e {
        E::Http(_) | E::Io(_) | E::Remux(_) => true,
        E::Site(m) => !m.to_ascii_lowercase().contains("internal error"),
        E::Invalid(m) => !m.contains("internal error") && !m.contains("unknown"),
        E::Unsupported(_) | E::Limit(_) | E::Cancelled => false,
    }
}

/// yt-dlp's errors when the site changed under it (a newer yt-dlp usually fixes them).
fn looks_outdated(e: &media_engine::MediaError) -> bool {
    let m = e.to_string().to_ascii_lowercase();
    ["requested format is not available", "nsig", "signature", "unable to extract", "http error 403", "player response"]
        .iter()
        .any(|k| m.contains(k))
}

/// Remove a job's work folder (only ever a `.velox-media-` folder the job created).
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Keep the job's clock right after any change: a run starts when it becomes active, and its time is added
/// (and the stop time noted) when it stops. Paused time does not count.
fn track_time(d: &mut MediaJobDto, now: u64) {
    match (d.status.is_active(), d.running_since_ms) {
        (true, None) => {
            d.running_since_ms = Some(now);
            d.ended_at_ms = None;
        }
        (false, Some(since)) => {
            d.active_ms += now.saturating_sub(since);
            d.running_since_ms = None;
            d.ended_at_ms = Some(now);
        }
        _ => {}
    }
}

fn remove_work_dir(j: &Job) {
    if j.dto.status == MediaStatus::Completed {
        return; // the engine removed it already
    }
    if let Some(w) = &j.work_dir {
        if w.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".velox-media-")) {
            let _ = std::fs::remove_dir_all(w);
        }
    }
}

/// The video file being written in a work folder, and whether it still has yt-dlp's `.part` suffix: yt-dlp's
/// video part (`media.f<id>.<ext>`; the picture comes before the sound), or a stream's `video.<ext>`.
fn growing_video(dir: &Path) -> Option<(PathBuf, bool)> {
    let mut best: Option<(PathBuf, bool, u64)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let (base, part_file) = match name.strip_suffix(".part") {
            Some(b) => (b.to_string(), true),
            None => (name.clone(), false),
        };
        let video = (base.starts_with("media.") || base.starts_with("video.") || base.starts_with("combined."))
            && super::preview::media_type(&base).is_some_and(|t| t.starts_with("video/") && t != "video/mp2t");
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
        if !video || !meta.is_file() || meta.len() == 0 {
            continue;
        }
        // The first video part (the largest when there are several): it is the picture.
        if best.as_ref().is_none_or(|(_, _, n)| meta.len() > *n) {
            best = Some((e.path(), part_file, meta.len()));
        }
    }
    best.map(|(p, d, _)| (p, d))
}

#[cfg(test)]
mod restart_tests {
    use super::limit_needs_restart;

    #[test]
    fn video_failures_get_a_stable_code_for_the_ui() {
        use media_engine::MediaError as E;
        assert_eq!(
            super::media_error_code(&E::Unsupported("this stream is protected (DRM). Protected streams are not supported".into())),
            "media_drm"
        );
        assert_eq!(
            super::media_error_code(&E::Unsupported("this is a live stream (it has no end), which is not supported".into())),
            "media_live"
        );
        assert_eq!(
            super::media_error_code(&E::Site("this link is a playlist or a channel; open one video and try again".into())),
            "media_playlist"
        );
        assert_eq!(
            super::media_error_code(&E::Site("the site wants you to sign in to see this video: members only".into())),
            "media_sign_in"
        );
        assert_eq!(super::media_error_code(&E::Site("no downloadable video was found on this page".into())), "media_no_video");
        assert_eq!(super::media_error_code(&E::Site("something else entirely".into())), "media_site");
        assert_eq!(super::media_error_code(&E::Remux("x".into())), "media_remux");
        assert_eq!(super::media_error_code(&E::Io(std::io::Error::other("No space left on device (os error 28)"))), "disk_full");
        assert_eq!(super::media_error_code(&E::Io(std::io::Error::other("strange"))), "media_file");
    }

    #[test]
    fn only_a_real_change_of_limit_restarts_a_video_job() {
        let mb = 1024 * 1024;
        assert!(!limit_needs_restart(None, None));
        assert!(limit_needs_restart(None, Some(mb)), "a cap appears");
        assert!(limit_needs_restart(Some(mb), None), "the cap goes");
        assert!(!limit_needs_restart(Some(2 * mb), Some(3 * mb)), "a step up of 1.5x");
        assert!(!limit_needs_restart(Some(4 * mb), Some(3 * mb)));
        assert!(limit_needs_restart(Some(mb), Some(2 * mb)));
        assert!(limit_needs_restart(Some(10 * mb), Some(mb)));
    }
}

#[cfg(test)]
mod clock_tests {
    use super::{track_time, MediaJobDto, MediaStatus};

    fn job() -> MediaJobDto {
        MediaJobDto {
            id: 1,
            title: "v".into(),
            host: "h".into(),
            status: MediaStatus::Preparing,
            bytes: 0,
            segments_done: 0,
            segments_total: 0,
            percent: None,
            folder: String::new(),
            files: Vec::new(),
            notes: Vec::new(),
            error: None,
            error_code: None,
            site: true,
            speed_bps: 0,
            eta_secs: None,
            resumable: false,
            playable: false,
            url: "https://example.com/watch".into(),
            quality: None,
            format: None,
            audio_only: false,
            subtitles: 0,
            started_at_ms: 1_000,
            ended_at_ms: None,
            active_ms: 0,
            running_since_ms: Some(1_000),
            file_size: None,
            retries: 0,
            playlist: None,
        }
    }

    #[test]
    fn the_details_clock_counts_running_time_only() {
        let mut d = job();
        d.status = MediaStatus::Downloading;
        track_time(&mut d, 3_000);
        assert_eq!((d.running_since_ms, d.active_ms, d.ended_at_ms), (Some(1_000), 0, None), "still the same run");
        d.status = MediaStatus::Paused;
        track_time(&mut d, 5_000);
        assert_eq!((d.running_since_ms, d.active_ms, d.ended_at_ms), (None, 4_000, Some(5_000)));
        track_time(&mut d, 9_000);
        assert_eq!(d.active_ms, 4_000, "paused time does not count");
        d.status = MediaStatus::Preparing; // Resume
        track_time(&mut d, 20_000);
        assert_eq!((d.running_since_ms, d.ended_at_ms), (Some(20_000), None));
        d.status = MediaStatus::Completed;
        track_time(&mut d, 26_000);
        assert_eq!((d.active_ms, d.ended_at_ms, d.running_since_ms), (10_000, Some(26_000), None));
        d.status = MediaStatus::Failed;
        track_time(&mut d, 99_000);
        assert_eq!(d.ended_at_ms, Some(26_000), "an end is noted once");
    }
}
