//! Updates (Phase 19): is there a newer VeloxFetch, fetch it, and hand a verified package to the app to install.
//!
//! ```text
//! fixed HTTPS address --> latest.json + latest.json.sig  (64 KiB / 1 KiB at most)
//!     --> update_verify: signature (a key compiled in), product, version, never older than one seen
//!     --> the package for this kind of installation --> <data>/updates/<name>.part  (its exact size, at most)
//!     --> size + SHA-256 from the signed manifest --> renamed to <name>  ("ready")
//!     --> install: checked again, by the app, right before it is opened (see system-integration `update`)
//! ```
//! Nothing from the network is run, opened or kept under its final name before it matched the signed manifest.
//! The addresses come from the build (`UpdateConfig`), never from the UI or a page. No cookies, no credentials
//! and no identifier are sent: the request is the same for every copy of the app.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use update_verify::{Decision, Package, UpdateError, Version};

use super::DownloadManager;
use crate::api::{ManagerError, ManagerEvent, UpdateConfig, UpdateDto};

const AUTO_KEY: &str = "update.auto";
/// The newest `released` of a manifest accepted so far (a replayed older manifest is refused).
const RELEASED_KEY: &str = "update.released";
const CHECKED_KEY: &str = "update.checked";
/// A server that sends nothing for this long is given up.
const STALL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available,
    /// A newer version exists, but not as a package this installation can use.
    Elsewhere,
    Downloading,
    Ready,
    Failed,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Checking => "checking",
            Phase::UpToDate => "upToDate",
            Phase::Available => "available",
            Phase::Elsewhere => "elsewhere",
            Phase::Downloading => "downloading",
            Phase::Ready => "ready",
            Phase::Failed => "failed",
        }
    }
}

pub(crate) struct UpdateState {
    phase: Phase,
    /// Why the last check, download or install failed (a stable code for the UI's texts).
    error: Option<&'static str>,
    newer: Option<Version>,
    package: Option<Package>,
    ready: Option<PathBuf>,
    checked_at: Option<u64>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self { phase: Phase::Idle, error: None, newer: None, package: None, ready: None, checked_at: None }
    }
}

#[derive(Default)]
pub(crate) struct UpdateRuntime {
    pub(crate) state: std::sync::Mutex<UpdateState>,
    /// One check or download at a time.
    pub(crate) busy: tokio::sync::Mutex<()>,
    got: AtomicU64,
    total: AtomicU64,
}

/// A verified package, ready for the app to install.
#[derive(Debug, Clone)]
pub struct UpdatePlan {
    pub path: PathBuf,
    pub version: String,
    pub size: u64,
    pub sha256: [u8; 32],
    /// `nsis`, `appimage`, `deb`, `rpm` or `dmg`.
    pub kind: String,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// How the UI should word the install button for a kind of installation.
fn install_style(kind: Option<&str>) -> &'static str {
    match kind {
        // The app replaces itself (AppImage) or runs the installer, then starts again.
        Some("appimage") | Some("nsis") => "restart",
        // The system's package installer or disk image is opened; the user finishes there.
        Some("deb") | Some("rpm") | Some("dmg") => "package",
        // A portable copy: only the release page.
        _ => "page",
    }
}

fn kind_of(target: &str) -> &str {
    target.rsplit('-').next().unwrap_or("")
}

impl DownloadManager {
    fn update_cfg(&self) -> Option<&UpdateConfig> {
        self.inner.cfg.update.as_ref().filter(|c| !c.keys.is_empty())
    }

    fn update_state(&self) -> std::sync::MutexGuard<'_, UpdateState> {
        self.inner.update.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn update_dir(&self) -> PathBuf {
        match self.inner.cfg.update.as_ref().and_then(|c| c.dir.clone()) {
            Some(d) => d,
            None => self.inner.cfg.db_path.parent().map(|p| p.join("updates")).unwrap_or_else(|| PathBuf::from("updates")),
        }
    }

    fn update_auto(&self) -> bool {
        self.inner.store.get_setting(AUTO_KEY).ok().flatten().as_deref() != Some("0")
    }

    pub fn update_status(&self) -> UpdateDto {
        let cfg = self.inner.cfg.update.as_ref();
        let s = self.update_state();
        let downloading = s.phase == Phase::Downloading;
        UpdateDto {
            supported: self.update_cfg().is_some(),
            auto_check: self.update_auto(),
            phase: s.phase.name(),
            current: cfg.map(|c| c.current.to_string()).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
            version: s.newer.map(|v| v.to_string()),
            size: s.package.as_ref().map(|p| p.size),
            received: if downloading { self.inner.update.got.load(Ordering::Relaxed) } else { 0 },
            total: if downloading { self.inner.update.total.load(Ordering::Relaxed) } else { 0 },
            checked_at: s.checked_at.or_else(|| self.inner.store.get_setting(CHECKED_KEY).ok().flatten().and_then(|v| v.parse().ok())),
            error: s.error,
            install: install_style(cfg.and_then(|c| c.kind.as_deref())),
            release_page: cfg.map(|c| c.release_page.clone()),
        }
    }

