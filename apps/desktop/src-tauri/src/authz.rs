//! Per-window authorization.
//!
//! Defense in depth: Tauri capabilities (`capabilities/*.json`) already decide which window may
//! call which command. This matrix repeats the decision in Rust, so a mistake in a capability file
//! (or a window that gets a broader grant later) still cannot widen access on its own. A test
//! keeps the two in lock-step.

/// Window labels.
pub const MAIN: &str = "main";
pub const SETTINGS: &str = "settings";
pub const DOWNLOAD_DETAIL: &str = "download-detail";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    CreateDownload,
    StartDownload,
    PauseDownload,
    ResumeDownload,
    CancelDownload,
    RetryDownload,
    Redownload,
    UseNewLink,
    DeleteDownload,
    GetDownload,
    ListDownloads,
    SetPriority,
    SetSpeedLimit,
    GetDownloadDetail,
    ListHistory,
    GetMetrics,
    GetQueueState,
    SetQueueConfig,
    PauseQueue,
    ResumeQueue,
    SetSchedule,
    SetPowerAction,
    SetKeepAwake,
    GetDesktopOptions,
    SetDesktopOptions,
    GetBrowserIntegration,
    SetBrowserIntegration,
    InstallBrowserExtension,
    ListPendingDownloads,
    ConfirmPendingDownload,
    DismissPendingDownload,
    SetBrowserConfirm,
    PreviewBatch,
    AddBatch,
    ReviewLink,
    AnalyzeLink,
    CheckDuplicate,
    GetClipboardWatch,
    SetClipboardWatch,
    AnalyzeMedia,
    StartMedia,
    ListMediaJobs,
    CancelMediaJob,
    PauseMediaJob,
    ResumeMediaJob,
    ClearMediaJob,
    GetStorage,
    SetOrganize,
    SetFolderNames,
    ResetDownloadFolder,
    PickFolder,
    ChooseDownloadFolder,
    GetNetwork,
    SetNetwork,
    GetMultiNetwork,
    SetMultiNetwork,
    SetDownloadCredentials,
    ShowInFolder,
    OpenFile,
    OpenDownloadFolder,
    ForgetHistory,
    GetSpeedLimits,
    PreviewPendingFolder,
    GetDownloadDiagnostics,
    GetLogs,
    SetDetailedLogging,
    GetDiagnosticsReport,
    OpenLogFolder,
    GetSmartThrottle,
    GetProfiles,
    ProfileAction,
    SetSmartThrottle,
    SetPreviewMode,
    GetPreviewInfo,
    GetPreviewBase,
    GetVideoTools,
    FetchVideoTools,
    SetVideoBar,
    WindowControl,
    GetQueues,
    SaveQueue,
    DeleteQueue,
    QueueAction,
    PlaceDownload,
    GetUpdateStatus,
    CheckForUpdates,
    DownloadUpdate,
    InstallUpdate,
    SetUpdateAuto,
    OpenReleasePage,
}

