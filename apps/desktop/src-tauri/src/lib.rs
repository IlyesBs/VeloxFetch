//! VeloxFetch desktop shell.
//!
//! ```text
//! React UI  --IPC-->  commands (this file)  -->  DownloadManager  -->  download engine
//!                     authz + validation          lifecycle/events      network + disk
//! ```
//! Commands are deliberately dumb: authorize the calling window, hand the ids / DTOs to the
//! manager (which validates everything), and return the result. There is no generic command such
//! as "run", "read file" or "write file": the UI can only ask for the specific things below.

mod authz;
mod bridge;
mod clipboard;
mod desktop;
mod media_env;
mod preview_server;
mod smart_throttle;
mod webview_guard;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use authz::Command;
use download_manager::intelligence::{AnalysisDto, AnalyzeLink, CheckDuplicate, DuplicateDto};
use download_manager::profiles::{ProfileAction, ProfilesDto};
use download_manager::relink::{NewLinkOutcome, UseNewLink};
use download_manager::{
    ApiError, BatchPreview, BatchRequest, BatchResult, ConfirmPending, DiagnosticsDto, DownloadDetailDto, DownloadDto, DownloadManager,
    FileTarget, HistoryDto, ListFilter, LogsDto, ManagerConfig, MediaAnalysisDto, MediaJobDto, MediaRequest, MetricsDto, MultiNetworkDto,
    NetworkDto, NewDownload, PauseQueue, PendingDto, PendingFolderQuery, PickedFolder, PlaceDownload, PreviewDto, QueueAction,
    QueueStateDto, QueuesDto, SaveQueue, Schedule, SetCredentials, SetFolderNames, SetMultiNetwork, SetNetwork, SetPowerAction,
    SetQueueConfig, SetSmartThrottle, SetSpeedLimit, SmartThrottleDto, SpeedLimitsDto, StorageDto, VideoToolsDto,
};
use std::sync::Arc;
use tauri::{Emitter, EventTarget, Manager, RunEvent, State, WebviewWindow};

/// Event channel the UI listens on.
pub const EVENT_CHANNEL: &str = "velox://event";

struct AppState {
    manager: DownloadManager,
    bridge: bridge::Bridge,
    clipboard: clipboard::ClipboardWatch,
}

fn forbidden(window: &str, cmd: Command) -> ApiError {
    tracing::warn!(window, command = cmd.name(), "blocked IPC call from a window that is not authorized for it");
    ApiError { code: "forbidden", message: "this window may not perform that action".into() }
}

/// Reject the call unless the calling window is allowed to use `cmd`.
fn guard(window: &WebviewWindow, cmd: Command) -> Result<(), ApiError> {
    if authz::allowed(window.label(), cmd) {
        Ok(())
    } else {
        Err(forbidden(window.label(), cmd))
    }
}

