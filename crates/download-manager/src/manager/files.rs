//! What the user does with finished files and failed downloads: show a file in its folder, open it, forget
//! history lines, sign in to a server that asked for a password, speed limits per kind of file.
//!
//! Paths always come from the database, never from the UI, and are used only when they lie inside a folder
//! downloads may be written to ([`path_safety::AllowedRoots`]). Programs, scripts and web pages are never
//! opened, only shown (see `system_integration::files`).

use downloader_core::FileKind;

use super::*;

impl DownloadManager {
    /// The saved file behind `target`, or the folder it is being saved in while it is not finished.
    async fn target_path(&self, target: FileTarget) -> Result<(PathBuf, bool), ManagerError> {
        let (path, finished) = match target {
            FileTarget::Download { id } => {
                let rec = self.load(id).await?;
                if rec.is_unstarted() {
                    (rec.destination.clone(), false)
                } else {
                    let done = rec.status == DownloadStatus::Completed;
                    (if done { rec.destination.clone() } else { rec.destination.parent().map(Path::to_path_buf).unwrap_or_default() }, done)
                }
            }
            FileTarget::History { id } => {
                let store = self.inner.store.clone();
                let h = blocking(move || store.history_entry(id)).await?.ok_or(ManagerError::NotFound(id))?;
                (h.destination, true)
            }
            FileTarget::Media { id } => self.media_target(id)?,
        };
        if self.inner.roots.check(&path).is_err() {
            return Err(ManagerError::invalid("that file is not in a download folder VeloxFetch knows"));
        }
        Ok((path, finished))
    }

    /// Show the file selected in the system file manager (its folder while it is still downloading).
    pub async fn show_in_folder(&self, target: FileTarget) -> Result<(), ManagerError> {
        let (path, finished) = self.target_path(target).await?;
        let exists = tokio::fs::try_exists(&path).await.unwrap_or(false);
        let shown = if exists {
            path
        } else if finished {
            // Moved or deleted since: show the folder it was in, if that is still there.
            match path.parent().filter(|p| p.is_dir()) {
                Some(dir) => dir.to_path_buf(),
                None => return Err(ManagerError::invalid("the file and its folder are gone (moved or deleted)")),
            }
        } else {
            return Err(ManagerError::invalid("the folder of this download does not exist yet"));
        };
        let shell = self.inner.cfg.shell.clone();
        tokio::task::spawn_blocking(move || shell.0.reveal(&shown))
            .await
            .map_err(|e| ManagerError::Internal(e.to_string()))?
            .map_err(|e| ManagerError::Internal(format!("cannot open the file manager: {e}")))
    }

    /// Open a finished file with its usual application. Programs, scripts and web pages are refused.
    pub async fn open_file(&self, target: FileTarget) -> Result<(), ManagerError> {
        let (path, finished) = self.target_path(target).await?;
        if !finished {
            return Err(ManagerError::invalid("the file is not finished yet"));
        }
        if system_integration::files::is_risky_to_open(&path) {
            return Err(ManagerError::invalid(
                "for your safety VeloxFetch does not open programs, scripts or web pages: use Show in folder and open it yourself",
            ));
        }
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Err(ManagerError::invalid("the file is gone (moved or deleted)"));
        }
        let shell = self.inner.cfg.shell.clone();
        tokio::task::spawn_blocking(move || shell.0.open(&path))
            .await
            .map_err(|e| ManagerError::Internal(e.to_string()))?
            .map_err(|e| ManagerError::Internal(format!("cannot open the file: {e}")))
    }

    /// Open the folder new downloads go to.
    pub async fn open_download_folder(&self) -> Result<(), ManagerError> {
        let root = self.download_root();
        tokio::fs::create_dir_all(&root).await.map_err(|e| ManagerError::Internal(format!("cannot create the folder: {e}")))?;
        let shell = self.inner.cfg.shell.clone();
        tokio::task::spawn_blocking(move || shell.0.reveal(&root))
            .await
            .map_err(|e| ManagerError::Internal(e.to_string()))?
            .map_err(|e| ManagerError::Internal(format!("cannot open the file manager: {e}")))
    }

    /// Remove one line of the history (`Some(id)`) or all of it. Files stay where they are.
    pub async fn forget_history(&self, id: Option<i64>) -> Result<usize, ManagerError> {
        let store = self.inner.store.clone();
        let n = blocking(move || store.forget_history(id)).await?;
        if let Some(id) = id.filter(|_| n == 0) {
            return Err(ManagerError::NotFound(id));
        }
        self.inner.stats.lock().unwrap().dirty = true;
        Ok(n)
    }

    /// Give a download the user name and password its server asked for, and start it again.
    pub async fn set_download_credentials(&self, req: SetCredentials) -> Result<DownloadDto, ManagerError> {
        let auth = validate::credentials(req.username.trim(), &req.password)?;
        if self.is_active(req.id) {
            return Err(ManagerError::invalid("pause the download before changing its sign-in"));
        }
        let rec = self.load(req.id).await?;
        if matches!(
            rec.status,
            DownloadStatus::Completed | DownloadStatus::Cancelled | DownloadStatus::Downloading | DownloadStatus::Verifying
        ) {
            return Err(ManagerError::InvalidState { action: "sign in to", status: rec.status });
        }
        self.patch_saved(req.id, move |r| {
            r.request_headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
            r.request_headers.push(("authorization".to_string(), auth));
        })
        .await?;
        self.start_download(req.id).await
    }

    /// The global limit and the limits per kind of file.
    pub fn speed_limits(&self) -> SpeedLimitsDto {
        let limits = &self.inner.limits;
        SpeedLimitsDto {
            global: limits.global(),
            by_type: FileKind::ALL.into_iter().filter_map(|k| limits.category(k.key()).map(|v| (k.key().to_string(), v))).collect(),
        }
    }

    /// Where a waiting browser download would go if saved as `filename` (the popup updates as the user types).
    pub fn pending_folder(&self, q: PendingFolderQuery) -> Result<String, ManagerError> {
        let content_type = self.pending_content_type(q.token)?;
        let name = downloader_core::filename::sanitize_filename(q.filename.trim());
        Ok(self.predicted_dir(&name, content_type.as_deref()).to_string_lossy().into_owned())
    }
}
