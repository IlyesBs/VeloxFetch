//! Updates of the video tools: offered, never fetched by themselves.
//!
//! The tools come with VeloxFetch (the installer's copies, `tools.rs`): nothing is downloaded to install them, and
//! VeloxFetch never replaces one on its own (on mobile data, that cost the user without asking). In the background,
//! when "check for updates automatically" is on (Settings > Updates, `update.auto`), about once a day, a few small
//! files tell whether a newer version was published:
//!
//! * yt-dlp: its published checksum list against the installed program (sites change; the one that matters);
//! * Deno: its latest version, when the first two numbers changed (patch releases are not offered);
//! * ffmpeg: never offered (joining and converting do not depend on the sites); a new VeloxFetch brings a new one.
//!
//! A video page failing the way an old yt-dlp fails asks for a check too (at most every 15 minutes). An update found
//! is shown with its download size, on every screen and in Settings; the user accepts it (it is downloaded and
//! checked like on first use, `install`) or declines it (that version is not offered again; a newer one is).

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::tools::{deno_asset, join, parse_checksum, read_records, sha256_file, to_hex, version_numbers, version_ok, ytdlp_asset, Record, Tool};
use super::DownloadManager;
use crate::api::{ManagerEvent, ToolsUpdateDto, VideoToolsDto};

/// What the user declined, by tool: `{"ytdlp": "<the published SHA-256>", "deno": "2.6.0"}`.
const DECLINED_KEY: &str = "tools.declined";
/// The first background check, after start-up.
const FIRST_CHECK: Duration = Duration::from_secs(120);
/// Then about once a day.
const EVERY: Duration = Duration::from_secs(24 * 3600);
/// A check asked for by a failing video page comes at least this long after the last one.
const AGAIN_AFTER: Duration = Duration::from_secs(15 * 60);

/// An update found and not answered yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Offer {
    pub tool: Tool,
    /// What is offered: the published SHA-256 (yt-dlp) or the version (Deno). A declined one is not offered again.
    pub ident: String,
    pub version: Option<String>,
    /// The download's size in bytes (0: the server did not say).
    pub size: u64,
}

/// The version in a GitHub release download address: `.../releases/download/<version>/<asset>`.
pub(crate) fn release_version(final_url: &str, asset: &str) -> Option<String> {
    let url = url::Url::parse(final_url).ok()?;
    let parts: Vec<&str> = url.path_segments()?.collect();
    match parts.as_slice() {
        [.., "download", version, last] if *last == asset && version_numbers(version).is_some() => Some(version.to_string()),
        _ => None,
    }
}

/// `latest` is newer than `have` in its first two numbers (2.6.0 over 2.5.4; not 2.5.5 over 2.5.4).
pub(crate) fn newer_minor(latest: &str, have: &str) -> bool {
    match (version_numbers(latest), version_numbers(have)) {
        (Some(l), Some(h)) => l.iter().take(2).cmp(h.iter().take(2)).is_gt(),
        _ => false,
    }
}

impl DownloadManager {
    /// This copy can look for updates at all (it knows the tools' servers).
    fn offers_possible(&self) -> bool {
        let s = &self.inner.cfg.tools;
        (s.ytdlp.is_some() && ytdlp_asset().is_some()) || (s.deno.is_some() && deno_asset().is_some())
    }

