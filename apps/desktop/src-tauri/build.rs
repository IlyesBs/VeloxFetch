// Declaring the app's commands here makes Tauri generate one `allow-<command>` /
// `deny-<command>` permission per command. Capabilities (see ./capabilities) then grant each
// window exactly the commands it needs, instead of exposing everything to everyone.
const COMMANDS: &[&str] = &[
    "create_download",
    "start_download",
    "pause_download",
    "resume_download",
    "cancel_download",
    "retry_download",
    "delete_download",
    "get_download",
    "list_downloads",
    "set_priority",
    "set_speed_limit",
    "get_download_detail",
    "list_history",
    "get_metrics",
    "get_queue_state",
    "set_queue_config",
    "pause_queue",
    "resume_queue",
    "set_schedule",
    "set_power_action",
    "set_keep_awake",
    "get_queues",
    "save_queue",
    "delete_queue",
    "queue_action",
    "place_download",
    "get_browser_integration",
    "set_browser_integration",
    "install_browser_extension",
    "list_pending_downloads",
    "confirm_pending_download",
    "dismiss_pending_download",
    "set_browser_confirm",
    "preview_batch",
    "add_batch",
    "review_link",
    "analyze_link",
    "check_duplicate",
    "get_clipboard_watch",
    "set_clipboard_watch",
    "analyze_media",
    "start_media",
    "list_media_jobs",
    "cancel_media_job",
    "pause_media_job",
    "resume_media_job",
    "clear_media_job",
    "get_storage",
    "set_organize",
    "set_folder_names",
    "reset_download_folder",
    "pick_folder",
    "choose_download_folder",
    "get_network",
    "set_network",
    "set_download_credentials",
    "show_in_folder",
    "open_file",
    "open_download_folder",
    "forget_history",
    "get_speed_limits",
    "preview_pending_folder",
    "get_download_diagnostics",
    "get_logs",
    "set_detailed_logging",
    "get_diagnostics_report",
    "open_log_folder",
    "get_smart_throttle",
    "get_profiles",
    "profile_action",
    "set_smart_throttle",
    "set_preview_mode",
    "get_preview_info",
    "get_preview_base",
    "get_video_tools",
    "fetch_video_tools",
    "set_video_bar",
    "window_control",
    "get_desktop_options",
    "set_desktop_options",
];

/// The UI's locale files, embedded so the few texts Rust shows itself (tray menu, notifications) use the same words
/// in the same languages. Every file in the folder is picked up, like the UI does.
fn embed_locales() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/locales");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("the UI's locale folder")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut out = String::from("/// `(language code, locale file text)` for every UI language.\npub const LOCALES: &[(&str, &str)] = &[\n");
    for f in files {
        let code = f.file_stem().unwrap().to_string_lossy().into_owned();
        let path = f.canonicalize().unwrap();
        out.push_str(&format!("    ({code:?}, include_str!({:?})),\n", path.to_string_lossy()));
    }
    out.push_str("];\n");
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("locales.rs"), out).unwrap();
}

fn main() {
    embed_locales();
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)))
        .expect("failed to run tauri-build");
}