impl Command {
    #[cfg(test)]
    pub const ALL: [Command; 90] = [
        Command::CreateDownload,
        Command::StartDownload,
        Command::PauseDownload,
        Command::ResumeDownload,
        Command::CancelDownload,
        Command::RetryDownload,
        Command::Redownload,
        Command::UseNewLink,
        Command::DeleteDownload,
        Command::GetDownload,
        Command::ListDownloads,
        Command::SetPriority,
        Command::SetSpeedLimit,
        Command::GetDownloadDetail,
        Command::ListHistory,
        Command::GetMetrics,
        Command::GetQueueState,
        Command::SetQueueConfig,
        Command::PauseQueue,
        Command::ResumeQueue,
        Command::SetSchedule,
        Command::SetPowerAction,
        Command::SetKeepAwake,
        Command::GetDesktopOptions,
        Command::SetDesktopOptions,
        Command::GetBrowserIntegration,
        Command::SetBrowserIntegration,
        Command::InstallBrowserExtension,
        Command::ListPendingDownloads,
        Command::ConfirmPendingDownload,
        Command::DismissPendingDownload,
        Command::SetBrowserConfirm,
        Command::PreviewBatch,
        Command::AddBatch,
        Command::ReviewLink,
        Command::AnalyzeLink,
        Command::CheckDuplicate,
        Command::GetClipboardWatch,
        Command::SetClipboardWatch,
        Command::AnalyzeMedia,
        Command::StartMedia,
        Command::ListMediaJobs,
        Command::CancelMediaJob,
        Command::PauseMediaJob,
        Command::ResumeMediaJob,
        Command::ClearMediaJob,
        Command::GetStorage,
        Command::SetOrganize,
        Command::SetFolderNames,
        Command::ResetDownloadFolder,
        Command::PickFolder,
        Command::ChooseDownloadFolder,
        Command::GetNetwork,
        Command::SetNetwork,
        Command::GetMultiNetwork,
        Command::SetMultiNetwork,
        Command::SetDownloadCredentials,
        Command::ShowInFolder,
        Command::OpenFile,
        Command::OpenDownloadFolder,
        Command::ForgetHistory,
        Command::GetSpeedLimits,
        Command::PreviewPendingFolder,
        Command::GetDownloadDiagnostics,
        Command::GetLogs,
        Command::SetDetailedLogging,
        Command::GetDiagnosticsReport,
        Command::OpenLogFolder,
        Command::GetSmartThrottle,
        Command::GetProfiles,
        Command::ProfileAction,
        Command::SetSmartThrottle,
        Command::SetPreviewMode,
        Command::GetPreviewInfo,
        Command::GetPreviewBase,
        Command::GetVideoTools,
        Command::FetchVideoTools,
        Command::SetVideoBar,
        Command::WindowControl,
        Command::GetQueues,
        Command::SaveQueue,
        Command::DeleteQueue,
        Command::QueueAction,
        Command::PlaceDownload,
        Command::GetUpdateStatus,
        Command::CheckForUpdates,
        Command::DownloadUpdate,
        Command::InstallUpdate,
        Command::SetUpdateAuto,
        Command::OpenReleasePage,
    ];

