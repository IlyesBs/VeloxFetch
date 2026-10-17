// The only place the UI talks to Rust. Every function maps to exactly one narrowly-scoped Tauri
// command that takes an id or a structured DTO. There is deliberately no generic "run"/"read file"
// escape hatch: the frontend cannot touch the filesystem, network or OS directly.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getLang, t } from "./i18n";
import { MESSAGES } from "./messages";

export type WindowAction = "ready" | "minimize" | "toggleMaximize" | "close" | "isMaximized" | "themeLight" | "themeDark" | "themeSystem";
import type {
  ExtensionEvent,
  Analysis,
  DuplicateMatch,
  NewLinkOutcome,
  ProfileAction,
  Profiles,
  PreviewInfo,
  SmartThrottle,
  UpdateStatus,
  VideoTools,
  ApiError,
  BrowserIntegration,
  ExtensionInstall,
  BatchPreview,
  BatchRequest,
  BatchResult,
  ClipboardState,
  PendingDownload,
  PickedFolder,
  Storage,
  FileTarget,
  MultiNetwork,
  NetworkSettings,
  SetMultiNetwork,
  SetNetwork,
  SpeedLimits,
  BrowserName,
  Diagnostics,
  Download,
  DownloadDetail,
  Logs,
  HistoryEntry,
  ManagerEvent,
  MediaAnalysis,
  MediaJob,
  MediaRequest,
  Metrics,
  NewDownload,
  PowerAction,
  Priority,
  PlaceDownload,
  QueueAction,
  Queues,
  QueueState,
  SaveQueue,
  Schedule,
  SpeedTarget,
  DesktopPrefs,
  SetDesktopPrefs,
  SetSmartThrottle,
  SetGameMode,
} from "./types";

export const EVENT_CHANNEL = "velox://event";

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw normalizeError(e);
  }
}

export function normalizeError(e: unknown): ApiError {
  if (typeof e === "object" && e !== null && "code" in e && "message" in e) {
    const err = e as ApiError;
    return { ...err, message: localMessage(err.message) };
  }
  return { code: "internal", message: String(e) };
}

/** A message from Rust in the user's language when it is a known one (`messages.ts`); otherwise as it came. */
export function localMessage(message: string): string {
  if (getLang() === "en") return message;
  const key = MESSAGES[message.replace(/^invalid input: /, "")];
  return key ? t(key) : message;
}