macro_rules! command {
    ($(#[$m:meta])* $name:ident, $variant:ident, ($($arg:ident : $ty:ty),*) -> $ret:ty, |$mgr:ident| $body:expr) => {
        $(#[$m])*
        #[tauri::command]
        async fn $name(window: WebviewWindow, state: State<'_, AppState> $(, $arg: $ty)*) -> Result<$ret, ApiError> {
            guard(&window, Command::$variant)?;
            let $mgr = &state.manager;
            $body.await.map_err(ApiError::from)
        }
    };
}

command!(create_download, CreateDownload, (request: NewDownload) -> DownloadDto, |m| m.create_download(request));
command!(start_download, StartDownload, (id: i64) -> DownloadDto, |m| m.start_download(id));
command!(pause_download, PauseDownload, (id: i64) -> DownloadDto, |m| m.pause_download(id));
command!(resume_download, ResumeDownload, (id: i64) -> DownloadDto, |m| m.resume_download(id));
command!(cancel_download, CancelDownload, (id: i64) -> DownloadDto, |m| m.cancel_download(id));
command!(retry_download, RetryDownload, (id: i64) -> DownloadDto, |m| m.retry_download(id));
command!(redownload, Redownload, (id: i64) -> DownloadDto, |m| m.redownload(id));
command!(use_new_link, UseNewLink, (request: UseNewLink) -> NewLinkOutcome, |m| m.use_new_link(request));
command!(delete_download, DeleteDownload, (id: i64, delete_file: bool) -> (), |m| m.delete_download(id, delete_file));
command!(get_download, GetDownload, (id: i64) -> DownloadDto, |m| m.get_download(id));
command!(list_downloads, ListDownloads, (filter: ListFilter) -> Vec<DownloadDto>, |m| m.list_downloads(filter));
command!(
    set_priority,
    SetPriority,
    (id: i64, priority: download_manager::Priority) -> DownloadDto,
    |m| m.set_priority(id, priority)
);
command!(set_speed_limit, SetSpeedLimit, (request: SetSpeedLimit) -> (), |m| m.set_speed_limit(request));
command!(get_download_detail, GetDownloadDetail, (id: i64) -> DownloadDetailDto, |m| m.get_download_detail(id));
command!(list_history, ListHistory, (limit: u32) -> Vec<HistoryDto>, |m| m.list_history(limit));
command!(get_metrics, GetMetrics, () -> MetricsDto, |m| m.get_metrics());
command!(get_queue_state, GetQueueState, () -> QueueStateDto, |m| m.get_queue_state());
command!(set_queue_config, SetQueueConfig, (request: SetQueueConfig) -> QueueStateDto, |m| m.set_queue_config(request));
command!(pause_queue, PauseQueue, (request: PauseQueue) -> QueueStateDto, |m| m.pause_queue(request));
command!(resume_queue, ResumeQueue, () -> QueueStateDto, |m| m.resume_queue());
command!(set_schedule, SetSchedule, (schedule: Schedule) -> QueueStateDto, |m| m.set_schedule(schedule));
/// Browser integration lives outside the download manager, so these two are written out by hand.
#[tauri::command]
async fn get_browser_integration(window: WebviewWindow, state: State<'_, AppState>) -> Result<bridge::BrowserIntegrationDto, ApiError> {
    guard(&window, Command::GetBrowserIntegration)?;
    Ok(state.bridge.integration())
}

/// Registers or removes the native host with a browser (per user). Main window only, and only ever
/// reached from an explicit button in Settings.
#[tauri::command]
async fn set_browser_integration(
    window: WebviewWindow,
    state: State<'_, AppState>,
    request: bridge::SetBrowserIntegration,
) -> Result<bridge::BrowserIntegrationDto, ApiError> {
    guard(&window, Command::SetBrowserIntegration)?;
    state.bridge.set(request)
}

/// "Install extension" button: connects the browser, puts the extension files in a fixed folder,
/// opens the browser on its extensions page and shows the folder. Main window only. The browser
/// choice is an enum; no path or URL comes from the UI.
#[tauri::command]
async fn install_browser_extension(
    app: tauri::AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    request: bridge::InstallExtension,
) -> Result<bridge::ExtensionInstallDto, ApiError> {
    guard(&window, Command::InstallBrowserExtension)?;
    let resources = app.path().resource_dir().ok();
    state.bridge.install_extension(request, resources.as_deref())
}

command!(list_pending_downloads, ListPendingDownloads, () -> Vec<PendingDto>, |m| async { Ok::<_, download_manager::ManagerError>(m.list_pending()) });
command!(confirm_pending_download, ConfirmPendingDownload, (request: ConfirmPending) -> DownloadDto, |m| m.confirm_pending(request));
command!(dismiss_pending_download, DismissPendingDownload, (token: u64) -> (), |m| async { m.dismiss_pending(token) });
command!(set_browser_confirm, SetBrowserConfirm, (enabled: bool) -> (), |m| m.set_browser_confirm(enabled));
command!(set_video_bar, SetVideoBar, (enabled: bool) -> (), |m| m.set_video_bar(enabled));

/// What the UI's own title bar may ask of the window it is drawn in. Never another window: the action is
/// applied to the calling window, and only the main window may call it (`authz.rs`).
#[derive(serde::Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
enum WindowAction {
    /// The UI has painted: show the window (unless this run started hidden in the tray).
    Ready,
    Minimize,
    ToggleMaximize,
    Close,
    IsMaximized,
    /// The native frame (Mica tint, title bar) follows the app's theme.
    ThemeLight,
    ThemeDark,
    ThemeSystem,
}

/// Minimize / maximize / close the calling window, or set its frame theme. Returns whether it is maximized.
#[tauri::command]
async fn window_control(window: WebviewWindow, action: WindowAction) -> Result<bool, ApiError> {
    guard(&window, Command::WindowControl)?;
    let failed = |e: tauri::Error| ApiError { code: "window", message: format!("the window could not do that: {e}") };
    match action {
        WindowAction::Ready => desktop::window_ready(&window),
        WindowAction::Minimize => window.minimize().map_err(failed)?,
        WindowAction::ToggleMaximize => {
            let was = window.is_maximized().unwrap_or(false);
            if was {
                window.unmaximize().map_err(failed)?
            } else {
                window.maximize().map_err(failed)?
            }
            // On Linux (X11) the window manager applies it a moment later: answer with the new state, not the
            // old one. Windows and macOS have already changed it here, so this returns at once.
            for _ in 0..25 {
                if window.is_maximized().unwrap_or(was) != was {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
        WindowAction::Close => window.close().map_err(failed)?,
        WindowAction::IsMaximized => {}
        WindowAction::ThemeLight => {
            paint_behind(&window, Some(tauri::Theme::Light));
            window.set_theme(Some(tauri::Theme::Light)).map_err(failed)?
        }
        WindowAction::ThemeDark => {
            paint_behind(&window, Some(tauri::Theme::Dark));
            window.set_theme(Some(tauri::Theme::Dark)).map_err(failed)?
        }
        WindowAction::ThemeSystem => {
            window.set_theme(None).map_err(failed)?;
            paint_behind(&window, None);
        }
    }
    Ok(window.is_maximized().unwrap_or(false))
}

command!(preview_batch, PreviewBatch, (request: BatchRequest) -> BatchPreview, |m| m.preview_batch(request));
command!(add_batch, AddBatch, (request: BatchRequest) -> BatchResult, |m| m.add_batch(request));
command!(review_link, ReviewLink, (url: String) -> PendingDto, |m| m.review_link(url));
// Download Intelligence for the Add dialog: a read-only check of a link (`url: None` cancels a running one).
command!(analyze_link, AnalyzeLink, (request: AnalyzeLink) -> Option<AnalysisDto>, |m| m.analyze_link(request));
// Smart Duplicate Detection for the Add bar / dialog: read-only, no network (only what the app already knows).
command!(check_duplicate, CheckDuplicate, (request: CheckDuplicate) -> Option<DuplicateDto>, |m| m.check_duplicate(request));

// Stream (HLS / DASH) downloads: the UI sends a link and track ids from an earlier analysis, never a
// segment address, a path, or anything for a command line.
command!(analyze_media, AnalyzeMedia, (url: String, pending: Option<u64>) -> MediaAnalysisDto, |m| m.analyze_media_for(url, pending));
command!(start_media, StartMedia, (request: MediaRequest) -> MediaJobDto, |m| m.start_media(request));
command!(list_media_jobs, ListMediaJobs, () -> Vec<MediaJobDto>, |m| async { Ok::<_, download_manager::ManagerError>(m.list_media()) });
command!(cancel_media_job, CancelMediaJob, (id: u64) -> (), |m| m.cancel_media(id));
command!(pause_media_job, PauseMediaJob, (id: u64) -> (), |m| m.pause_media(id));
command!(resume_media_job, ResumeMediaJob, (id: u64) -> MediaJobDto, |m| m.resume_media(id));
command!(clear_media_job, ClearMediaJob, (id: u64, delete_files: Option<bool>) -> (), |m| m.clear_media(id, delete_files.unwrap_or(false)));

/// Clipboard detection lives outside the download manager (it needs the window), so these two are
/// written out by hand.
#[tauri::command]
async fn get_clipboard_watch(window: WebviewWindow, state: State<'_, AppState>) -> Result<clipboard::ClipboardState, ApiError> {
    guard(&window, Command::GetClipboardWatch)?;
    Ok(state.clipboard.state())
}

/// Turns clipboard detection on or off. Only ever reached from a checkbox in Settings.
#[tauri::command]
async fn set_clipboard_watch(
    window: WebviewWindow,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<clipboard::ClipboardState, ApiError> {
    guard(&window, Command::SetClipboardWatch)?;
    state.clipboard.set(&state.manager, enabled).await
}

/// Tray, start at login, notifications, language: stored by the manager; the tray menu follows the language.
#[tauri::command]
async fn get_desktop_options(window: WebviewWindow, state: State<'_, AppState>) -> Result<download_manager::DesktopPrefsDto, ApiError> {
    guard(&window, Command::GetDesktopOptions)?;
    Ok(state.manager.desktop_prefs())
}

#[tauri::command]
async fn set_desktop_options(
    window: WebviewWindow,
    state: State<'_, AppState>,
    request: download_manager::SetDesktopPrefs,
) -> Result<download_manager::DesktopPrefsDto, ApiError> {
    guard(&window, Command::SetDesktopOptions)?;
    let relabel = request.language.is_some();
    let prefs = state.manager.set_desktop_prefs(request).await.map_err(ApiError::from)?;
    if relabel {
        desktop::relabel_tray(window.app_handle(), &prefs.language);
    }
    Ok(prefs)
}

command!(set_power_action, SetPowerAction, (request: SetPowerAction) -> QueueStateDto, |m| m.set_power_action(request));
command!(get_queues, GetQueues, () -> QueuesDto, |m| m.get_queues());
command!(save_queue, SaveQueue, (request: SaveQueue) -> QueuesDto, |m| m.save_queue(request));
command!(delete_queue, DeleteQueue, (id: i64) -> QueuesDto, |m| m.delete_queue(id));
command!(queue_action, QueueAction, (id: i64, action: QueueAction) -> QueuesDto, |m| m.queue_action(id, action));
command!(place_download, PlaceDownload, (request: PlaceDownload) -> DownloadDto, |m| m.place_download(request));
command!(set_keep_awake, SetKeepAwake, (enabled: bool) -> QueueStateDto, |m| m.set_keep_awake(enabled));

// Where downloads go. No command takes a path: a folder is chosen in a native dialog that only Rust can open
// (`pick_folder` / `choose_download_folder`), and the UI holds a token for it.
command!(get_storage, GetStorage, () -> StorageDto, |m| async { Ok::<_, download_manager::ManagerError>(m.get_storage()) });
command!(set_organize, SetOrganize, (enabled: bool) -> StorageDto, |m| m.set_organize(enabled));
command!(set_folder_names, SetFolderNames, (request: SetFolderNames) -> StorageDto, |m| m.set_folder_names(request));
command!(reset_download_folder, ResetDownloadFolder, () -> StorageDto, |m| m.reset_default_folder());

// Network settings (proxy, user agent). The proxy password is accepted here but never sent back.
command!(get_network, GetNetwork, () -> NetworkDto, |m| async { Ok::<_, download_manager::ManagerError>(m.get_network()) });
command!(set_network, SetNetwork, (request: SetNetwork) -> NetworkDto, |m| m.set_network(request));
command!(get_multi_network, GetMultiNetwork, () -> MultiNetworkDto, |m| async { Ok::<_, download_manager::ManagerError>(m.get_multi_network().await) });
command!(set_multi_network, SetMultiNetwork, (request: SetMultiNetwork) -> MultiNetworkDto, |m| m.set_multi_network(request));
command!(get_speed_limits, GetSpeedLimits, () -> SpeedLimitsDto, |m| async { Ok::<_, download_manager::ManagerError>(m.speed_limits()) });
command!(
    set_download_credentials,
    SetDownloadCredentials,
    (request: SetCredentials) -> DownloadDto,
    |m| m.set_download_credentials(request)
);
command!(
    preview_pending_folder,
    PreviewPendingFolder,
    (request: PendingFolderQuery) -> String,
    |m| async { m.pending_folder(request) }
);

// Finished files: shown in the file manager or opened with their usual application. The UI names a download or a
// history line by id; the path comes from the database and must be inside a download folder. Programs, scripts and
// web pages are never opened (only shown).
command!(show_in_folder, ShowInFolder, (target: FileTarget) -> (), |m| m.show_in_folder(target));
command!(open_file, OpenFile, (target: FileTarget) -> (), |m| m.open_file(target));
command!(open_download_folder, OpenDownloadFolder, () -> (), |m| m.open_download_folder());
command!(forget_history, ForgetHistory, (id: Option<i64>) -> usize, |m| m.forget_history(id));

// Diagnostics: everything stays on this computer and is cleaned of secrets in Rust before the UI sees it. The log
// folder is known to Rust only; the UI can ask to show it, never name it.
command!(get_download_diagnostics, GetDownloadDiagnostics, (id: i64) -> DiagnosticsDto, |m| m.get_download_diagnostics(id));
command!(
    get_logs,
    GetLogs,
    (limit: u32, problems_only: bool) -> LogsDto,
    |m| async { Ok::<_, download_manager::ManagerError>(m.get_logs(limit, problems_only)) }
);
command!(set_detailed_logging, SetDetailedLogging, (enabled: bool) -> (), |m| m.set_detailed_logging(enabled));
command!(get_diagnostics_report, GetDiagnosticsReport, () -> String, |m| m.diagnostics_report());
command!(open_log_folder, OpenLogFolder, () -> (), |m| m.open_log_folder());
command!(get_smart_throttle, GetSmartThrottle, () -> SmartThrottleDto, |m| async {
    Ok::<_, download_manager::ManagerError>(m.smart_throttle())
});
// Download Profiles: presets over the existing settings (see download-manager `profiles.rs`), main window only.
command!(get_profiles, GetProfiles, () -> ProfilesDto, |m| async { Ok::<_, download_manager::ManagerError>(m.get_profiles()) });
command!(profile_action, ProfileAction, (request: ProfileAction) -> ProfilesDto, |m| m.profile_action(request));
command!(set_smart_throttle, SetSmartThrottle, (request: SetSmartThrottle) -> SmartThrottleDto, |m| m.set_smart_throttle(request));
command!(set_preview_mode, SetPreviewMode, (id: i64, enabled: bool) -> PreviewDto, |m| m.set_preview_mode(id, enabled));
command!(get_preview_info, GetPreviewInfo, (id: i64) -> PreviewDto, |m| m.preview_info(id));
// Where the player reads previews from, when not the custom protocol (Linux and macOS: see `preview_server::loopback`).
command!(get_preview_base, GetPreviewBase, () -> Option<String>, |_m| async {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let base = preview_server::loopback::base();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let base = None;
    Ok::<_, download_manager::ManagerError>(base)
});
command!(get_video_tools, GetVideoTools, () -> VideoToolsDto, |m| async { Ok::<_, download_manager::ManagerError>(m.video_tools()) });
command!(fetch_video_tools, FetchVideoTools, () -> VideoToolsDto, |m| async {
    Ok::<_, download_manager::ManagerError>(m.fetch_video_tools().await)
});

// Updates (Phase 19, docs/auto-update.md): the manager checks the signed manifest and downloads the package; the
// install itself (a verified installer, an AppImage replaced, the system's package installer) is done here.
command!(get_update_status, GetUpdateStatus, () -> download_manager::UpdateDto, |m| async {
    Ok::<_, download_manager::ManagerError>(m.update_status())
});
command!(check_for_updates, CheckForUpdates, () -> download_manager::UpdateDto, |m| async {
    Ok::<_, download_manager::ManagerError>(m.check_for_updates().await)
});
command!(download_update, DownloadUpdate, () -> download_manager::UpdateDto, |m| m.download_update());
command!(set_update_auto, SetUpdateAuto, (enabled: bool) -> download_manager::UpdateDto, |m| m.set_update_auto(enabled));

/// Install the downloaded update. Returns `restart` (the app quits; the new version takes over) or `opened` (the
/// system's installer is open). The package is checked again by the manager and once more right before it is used.
#[tauri::command]
async fn install_update(window: WebviewWindow, state: State<'_, AppState>) -> Result<&'static str, ApiError> {
    guard(&window, Command::InstallUpdate)?;
    let plan = state.manager.prepare_update_install().await.map_err(ApiError::from)?;
    if dry_run_shell() {
        tracing::info!(version = %plan.version, kind = %plan.kind, "dry run: the update would be installed now");
        return Ok("opened");
    }
    let p = plan.clone();
    let outcome = tokio::task::spawn_blocking(move || system_integration::update::install(&p.kind, &p.path, p.size, &p.sha256)).await;
    match outcome {
        Ok(Ok(system_integration::update::Installed::Restart)) => {
            tracing::info!(version = %plan.version, "installing the update: VeloxFetch closes");
            window.app_handle().exit(0);
            Ok("restart")
        }
        Ok(Ok(system_integration::update::Installed::Opened)) => Ok("opened"),
        other => {
            let why = match other {
                Ok(Err(e)) => e.to_string(),
                Err(e) => e.to_string(),
                Ok(Ok(_)) => String::new(),
            };
            tracing::warn!(error = %why, "the update could not be installed");
            state.manager.update_install_failed();
            Err(ApiError { code: "update", message: "the update could not be started".into() })
        }
    }
}

/// The release page (a constant of the build), for a portable copy or a newer version not packaged for this system.
#[tauri::command]
async fn open_release_page(window: WebviewWindow, state: State<'_, AppState>) -> Result<(), ApiError> {
    guard(&window, Command::OpenReleasePage)?;
    let Some(url) = state.manager.update_status().release_page else {
        return Err(ApiError { code: "update", message: "no release page".into() });
    };
    if dry_run_shell() {
        tracing::info!("dry run: the release page would open");
        return Ok(());
    }
    system_integration::update::open_release_page(&url)
        .map_err(|e| ApiError { code: "update", message: format!("the page could not be opened: {e}") })
}

/// Debug builds in the end-to-end tests: nothing is installed or opened.
fn dry_run_shell() -> bool {
    cfg!(debug_assertions) && std::env::var_os("VELOX_DRY_RUN_SHELL").is_some()
}

/// The public keys this build trusts for updates (empty: it cannot update itself).
const UPDATE_KEYS: &str = include_str!("../update-keys.txt");
/// The public download repository (the source repository is private): the release page, the update feed and the
/// packages all come from it. `scripts/update-key.mjs` and `scripts/publish-release.sh` name the same one.
const RELEASE_PAGE: &str = "https://github.com/IlyesBs/VeloxFetch-releases/releases";
/// GitHub's "latest release" address: drafts and pre-releases are never served here.
const UPDATE_FEED: &str = "https://github.com/IlyesBs/VeloxFetch-releases/releases/latest/download/latest.json";
const UPDATE_PACKAGES: &str = "https://github.com/IlyesBs/VeloxFetch-releases/releases/download/";

/// Where updates come from. Debug builds never ask the real release server: only a local one, when the end-to-end
/// test sets it up (`VELOX_UPDATE_FEED`, `VELOX_UPDATE_PREFIX`, `VELOX_UPDATE_KEYS`).
fn update_config() -> Option<download_manager::UpdateConfig> {
    let current = update_verify::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let mut cfg = download_manager::UpdateConfig {
        keys: update_verify::TrustedKeys::parse(UPDATE_KEYS)
            .map_err(|e| tracing::error!(error = %e, "the trusted update keys of this build are unreadable: no updates"))
            .ok()?,
        policy: update_verify::Policy { product: "VeloxFetch".into(), url_prefix: UPDATE_PACKAGES.into(), allow_loopback_http: false },
        feed: UPDATE_FEED.into(),
        release_page: RELEASE_PAGE.into(),
        current,
        kind: system_integration::update::installation_kind().map(String::from),
        allow_loopback_http: false,
        first_check: std::time::Duration::from_secs(60),
        every: std::time::Duration::from_secs(12 * 3600),
        dir: None,
    };
    if cfg!(debug_assertions) {
        let (feed, prefix, keys) = (dev_only("VELOX_UPDATE_FEED")?, dev_only("VELOX_UPDATE_PREFIX")?, dev_only("VELOX_UPDATE_KEYS")?);
        cfg.keys = update_verify::TrustedKeys::parse(&std::fs::read_to_string(keys).ok()?).ok()?;
        cfg.feed = feed;
        cfg.policy.url_prefix = prefix;
        cfg.policy.allow_loopback_http = true;
        cfg.allow_loopback_http = true;
        cfg.first_check = std::time::Duration::from_secs(2);
        // The installation kind to act as (the test build itself is never "installed"); install is a dry run there.
        if let Some(kind) = dev_only("VELOX_UPDATE_KIND") {
            cfg.kind = Some(kind);
        }
    }
    Some(cfg)
}

/// The log of this run: `<app data>/logs/velox.log` (rotated, a few MB at most), secrets removed from every line.
/// Installed before anything else so the start-up is logged too; crashes (panics) are written to it.
fn start_log(data_dir: &std::path::Path) -> diagnostics::LogHandle {
    let log = diagnostics::LogHandle::new(diagnostics::LogConfig {
        dir: Some(data_dir.join("logs")),
        home: diagnostics::home_dir(),
        app: format!("VeloxFetch {}", env!("CARGO_PKG_VERSION")),
        ..Default::default()
    });
    if !log.install() {
        tracing::warn!("a logger was already installed; this one only keeps lines for the diagnostics page");
    }
    log
}

/// The native "choose a folder" dialog, modal to the window. Debug builds can be told the answer
/// (`VELOX_TEST_PICK_FILE=<file>`) so the end-to-end tests never wait for a person.
async fn choose_folder(window: &WebviewWindow, title: &str) -> Option<PathBuf> {
    // The file holds the answer (a folder, or the word `cancel`) and is read every time, so a test can change it.
    if let Some(file) = dev_only("VELOX_TEST_PICK_FILE") {
        let answer = std::fs::read_to_string(file).unwrap_or_default().trim().to_string();
        return (!answer.is_empty() && answer != "cancel").then(|| PathBuf::from(answer));
    }
    rfd::AsyncFileDialog::new().set_title(title).set_parent(window).pick_folder().await.map(|h| h.path().to_path_buf())
}

/// "Change..." next to a single download: pick a folder for it. Returns a token, not a path, and `null` if
/// the user closed the dialog. Folders that are system, program or autostart locations are refused.
#[tauri::command]
async fn pick_folder(window: WebviewWindow, state: State<'_, AppState>) -> Result<Option<PickedFolder>, ApiError> {
    guard(&window, Command::PickFolder)?;
    let Some(path) = choose_folder(&window, "Choose where to save this download").await else { return Ok(None) };
    state.manager.register_pick(&path).map(Some).map_err(ApiError::from)
}

/// Settings > "Change...": pick the folder new downloads go to.
#[tauri::command]
async fn choose_download_folder(window: WebviewWindow, state: State<'_, AppState>) -> Result<Option<StorageDto>, ApiError> {
    guard(&window, Command::ChooseDownloadFolder)?;
    let Some(path) = choose_folder(&window, "Choose the download folder").await else { return Ok(None) };
    let pick = state.manager.register_pick(&path).map_err(ApiError::from)?;
    state.manager.use_folder_as_default(pick.token).await.map(Some).map_err(ApiError::from)
}

/// Where state and downloads live. `VELOX_DATA_DIR` / `VELOX_DOWNLOAD_ROOT` are honoured in debug
/// builds only (used by the end-to-end tests); release builds always use the OS locations.
fn locations(app: &tauri::AppHandle) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let dev = |var: &str| cfg!(debug_assertions).then(|| std::env::var_os(var).map(PathBuf::from)).flatten();
    let paths = app.path();
    let home = paths.home_dir().ok();
    // Some systems do not report every folder (no XDG user dirs, odd profiles): fall back instead of
    // refusing to start. The temp folder always exists.
    let data = dev("VELOX_DATA_DIR").unwrap_or_else(|| {
        first_known(
            "data",
            [paths.app_data_dir().ok(), paths.app_local_data_dir().ok(), home.as_ref().map(|h| h.join(".veloxfetch"))],
            std::env::temp_dir().join("VeloxFetch-data"),
        )
    });
    let root = dev("VELOX_DOWNLOAD_ROOT").unwrap_or_else(|| {
        first_known("download", [paths.download_dir().ok(), home.as_ref().map(|h| h.join("Downloads")), None], std::env::temp_dir())
            .join("VeloxFetch")
    });
    Ok((data.join("velox.db"), root))
}

/// The first folder the system knows, else `last_resort` (logged, so a support report shows it).
fn first_known(what: &str, candidates: [Option<PathBuf>; 3], last_resort: PathBuf) -> PathBuf {
    for (i, dir) in candidates.into_iter().enumerate() {
        if let Some(dir) = dir.filter(|d| d.is_absolute()) {
            if i > 0 {
                tracing::warn!(folder = what, "the system's usual folder is unknown; using a fallback");
            }
            return dir;
        }
    }
    tracing::warn!(folder = what, "no usual folder is known on this system; using the temp folder");
    last_resort
}

/// Debug builds only (end-to-end tests): make power actions a harmless dry run and shorten the
/// countdown. Release builds ignore these variables, so a real shutdown can never be faked away
/// or triggered by the environment.
/// An environment variable, honoured in debug builds only (release builds never read it).
fn dev_only(name: &str) -> Option<String> {
    cfg!(debug_assertions).then(|| std::env::var(name).ok()).flatten().filter(|v| !v.is_empty())
}

fn dev_overrides(cfg: &mut ManagerConfig) {
    if !cfg!(debug_assertions) {
        return;
    }
    // Power actions, "Show in folder" / "Open", keeping awake and notifications only print what they would do:
    // the end-to-end tests must never shut the machine down or open windows.
    if std::env::var_os("VELOX_DRY_RUN_POWER").is_some() || std::env::var_os("VELOX_DRY_RUN_SHELL").is_some() {
        cfg.system = download_manager::SystemHandle(std::sync::Arc::new(download_manager::DryRunSystem::new()));
    }
    // A stand-in ffmpeg for the end-to-end tests (debug builds only; release builds never read it).
    if let Some(p) = dev_only("VELOX_YTDLP") {
        cfg.ytdlp_path = Some(std::path::PathBuf::from(p));
    }
    if let Some(p) = dev_only("VELOX_FFMPEG") {
        cfg.ffmpeg_path = Some(std::path::PathBuf::from(p));
    }
    // Sorting by type on / off for the end-to-end tests (`0` = off), so the older tests keep looking for
    // their files in the download folder itself. Debug builds only.
    if let Some(v) = dev_only("VELOX_ORGANIZE") {
        cfg.organize_default = v != "0";
    }
    // Stand-in networks for "several networks at once" (`name=interface@address,...`), spreading every link (the
    // test servers are local, plain http). Debug builds only.
    if let Some(v) = dev_only("VELOX_TEST_NETWORKS") {
        let nets: Vec<download_manager::RouteSpec> = v
            .split(',')
            .filter_map(|n| {
                let (name, rest) = n.split_once('=')?;
                let (iface, addr) = rest.split_once('@')?;
                let ip: std::net::IpAddr = addr.parse().ok()?;
                let v4 = match ip {
                    std::net::IpAddr::V4(a) => Some(a),
                    _ => None,
                };
                // The system reaches the local test server from 127.0.0.1: one of "these networks'" addresses.
                let addresses = vec![ip, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)];
                Some(download_manager::RouteSpec { name: name.to_string(), interface: Some(iface.to_string()), v4, v6: None, addresses })
            })
            .collect();
        cfg.client.spread_any_link = true;
        cfg.test_networks = Some(nets);
    }
    if let Some(secs) = std::env::var("VELOX_POWER_GRACE_SECS").ok().and_then(|v| v.parse::<u64>().ok()) {
        cfg.power_grace = std::time::Duration::from_secs(secs);
    }
}

/// Create the main window from `tauri.conf.json`. Done in code (rather than by the config) so that
/// **debug builds only** can open a WebView2 debugging port for the end-to-end test when
/// `VELOX_E2E_DEBUG_PORT` is set. Release builds never do this.
fn create_main_window(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = app.config().app.windows.first().cloned().ok_or("no window configured")?;
    #[allow(unused_mut)]
    let mut builder = tauri::WebviewWindowBuilder::from_config(app, &cfg)?;
    #[cfg(all(debug_assertions, windows))]
    if let Ok(port) = std::env::var("VELOX_E2E_DEBUG_PORT") {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            // The web view's language (the interface follows it on first start): the test's, else English.
            let lang = std::env::var("VELOX_E2E_LANG")
                .ok()
                .filter(|l| !l.is_empty() && l.len() <= 16 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
            builder = builder.additional_browser_args(&format!(
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port={port} --remote-allow-origins=* --lang={}",
                lang.as_deref().unwrap_or("en-US")
            ));
        }
    }
    let window = window_chrome(builder).build()?;
    fit_to_screen(&window);
    // If the web view under the window dies (see webview_guard.rs), rebuild the window instead of leaving
    // an empty dark one behind.
    let owner = app.clone();
    webview_guard::watch(&window, move |webview_guard::Recovery::RebuildWindow| {
        let app = owner.clone();
        let _ = owner.run_on_main_thread(move || rebuild_main_window(&app));
    });
    // The window is created hidden and shown by the UI once it has painted (no blank web-page
    // frame). If the UI never gets there, show it anyway so the app can't stay invisible.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(6));
        if !desktop::starts_hidden() && !window.is_visible().unwrap_or(true) {
            let _ = window.show();
        }
    });
    Ok(())
}

/// The window opens at the dashboard's size (tauri.conf.json), but never larger than most of the screen it
/// is on (a small laptop): then it takes 92% of the work area, centred. Nothing to do when the size fits.
fn fit_to_screen(window: &tauri::WebviewWindow) {
    let (Ok(Some(monitor)), Ok(size)) = (window.current_monitor(), window.outer_size()) else { return };
    let area = monitor.work_area().size;
    let max_w = (f64::from(area.width) * 0.92) as u32;
    let max_h = (f64::from(area.height) * 0.92) as u32;
    if size.width <= max_w && size.height <= max_h {
        return;
    }
    let _ = window.set_size(tauri::PhysicalSize::new(size.width.min(max_w), size.height.min(max_h)));
    let _ = window.center();
}

/// The window frame for each system. The UI draws its own title bar, and is told which frame it got
/// (`window.__VELOX_CHROME__`, a frozen constant) so it can style itself to match:
/// - Windows 11: no system title bar, Mica behind a transparent web view (`surface: mica`).
/// - Windows 10: no system title bar, an opaque window (Mica does not exist there).
/// - macOS: the native traffic lights over the UI's own title bar (no private APIs, so no transparency).
/// - Linux: no system title bar, an opaque window (compositors differ too much for transparency).
fn window_chrome<'a>(
    builder: tauri::WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle>,
) -> tauri::WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle> {
    #[cfg(windows)]
    let (builder, platform, surface) = {
        let b = builder.decorations(false).shadow(true);
        if windows_11() {
            let mica = tauri::utils::config::WindowEffectsConfig {
                effects: vec![tauri::window::Effect::Mica],
                state: None,
                radius: None,
                color: None,
            };
            (b.transparent(true).background_color(tauri::webview::Color(0, 0, 0, 0)).effects(mica), "windows", "mica")
        } else {
            (b, "windows", "solid")
        }
    };
    #[cfg(target_os = "macos")]
    let (builder, platform, surface) = (builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true), "macos", "glass");
    #[cfg(not(any(windows, target_os = "macos")))]
    let (builder, platform, surface) = (builder.decorations(false), "linux", "solid");
    let _ = SURFACE.set(surface);
    builder.initialization_script(format!(
        "Object.defineProperty(window,'__VELOX_CHROME__',{{value:Object.freeze({{platform:'{platform}',surface:'{surface}'}})}});"
    ))
}

/// The window surface chosen by [`window_chrome`] (`mica`, `glass` or `solid`).
static SURFACE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// The page's canvas colour in each theme (`--canvas` in styles.css, opaque surface).
const CANVAS_DARK: tauri::webview::Color = tauri::webview::Color(7, 13, 29, 255);
const CANVAS_LIGHT: tauri::webview::Color = tauri::webview::Color(244, 246, 249, 255);

/// The colour behind the page, seen for a moment when the window appears before the web view's first frame: the
/// app's canvas in the theme the UI uses, so there is no dark flash before a light interface (or the reverse).
/// Mica stays see-through.
fn paint_behind(window: &WebviewWindow, theme: Option<tauri::Theme>) {
    if SURFACE.get() == Some(&"mica") {
        return;
    }
    let dark = theme.or_else(|| window.theme().ok()).is_none_or(|t| t == tauri::Theme::Dark);
    let _ = window.set_background_color(Some(if dark { CANVAS_DARK } else { CANVAS_LIGHT }));
}

/// Mica needs Windows 11 (build 22000 or later).
#[cfg(windows)]
fn windows_11() -> bool {
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
    // RtlGetVersion, not GetVersionEx: the latter reports Windows 8 to programs without a compatibility
    // manifest. RtlGetVersion always returns the real version.
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> i32;
    }
    let mut info = OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: fills a struct we own, whose size field is set as the API requires; returns an NTSTATUS.
    let status = unsafe { RtlGetVersion(&mut info) };
    status == 0 && info.dwMajorVersion >= 10 && info.dwBuildNumber >= 22000
}