    fn update_changed(&self) {
        self.emit(ManagerEvent::Update { update: self.update_status() });
    }

    fn set_update(&self, f: impl FnOnce(&mut UpdateState)) {
        f(&mut self.update_state());
        self.update_changed();
    }

    pub async fn set_update_auto(&self, enabled: bool) -> Result<UpdateDto, ManagerError> {
        self.inner.store.set_setting(AUTO_KEY, if enabled { "1" } else { "0" })?;
        self.update_changed();
        Ok(self.update_status())
    }

    /// A client for update requests: the user's proxy, no cookies, no extra headers, the plain product name.
    fn update_client(&self, cfg: &UpdateConfig) -> Result<http_client::HttpClient, &'static str> {
        let mut c = self.network().client_config(&self.inner.cfg.client);
        c.cookies = false;
        c.headers.clear();
        c.basic_auth = None;
        c.user_agent = format!("VeloxFetch/{}", cfg.current);
        c.max_redirects = 5;
        http_client::HttpClient::new(&c).map_err(|_| "network")
    }

    fn secure_url(&self, cfg: &UpdateConfig, u: &url::Url) -> bool {
        u.scheme() == "https" || (cfg.allow_loopback_http && u.scheme() == "http" && u.host_str() == Some("127.0.0.1"))
    }

    async fn fetch_small(
        &self,
        cfg: &UpdateConfig,
        client: &http_client::HttpClient,
        url: &url::Url,
        max: usize,
    ) -> Result<Vec<u8>, &'static str> {
        if !self.secure_url(cfg, url) {
            return Err("rejected");
        }
        let mut resp = tokio::time::timeout(STALL, client.get(url)).await.map_err(|_| "network")?.map_err(|_| "network")?;
        // A redirect may not leave encrypted connections.
        if !self.secure_url(cfg, resp.url()) {
            return Err("rejected");
        }
        if resp.content_length().is_some_and(|n| n > max as u64) {
            return Err("malformed");
        }
        let mut out = Vec::new();
        while let Some(chunk) = tokio::time::timeout(STALL, resp.chunk()).await.map_err(|_| "network")?.map_err(|_| "network")? {
            out.extend_from_slice(&chunk);
            if out.len() > max {
                return Err("malformed");
            }
        }
        Ok(out)
    }

    /// Ask the release server whether there is a newer version. Never downloads the package.
    pub async fn check_for_updates(&self) -> UpdateDto {
        let Some(cfg) = self.update_cfg().cloned() else {
            return self.update_status();
        };
        let Ok(_busy) = self.inner.update.busy.try_lock() else {
            return self.update_status(); // a check or a download is running
        };
        let before = {
            let s = self.update_state();
            (s.phase, s.newer)
        };
        if before.0 == Phase::Ready {
            return self.update_status(); // already downloaded and verified
        }
        self.set_update(|s| {
            s.phase = Phase::Checking;
            s.error = None;
        });
        let outcome = self.check_once(&cfg).await;
        let at = now();
        let _ = self.inner.store.set_setting(CHECKED_KEY, &at.to_string());
        self.set_update(|s| {
            s.checked_at = Some(at);
            match outcome {
                Ok(Choice::UpToDate) => {
                    s.phase = Phase::UpToDate;
                    s.newer = None;
                    s.package = None;
                }
                Ok(Choice::Available(package, version)) => {
                    s.phase = Phase::Available;
                    s.newer = Some(version);
                    s.package = Some(package);
                }
                Ok(Choice::NoPackage(version)) => {
                    s.phase = Phase::Elsewhere;
                    s.newer = Some(version);
                    s.package = None;
                }
                Err(code) => {
                    s.phase = Phase::Failed;
                    s.error = Some(code);
                }
            }
        });
        if let Some(v) = self.update_state().newer {
            tracing::info!(version = %v, "a newer VeloxFetch is available");
        }
        self.update_status()
    }

    async fn check_once(&self, cfg: &UpdateConfig) -> Result<Choice, &'static str> {
        let client = self.update_client(cfg)?;
        let feed = url::Url::parse(&cfg.feed).map_err(|_| "rejected")?;
        let sig_url = url::Url::parse(&format!("{}.sig", cfg.feed)).map_err(|_| "rejected")?;
        let manifest = self.fetch_small(cfg, &client, &feed, update_verify::MAX_MANIFEST).await?;
        let signature = self.fetch_small(cfg, &client, &sig_url, update_verify::MAX_SIGNATURE).await?;
        let m = update_verify::verify_manifest(&manifest, &signature, &cfg.keys, &cfg.policy).map_err(|e| {
            tracing::warn!(error = %e, "update information refused");
            e.code()
        })?;
        let last: Option<u64> = self.inner.store.get_setting(RELEASED_KEY).ok().flatten().and_then(|v| v.parse().ok());
        let target = cfg.kind.as_deref().and_then(update_verify::target_for);
        let decision = update_verify::decide(cfg.current, &m, last, target.as_deref()).map_err(|e: UpdateError| {
            tracing::warn!(error = %e, "update information refused");
            e.code()
        })?;
        if last.is_none_or(|l| m.released > l) {
            let _ = self.inner.store.set_setting(RELEASED_KEY, &m.released.to_string());
        }
        Ok(match decision {
            Decision::UpToDate => Choice::UpToDate,
            Decision::Available(p) => Choice::Available(p, m.version),
            Decision::NoPackage => Choice::NoPackage(m.version),
        })
    }

    /// Download the package found by the last check, and verify it. The file is kept under its final name only
    /// once its size and SHA-256 match the signed manifest.
    pub async fn download_update(&self) -> Result<UpdateDto, ManagerError> {
        let Some(cfg) = self.update_cfg().cloned() else {
            return Err(ManagerError::invalid("this copy of VeloxFetch cannot update itself"));
        };
        let Ok(_busy) = self.inner.update.busy.try_lock() else {
            return Ok(self.update_status());
        };
        let (package, phase) = {
            let s = self.update_state();
            (s.package.clone(), s.phase)
        };
        if phase == Phase::Ready {
            return Ok(self.update_status());
        }
        let Some(package) = package else {
            return Err(ManagerError::invalid("there is no update to download"));
        };
        self.inner.update.got.store(0, Ordering::Relaxed);
        self.inner.update.total.store(package.size, Ordering::Relaxed);
        self.set_update(|s| {
            s.phase = Phase::Downloading;
            s.error = None;
        });
        let outcome = self.fetch_package(&cfg, &package).await;
        self.set_update(|s| match outcome {
            Ok(path) => {
                s.phase = Phase::Ready;
                s.ready = Some(path);
            }
            Err(code) => {
                s.phase = Phase::Available;
                s.error = Some(code);
            }
        });
        Ok(self.update_status())
    }

    async fn fetch_package(&self, cfg: &UpdateConfig, package: &Package) -> Result<PathBuf, &'static str> {
        let dir = self.update_dir();
        tokio::fs::create_dir_all(&dir).await.map_err(|_| "disk")?;
        let final_path = dir.join(&package.name);
        let part = dir.join(format!("{}.part", package.name));
        // Only this package's files are kept: an older update (or anything else) in the folder is removed.
        clear_dir_except(&dir, &[&final_path, &part]);
        // Downloaded before (and verified then)? Verified again, as nothing on disk is trusted.
        if final_path.is_file() {
            let (p, pkg) = (final_path.clone(), package.clone());
            if tokio::task::spawn_blocking(move || update_verify::check_file(&p, &pkg)).await.is_ok_and(|r| r.is_ok()) {
                return Ok(final_path);
            }
            let _ = std::fs::remove_file(&final_path);
        }
        let url = url::Url::parse(&package.url).map_err(|_| "rejected")?;
        if !self.secure_url(cfg, &url) {
            return Err("rejected");
        }
        let client = self.update_client(cfg)?;
        let mut tries = 0;
        loop {
            match self.fetch_into(&client, &url, &part, package.size).await {
                Ok(()) => break,
                // Only a network failure is tried again: bytes that do not fit the signed size never get better.
                Err("network") if tries < 4 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(500 * tries)).await;
                }
                Err(e) => return Err(e),
            }
        }
        let (p, pkg) = (part.clone(), package.clone());
        let verdict = tokio::task::spawn_blocking(move || update_verify::check_file(&p, &pkg)).await.map_err(|_| "disk")?;
        if let Err(e) = verdict {
            tracing::warn!(error = %e, file = %package.name, "the downloaded update was refused");
            let _ = std::fs::remove_file(&part);
            return Err(e.code());
        }
        std::fs::rename(&part, &final_path).map_err(|_| "disk")?;
        tracing::info!(file = %package.name, "update downloaded and verified");
        Ok(final_path)
    }

    /// Continue (or start) `part` up to `size` bytes; never writes past `size`.
    async fn fetch_into(&self, client: &http_client::HttpClient, url: &url::Url, part: &Path, size: u64) -> Result<(), &'static str> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let mut have = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
        if have > size {
            let _ = tokio::fs::remove_file(part).await;
            have = 0;
        }
        if have == size {
            return Ok(());
        }
        let mut file = tokio::fs::OpenOptions::new().create(true).append(true).open(part).await.map_err(|_| "disk")?;
        let mut body: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, http_client::HttpError>> + Send>> =
            match client.get_range(url, have, size - 1, None).await {
                Ok(r) => Box::pin(r.into_stream()),
                Err(_) if have == 0 => {
                    let resp = client.get(url).await.map_err(|_| "network")?;
                    Box::pin(http_client::response_stream(resp))
                }
                // The server would not continue: start again from the beginning.
                Err(_) => {
                    file.set_len(0).await.map_err(|_| "disk")?;
                    have = 0;
                    let resp = client.get(url).await.map_err(|_| "network")?;
                    Box::pin(http_client::response_stream(resp))
                }
            };
        self.inner.update.got.store(have, Ordering::Relaxed);
        while let Some(chunk) = tokio::time::timeout(STALL, body.next()).await.map_err(|_| "network")? {
            let chunk = chunk.map_err(|_| "network")?;
            if have + chunk.len() as u64 > size {
                drop(file);
                let _ = tokio::fs::remove_file(part).await;
                return Err("corrupt");
            }
            file.write_all(&chunk).await.map_err(|_| "disk")?;
            have += chunk.len() as u64;
            self.inner.update.got.store(have, Ordering::Relaxed);
        }
        file.sync_all().await.map_err(|_| "disk")?;
        if have != size {
            return Err("network");
        }
        Ok(())
    }

    /// The verified package, checked once more, for the app to install.
    pub async fn prepare_update_install(&self) -> Result<UpdatePlan, ManagerError> {
        self.update_cfg().ok_or_else(|| ManagerError::invalid("this copy of VeloxFetch cannot update itself"))?;
        let (path, package, newer) = {
            let s = self.update_state();
            (s.ready.clone(), s.package.clone(), s.newer)
        };
        let (Some(path), Some(package), Some(newer)) = (path, package, newer) else {
            return Err(ManagerError::invalid("the update is not downloaded yet"));
        };
        let (p, pkg) = (path.clone(), package.clone());
        let verdict = tokio::task::spawn_blocking(move || update_verify::check_file(&p, &pkg))
            .await
            .map_err(|e| ManagerError::Internal(e.to_string()))?;
        if verdict.is_err() {
            let _ = std::fs::remove_file(&path);
            self.set_update(|s| {
                s.phase = Phase::Available;
                s.ready = None;
                s.error = Some("corrupt");
            });
            return Err(ManagerError::invalid("the downloaded update changed on disk; download it again"));
        }
        Ok(UpdatePlan {
            path,
            version: newer.to_string(),
            size: package.size,
            sha256: package.sha256,
            kind: kind_of(&package.target).to_string(),
        })
    }

    /// The install could not start (the app stays as it is).
    pub fn update_install_failed(&self) {
        self.set_update(|s| s.error = Some("install"));
    }

    /// Check now and then, in the background, when the user wants it (on by default). Only a check: the package
    /// is downloaded when the user asks.
    pub(super) fn spawn_update_checks(&self) {
        let Some(cfg) = self.update_cfg() else {
            return;
        };
        let (first, every) = (cfg.first_check, cfg.every);
        let mgr = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(first).await;
            loop {
                if mgr.inner.shutting_down.load(Ordering::Relaxed) {
                    return;
                }
                if mgr.update_auto() {
                    mgr.check_for_updates().await;
                }
                tokio::time::sleep(every).await;
            }
        });
    }
}

