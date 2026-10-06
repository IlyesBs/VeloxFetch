// Mirrors the DTOs in crates/download-manager/src/api.rs (serde camelCase).
// Keep in sync; a code generator (ts-rs / specta) is a later improvement.

export type Priority = "lowest" | "low" | "normal" | "high" | "highest";
export const PRIORITIES: Priority[] = ["lowest", "low", "normal", "high", "highest"];

export type DownloadStatus =
  | "queued"
  | "downloading"
  | "paused"
  | "verifying"
  | "completed"
  | "failed"
  | "cancelled";

export type SegmentStatus = "pending" | "downloading" | "paused" | "completed" | "failed" | "retrying" | "cancelled";

export interface Download {
  id: number;
  url: string;
  filename: string;
  destination: string;
  status: DownloadStatus;
  totalSize: number | null;
  downloaded: number;
  speedBps: number;
  etaSecs: number | null;
  connections: number;
  priority: Priority;
  category: string | null;
  speedLimit: number | null;
  resumable: boolean;
  /** What went wrong, in plain words. */
  error: string | null;
  /** The engine's own message behind `error` (details view). */
  errorDetail: string | null;
  /** What only the user can fix: offers the matching button. */
  needs: Needs | null;
  /** Stable short code of the error (`http_404`, `disk_full`, `timeout`...), for diagnostics. */
  errorCode: string | null;
  kind: FileKind;
  /** Queued but not running nor waiting for a slot (added with "Download later"): offer "Start". */
  idle: boolean;
}

export type Needs = "signIn" | "proxySignIn" | "space" | "folder";

/** One entry of a download's timeline. */
export interface DiagEvent {
  /** Unix milliseconds. */
  at: number;
  level: "info" | "warn" | "error";
  text: string;
}

/** What happened to one download, over every attempt. Secrets are never part of it. */
export interface Diagnostics {
  id: number;
  status: DownloadStatus;
  errorCode: string | null;
  error: string | null;
  /** The engine's message, with link credentials, query values and tokens hidden. */
  errorDetail: string | null;
  /** Times it was started (first start, resumes, retries). */
  attempts: number;
  /** Automatic retries (server checks, connections), over every attempt. */
  retries: number;
  /** The last HTTP status the server answered. */
  httpStatus: number | null;
  connections: number;
  peakConnections: number;
  /** Bytes received over the time spent transferring, over every attempt. */
  avgSpeedBps: number;
  activeSecs: number;
  transferred: number;
  events: DiagEvent[];
  /** Plain-text summary to paste in a bug report (already cleaned of secrets). */
  report: string;
}

export interface LogLine {
  at: number;
  level: "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE";
  target: string;
  text: string;
}

/** Settings > Speed > Smart throttle. */
export interface SmartThrottle {
  enabled: boolean;
  apps: string[];
  sharePercent: number;
  /** The listed program running right now (downloads are limited). */
  activeApp: string | null;
  limitBps: number | null;
  /** false: running programs could not be listed, so nothing is slowed down. */
  scanOk: boolean;
}

/** What the live preview can play so far. */
export interface PreviewInfo {
  id: number;
  contentType: string;
  total: number;
  readyFromStart: number;
  previewMode: boolean;
  complete: boolean;
}

export interface Logs {
  detailed: boolean;
  /** The log is also written to files on this computer. */
  fileLogging: boolean;
  lines: LogLine[];
}

export interface Segment {
  index: number;
  start: number;
  end: number;
  downloaded: number;
  status: SegmentStatus;
  retries: number;
}

export interface DownloadDetail {
  download: Download;
  segments: Segment[];
  mode: "pending" | "parallel" | "sequential";
  remaining: number | null;
  avgSpeedBps: number;
  elapsedSecs: number | null;
  retries: number;
  contentType: string | null;
  server: string | null;
  etag: string | null;
  lastModified: string | null;
  checksum: string | null;
}

export interface HistoryEntry {
  id: number;
  downloadId: number | null;
  url: string;
  filename: string;
  destination: string;
  size: number | null;
  outcome: string;
  finishedAt: number;
  kind: FileKind;
  /** The file is still where it was saved. */
  exists: boolean;
}

/** A saved file to show or open: a download of the list or a history line (by id, never by path). */
export type FileTarget = { kind: "download"; id: number } | { kind: "history"; id: number } | { kind: "media"; id: number };

export type ProxyMode = "system" | "none" | "manual";

export interface NetworkSettings {
  proxyMode: ProxyMode;
  proxyUrl: string | null;
  proxyUsername: string | null;
  /** A password is saved (it is never sent back). */
  proxyHasPassword: boolean;
  userAgent: string | null;
  defaultUserAgent: string;
  secretsProtected: boolean;
}