    /// The Rust/JS command name, e.g. `create_download`.
    pub fn name(self) -> &'static str {
        match self {
            Command::CreateDownload => "create_download",
            Command::StartDownload => "start_download",
            Command::PauseDownload => "pause_download",
            Command::ResumeDownload => "resume_download",
            Command::CancelDownload => "cancel_download",
            Command::RetryDownload => "retry_download",
            Command::Redownload => "redownload",
            Command::UseNewLink => "use_new_link",
            Command::DeleteDownload => "delete_download",
            Command::GetDownload => "get_download",
            Command::ListDownloads => "list_downloads",
            Command::SetPriority => "set_priority",
            Command::SetSpeedLimit => "set_speed_limit",
            Command::GetDownloadDetail => "get_download_detail",
            Command::ListHistory => "list_history",
            Command::GetMetrics => "get_metrics",
            Command::GetQueueState => "get_queue_state",
            Command::SetQueueConfig => "set_queue_config",
            Command::PauseQueue => "pause_queue",
            Command::ResumeQueue => "resume_queue",
            Command::SetSchedule => "set_schedule",
            Command::SetPowerAction => "set_power_action",
            Command::SetKeepAwake => "set_keep_awake",
            Command::GetDesktopOptions => "get_desktop_options",
            Command::SetDesktopOptions => "set_desktop_options",
            Command::GetBrowserIntegration => "get_browser_integration",
            Command::SetBrowserIntegration => "set_browser_integration",
            Command::InstallBrowserExtension => "install_browser_extension",
            Command::ListPendingDownloads => "list_pending_downloads",
            Command::ConfirmPendingDownload => "confirm_pending_download",
            Command::DismissPendingDownload => "dismiss_pending_download",
            Command::SetBrowserConfirm => "set_browser_confirm",
            Command::PreviewBatch => "preview_batch",
            Command::AddBatch => "add_batch",
            Command::ReviewLink => "review_link",
            Command::AnalyzeLink => "analyze_link",
            Command::CheckDuplicate => "check_duplicate",
            Command::GetClipboardWatch => "get_clipboard_watch",
            Command::SetClipboardWatch => "set_clipboard_watch",
            Command::AnalyzeMedia => "analyze_media",
            Command::StartMedia => "start_media",
            Command::ListMediaJobs => "list_media_jobs",
            Command::CancelMediaJob => "cancel_media_job",
            Command::PauseMediaJob => "pause_media_job",
            Command::ResumeMediaJob => "resume_media_job",
            Command::ClearMediaJob => "clear_media_job",
            Command::GetStorage => "get_storage",
            Command::SetOrganize => "set_organize",
            Command::SetFolderNames => "set_folder_names",
            Command::ResetDownloadFolder => "reset_download_folder",
            Command::PickFolder => "pick_folder",
            Command::ChooseDownloadFolder => "choose_download_folder",
            Command::GetNetwork => "get_network",
            Command::SetNetwork => "set_network",
            Command::GetMultiNetwork => "get_multi_network",
            Command::SetMultiNetwork => "set_multi_network",
            Command::SetDownloadCredentials => "set_download_credentials",
            Command::ShowInFolder => "show_in_folder",
            Command::OpenFile => "open_file",
            Command::OpenDownloadFolder => "open_download_folder",
            Command::ForgetHistory => "forget_history",
            Command::GetSpeedLimits => "get_speed_limits",
            Command::PreviewPendingFolder => "preview_pending_folder",
            Command::GetDownloadDiagnostics => "get_download_diagnostics",
            Command::GetLogs => "get_logs",
            Command::SetDetailedLogging => "set_detailed_logging",
            Command::GetDiagnosticsReport => "get_diagnostics_report",
            Command::OpenLogFolder => "open_log_folder",
            Command::GetSmartThrottle => "get_smart_throttle",
            Command::GetProfiles => "get_profiles",
            Command::ProfileAction => "profile_action",
            Command::SetSmartThrottle => "set_smart_throttle",
            Command::SetPreviewMode => "set_preview_mode",
            Command::GetPreviewInfo => "get_preview_info",
            Command::GetPreviewBase => "get_preview_base",
            Command::GetVideoTools => "get_video_tools",
            Command::FetchVideoTools => "fetch_video_tools",
            Command::SetVideoBar => "set_video_bar",
            Command::WindowControl => "window_control",
            Command::GetQueues => "get_queues",
            Command::SaveQueue => "save_queue",
            Command::DeleteQueue => "delete_queue",
            Command::QueueAction => "queue_action",
            Command::PlaceDownload => "place_download",
            Command::GetUpdateStatus => "get_update_status",
            Command::CheckForUpdates => "check_for_updates",
            Command::DownloadUpdate => "download_update",
            Command::InstallUpdate => "install_update",
            Command::SetUpdateAuto => "set_update_auto",
            Command::OpenReleasePage => "open_release_page",
        }
    }

    /// The capability permission id that grants this command, e.g. `allow-create-download`.
    #[cfg(test)]
    pub fn permission(self) -> String {
        format!("allow-{}", self.name().replace('_', "-"))
    }
}

/// May the window with this label call `cmd`? Unknown windows may call nothing.
pub fn allowed(window: &str, cmd: Command) -> bool {
    use Command::*;
    match window {
        MAIN => true,
        SETTINGS => matches!(cmd, SetSpeedLimit),
        DOWNLOAD_DETAIL => {
            matches!(
                cmd,
                GetDownload
                    | GetDownloadDetail
                    | PauseDownload
                    | ResumeDownload
                    | CancelDownload
                    | RetryDownload
                    | SetPriority
                    | SetSpeedLimit
            )
        }
        _ => false,
    }
}

