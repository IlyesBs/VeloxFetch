//! Live preview: watch a video (or listen to audio) while it is still downloading.
//!
//! ```text
//! <video src="velox-preview://localhost/12">  --Range: bytes=a-b-->  src-tauri/src/preview_server.rs
//!      --> read_preview(12, "bytes=a-b") --> only bytes that are already on disk --> 206 Partial Content
//! ```
//!
//! * Only ids come in; the file is the download's own (`.part` while downloading, the saved file once
//!   finished), and it must lie inside a download folder VeloxFetch knows.
//! * Only audio and video files, always sent with their media type and `nosniff`: a downloaded web page
//!   or program can never be loaded through this.
//! * Only bytes that are really written are served: a segment's last [`SAFETY_MARGIN`] bytes are held back
//!   until it is complete (the writer may still have them in flight). A player asking further ahead waits
//!   a little for them; it never gets the zeros of a file that is not written yet.
//! * Preview mode ([`DownloadManager::set_preview_mode`]) makes the engine fetch the start and the end of
//!   the file first (players read both before showing anything), then everything in order.
//! * Anything wrong (no such download, not a video, nothing there yet) is an HTTP status for the player,
//!   never a panic and never a change to the download itself.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

use downloader_core::FileKind;

use super::*;

/// The most one answer carries; the player simply asks for the next part.
pub const MAX_CHUNK: u64 = 2 * 1024 * 1024;
/// Bytes at the end of an unfinished segment that are not served yet.
pub const SAFETY_MARGIN: u64 = 512 * 1024;
/// How long a request for bytes that are about to arrive waits for them.
const WAIT_FOR_DATA: Duration = Duration::from_secs(20);

#[derive(Default)]
pub(super) struct PreviewState {
    wanted: HashSet<i64>,
}

/// One answer for the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewResponse {
    /// 206, or an error status (404, 415, 416, 503).
    pub status: u16,
    pub content_type: String,
    /// `Content-Range` (for 206 and 416).
    pub content_range: Option<String>,
    pub body: Vec<u8>,
}

impl PreviewResponse {
    fn error(status: u16) -> Self {
        Self { status, content_type: "text/plain".into(), content_range: None, body: Vec::new() }
    }
}

/// A requested byte range: `bytes=a-`, `bytes=a-b` or `bytes=-n` (the last `n` bytes). Only the first range of
/// a list is used. `None` = no (usable) Range header: the start of the file.
pub fn parse_range(header: Option<&str>) -> Option<(Option<u64>, Option<u64>)> {
    let spec = header?.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    let num = |s: &str| if s.trim().is_empty() { Ok(None) } else { s.trim().parse::<u64>().map(Some) };
    let (a, b) = (num(a).ok()?, num(b).ok()?);
    match (a, b) {
        (None, None) => None,
        (Some(x), Some(y)) if y < x => None,
        r => Some(r),
    }
}

/// Audio / video types the preview serves, by file extension. Anything else is refused.
pub fn media_type(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "ts" => "video/mp2t",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        _ => return None,
    })
}