/// The main window is being replaced (its web view died): closing it must not end the app.
static REBUILDING: AtomicBool = AtomicBool::new(false);
static REBUILDS: std::sync::Mutex<webview_guard::RebuildBudget> =
    std::sync::Mutex::new(webview_guard::RebuildBudget { recent: std::collections::VecDeque::new() });

/// Destroy the main window; `run` creates a fresh one when it is gone. At most 3 times in 2 minutes.
fn rebuild_main_window(app: &tauri::AppHandle) {
    let allowed =
        REBUILDS.lock().unwrap_or_else(|p| p.into_inner()).allow(std::time::Instant::now(), 3, std::time::Duration::from_secs(120));
    if !allowed {
        tracing::error!("the window's web view keeps stopping; asking the user before rebuilding it again");
        // Say why instead of leaving a dead window, and try once more when the user is ready.
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let again = rfd::AsyncMessageDialog::new()
                .set_level(rfd::MessageLevel::Warning)
                .set_title("VeloxFetch")
                .set_description(
                    "The VeloxFetch window keeps closing unexpectedly. Your downloads keep running.\n\n\
                     This is usually caused by an on-screen overlay that injects itself into other programs, for \
                     example RivaTuner Statistics Server (MSI Afterburner). Add velox-desktop.exe and \
                     msedgewebview2.exe to its exclusions, or close it.\n\nOpen the window again?",
                )
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                .await;
            if again == rfd::MessageDialogResult::Yes {
                REBUILDS.lock().unwrap_or_else(|p| p.into_inner()).recent.clear();
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || rebuild_main_window(&handle));
            }
        });
        return;
    }
    let Some(window) = app.get_webview_window(authz::MAIN) else { return };
    REBUILDING.store(true, Ordering::SeqCst);
    if let Err(e) = window.destroy() {
        REBUILDING.store(false, Ordering::SeqCst);
        tracing::error!(error = %e, "could not close the broken window");
    }
}

