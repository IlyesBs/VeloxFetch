// The details page of a video download: the numbers it shows, from the job the engine sends (pure functions).

import type { Key } from "./i18n";
import type { MediaJob } from "./types";

/** Time spent running (paused time not counted), in ms. */
export function elapsedMs(j: MediaJob, now = Date.now()): number {
  const run = j.runningSinceMs != null ? Math.max(0, now - j.runningSinceMs) : 0;
  return (j.activeMs ?? 0) + run;
}

/** Average speed over the time it ran (bytes/s); 0 until there is something to say. */
export function averageBps(j: MediaJob, now = Date.now()): number {
  const ms = elapsedMs(j, now);
  return ms >= 1000 && j.bytes > 0 ? Math.round(j.bytes / (ms / 1000)) : 0;
}

/** The size: the saved files once finished, else an estimate from the share done (null when unknown). */
export function sizeOf(j: MediaJob): { bytes: number; estimate: boolean } | null {
  if (j.status === "completed") return { bytes: j.fileSize ?? j.bytes, estimate: false };
  if (j.percent && j.percent >= 1 && j.bytes > 0) return { bytes: Math.round((j.bytes * 100) / j.percent), estimate: true };
  return null;
}

/** Full paths of the saved files (the folder as the system writes it). */
export function savedPaths(j: MediaJob): string[] {
  const sep = j.folder.includes("\\") && !j.folder.includes("/") ? "\\" : "/";
  const dir = j.folder.endsWith(sep) ? j.folder.slice(0, -1) : j.folder;
  return j.files.map((f) => `${dir}${sep}${f}`);
}

/** What was asked for: the type of download. */
export function typeKey(j: MediaJob): Key {
  return j.audioOnly ? "md.audioOnly" : j.site ? "md.typeSite" : "md.typeStream";
}

/** The quality as shown ("1080p" for a bare number, as given otherwise; null = the best). */
export function qualityText(q: string | null | undefined): string | null {
  if (!q) return null;
  return /^\d{3,4}$/.test(q) ? `${q}p` : q;
}
