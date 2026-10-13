import { getLang, t, type Key } from "./i18n";
const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** 1536 -> "1.5 KB". Decimal-looking units, binary maths (what OS file managers show). */
export function formatBytes(n: number | null | undefined, digits = 1): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return "—";
  let v = Math.max(0, n);
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  return `${i === 0 ? Math.round(v) : v.toFixed(v >= 100 ? 0 : digits)} ${UNITS[i]}`;
}

export function formatSpeed(bps: number): string {
  return bps > 0 ? `${formatBytes(bps)}/s` : "0 B/s";
}

/** 92 -> "1:32", 3725 -> "1:02:05", null -> "—". */
export function formatEta(secs: number | null | undefined): string {
  if (secs === null || secs === undefined) return "—";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = Math.floor(secs % 60);
  const pad = (v: number) => String(v).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

const HTTP_NAMED = [206, 304, 400, 401, 403, 404, 407, 408, 410, 416, 429, 500, 502, 503, 504];

/** 404 -> "404 Not found"; null -> "—". */
export function formatHttpStatus(code: number | null | undefined): string {
  if (code === null || code === undefined) return "—";
  const name = code === 200 ? "OK" : HTTP_NAMED.includes(code) ? t(`http.${code}` as Key) : undefined;
  return name ? `${code} ${name}` : String(code);
}

/** 42 -> "42 s", 3725 -> "1 h 2 min". */
export function formatDuration(secs: number): string {
  if (!Number.isFinite(secs) || secs < 1) return t("dur.under");
  if (secs < 60) return t("dur.s", { s: Math.round(secs) });
  if (secs < 3600) return t("dur.ms", { m: Math.floor(secs / 60), s: Math.round(secs % 60) });
  return t("dur.hm", { h: Math.floor(secs / 3600), m: Math.floor((secs % 3600) / 60) });
}

export function percent(done: number, total: number | null | undefined): number {
  if (!total || total <= 0) return 0;
  return Math.min(100, Math.max(0, (done / total) * 100));
}

/** A share as the app shows it: one decimal ("12.0%"), never the raw number ("11.992703192112195%"). */
export function formatPercent(done: number, total: number | null | undefined): string {
  return `${percent(done, total).toFixed(1)}%`;
}

/** 0..1 fraction, clamped. */
export function fraction(done: number, total: number | null | undefined): number {
  return percent(done, total) / 100;
}

export function formatRelative(unixSecs: number, nowMs = Date.now()): string {
  const diff = Math.max(0, Math.round(nowMs / 1000 - unixSecs));
  if (diff < 45) return t("rel.now");
  if (diff < 3600) return t("rel.min", { n: Math.round(diff / 60) });
  if (diff < 86400) return t("rel.h", { n: Math.round(diff / 3600) });
  if (diff < 86400 * 30) return t("rel.d", { n: Math.round(diff / 86400) });
  return new Date(unixSecs * 1000).toLocaleDateString(getLang());
}

/** "500K", "2M", "1.5M", "0" -> bytes per second, or null when empty/invalid/zero. */
export function parseRate(input: string): number | null {
  const m = /^\s*(\d+(?:\.\d+)?)\s*([kmg]?)(?:b\/?s?)?\s*$/i.exec(input);
  if (!m) return null;
  const mult = { "": 1, k: 1024, m: 1024 ** 2, g: 1024 ** 3 }[m[2]!.toLowerCase() as "" | "k" | "m" | "g"];
  const v = Math.round(Number(m[1]) * mult);
  return v > 0 ? v : null;
}

export function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/**
 * Video pages read through yt-dlp: `[host suffix, path parts that mark a video page]` (empty = any page
 * but the home page). Mirrors `VIDEO_SITES` in crates/media-engine/src/site.rs.
 */
const VIDEO_SITES: [string, string[]][] = [
  ["youtube.com", ["/watch", "/shorts/", "/embed/", "/live/"]],
  ["youtu.be", []],
  ["youtube-nocookie.com", ["/embed/"]],
  ["vimeo.com", []],
  ["dailymotion.com", ["/video/"]],
  ["dai.ly", []],
  ["twitch.tv", ["/videos/", "/clip/"]],
  ["tiktok.com", ["/video/"]],
  ["x.com", ["/status/"]],
  ["twitter.com", ["/status/"]],
  ["facebook.com", ["/watch", "/videos/", "/reel/"]],
  ["fb.watch", []],
  ["instagram.com", ["/p/", "/reel/", "/tv/"]],
  ["reddit.com", ["/comments/"]],
  ["streamable.com", []],
  ["rumble.com", []],
  ["bilibili.com", ["/video/"]],
  ["soundcloud.com", []],
];

/** A video page on YouTube, Vimeo and similar sites (downloaded through yt-dlp). */
export function isVideoPage(url: string): boolean {
  try {
    const u = new URL(url.trim());
    if (u.protocol !== "http:" && u.protocol !== "https:") return false;
    const host = u.hostname.toLowerCase();
    return VIDEO_SITES.some(
      ([h, paths]) => (host === h || host.endsWith("." + h)) && (paths.length === 0 ? u.pathname.length > 1 : paths.some((p) => u.pathname.includes(p))),
    );
  } catch {
    return false;
  }
}

/** Links that open the video dialog (quality choice) instead of a plain file download. */
export function opensVideoDialog(url: string): boolean {
  return isStreamLink(url) || isVideoPage(url);
}

/** `.m3u8` / `.mpd` links are video streams (HLS / DASH), not plain files. */
export function isStreamLink(url: string): boolean {
  try {
    const last = new URL(url.trim()).pathname.split("/").filter(Boolean).pop() ?? "";
    return /\.(m3u8|mpd)$/i.test(last);
  } catch {
    return false;
  }
}
