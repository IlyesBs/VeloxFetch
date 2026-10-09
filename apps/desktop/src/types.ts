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
  /** The Smart Download Queue it is in (1 = the main queue). */
  queueId: number;
  /** Its place in the queue (lower runs first, after priority). */
  position: number;
  /** Local time `YYYY-MM-DDTHH:MM` at which it starts on its own. */
  startAt: string | null;
  /** Running, but every connection is waiting to try again after an error. */
  retrying: boolean;
  /** Paused by its queue's rules: resumes on its own when the queue opens again. */
  held: boolean;
  /** Download Doctor: why it failed (or what the engine retries), where the problem is, what to do. */
  doctor: Diagnosis | null;
}

/** Download Doctor (crates/download-manager/src/doctor.rs): every field comes from observed evidence. */
export type DoctorCategory =
  | "offline" | "dns" | "timeout" | "connectionReset" | "connectionFailed" | "tls" | "authRequired" | "proxyAuth"
  | "accessDenied" | "sessionExpired" | "linkExpired" | "notFound" | "requestTimeout" | "conflict" | "rateLimited"
  | "serverTemporary" | "serverError" | "httpOther" | "redirect" | "rangeFailure" | "diskFull" | "permission"
  | "folderNotAllowed" | "writeFailed" | "fileExists" | "integrity" | "incomplete" | "persistence" | "webPage"
  | "invalidResource" | "proxyInvalid" | "cancelled" | "unknown";
export type DoctorSource = "server" | "network" | "thisComputer" | "link" | "account" | "content" | "unknown";
export type RetryClass = "retryable" | "retryableWithDelay" | "retryAfterUserAction" | "notRetryable" | "unknown";
export type DoctorSeverity = "info" | "warning" | "recoverable" | "blocking" | "critical";
export type DoctorAction =
  | "retry" | "retryLater" | "restartSafely" | "signIn" | "reauthInBrowser" | "newLink" | "freeSpace"
  | "checkConnection" | "checkProxy" | "checkFolder" | "cancel";
export type FactorCode =
  | "repeatedRetries" | "unstableConnection" | "serverThrottled" | "workedBefore" | "sessionUsed" | "temporaryLink"
  | "sameAsBefore" | "offline" | "startedOver" | "noResume";

export interface Diagnosis {
  category: DoctorCategory;
  code: string;
  source: DoctorSource;
  httpStatus: number | null;
  retry: RetryClass;
  /** The wait the server asked for (Retry-After), in seconds. */
  retryAfterSecs: number | null;
  /** While the engine retries on its own: when the next try starts (Unix ms). */
  retryAtMs: number | null;
  severity: DoctorSeverity;
  factors: { code: FactorCode; n: number | null }[];
  actions: DoctorAction[];
  neededBytes: number | null;
  freeBytes: number | null;
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
  /** Download Doctor's diagnosis of the last failure, while it is failed. */
  doctor: Diagnosis | null;
  /** Plain-text summary to paste in a bug report (already cleaned of secrets). */
  report: string;
}

export interface LogLine {
  at: number;
  level: "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE";
  target: string;
  text: string;
}

/** Settings > Appearance > On the desktop (tray, start at login, notifications). Mirrors `DesktopPrefsDto`. */
export interface DesktopPrefs {
  /** Closing the window keeps VeloxFetch running in the tray (when there is a tray). */
  closeToTray: boolean;
  /** Started in the tray when the user logs in (read from the system). */
  startAtLogin: boolean;
  /** A notification when a download finishes or fails while the window is not in front. */
  notifyFinished: boolean;
  language: string;
  /** This desktop shows tray icons (some Linux desktops do not). */
  trayAvailable: boolean;
  startAtLoginAvailable: boolean;
}

export interface SetDesktopPrefs {
  closeToTray?: boolean;
  startAtLogin?: boolean;
  notifyFinished?: boolean;
  language?: string;
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
  /** Automatic mode: give way while the user uses the internet, full speed when it is free. */
  auto: boolean;
  /** The least downloads keep (bytes/s): they never stop. */
  minBps: number;
  /** What it does now. */
  reason: ThrottleReason;
  /** The computer's other download traffic, when known. */
  otherBps: number | null;
  /** The user is away from the computer, when the system says. */
  userIdle: boolean | null;
  /** The best download speed seen on this connection. */
  peakBps: number | null;
  /** What the caps are a share of: what downloads got lately while free, at most the best speed seen. */
  referenceBps: number | null;
  /** What downloads do while a listed program (a game, a call) runs. */
  gamePolicy: GamePolicy;
  /** The speed limit of the "low" policy (bytes/s). */
  gameLimitBps: number;
  /** The "pause" policy holds the queue right now. */
  gameHold: boolean;
}

export type ThrottleReason = "off" | "fullIdle" | "fullFree" | "browsing" | "busy" | "app" | "easing" | "unknown";

export interface SetSmartThrottle {
  enabled?: boolean;
  apps?: string[];
  sharePercent?: number;
  auto?: boolean;
  minBps?: number;
  gamePolicy?: GamePolicy;
  gameLimitBps?: number;
}