/// Byte runs `[start, end)` that may be served, merged, from the download's segments.
pub fn available_runs(segments: &[downloader_core::Segment]) -> Vec<(u64, u64)> {
    let mut runs: Vec<(u64, u64)> = segments
        .iter()
        .filter_map(|s| {
            let len = s.end - s.start + 1;
            let ready = if s.downloaded >= len { len } else { s.downloaded.saturating_sub(SAFETY_MARGIN) };
            (ready > 0).then_some((s.start, s.start + ready))
        })
        .collect();
    runs.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(runs.len());
    for (a, b) in runs {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// The inclusive range to answer with: from `start`, inside one available run, at most [`MAX_CHUNK`].
pub fn serve_window(runs: &[(u64, u64)], start: u64, wanted_end: Option<u64>) -> Option<(u64, u64)> {
    let run = runs.iter().find(|(a, b)| *a <= start && start < *b)?;
    let mut end = run.1 - 1;
    end = end.min(start + MAX_CHUNK - 1);
    if let Some(w) = wanted_end {
        end = end.min(w);
    }
    Some((start, end))
}

/// The start of an answer for a download still being written: a player is sent the headers at once and the bytes
/// as the download writes them, like any web server serving a growing file. (Answering "not there yet" instead made
/// WebKit's player, on Linux and macOS, stop the preview for good.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewHead {
    pub content_type: &'static str,
    pub total: u64,
    /// The download is running: the missing bytes will come.
    pub active: bool,
    /// The first byte asked for.
    pub start: u64,
}

/// What the preview needs to know about a download right now.
struct Source {
    path: PathBuf,
    total: u64,
    content_type: &'static str,
    runs: Vec<(u64, u64)>,
    active: bool,
}

impl DownloadManager {
    /// Is preview order wanted for this download? (Read when its transfer starts.)
    pub(super) fn preview_wanted(&self, id: i64) -> bool {
        self.inner.preview.lock().unwrap().wanted.contains(&id)
    }

    /// Turn preview order on or off. Turning it on for a download that is running restarts its transfer
    /// (nothing downloaded is lost) so the start and the end come first.
    pub async fn set_preview_mode(&self, id: i64, enabled: bool) -> Result<PreviewDto, ManagerError> {
        let rec = self.load(id).await?;
        // Before the server answered, the name is not known yet: the check is made again when serving.
        if enabled && !rec.is_unstarted() && media_type(&rec.destination.to_string_lossy()).is_none() {
            return Err(ManagerError::invalid("only video and audio files can be previewed"));
        }
        let changed = {
            let mut p = self.inner.preview.lock().unwrap();
            if enabled {
                p.wanted.insert(id)
            } else {
                p.wanted.remove(&id)
            }
        };
        let running = self.active_handle(id).map(|(c, _)| c.preview() != enabled).unwrap_or(false);
        // Only turning it on restarts (to fetch the end now); turning it off just lets the order apply next time.
        if enabled && changed && running && rec.resumable {
            let _ = self.pause_download(id).await;
            self.resume_download(id).await?;
        }
        match self.preview_info(id).await {
            Err(ManagerError::InvalidInput(_)) if enabled => {
                Ok(PreviewDto { id, content_type: String::new(), total: 0, ready_from_start: 0, preview_mode: true, complete: false })
            }
            other => other,
        }
    }

    /// How much of the file can be played so far.
    pub async fn preview_info(&self, id: i64) -> Result<PreviewDto, ManagerError> {
        let s = self.preview_source(id).await?;
        let playable_from_start = s.runs.first().filter(|r| r.0 == 0).map_or(0, |r| r.1);
        Ok(PreviewDto {
            id,
            content_type: s.content_type.to_string(),
            total: s.total,
            ready_from_start: playable_from_start,
            preview_mode: self.preview_wanted(id),
            complete: !s.active && s.runs == [(0, s.total)],
        })
    }

    async fn preview_source(&self, id: i64) -> Result<Source, ManagerError> {
        let rec = self.load(id).await?;
        let name = rec.destination.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(content_type) = media_type(&name) else {
            return Err(ManagerError::invalid("only video and audio files can be previewed"));
        };
        if downloader_core::classify_file(&name, None) != FileKind::Video && downloader_core::classify_file(&name, None) != FileKind::Audio
        {
            return Err(ManagerError::invalid("only video and audio files can be previewed"));
        }
        let total = rec.total_size.filter(|t| *t > 0).ok_or_else(|| ManagerError::invalid("the size of this file is not known yet"))?;
        let (path, runs, active) = if rec.status == DownloadStatus::Completed {
            (rec.destination.clone(), vec![(0, total)], false)
        } else {
            let live = self.active_handle_progress(id).map(|p| p.snapshot().segments).filter(|s| !s.is_empty());
            let active = self.is_active(id);
            let segments = live.unwrap_or_else(|| rec.segments.clone());
            (downloader_core::part_path(&rec.destination), available_runs(&segments), active)
        };
        if self.inner.roots.check(&path).is_err() {
            return Err(ManagerError::invalid("that file is not in a download folder VeloxFetch knows"));
        }
        Ok(Source { path, total, content_type, runs, active })
    }

    /// What a player needs to start an answer before the bytes it asks for are written (see [`PreviewHead`]).
    /// `None`: not previewable (no such download, not audio / video, size unknown, outside the download folders) or
    /// a start past the end: the ordinary answer ([`read_preview`](Self::read_preview)) then says why.
    pub async fn preview_head(&self, id: i64, range: Option<&str>) -> Option<PreviewHead> {
        let src = self.preview_source(id).await.ok()?;
        let start = match parse_range(range) {
            None | Some((None, None)) => 0,
            Some((Some(a), _)) => a,
            Some((None, Some(n))) => src.total.saturating_sub(n.max(1)),
        };
        (start < src.total).then_some(PreviewHead { content_type: src.content_type, total: src.total, active: src.active, start })
    }

    /// Answer one player request. Never fails: every problem is a status code.
    pub async fn read_preview(&self, id: i64, range: Option<&str>) -> PreviewResponse {
        let wanted = parse_range(range);
        let deadline = Instant::now() + WAIT_FOR_DATA;
        loop {
            let src = match self.preview_source(id).await {
                Ok(s) => s,
                Err(ManagerError::NotFound(_)) => return PreviewResponse::error(404),
                Err(_) => return PreviewResponse::error(415),
            };
            let (start, end) = match wanted {
                None => (0, None),
                Some((Some(a), b)) => (a, b),
                Some((None, Some(n))) => (src.total.saturating_sub(n.max(1)), None), // the last n bytes
                Some((None, None)) => (0, None),
            };
            if start >= src.total {
                let mut r = PreviewResponse::error(416);
                r.content_range = Some(format!("bytes */{}", src.total));
                return r;
            }
            if let Some((a, b)) = serve_window(&src.runs, start, end.map(|e| e.min(src.total - 1))) {
                let path = src.path.clone();
                let read = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
                    let mut f = std::fs::File::open(&path)?;
                    f.seek(SeekFrom::Start(a))?;
                    let mut buf = vec![0u8; (b - a + 1) as usize];
                    f.read_exact(&mut buf)?;
                    Ok(buf)
                })
                .await;
                return match read {
                    Ok(Ok(body)) => PreviewResponse {
                        status: 206,
                        content_type: src.content_type.to_string(),
                        content_range: Some(format!("bytes {a}-{b}/{}", src.total)),
                        body,
                    },
                    // Renamed into place this very moment (finished), or unreadable: look again, then give up.
                    _ if Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    _ => PreviewResponse::error(503),
                };
            }
            // Not there yet: wait for it while the download runs (the player shows "loading").
            if !src.active || Instant::now() >= deadline {
                return PreviewResponse::error(503);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// Bytes at the end of a growing video file (written in order) that are not served yet.
const GROWING_MARGIN: u64 = 64 * 1024;

impl DownloadManager {
    /// Answer one player request for a video job (YouTube..., a stream): the finished file, or the video part
    /// being written (only bytes already on disk; the player waits for the rest). Never fails: a status code.
    pub async fn read_media_preview(&self, id: u64, range: Option<&str>) -> PreviewResponse {
        let wanted = parse_range(range);
        let deadline = Instant::now() + WAIT_FOR_DATA;
        loop {
            let Some((path, total, growing)) = self.media_preview_source(id) else {
                return PreviewResponse::error(404);
            };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Some(content_type) = media_type(name.strip_suffix(".part").unwrap_or(&name)) else {
                return PreviewResponse::error(415);
            };
            if self.inner.roots.check(&path).is_err() {
                return PreviewResponse::error(415);
            }
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let ready = if growing { len.saturating_sub(GROWING_MARGIN) } else { len };
            let size = total.unwrap_or(0).max(len);
            let (start, end) = match wanted {
                None | Some((None, None)) => (0, None),
                Some((Some(a), b)) => (a, b),
                Some((None, Some(n))) if size > 0 => (size.saturating_sub(n.max(1)), None),
                Some((None, Some(_))) => return PreviewResponse::error(416),
            };
            if total.is_some_and(|t| start >= t) {
                let mut r = PreviewResponse::error(416);
                r.content_range = Some(format!("bytes */{}", total.unwrap_or(0)));
                return r;
            }
            if start < ready {
                let mut b = (ready - 1).min(start + MAX_CHUNK - 1);
                if let Some(w) = end {
                    b = b.min(w);
                }
                let (a, p) = (start, path.clone());
                let read = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
                    let mut f = std::fs::File::open(&p)?;
                    f.seek(SeekFrom::Start(a))?;
                    let mut buf = vec![0u8; (b - a + 1) as usize];
                    f.read_exact(&mut buf)?;
                    Ok(buf)
                })
                .await;
                match read {
                    Ok(Ok(body)) => {
                        return PreviewResponse {
                            status: 206,
                            content_type: content_type.to_string(),
                            content_range: Some(match total {
                                Some(t) => format!("bytes {a}-{b}/{t}"),
                                None => format!("bytes {a}-{b}/*"),
                            }),
                            body,
                        }
                    }
                    // Renamed this very moment (the part finished): look again.
                    _ if Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    _ => return PreviewResponse::error(503),
                }
            }
            if !growing || Instant::now() >= deadline {
                return PreviewResponse::error(503);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use downloader_core::Segment;

    fn seg(id: usize, start: u64, end: u64, downloaded: u64) -> Segment {
        let mut s = Segment::new(id, start, end);
        s.downloaded = downloaded;
        s
    }

    #[test]
    fn ranges_are_parsed_strictly() {
        assert_eq!(parse_range(Some("bytes=0-")), Some((Some(0), None)));
        assert_eq!(parse_range(Some("bytes=100-199")), Some((Some(100), Some(199))));
        assert_eq!(parse_range(Some("bytes=-500")), Some((None, Some(500))));
        assert_eq!(parse_range(Some("bytes=5-9, 20-30")), Some((Some(5), Some(9))), "first range only");
        for bad in ["bytes=9-5", "bytes=-", "items=0-1", "bytes=a-b", "bytes=-1-2", "", "bytes=18446744073709551616-"] {
            assert_eq!(parse_range(Some(bad)), None, "{bad}");
        }
        assert_eq!(parse_range(None), None);
    }

    #[test]
    fn only_audio_and_video_are_served() {
        assert_eq!(media_type("Film.MP4"), Some("video/mp4"));
        assert_eq!(media_type("song.flac"), Some("audio/flac"));
        for no in ["page.html", "setup.exe", "a.svg", "x.pdf", "noext", "x.mp4.exe"] {
            assert_eq!(media_type(no), None, "{no}");
        }
    }

    #[test]
    fn unwritten_bytes_are_never_served() {
        const M: u64 = 1024 * 1024;
        let segs =
            [seg(0, 0, 4 * M - 1, 4 * M), seg(1, 4 * M, 8 * M - 1, 3 * M), seg(2, 8 * M, 12 * M - 1, 0), seg(3, 12 * M, 16 * M - 1, 4 * M)];
        let runs = available_runs(&segs);
        // Segment 0 complete and 1 partly (minus the margin) join up; the complete last segment stands alone.
        assert_eq!(runs, [(0, 4 * M + 3 * M - SAFETY_MARGIN), (12 * M, 16 * M)]);
        assert_eq!(serve_window(&runs, 0, None), Some((0, MAX_CHUNK - 1)), "at most one chunk");
        assert_eq!(serve_window(&runs, 100, Some(199)), Some((100, 199)));
        assert_eq!(serve_window(&runs, 7 * M - SAFETY_MARGIN - 10, None), Some((7 * M - SAFETY_MARGIN - 10, 7 * M - SAFETY_MARGIN - 1)));
        assert_eq!(serve_window(&runs, 9 * M, None), None, "a gap: wait, never zeros");
        assert_eq!(serve_window(&runs, 16 * M - 5, None), Some((16 * M - 5, 16 * M - 1)), "the end, fetched first in preview mode");
        assert!(available_runs(&[seg(0, 0, 99, 50)]).is_empty(), "less than the margin: nothing yet");
    }
}
