//! Browser downloads that wait for the user's confirmation.
//!
//! A handoff from the browser extension does not start anything by itself (unless the user turned
//! confirmation off): it is parked here, the UI shows a dialog with the file's details, and only
//! `confirm_pending` turns it into a real download. Nothing is written to disk while it waits, and
//! the list is in memory only (a request the user never answered is simply gone after a restart).

use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

/// How many unanswered requests are kept (a page cannot make the list grow without bound).
const MAX_PENDING: usize = 50;
const CONFIRM_KEY: &str = "browser.confirm_downloads";
const CLIPBOARD_KEY: &str = "clipboard.watch";
const VIDEO_BAR_KEY: &str = "browser.video_bar";

pub(super) struct PendingEntry {
    pub dto: PendingDto,
    req: NewDownload,
    headers: Vec<(String, String)>,
}

#[derive(Default)]
pub(super) struct PendingList {
    next: u64,
    entries: Vec<PendingEntry>,
}

impl DownloadManager {
    /// Ask the user before browser downloads start (default: yes).
    pub fn browser_confirm(&self) -> bool {
        self.inner.store.get_setting(CONFIRM_KEY).ok().flatten().is_none_or(|v| v != "false")
    }

    pub async fn set_browser_confirm(&self, on: bool) -> Result<(), ManagerError> {
        self.inner.store.set_setting(CONFIRM_KEY, if on { "true" } else { "false" })?;
        Ok(())
    }

    /// Show the "Download video" bar over videos in the browser (default: yes). The extension reads it with
    /// its ping, so the switch in VeloxFetch's settings decides.
    pub fn video_bar(&self) -> bool {
        self.inner.store.get_setting(VIDEO_BAR_KEY).ok().flatten().is_none_or(|v| v != "false")
    }

    pub async fn set_video_bar(&self, on: bool) -> Result<(), ManagerError> {
        self.inner.store.set_setting(VIDEO_BAR_KEY, if on { "true" } else { "false" })?;
        Ok(())
    }

    /// Watch the clipboard for download links (default: no: reading the clipboard is opt-in).
    pub fn clipboard_watch(&self) -> bool {
        self.inner.store.get_setting(CLIPBOARD_KEY).ok().flatten().is_some_and(|v| v == "true")
    }

    pub async fn set_clipboard_watch(&self, on: bool) -> Result<(), ManagerError> {
        self.inner.store.set_setting(CLIPBOARD_KEY, if on { "true" } else { "false" })?;
        Ok(())
    }

    /// Park a browser download until the user decides. Validates everything a real download would
    /// (URL, file name, headers), then looks at the server in the background for size and type.
    pub async fn add_pending(&self, req: NewDownload, ctx: BrowserContext) -> Result<PendingDto, ManagerError> {
        let url = validate::url(&req.url)?;
        let url = crate::links::direct(&url).unwrap_or(url);
        let headers = validate::browser_headers(&ctx)?;
        let requested = req.filename.as_deref().map(validate::filename).transpose()?;
        let guess = requested.clone().unwrap_or_else(|| {
            let s = downloader_core::filename::sanitize_filename(url.path_segments().and_then(|mut s| s.next_back()).unwrap_or(""));
            if s.is_empty() {
                "download".to_string()
            } else {
                s
            }
        });
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let token;
        let dto;
        {
            let mut list = self.inner.pending.lock().unwrap();
            if list.entries.len() >= MAX_PENDING {
                return Err(ManagerError::invalid("too many downloads are waiting for confirmation"));
            }
            list.next += 1;
            token = list.next;
            dto = PendingDto {
                token,
                url: url.to_string(),
                host: url.host_str().unwrap_or("").to_string(),
                filename: guess.clone(),
                size: None,
                content_type: None,
                resumable: None,
                save_dir: self.predicted_dir(&guess, None).to_string_lossy().into_owned(),
                probing: true,
                incognito: ctx.incognito,
                created_at_ms: now_ms,
                media: None,
            };
            list.entries.push(PendingEntry { dto: dto.clone(), req, headers: headers.clone() });
        }
        self.emit(ManagerEvent::Pending { pending: dto.clone() });

        let this = self.clone();
        let named = requested.is_some();
        tokio::spawn(async move {
            let probe = tokio::time::timeout(Duration::from_secs(20), this.inner.downloader.probe_quick(url.as_str(), &headers)).await;
            let updated = {
                let mut list = this.inner.pending.lock().unwrap();
                let Some(e) = list.entries.iter_mut().find(|e| e.dto.token == token) else { return }; // already answered
                e.dto.probing = false;
                if let Ok(Ok(info)) = probe {
                    e.dto.size = info.content_length;
                    e.dto.resumable = Some(info.supports_parallel());
                    if !named {
                        e.dto.filename = downloader_core::filename::choose_filename(&info, &url);
                    }
                    e.dto.content_type = info.content_type;
                }
                e.dto.save_dir = this.predicted_dir(&e.dto.filename, e.dto.content_type.as_deref()).to_string_lossy().into_owned();
                e.dto.clone()
            };
            this.emit(ManagerEvent::Pending { pending: updated });
        });
        Ok(dto)
    }