/// Show the window of the VeloxFetch that is already running (Windows; elsewhere the second launch just
/// leaves). Found by its window class and exact title, so nothing else can be picked.
fn focus_running_instance() {
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW};
        // SAFETY: plain user32 calls with constant strings and the handle they return.
        unsafe {
            if let Ok(h) = FindWindowW(w!("Tauri Window"), w!("VeloxFetch")) {
                let _ = ShowWindow(h, if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW });
                let _ = SetForegroundWindow(h);
            }
        }
    }
}

/// A browser download is waiting for a decision: make sure the window that shows the dialog is
/// visible and in front, even if it was minimised or behind another window.
fn bring_to_front(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(authz::MAIN) {
        let _ = w.show();
        let _ = w.unminimize();
        // Windows refuses to focus a window from the background; a brief always-on-top forces it forward.
        let _ = w.set_always_on_top(true);
        let _ = w.set_focus();
        let _ = w.set_always_on_top(false);
    }
}

/// Where the installer puts the video tools, next to the program: `<install folder>\tools` on Windows,
/// `/usr/lib/velox-fetch/tools` for the Linux packages (the program is in /usr/bin). None on macOS (a signed
/// app bundle must not change; the app fetches them into the user's data folder at first start).
fn shared_tools_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    if cfg!(target_os = "macos") {
        return None;
    }
    if cfg!(target_os = "linux") && dir.starts_with("/usr") {
        return Some(PathBuf::from("/usr/lib/velox-fetch/tools"));
    }
    Some(dir.join("tools"))
}