/** While a listed program runs: the mode's share (adaptive), a fixed low limit, or pause. */
export type GamePolicy = "adaptive" | "low" | "pause";

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
  retrying: boolean;
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
  /** Keep the computer from sleeping on its own while downloads run (on by default). */
  keepAwake: boolean;
  /** It is being kept awake right now. */
  keptAwake: boolean;
  /** This system can do it (false once it failed here). */
  keepAwakeAvailable: boolean;
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
  /** Held because a listed program runs (Smart throttle's "pause" policy). */
  gameHold?: boolean;
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
  /** Download Intelligence: what the check before the download found (null while it runs, and for videos). */
  analysis: Analysis | null;
}

/** Download Intelligence (crates/download-manager/src/intelligence.rs): observations only, no score. */
export type ResourceKind = "file" | "webPage" | "stream" | "videoPage" | "unknown";
export type ResumeSupport = "supported" | "likelySupported" | "notSupported" | "unknown";
export type DownloadStrategy = "parallel" | "single" | "conservative" | "stream";
export type Access = "public" | "authRequired" | "browserSessionRequired" | "unknown";
export type StorageCheck = "ok" | "low" | "insufficient" | "tooLargeForDrive" | "cannotVerify" | "unavailable";
export type Health = "good" | "caution" | "problem" | "unknown";
export type FindingLevel = "info" | "warning" | "problem";
export type FindingCode =
  | "insecure"
  | "httpsDowngrade"
  | "otherSite"
  | "authRequired"
  | "sessionRequired"
  | "sessionSent"
  | "accessRefused"
  | "serverError"
  | "unreachable"
  | "slowServer"
  | "temporaryLink"
  | "expiredLink"
  | "webPage"
  | "noSpace"
  | "tooLargeForDrive"
  | "lowSpace"
  | "storageUnverified"
  | "noResume"
  | "resumeUnverified"
  | "unknownSize"
  | "duplicate"
  | "stream";

export interface Finding {
  code: FindingCode;
  level: FindingLevel;
  /** A host, an error code (`http_404`) or a file name, depending on the code. */
  detail: string | null;
  bytes: number | null;
  free: number | null;
  atMs: number | null;
}

export interface DuplicateMatch {
  /** null: only in the history (downloaded before, removed from the list since). */
  id: number | null;
  status: DownloadStatus | null;
  filename: string;
  reason: "sameLink" | "sameFinalLink" | "sameFile";
}

export interface Analysis {
  kind: ResourceKind;
  filename: string;
  size: number | null;
  contentType: string | null;
  https: boolean;
  redirected: boolean;
  /** Host the link finally led to (never the full address). */
  finalHost: string | null;
  rangeClaimed: boolean;
  rangeVerified: boolean;
  etag: boolean;
  lastModified: boolean;
  resume: ResumeSupport;
  strategy: DownloadStrategy;
  connections: number | null;
  access: Access;
  temporary: boolean;
  expiresAtMs: number | null;
  storage: StorageCheck;
  freeBytes: number | null;
  duplicate: DuplicateMatch | null;
  health: Health;
  /** Most serious first. */
  findings: Finding[];
  errorCode: string | null;
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
  | { type: "queues"; queues: Queues }
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
  /** The Smart Download Queue to put it in (default: the main queue). */
  queueId?: number;
  /** Local `YYYY-MM-DDTHH:MM`: added without starting, starts on its own then. */
  startAt?: string;
}

// ------------------------------------------------------------------ Smart Download Queue

/** When a queue may download. `days[0]` is Monday. */
export type QueueWhen =
  | { kind: "anytime" }
  | { kind: "between"; start: string; stop: string; days: boolean[] }
  | { kind: "startAt"; time: string; days: boolean[] };

export type ConnectionRule = "any" | "wifi" | "wired" | "notMobile";
export type ConnectionKind = "wifi" | "wired" | "mobile" | "other";

export interface QueueRules {
  when: QueueWhen;
  onlyWhenIdle: boolean;
  connection: ConnectionRule;
  /** Pause while other traffic is above this (bytes/s). */
  pauseAboveBps: number | null;
  /** Downloads bigger than this (bytes) move to this queue. */
  largeFilesOver: number | null;
}

/** Why a queue starts nothing right now. */
export type QueueGate = "open" | "paused" | "time" | "offline" | "connection" | "notIdle" | "busy";

export interface QueueSpan {
  from: string;
  to: string;
}

export interface QueueInfo {
  id: number;
  /** Empty for the main queue (named in the UI's language). */
  name: string;
  rules: QueueRules;
  paused: boolean;
  gate: QueueGate;
  opensAt: string | null;
  /** "Start queue" was pressed: it runs until it is empty. */
  forced: boolean;
  /** When it may download in the next 24 hours. */
  spans: QueueSpan[];
}

export interface Queues {
  queues: QueueInfo[];
  /** Smart Throttle is slowing downloads: Low-priority ones wait for a free line. */
  givingWay: boolean;
  idleKnown: boolean;
  online: boolean | null;
  connection: ConnectionKind | null;
  /** Local `YYYY-MM-DDTHH:MM` these were worked out at. */
  now: string;
}

export interface SaveQueue {
  id: number | null;
  name: string;
  rules: QueueRules;
}

export type QueueAction =
  | { kind: "start" }
  | { kind: "pause" }
  | { kind: "clearCompleted" }
  | { kind: "moveUp" }
  | { kind: "moveDown" }
  | { kind: "priority"; priority: Priority };

export type StartWhen = { kind: "now" } | { kind: "later" } | { kind: "at"; at: string };
export type QueueStep = "up" | "down" | "top";

export interface PlaceDownload {
  id: number;
  queueId?: number;
  start?: StartWhen;
  step?: QueueStep;
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