export const api = {
  createDownload: (request: NewDownload) => call<Download>("create_download", { request }),
  startDownload: (id: number) => call<Download>("start_download", { id }),
  pauseDownload: (id: number) => call<Download>("pause_download", { id }),
  resumeDownload: (id: number) => call<Download>("resume_download", { id }),
  cancelDownload: (id: number) => call<Download>("cancel_download", { id }),
  retryDownload: (id: number) => call<Download>("retry_download", { id }),
  redownload: (id: number) => call<Download>("redownload", { id }),
  /** Continue a paused / failed download from a new link (`url`) or a link the browser handed over (`pending`). */
  useNewLink: (id: number, source: { url: string } | { pending: number }, mode: "auto" | "confirm" = "auto") =>
    call<NewLinkOutcome>("use_new_link", { request: { id, ...source, mode } }),
  deleteDownload: (id: number, deleteFile: boolean) => call<void>("delete_download", { id, deleteFile }),
  getDownload: (id: number) => call<Download>("get_download", { id }),
  getDownloadDetail: (id: number) => call<DownloadDetail>("get_download_detail", { id }),
  listDownloads: () => call<Download[]>("list_downloads", { filter: {} }),
  listHistory: (limit = 200) => call<HistoryEntry[]>("list_history", { limit }),
  getMetrics: () => call<Metrics>("get_metrics"),
  setPriority: (id: number, priority: Priority) => call<Download>("set_priority", { id, priority }),
  setSpeedLimit: (target: SpeedTarget, bytesPerSec: number | null) =>
    call<void>("set_speed_limit", { request: { target, bytesPerSec } }),
  getQueueState: () => call<QueueState>("get_queue_state"),
  setQueueConfig: (config: { maxActive?: number; maxConnections?: number }) =>
    call<QueueState>("set_queue_config", { request: config }),
  pauseQueue: (pauseRunning: boolean) => call<QueueState>("pause_queue", { request: { pauseRunning } }),
  resumeQueue: () => call<QueueState>("resume_queue"),
  setSchedule: (schedule: Schedule) => call<QueueState>("set_schedule", { schedule }),
  setPowerAction: (action: PowerAction | null) => call<QueueState>("set_power_action", { request: { action } }),
  setKeepAwake: (enabled: boolean) => call<QueueState>("set_keep_awake", { enabled }),
  getQueues: () => call<Queues>("get_queues"),
  saveQueue: (request: SaveQueue) => call<Queues>("save_queue", { request }),
  deleteQueue: (id: number) => call<Queues>("delete_queue", { id }),
  queueAction: (id: number, action: QueueAction) => call<Queues>("queue_action", { id, action }),
  placeDownload: (request: PlaceDownload) => call<Download>("place_download", { request }),
  getDesktopPrefs: () => call<DesktopPrefs>("get_desktop_options"),
  setDesktopPrefs: (request: SetDesktopPrefs) => call<DesktopPrefs>("set_desktop_options", { request }),
  getBrowserIntegration: () => call<BrowserIntegration>("get_browser_integration"),
  previewBatch: (request: BatchRequest) => call<BatchPreview>("preview_batch", { request }),
  addBatch: (request: BatchRequest) => call<BatchResult>("add_batch", { request }),
  reviewLink: (url: string) => call<unknown>("review_link", { url }),
  /** Download Intelligence for the Add dialog: a read-only check of a link. `null` cancels the check that runs. */
  analyzeLink: (url: string | null) => call<Analysis | null>("analyze_link", { request: { url } }),
  getProfiles: () => call<Profiles>("get_profiles"),
  profileAction: (request: ProfileAction) => call<Profiles>("profile_action", { request }),
  /** A download the app already has for this link (or checksum): read-only, no network. */
  checkDuplicate: (url: string, checksum?: string) => call<DuplicateMatch | null>("check_duplicate", { request: { url, ...(checksum && { checksum }) } }),
  getClipboardWatch: () => call<ClipboardState>("get_clipboard_watch"),
  setClipboardWatch: (enabled: boolean) => call<ClipboardState>("set_clipboard_watch", { enabled }),
  /** Look for the publisher's checksum next to each file and check the file against it (default on). */
  getPublisherCheck: () => call<boolean>("get_publisher_check"),
  setPublisherCheck: (enabled: boolean) => call<boolean>("set_publisher_check", { enabled }),
  analyzeMedia: (url: string, pending?: number) => call<MediaAnalysis>("analyze_media", { url, pending: pending ?? null }),
  startMedia: (request: MediaRequest) => call<MediaJob>("start_media", { request }),
  listMediaJobs: () => call<MediaJob[]>("list_media_jobs"),
  cancelMediaJob: (id: number) => call<void>("cancel_media_job", { id }),
  pauseMediaJob: (id: number) => call<void>("pause_media_job", { id }),
  resumeMediaJob: (id: number) => call<MediaJob>("resume_media_job", { id }),
  /** Remove a finished video download from the list; `deleteFiles` also deletes its saved files from the disk. */
  clearMediaJob: (id: number, deleteFiles = false) => call<void>("clear_media_job", { id, deleteFiles }),
  listPendingDownloads: () => call<PendingDownload[]>("list_pending_downloads"),
  /** `login`: the user name and password the site asks for (sealed with the download, never sent back). */
  confirmPendingDownload: (token: number, filename?: string, start = true, folderToken?: number, login?: { username: string; password: string }) =>
    call<Download>("confirm_pending_download", { request: { token, filename, start, folderToken, ...login } }),
  getStorage: () => call<Storage>("get_storage"),
  setOrganize: (enabled: boolean) => call<Storage>("set_organize", { enabled }),
  setFolderNames: (names: Record<string, string>) => call<Storage>("set_folder_names", { request: { names } }),
  resetDownloadFolder: () => call<Storage>("reset_download_folder"),
  /** Opens the native folder dialog. `null` = the user closed it. */
  pickFolder: () => call<PickedFolder | null>("pick_folder"),
  /** Opens the native folder dialog and makes the answer the default download folder. */
  chooseDownloadFolder: () => call<Storage | null>("choose_download_folder"),
  getNetwork: () => call<NetworkSettings>("get_network"),
  setNetwork: (request: SetNetwork) => call<NetworkSettings>("set_network", { request }),
  getMultiNetwork: () => call<MultiNetwork>("get_multi_network"),
  setMultiNetwork: (request: SetMultiNetwork) => call<MultiNetwork>("set_multi_network", { request }),
  getSpeedLimits: () => call<SpeedLimits>("get_speed_limits"),
  /** Give a download the user name / password its server asked for and start it again. */
  signIn: (id: number, username: string, password: string) => call<Download>("set_download_credentials", { request: { id, username, password } }),
  /** Where a waiting browser download would be saved under another name. */
  previewPendingFolder: (token: number, filename: string) => call<string>("preview_pending_folder", { request: { token, filename } }),
  showInFolder: (target: FileTarget) => call<void>("show_in_folder", { target }),
  openFile: (target: FileTarget) => call<void>("open_file", { target }),
  openDownloadFolder: () => call<void>("open_download_folder"),
  /** Remove one history line, or all of them (`null`). Files are not touched. */
  forgetHistory: (id: number | null) => call<number>("forget_history", { id }),
  /** Diagnostics: everything local, secrets removed in Rust before anything reaches the UI. */
  getDownloadDiagnostics: (id: number) => call<Diagnostics>("get_download_diagnostics", { id }),
  getSmartThrottle: () => call<SmartThrottle>("get_smart_throttle"),
  getVideoTools: () => call<VideoTools>("get_video_tools"),
  fetchVideoTools: () => call<VideoTools>("fetch_video_tools"),
  /** Updates: only Rust talks to the release server and checks the signature; the UI asks and shows. */
  getUpdateStatus: () => call<UpdateStatus>("get_update_status"),
  checkForUpdates: () => call<UpdateStatus>("check_for_updates"),
  downloadUpdate: () => call<UpdateStatus>("download_update"),
  /** "restart": the app closes and the new version takes over; "opened": the system's installer is open. */
  installUpdate: () => call<"restart" | "opened">("install_update"),
  setUpdateAuto: (enabled: boolean) => call<UpdateStatus>("set_update_auto", { enabled }),
  openReleasePage: () => call<void>("open_release_page"),
  setSmartThrottle: (request: SetSmartThrottle) => call<SmartThrottle>("set_smart_throttle", { request }),
  setGameMode: (request: SetGameMode) => call<SmartThrottle>("set_game_mode", { request }),
  setPreviewMode: (id: number, enabled: boolean) => call<PreviewInfo>("set_preview_mode", { id, enabled }),
  getPreviewInfo: (id: number) => call<PreviewInfo>("get_preview_info", { id }),
  getPreviewBase: () => call<string | null>("get_preview_base"),
  getLogs: (limit = 300, problemsOnly = false) => call<Logs>("get_logs", { limit, problemsOnly }),
  setDetailedLogging: (enabled: boolean) => call<void>("set_detailed_logging", { enabled }),
  getDiagnosticsReport: () => call<string>("get_diagnostics_report"),
  openLogFolder: () => call<void>("open_log_folder"),
  dismissPendingDownload: (token: number) => call<void>("dismiss_pending_download", { token }),
  setBrowserConfirm: (enabled: boolean) => call<void>("set_browser_confirm", { enabled }),
  setVideoBar: (enabled: boolean) => call<void>("set_video_bar", { enabled }),
  /** The title bar's buttons and the frame theme, on this window only. Resolves whether it is maximized. */
  windowControl: (action: WindowAction) => call<boolean>("window_control", { action }),
  installBrowserExtension: (browser: BrowserName | null) =>
    call<ExtensionInstall>("install_browser_extension", { request: { browser } }),
  setBrowserIntegration: (browser: BrowserName, enabled: boolean) =>
    call<BrowserIntegration>("set_browser_integration", { request: { browser, enabled } }),
};

/** Download links the user copied (only when clipboard detection is on). */
export function onClipboardLinks(handler: (links: string[]) => void): Promise<UnlistenFn> {
  return listen<{ links: string[] }>("velox://clipboard", (ev) => handler(ev.payload.links));
}

/** The browser extension reached the app, or the first start waits for it (main window). */
export function onExtensionEvent(handler: (e: ExtensionEvent) => void): Promise<UnlistenFn> {
  return listen<ExtensionEvent>("velox://extension", (ev) => handler(ev.payload));
}

export function onManagerEvent(handler: (e: ManagerEvent) => void): Promise<UnlistenFn> {
  return listen<ManagerEvent>(EVENT_CHANNEL, (ev) => handler(ev.payload));
}