    /// Park a video the browser handed over (from its "Download video" bar). It waits for the user to
    /// pick a quality in the video dialog; nothing is fetched until then. Refused at once for services
    /// that protect every video with DRM.
    pub async fn add_pending_media(&self, url: &str, ctx: BrowserContext, page: bool) -> Result<PendingDto, ManagerError> {
        let url = validate::url_without_credentials(url)?;
        let headers = validate::browser_headers(&ctx)?;
        if let Some(name) = media_engine::protected_service(&url) {
            return Err(ManagerError::invalid(format!(
                "{name} protects its videos with DRM (copy protection), so they cannot be downloaded"
            )));
        }
        let kind = if page || media_engine::is_site_page(&url) { PendingMedia::Site } else { PendingMedia::Stream };
        if kind == PendingMedia::Site && !media_engine::is_site_page(&url) {
            return Err(ManagerError::invalid("this page is not on a video site VeloxFetch knows"));
        }
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let dto = {
            let mut list = self.inner.pending.lock().unwrap();
            if list.entries.len() >= MAX_PENDING {
                return Err(ManagerError::invalid("too many downloads are waiting for confirmation"));
            }
            list.next += 1;
            let dto = PendingDto {
                token: list.next,
                url: url.to_string(),
                host: url.host_str().unwrap_or("").to_string(),
                filename: String::new(),
                size: None,
                content_type: None,
                resumable: None,
                save_dir: self.predicted_dir("video.mp4", Some("video/mp4")).to_string_lossy().into_owned(),
                probing: false,
                incognito: ctx.incognito,
                created_at_ms: now_ms,
                media: Some(kind),
            };
            let req = NewDownload { url: url.to_string(), ..Default::default() };
            list.entries.push(PendingEntry { dto: dto.clone(), req, headers });
            dto
        };
        self.emit(ManagerEvent::Pending { pending: dto.clone() });
        Ok(dto)
    }

    /// The link and the browser's headers of a waiting video (it stays waiting).
    pub(super) fn pending_media(&self, token: u64) -> Result<(url::Url, Vec<(String, String)>), ManagerError> {
        let list = self.inner.pending.lock().unwrap();
        let e = list
            .entries
            .iter()
            .find(|e| e.dto.token == token && e.dto.media.is_some())
            .ok_or_else(|| ManagerError::invalid("this video is no longer waiting"))?;
        Ok((validate::url(&e.req.url)?, e.headers.clone()))
    }

    pub fn list_pending(&self) -> Vec<PendingDto> {
        self.inner.pending.lock().unwrap().entries.iter().map(|e| e.dto.clone()).collect()
    }

    /// The user said yes: create (and, unless told otherwise, start) the download.
    pub async fn confirm_pending(&self, req: ConfirmPending) -> Result<DownloadDto, ManagerError> {
        let rename = req.filename.as_deref().map(validate::filename).transpose()?;
        if self.inner.pending.lock().unwrap().entries.iter().any(|e| e.dto.token == req.token && e.dto.media.is_some()) {
            return Err(ManagerError::invalid("this is a video: choose its quality in the video dialog"));
        }
        let entry = self.take_pending(req.token)?;
        let mut new = entry.req;
        if let Some(name) = rename {
            new.filename = Some(name);
        } else if new.filename.is_none() && !entry.dto.filename.is_empty() {
            new.filename = Some(entry.dto.filename.clone());
        }
        new.start = Some(req.start.unwrap_or(true));
        if req.folder_token.is_some() {
            new.folder_token = req.folder_token;
        }
        self.create_inner(new, entry.headers).await
    }

    /// The type the server announced for a waiting download (once it has been asked).
    pub(super) fn pending_content_type(&self, token: u64) -> Result<Option<String>, ManagerError> {
        let list = self.inner.pending.lock().unwrap();
        let e =
            list.entries.iter().find(|e| e.dto.token == token).ok_or_else(|| ManagerError::invalid("this request is no longer waiting"))?;
        Ok(e.dto.content_type.clone())
    }

    /// The user said no: forget it.
    pub fn dismiss_pending(&self, token: u64) -> Result<(), ManagerError> {
        self.take_pending(token).map(|_| ())
    }

    pub(super) fn take_pending(&self, token: u64) -> Result<PendingEntry, ManagerError> {
        let entry = {
            let mut list = self.inner.pending.lock().unwrap();
            let i = list
                .entries
                .iter()
                .position(|e| e.dto.token == token)
                .ok_or_else(|| ManagerError::invalid("this request is no longer waiting"))?;
            list.entries.remove(i)
        };
        self.emit(ManagerEvent::PendingRemoved { token });
        Ok(entry)
    }
}