/// `velox-desktop --install-video-tools`: run by the installer (Windows setup, Linux packages) to download
/// yt-dlp, Deno and ffmpeg next to the program, so videos work right after installing. Prints its progress
/// (the installer shows it) and never fails the installation: whatever is missing, the app fetches later.
fn install_video_tools_cli() -> i32 {
    let Some(dir) = shared_tools_dir() else {
        println!("The required dependencies are downloaded when VeloxFetch starts.");
        return 0;
    };
    let tmp = std::env::temp_dir().join(format!("velox-install-{}", std::process::id()));
    let result = tauri::async_runtime::block_on(async {
        let mut cfg = ManagerConfig::new(tmp.join("velox.db"), tmp.join("downloads"));
        cfg.tools = download_manager::ToolSources::official();
        cfg.tools_dir = Some(dir.clone());
        let m = DownloadManager::new(cfg).await.map_err(|e| e.to_string())?;
        println!("Downloading required dependencies...");
        let job = {
            let m = m.clone();
            tokio::spawn(async move { m.fetch_video_tools().await })
        };
        tokio::pin!(job);
        let started = std::time::Instant::now();
        let mut last = String::new();
        loop {
            tokio::select! {
                done = &mut job => break done.map_err(|e| e.to_string()),
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                    let t = m.video_tools();
                    // One line for the setup's details, without program names.
                    let all = [&t.ytdlp, &t.deno, &t.ffmpeg];
                    let ready = all.iter().filter(|s| s.installed).count();
                    let received: u64 = all.iter().map(|s| s.received).sum();
                    let line = if ready == all.len() {
                        "Required dependencies: ready".to_string()
                    } else {
                        format!("Required dependencies: {ready} of {} ready, {} MB downloaded", all.len(), received / 1_048_576)
                    };
                    if line != last {
                        println!("{line}");
                        last = line;
                    }
                    if started.elapsed() > std::time::Duration::from_secs(30 * 60) {
                        break Err("this is taking too long; VeloxFetch will finish it when it starts".to_string());
                    }
                }
            }
        }
    });
    let _ = std::fs::remove_dir_all(&tmp);
    match result {
        Ok(t) if t.ytdlp.installed && t.deno.installed && t.ffmpeg.installed => println!("Required dependencies: ready."),
        // The reasons name the programs: they go to the log, not to the installer's window.
        Ok(_) => println!("Some required dependencies could not be downloaded now. VeloxFetch will get them when it starts."),
        Err(e) => {
            tracing::warn!(error = %e, "installing the video tools failed");
            println!("The required dependencies could not be downloaded now. VeloxFetch will get them when it starts.")
        }
    }
    0
}