/// Which windows are sent download events.
pub fn may_receive_events(window: &str) -> bool {
    matches!(window, MAIN | DOWNLOAD_DETAIL)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::*;

    fn capability_files() -> Vec<serde_json::Value> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut v: Vec<serde_json::Value> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
            .collect();
        v.sort_by_key(|c| c["identifier"].as_str().unwrap().to_string());
        v
    }

    #[test]
    fn unknown_windows_can_do_nothing() {
        for cmd in Command::ALL {
            assert!(!allowed("evil", cmd) && !allowed("", cmd) && !allowed("Main", cmd));
        }
        assert!(!may_receive_events("settings") && !may_receive_events("evil"));
    }

    #[test]
    fn destructive_commands_are_main_window_only() {
        for cmd in [
            Command::CreateDownload,
            Command::DeleteDownload,
            Command::StartDownload,
            Command::Redownload,
            Command::UseNewLink,
            Command::ListDownloads,
            Command::ListHistory,
            Command::GetMetrics,
            Command::GetQueueState,
            Command::SetQueueConfig,
            Command::PauseQueue,
            Command::ResumeQueue,
            Command::SetSchedule,
            Command::SetPowerAction,
            Command::SetKeepAwake,
            Command::GetQueues,
            Command::SaveQueue,
            Command::DeleteQueue,
            Command::QueueAction,
            Command::PlaceDownload,
            Command::GetDesktopOptions,
            Command::SetDesktopOptions,
            Command::GetBrowserIntegration,
            Command::SetBrowserIntegration,
            Command::InstallBrowserExtension,
            Command::ListPendingDownloads,
            Command::ConfirmPendingDownload,
            Command::DismissPendingDownload,
            Command::SetBrowserConfirm,
            Command::PreviewBatch,
            Command::AddBatch,
            Command::ReviewLink,
            Command::AnalyzeLink,
            Command::CheckDuplicate,
            Command::GetClipboardWatch,
            Command::SetClipboardWatch,
            Command::AnalyzeMedia,
            Command::StartMedia,
            Command::ListMediaJobs,
            Command::CancelMediaJob,
            Command::PauseMediaJob,
            Command::ResumeMediaJob,
            Command::ClearMediaJob,
            Command::GetStorage,
            Command::SetOrganize,
            Command::SetFolderNames,
            Command::ResetDownloadFolder,
            Command::PickFolder,
            Command::ChooseDownloadFolder,
            Command::GetNetwork,
            Command::SetNetwork,
            Command::GetMultiNetwork,
            Command::SetMultiNetwork,
            Command::SetDownloadCredentials,
            Command::ShowInFolder,
            Command::OpenFile,
            Command::OpenDownloadFolder,
            Command::ForgetHistory,
            Command::GetSpeedLimits,
            Command::PreviewPendingFolder,
            Command::GetDownloadDiagnostics,
            Command::GetLogs,
            Command::SetDetailedLogging,
            Command::GetDiagnosticsReport,
            Command::OpenLogFolder,
            Command::GetSmartThrottle,
            Command::GetProfiles,
            Command::ProfileAction,
            Command::SetSmartThrottle,
            Command::SetPreviewMode,
            Command::GetPreviewInfo,
            Command::GetPreviewBase,
            Command::GetVideoTools,
            Command::FetchVideoTools,
            Command::SetVideoBar,
            Command::WindowControl,
            Command::GetUpdateStatus,
            Command::CheckForUpdates,
            Command::DownloadUpdate,
            Command::InstallUpdate,
            Command::SetUpdateAuto,
            Command::OpenReleasePage,
        ] {
            assert!(allowed(MAIN, cmd));
            assert!(!allowed(SETTINGS, cmd) && !allowed(DOWNLOAD_DETAIL, cmd), "{cmd:?}");
        }
        assert!(allowed(SETTINGS, Command::SetSpeedLimit) && !allowed(SETTINGS, Command::CancelDownload));
    }

    /// The capability files and the Rust matrix must describe exactly the same access.
    #[test]
    fn capability_files_match_the_authorization_matrix() {
        let caps = capability_files();
        assert_eq!(caps.len(), 3, "expected main / settings / download capability files");
        for cap in &caps {
            let windows: Vec<&str> = cap["windows"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
            assert_eq!(windows.len(), 1, "one capability per window: {}", cap["identifier"]);
            let granted: BTreeSet<String> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|p| p.as_str())
                .filter(|p| p.starts_with("allow-"))
                .map(str::to_string)
                .collect();
            let expected: BTreeSet<String> = Command::ALL.iter().filter(|c| allowed(windows[0], **c)).map(|c| c.permission()).collect();
            assert_eq!(granted, expected, "capability {} disagrees with authz.rs", cap["identifier"]);
        }
    }

    /// Least privilege: only our own commands plus event listening and showing the window. Nothing broad, no plugins.
    #[test]
    fn capabilities_grant_no_broad_or_dangerous_permissions() {
        // The UI's own title bar (no system one): dragging it and double-click to maximize. Its buttons, and showing
        // the window after the first paint (Rust decides: not when started hidden in the tray), go through our own
        // `window_control` command, which acts on the calling window only.
        let allowed_core: BTreeSet<&str> = [
            "core:event:allow-listen",
            "core:event:allow-unlisten",
            "core:window:allow-start-dragging",
            "core:window:allow-internal-toggle-maximize",
        ]
        .into();
        for cap in capability_files() {
            for w in cap["windows"].as_array().unwrap() {
                let w = w.as_str().unwrap();
                assert!(!w.contains('*'), "wildcard window in {}", cap["identifier"]);
            }
            assert!(cap.get("remote").is_none(), "no remote (web) origins may be granted IPC access");
            for p in cap["permissions"].as_array().unwrap() {
                let p = p.as_str().expect("permissions are plain identifiers without broad scopes");
                assert!(p.starts_with("allow-") || allowed_core.contains(p), "unexpected permission `{p}` in {}", cap["identifier"]);
                assert!(
                    !p.contains("default") && !p.contains("fs") && !p.contains("shell") && !p.contains("http") && !p.contains("opener")
                );
            }
        }
    }

    /// Every command the app registers has an allow-permission generated in build.rs.
    #[test]
    fn build_manifest_lists_every_command() {
        let build = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("build.rs")).unwrap();
        for cmd in Command::ALL {
            assert!(build.contains(&format!("\"{}\"", cmd.name())), "{} missing from build.rs", cmd.name());
        }
    }

    #[test]
    fn csp_is_strict() {
        let conf: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json")).unwrap())
                .unwrap();
        let csp = conf["app"]["security"]["csp"].as_object().expect("production CSP is a directive map");
        assert_eq!(csp["default-src"], "'self'");
        assert_eq!(csp["script-src"], "'self'");
        assert_eq!(csp["style-src"], "'self'");
        assert!(csp["connect-src"].as_str().unwrap().split(' ').all(|s| s == "ipc:" || s == "http://ipc.localhost"));
        // Media only from the app's own preview protocol (Windows serves it as http://velox-preview.localhost).
        assert!(csp["media-src"].as_str().unwrap().split(' ').all(|s| s == "velox-preview:" || s == "http://velox-preview.localhost"));
        for (k, v) in csp {
            let v = v.as_str().unwrap();
            assert!(!v.contains('*') && !v.contains("unsafe-eval") && !v.contains("unsafe-inline"), "{k}: {v}");
            let local =
                v.split(' ').filter(|s| s.contains("http")).all(|s| s == "http://ipc.localhost" || s == "http://velox-preview.localhost");
            assert!(local, "remote content in {k}");
        }
        assert_eq!(conf["app"]["security"]["freezePrototype"], true);
        assert_eq!(conf["app"]["withGlobalTauri"], false, "no global Tauri object for injected scripts");
    }
}