    /// Check now and then, in the background, when the user wants it (the same switch as VeloxFetch's own updates).
    /// Only a check: nothing is downloaded before the user accepts.
    pub(super) fn spawn_tool_update_checks(&self) {
        if !self.offers_possible() {
            return;
        }
        let mgr = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_CHECK).await;
            loop {
                if mgr.inner.shutting_down.load(Ordering::Relaxed) {
                    return;
                }
                if mgr.update_auto() {
                    mgr.check_video_tools_updates().await;
                }
                tokio::time::sleep(EVERY).await;
            }
        });
    }

    /// A video page failed the way an old yt-dlp fails: look for its update now, in the background (not more often
    /// than every 15 minutes, and only when the user lets VeloxFetch check for updates).
    pub(crate) fn check_tool_updates_soon(&self) {
        if !self.offers_possible() || !self.update_auto() {
            return;
        }
        if self.inner.tools.last_check.lock().unwrap_or_else(|p| p.into_inner()).is_some_and(|t| t.elapsed() < AGAIN_AFTER) {
            return;
        }
        let mgr = self.clone();
        tokio::spawn(async move {
            mgr.check_video_tools_updates().await;
        });
    }

    /// Look for updates of the installed tools now (small files only). What is found is offered, minus what the user
    /// declined; the UI hears of a change.
    pub async fn check_video_tools_updates(&self) -> VideoToolsDto {
        *self.inner.tools.last_check.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
        let declined = self.declined_updates();
        let mut found = Vec::new();
        for (tool, result) in [(Tool::YtDlp, self.ytdlp_offer().await), (Tool::Deno, self.deno_offer().await)] {
            match result {
                Ok(Some(o)) if declined.get(tool.key()) != Some(&o.ident) => found.push(o),
                Ok(_) => {}
                // Offline, a server down: nothing is offered this time.
                Err(e) => tracing::info!(tool = tool.label(), error = %e, "could not look for a video tool update"),
            }
        }
        let changed = {
            let mut offers = self.inner.tools.offers.lock().unwrap_or_else(|p| p.into_inner());
            let changed = *offers != found;
            *offers = found;
            changed
        };
        if changed {
            self.emit_video_tools();
        }
        self.video_tools()
    }

    /// The user's answer to the update offered: download and install it, or not (that version is then not offered
    /// again). Returns at once; installing goes on in the background and the UI hears when it ends.
    pub async fn answer_video_tools_update(&self, accept: bool) -> VideoToolsDto {
        let offers = std::mem::take(&mut *self.inner.tools.offers.lock().unwrap_or_else(|p| p.into_inner()));
        if accept {
            for offer in offers {
                // Shown as being fetched from now on (the task may wait for another install of this tool).
                self.inner.tools.installing[offer.tool.index()].store(true, Ordering::SeqCst);
                let mgr = self.clone();
                tokio::spawn(async move {
                    mgr.apply_offer(offer).await;
                    mgr.emit_video_tools();
                });
            }
        } else if !offers.is_empty() {
            let mut declined = self.declined_updates();
            for o in &offers {
                declined.insert(o.tool.key().to_string(), o.ident.clone());
            }
            if let Ok(text) = serde_json::to_string(&declined) {
                if let Err(e) = self.inner.store.set_setting(DECLINED_KEY, &text) {
                    tracing::warn!(error = %e, "could not save the declined video tool update");
                }
            }
        }
        self.emit_video_tools();
        self.video_tools()
    }

    /// The update offered, for the UI.
    pub(crate) fn tools_offer(&self) -> Option<ToolsUpdateDto> {
        let offers = self.inner.tools.offers.lock().unwrap_or_else(|p| p.into_inner());
        if offers.is_empty() {
            return None;
        }
        // The total only when every size is known.
        let size = if offers.iter().all(|o| o.size > 0) { offers.iter().map(|o| o.size).sum() } else { 0 };
        Some(ToolsUpdateDto { tools: offers.iter().map(|o| o.tool.key().to_string()).collect(), size })
    }

    fn emit_video_tools(&self) {
        self.emit(ManagerEvent::VideoTools { tools: self.video_tools() });
    }

    fn declined_updates(&self) -> BTreeMap<String, String> {
        self.inner.store.get_setting(DECLINED_KEY).ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Download the accepted update (the same checks as on first use: fixed source, published checksum).
    async fn apply_offer(&self, offer: Offer) {
        let tool = offer.tool;
        let _one_at_a_time = self.inner.tools.locks[tool.index()].lock().await;
        self.inner.tools.installing[tool.index()].store(true, Ordering::SeqCst);
        tracing::info!(tool = tool.label(), version = offer.version.as_deref().unwrap_or("?"), "installing a video tool update");
        let result = self.install(tool).await;
        self.inner.tools.installing[tool.index()].store(false, Ordering::SeqCst);
        let error = match result {
            Ok(path) => {
                // Note the version the offer named (the download itself does not say yt-dlp's).
                if let (Some(version), Some(dir)) = (offer.version.clone(), path.parent()) {
                    let placed = path.clone();
                    if let Ok(Ok(sha)) = tokio::task::spawn_blocking(move || sha256_file(&placed)).await {
                        self.write_record(dir, tool, Record { version: Some(version), sha256: to_hex(&sha), from: "online".into() });
                    }
                }
                None
            }
            Err(e) => {
                tracing::warn!(tool = tool.label(), error = %e, "could not install the video tool update");
                Some(e)
            }
        };
        self.inner.tools.last_error.lock().unwrap_or_else(|p| p.into_inner())[tool.index()] = error;
    }

    /// yt-dlp: the published checksum of the newest build against the installed program.
    async fn ytdlp_offer(&self) -> Result<Option<Offer>, String> {
        let (Some(base), Some(asset)) = (self.inner.cfg.tools.ytdlp.as_deref(), ytdlp_asset()) else {
            return Ok(None);
        };
        // A missing one is fetched on first need, not "updated"; one being installed is not checked.
        let Some(installed) = self.installed_tool(Tool::YtDlp).filter(|_| !self.tool_installing(Tool::YtDlp)) else {
            return Ok(None);
        };
        let sums = self.fetch_text(&join(base, "SHA2-256SUMS")?).await?;
        let latest = parse_checksum(&String::from_utf8_lossy(&sums), Some(asset)).ok_or("no checksum published for the file")?;
        let have = tokio::task::spawn_blocking(move || sha256_file(&installed)).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        if have == latest {
            return Ok(None);
        }
        let info = self.media_client().probe(&join(base, asset)?).await.ok();
        let version = info.as_ref().and_then(|i| release_version(&i.final_url, asset));
        let size = info.and_then(|i| i.content_length).unwrap_or(0);
        Ok(Some(Offer { tool: Tool::YtDlp, ident: to_hex(&latest), version, size }))
    }

    /// Deno: its latest version against the installed one's (when `versions.json` knows it).
    async fn deno_offer(&self) -> Result<Option<Offer>, String> {
        let (Some(base), Some(asset)) = (self.inner.cfg.tools.deno.as_deref(), deno_asset()) else {
            return Ok(None);
        };
        let Some(installed) = self.installed_tool(Tool::Deno).filter(|_| !self.tool_installing(Tool::Deno)) else {
            return Ok(None);
        };
        let Some(have) = installed.parent().and_then(|d| read_records(d).remove(Tool::Deno.key())).and_then(|r| r.version) else {
            return Ok(None); // which version it is is not known: nothing to compare
        };
        let latest = String::from_utf8_lossy(&self.fetch_text(&join(base, "release-latest.txt")?).await?).trim().to_string();
        if !version_ok(&latest) {
            return Err("unexpected Deno version".into());
        }
        if !newer_minor(&latest, &have) {
            return Ok(None);
        }
        let size = match self.media_client().probe(&join(base, &format!("release/{latest}/{asset}"))?).await {
            Ok(info) => info.content_length.unwrap_or(0),
            Err(_) => 0,
        };
        let version = latest.trim_start_matches('v').to_string();
        Ok(Some(Offer { tool: Tool::Deno, ident: version.clone(), version: Some(version), size }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_come_from_release_addresses_only() {
        let u = "https://github.com/yt-dlp/yt-dlp/releases/download/2025.10.22/yt-dlp.exe";
        assert_eq!(release_version(u, "yt-dlp.exe").as_deref(), Some("2025.10.22"));
        assert_eq!(release_version(u, "yt-dlp_linux"), None, "another file");
        assert_eq!(release_version("https://github.com/yt-dlp/yt-dlp/releases/download/latest/yt-dlp.exe", "yt-dlp.exe"), None);
        assert_eq!(release_version("http://127.0.0.1:9/yt/yt-dlp.exe", "yt-dlp.exe"), None);
        assert_eq!(release_version("not a link", "yt-dlp.exe"), None);
    }

    #[test]
    fn deno_is_offered_for_a_new_minor_version_only() {
        assert!(newer_minor("v2.6.0", "2.5.4"));
        assert!(newer_minor("v3.0.0", "2.9.9"));
        assert!(!newer_minor("v2.5.9", "2.5.4"), "a patch release");
        assert!(!newer_minor("v2.5.4", "2.5.4"));
        assert!(!newer_minor("v2.4.0", "2.5.4"), "older");
        assert!(!newer_minor("vX", "2.5.4"));
    }
}