pub fn run() {
    // Before any thread: the web view's processes inherit it (Linux AppImage only, see media_env.rs).
    media_env::prepare();
    if std::env::args().skip(1).any(|a| a == "--install-video-tools") {
        std::process::exit(install_video_tools_cli());
    }
    let shutting_down = std::sync::Arc::new(AtomicBool::new(false));
    #[allow(unused_mut)] // changed on Linux and macOS only
    let mut context = tauri::generate_context!();
    // Linux and macOS: previews are read from a private local address (see preview_server.rs); its exact origin joins
    // media-src. (Their web views' players cannot stream a file that is still growing through the custom protocol.)
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(origin) = preview_server::loopback::reserve() {
        preview_server::loopback::allow_in_csp(context.config_mut(), &origin);
    }

    let app = tauri::Builder::default()
        // Live preview of downloading videos (Tauri v2 asynchronous custom protocol, see preview_server.rs).
        .register_asynchronous_uri_scheme_protocol(preview_server::SCHEME, preview_server::handle)
        .setup(|app| {
            let (db_path, download_root) = locations(app.handle())?;
            let db_path_dir = db_path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
            // One VeloxFetch per user: a second launch brings the running one forward and leaves, before it
            // opens the database or touches the browser link.
            // Started by an update (the AppImage replaced itself): the old copy is still closing; wait for it.
            let mut claim = bridge::claim_link(&db_path_dir);
            if std::env::args().any(|a| a == system_integration::update::AFTER_UPDATE_ARG) {
                let since = std::time::Instant::now();
                while matches!(claim, bridge::LinkClaim::TakenByAnotherInstance) && since.elapsed() < std::time::Duration::from_secs(60) {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    claim = bridge::claim_link(&db_path_dir);
                }
            }
            let listener = match claim {
                bridge::LinkClaim::Ours(l) => Some(l),
                bridge::LinkClaim::Unavailable => None,
                bridge::LinkClaim::TakenByAnotherInstance => {
                    #[cfg(unix)]
                    desktop::ask_running_copy_to_show(&db_path_dir);
                    focus_running_instance();
                    std::process::exit(0);
                }
            };
            let log = start_log(&db_path_dir);
            let mut cfg = ManagerConfig::new(db_path, download_root);
            cfg.log = Some(log);
            // The real operating system (power actions, files, keeping awake): only the app itself gets it.
            cfg.system = download_manager::SystemHandle::native();
            // Starting at login: offered by release builds (debug builds and their tests never touch login items).
            cfg.app_launcher = if cfg!(debug_assertions) { None } else { desktop::launcher() };
            // New files are sorted into Video / Audio / Documents / ... folders unless the user turns it off in Settings.
            cfg.organize_default = true;
            // yt-dlp, Deno and ffmpeg are fetched from their official servers the first time a video page needs
            // them. Debug builds (tests) only when asked, so a test run never downloads them.
            let fetch_tools = !cfg!(debug_assertions) || dev_only("VELOX_TOOLS").as_deref() == Some("official");
            if fetch_tools {
                cfg.tools = download_manager::ToolSources::official();
            }
            // The copies the installer downloaded next to the program.
            cfg.shared_tools_dir = shared_tools_dir();
            cfg.update = update_config();
            dev_overrides(&mut cfg);
            // The key that seals cookies and credentials in the database lives in the operating
            // system's credential store. Debug builds use a throw-away in-memory key unless
            // `VELOX_SECRETS=os`, so running the tests never touches anybody's real credentials.
            if !cfg!(debug_assertions) || dev_only("VELOX_SECRETS").as_deref() == Some("os") {
                cfg.key_store = std::sync::Arc::new(secrets::OsKeyStore::new("app.velox.fetch"));
            }
            let manager =
                tauri::async_runtime::block_on(DownloadManager::new(cfg)).map_err(|e| format!("cannot start the download manager: {e}"))?;

            // Forward manager events to the windows that are allowed to see them.
            let mut rx = manager.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let _ = handle.emit_filter(EVENT_CHANNEL, &ev, |target| match target {
                                EventTarget::WebviewWindow { label } | EventTarget::Window { label } | EventTarget::Webview { label } => {
                                    authz::may_receive_events(label)
                                }
                                _ => false,
                            });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            let data_dir = db_path_dir.clone();
            let front = app.handle().clone();
            let bridge = bridge::Bridge::start(
                &data_dir,
                listener,
                manager.clone(),
                app.path().resource_dir().ok(),
                Arc::new(move || bring_to_front(&front)),
            );
            let source: Box<dyn system_integration::clipboard::ClipboardSource> = match dev_only("VELOX_CLIPBOARD_FILE") {
                // Debug builds only (end-to-end tests): text from a file, so the user's real clipboard is never touched.
                Some(file) => Box::new(system_integration::clipboard::FileClipboard(PathBuf::from(file))),
                None => Box::<system_integration::clipboard::SystemClipboard>::default(),
            };
            let test_source = dev_only("VELOX_CLIPBOARD_FILE").is_some();
            let clipboard = clipboard::ClipboardWatch::start(app.handle().clone(), manager.clone(), source, test_source);
            smart_throttle::spawn(manager.clone());
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            tauri::async_runtime::spawn(preview_server::loopback::start(manager.clone()));
            if fetch_tools {
                // Whatever the installer could not get (no internet then, a source down) is fetched right after
                // start-up, and continued from where it stopped if the app was closed in the middle.
                let m = manager.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    m.prefetch_video_tools();
                });
            }
            // The tray first: whether there is one decides what closing the window and `--hidden` do.
            let tray = desktop::create_tray(app.handle(), &manager);
            manager.set_tray_available(tray);
            desktop::note_start_args(tray);
            manager.refresh_start_at_login();
            desktop::watch_for_notifications(app.handle().clone(), manager.clone());
            #[cfg(unix)]
            desktop::listen_for_show(app.handle().clone(), &data_dir);
            app.manage(AppState { manager, bridge, clipboard });
            create_main_window(app.handle())?;
            // First start: set the browser extension up for the default browser (once; see auto_install_once).
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                let resources = handle.path().resource_dir().ok();
                let task = tokio::task::spawn_blocking(move || handle.state::<AppState>().bridge.auto_install_once(resources.as_deref()));
                if let Err(e) = task.await {
                    tracing::warn!(error = %e, "setting up the browser extension stopped unexpectedly");
                    // the app goes on
                }
            });
            Ok(())
        })
        // The close button hides to the tray when the user wants that (and there is a tray).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == authz::MAIN {
                    if let Some(state) = window.try_state::<AppState>() {
                        if desktop::close_to_tray(window.app_handle(), &state.manager) {
                            api.prevent_close();
                        }
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            create_download,
            start_download,
            pause_download,
            resume_download,
            cancel_download,
            retry_download,
            redownload,
            use_new_link,
            delete_download,
            get_download,
            list_downloads,
            set_priority,
            set_speed_limit,
            get_download_detail,
            list_history,
            get_metrics,
            get_queue_state,
            set_queue_config,
            pause_queue,
            resume_queue,
            set_schedule,
            set_power_action,
            get_queues,
            save_queue,
            delete_queue,
            queue_action,
            place_download,
            set_keep_awake,
            get_browser_integration,
            set_browser_integration,
            install_browser_extension,
            list_pending_downloads,
            confirm_pending_download,
            dismiss_pending_download,
            set_browser_confirm,
            set_video_bar,
            window_control,
            preview_batch,
            add_batch,
            review_link,
            analyze_link,
            check_duplicate,
            get_clipboard_watch,
            set_clipboard_watch,
            analyze_media,
            start_media,
            list_media_jobs,
            cancel_media_job,
            pause_media_job,
            resume_media_job,
            clear_media_job,
            get_storage,
            set_organize,
            set_folder_names,
            reset_download_folder,
            pick_folder,
            choose_download_folder,
            get_network,
            set_network,
            get_multi_network,
            set_multi_network,
            set_download_credentials,
            show_in_folder,
            open_file,
            open_download_folder,
            forget_history,
            get_speed_limits,
            preview_pending_folder,
            get_download_diagnostics,
            get_logs,
            set_detailed_logging,
            get_diagnostics_report,
            open_log_folder,
            get_smart_throttle,
            get_profiles,
            profile_action,
            set_smart_throttle,
            set_preview_mode,
            get_preview_info,
            get_preview_base,
            get_video_tools,
            fetch_video_tools,
            get_desktop_options,
            set_desktop_options,
            get_update_status,
            check_for_updates,
            download_update,
            install_update,
            set_update_auto,
            open_release_page,
        ])
        .build(context)
        .expect("error while building the VeloxFetch application");

    app.run(move |handle, event| {
        // The broken main window is gone: put a fresh one in its place.
        if let RunEvent::WindowEvent { label, event: tauri::WindowEvent::Destroyed, .. } = &event {
            if label == authz::MAIN && REBUILDING.load(Ordering::SeqCst) {
                if let Err(e) = create_main_window(handle) {
                    tracing::error!(error = %e, "could not rebuild the window");
                }
                REBUILDING.store(false, Ordering::SeqCst);
            }
        }
        // macOS: a click on the Dock icon while the window is hidden in the tray brings it back.
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = &event {
            desktop::show_main(handle);
        }
        // Pause everything and let it save before the process ends, so no progress is lost.
        if let RunEvent::ExitRequested { api, code, .. } = event {
            // Closing the last window while it is being replaced is not a request to quit.
            if code.is_none() && (REBUILDING.load(Ordering::SeqCst) || handle.get_webview_window(authz::MAIN).is_some()) {
                api.prevent_exit();
                return;
            }
            desktop::quitting();
            if !shutting_down.swap(true, Ordering::SeqCst) {
                api.prevent_exit();
                let handle = handle.clone();
                let manager = handle.state::<AppState>().manager.clone();
                tauri::async_runtime::spawn(async move {
                    manager.shutdown().await;
                    handle.exit(0);
                });
            }
        }
    });
}