export interface SetNetwork {
  proxyMode: ProxyMode;
  proxyUrl?: string;
  proxyUsername?: string;
  /** Omitted = keep the saved one; "" = remove it. */
  proxyPassword?: string;
  userAgent?: string;
}

export interface SpeedLimits {
  global: number | null;
  byType: Partial<Record<FileKind, number>>;
}

export interface Metrics {
  speedBps: number;
  active: number;
  queued: number;
  paused: number;
  failed: number;
  completed: number;
  totalDownloaded: number;
  sessionDownloaded: number;
  diskFree: number | null;
  diskTotal: number | null;
  folderBytes: number | null;
  globalLimit: number | null;
  /** The whole computer's network speed, both ways (null until measured or where unavailable). */
  netDownBps: number | null;
  netUpBps: number | null;
  /** Read / write speed of the download folder's disk (null likewise). */
  diskReadBps: number | null;
  diskWriteBps: number | null;
  /** Number of that reading: a new one adds a point to the side panels' charts. */
  ioSample: number;
}

export interface Progress {
  id: number;
  downloaded: number;
  totalSize: number | null;
  speedBps: number;
  etaSecs: number | null;
  connections: number;
}

export interface Tick {
  metrics: Metrics;
  downloads: Progress[];
}

export type PowerAction = "shutdown" | "sleep" | "hibernate";
export type PowerPhase = "off" | "armed" | "countdown";

export interface PowerState {
  action: PowerAction | null;
  phase: PowerPhase;
  /** Unix ms at which the action runs (countdown only). */
  countdownEndsAtMs: number | null;
  graceSecs: number;
  supported: PowerAction[];
  lastError: string | null;
}

/** `days[0]` is Monday; a day refers to the day the window starts. */
export interface Schedule {
  enabled: boolean;
  start: string;
  stop: string;
  days: boolean[];
}

export interface QueueState {
  maxActive: number;
  maxConnections: number;
  paused: boolean;
  gateOpen: boolean;
  running: number;
  waiting: number;
  held: number;
  schedule: Schedule;
  scheduleOpen: boolean;
  /** Local time of the next window boundary, `YYYY-MM-DDTHH:MM`. */
  nextChange: string | null;
  power: PowerState;
}

export type BrowserName = "chrome" | "edge" | "brave" | "firefox";

export interface BrowserEntry {
  browser: BrowserName;
  registered: boolean;
  supported: boolean;
  isDefault: boolean;
  found: boolean;
}

export interface ExtensionInstall {
  browser: BrowserName;
  /** "store": the browser offers the published extension itself; "developer": load the folder in Developer mode. */
  method: "store" | "developer";
  /** Folder to pick in the browser (Load unpacked / temporary add-on). */
  folder: string;
  openedBrowser: boolean;
  openedFolder: boolean;
  integration: BrowserIntegration;
}

export interface BrowserIntegration {
  /** The local link to browsers is up. */
  linkRunning: boolean;
  /** The native host program exists next to the app. */
  hostPresent: boolean;
  browsers: BrowserEntry[];
  chromeExtensionId: string;
  firefoxExtensionId: string;
  /** Ask before a browser download starts. */
  confirmDownloads: boolean;
  videoBar: boolean;
  /** Cookies / credentials are sealed with a key from the OS credential store (false: kept in memory only). */
  secretsProtected: boolean;
}

/** A download a browser handed over, waiting for the user's decision. */
export interface PendingDownload {
  token: number;
  url: string;
  host: string;
  filename: string;
  size: number | null;
  contentType: string | null;
  resumable: boolean | null;
  saveDir: string;
  probing: boolean;
  incognito: boolean;
  createdAtMs: number;
  /** A video from the browser's "Download video" bar: chosen in the video dialog, not confirmed as a file. */
  media: "stream" | "site" | null;
}

/** Batch downloads (a list of links, or a pattern like https://host/file_[001-100].zip). */
export interface BatchRequest {
  links?: string;
  pattern?: string;
  subdir?: string;
  start?: boolean;
}

export type BatchItemState = { kind: "ok" } | { kind: "duplicateInBatch" } | { kind: "alreadyInList" } | { kind: "invalid"; reason: string };

export interface BatchItem {
  line: number;
  url: string;
  filename: string;
  state: BatchItemState;
}

export interface BatchPreview {
  total: number;
  ok: number;
  duplicates: number;
  existing: number;
  invalid: number;
  items: BatchItem[];
  truncated: boolean;
}