/// What a check found.
enum Choice {
    UpToDate,
    Available(Package, Version),
    NoPackage(Version),
}

/// Remove every file of `dir` but `keep` (the update folder holds only the current package).
fn clear_dir_except(dir: &Path, keep: &[&Path]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if keep.iter().any(|k| *k == p) {
            continue;
        }
        // Never follow a link out of the folder: only plain files are removed.
        if e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_styles_follow_the_kind_of_installation() {
        assert_eq!(install_style(Some("nsis")), "restart");
        assert_eq!(install_style(Some("appimage")), "restart");
        assert_eq!(install_style(Some("deb")), "package");
        assert_eq!(install_style(Some("dmg")), "package");
        assert_eq!(install_style(None), "page");
        assert_eq!(kind_of("linux-x86_64-appimage"), "appimage");
    }

    #[test]
    fn the_update_folder_keeps_only_the_current_package() {
        let d = std::env::temp_dir().join(format!("velox-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        for f in ["old.exe", "a.deb", "a.deb.part"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        clear_dir_except(&d, &[&d.join("a.deb"), &d.join("a.deb.part")]);
        assert!(!d.join("old.exe").exists());
        assert!(d.join("a.deb").exists() && d.join("a.deb.part").exists() && d.join("sub").is_dir());
        let _ = std::fs::remove_dir_all(&d);
    }
}