#[cfg(test)]
mod location_tests {
    use super::first_known;
    use std::path::PathBuf;

    #[test]
    fn a_missing_system_folder_falls_back_instead_of_failing() {
        let abs = std::env::temp_dir();
        let last = abs.join("last");
        assert_eq!(first_known("t", [Some(abs.join("a")), Some(abs.join("b")), None], last.clone()), abs.join("a"));
        assert_eq!(first_known("t", [None, Some(abs.join("b")), None], last.clone()), abs.join("b"));
        // a relative answer (empty HOME and the like) is not trusted
        assert_eq!(first_known("t", [Some(PathBuf::from("rel")), None, Some(abs.join("c"))], last.clone()), abs.join("c"));
        assert_eq!(first_known("t", [None, None, None], last.clone()), last);
    }
}

#[cfg(test)]
mod update_tests {
    use super::{RELEASE_PAGE, UPDATE_FEED, UPDATE_KEYS, UPDATE_PACKAGES};

    #[test]
    fn the_shipped_update_settings_are_valid() {
        // A typo in the keys file would turn updates off for everyone: it must parse.
        update_verify::TrustedKeys::parse(UPDATE_KEYS).unwrap();
        for u in [RELEASE_PAGE, UPDATE_FEED, UPDATE_PACKAGES] {
            assert!(u.starts_with("https://github.com/IlyesBs/VeloxFetch-releases/releases"), "{u}");
        }
        assert!(update_verify::Version::parse(env!("CARGO_PKG_VERSION")).is_some(), "the app's version must be x.y.z");
    }
}