export interface BatchResult {
  added: number;
  duplicates: number;
  existing: number;
  invalid: number;
  failed: number;
}

export interface ClipboardState {
  /** The user turned clipboard detection on. */
  enabled: boolean;
  /** The operating system lets the app read the clipboard. */
  available: boolean;
}

export type ManagerEvent =
  | { type: "added"; download: Download }
  | { type: "updated"; download: Download }
  | { type: "tick"; tick: Tick }
  | { type: "removed"; id: number }
  | { type: "queue"; queue: QueueState }
  | { type: "pending"; pending: PendingDownload }
  | { type: "pendingRemoved"; token: number }
  | { type: "media"; job: MediaJob }
  | { type: "mediaRemoved"; id: number };

/** A quality of a stream (HLS / DASH). */
export interface StreamOption {
  id: string;
  label: string;
  bandwidth: number | null;
  width: number | null;
  height: number | null;
  codecs: string | null;
  group: string | null;
}

/** An audio or subtitle track of a stream. */
export interface TrackOption {
  id: string;
  label: string;
  language: string | null;
  group: string | null;
  default: boolean;
}

export interface MediaAnalysis {
  /** `site` = a video page read through yt-dlp (YouTube...). */
  kind: "hls" | "dash" | "site";
  title: string;
  durationSecs: number | null;
  /** Best first. */
  streams: StreamOption[];
  audio: TrackOption[];
  subtitles: TrackOption[];
  /** ffmpeg was found, so separate video and audio can be joined into one file. */
  combineAvailable: boolean;
}

export interface MediaRequest {
  url: string;
  stream?: string;
  /** A track id, `"none"`, or omitted for the default. */
  audio?: string;
  subtitles?: string[];
  subdir?: string;
  filename?: string;
  combine?: boolean;
  /** The file type: "mp4" | "mkv" | "webm", or (video pages) "mp3" | "m4a" | "opus" for only the sound. */
  format?: string;
  /** A video the browser handed over: its link and the browser's headers come from that request. */
  pending?: number;
}

export type MediaStatus = "preparing" | "downloading" | "combining" | "completed" | "failed" | "cancelled" | "paused";

export interface MediaJob {
  id: number;
  title: string;
  host: string;
  status: MediaStatus;
  bytes: number;
  segmentsDone: number;
  segmentsTotal: number;
  percent: number | null;
  folder: string;
  files: string[];
  notes: string[];
  error: string | null;
  /** A video page read through yt-dlp: there are no pieces to count, only a percentage. */
  site: boolean;
  speedBps: number;
  etaSecs: number | null;
  /** Paused or failed with its pieces kept: Resume / Retry continues from them. */
  resumable: boolean;
  /** Can be watched now (the part downloaded so far, or the finished file). */
  playable: boolean;
}

/** What a downloaded file is, for sorting it into a folder. */
export type FileKind = "video" | "audio" | "image" | "document" | "archive" | "program" | "other";

export interface FolderName {
  kind: FileKind;
  /** The folder this kind goes into. */
  folder: string;
  defaultFolder: string;
}

/** Where downloads go and how they are sorted. */
export interface Storage {
  /** The folder new downloads go to. */
  root: string;
  /** The folder the app started with. */
  defaultRoot: string;
  isDefault: boolean;
  /** New files are sorted into a folder per type. */
  organize: boolean;
  folders: FolderName[];
}

/** A folder chosen in the native dialog. The UI keeps the token; the path is for display only. */
export interface PickedFolder {
  token: number;
  path: string;
}

export interface NewDownload {
  url: string;
  /** A folder picked in the native dialog (see `api.pickFolder`). */
  folderToken?: number;
  subdir?: string;
  filename?: string;
  priority?: Priority;
  category?: string;
  speedLimit?: number;
  checksum?: string;
  start?: boolean;
  referrer?: string;
  cookies?: string;
  userAgent?: string;
  username?: string;
  password?: string;
}

export type SpeedTarget =
  | { kind: "download"; id: number }
  | { kind: "global" }
  | { kind: "category"; name: string };

export interface ApiError {
  code: "invalid_input" | "not_found" | "invalid_state" | "engine" | "internal" | "forbidden";
  message: string;
}

/** One helper program for video sites (fetched by VeloxFetch itself). */
export interface ToolStatus {
  installed: boolean;
  fetching: boolean;
  canFetch: boolean;
  error: string | null;
  /** While fetching: bytes received and the file's size (0 = not known yet). */
  received: number;
  total: number;
}

export interface VideoTools {
  ytdlp: ToolStatus;
  deno: ToolStatus;
  ffmpeg: ToolStatus;
}
