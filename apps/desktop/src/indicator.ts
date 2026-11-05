// The download indicator (components/DownloadIndicator.tsx): what is downloading right now, one line each.
//
// File downloads (running or checking their file) come first, oldest first, then the video downloads being worked
// on, oldest first. The order only changes when one starts or ends, so the first line (the one the bar always
// shows) stays the same while it runs.

import type { Download, FileKind, MediaJob, MediaStatus } from "./types";
import type { Key } from "./i18n";

export interface IndicatorItem {
  /** Unique across both lists ("d12", "m3"). */
  key: string;
  kind: "download" | "media";
  id: number;
  name: string;
  fileKind: FileKind;
  /** 0..1, or null while the size is not known yet. */
  fraction: number | null;
  etaSecs: number | null;
  speedBps: number;
  /** Bytes so far (shown when the size is not known). */
  bytes: number;
  /** A word for what it does when it is not just downloading ("Verifying", "Finishing"); null otherwise. */
  phase: Key | null;
}

const MEDIA_RUNNING: MediaStatus[] = ["preparing", "downloading", "combining"];

export const mediaRunning = (j: MediaJob) => MEDIA_RUNNING.includes(j.status);

function fromDownload(d: Download): IndicatorItem {
  const total = d.totalSize && d.totalSize > 0 ? d.totalSize : null;
  return {
    key: `d${d.id}`,
    kind: "download",
    id: d.id,
    name: d.filename,
    fileKind: d.kind,
    fraction: total === null ? null : Math.min(1, Math.max(0, d.downloaded / total)),
    etaSecs: d.status === "downloading" ? d.etaSecs : null,
    speedBps: d.status === "downloading" ? d.speedBps : 0,
    bytes: d.downloaded,
    phase: d.status === "verifying" ? "status.verifying" : null,
  };
}

function fromMedia(j: MediaJob): IndicatorItem {
  return {
    key: `m${j.id}`,
    kind: "media",
    id: j.id,
    name: j.title,
    fileKind: "video",
    fraction: j.percent === null ? null : Math.min(1, Math.max(0, j.percent / 100)),
    etaSecs: j.status === "downloading" ? j.etaSecs : null,
    speedBps: j.status === "downloading" ? j.speedBps : 0,
    bytes: j.bytes,
    phase: j.status === "preparing" ? "mj.preparing" : j.status === "combining" ? "mj.finishing" : null,
  };
}

/** Every download running now, the first one first (see the top of this file). */
export function indicatorItems(active: number[], byId: Record<number, Download>, media: MediaJob[]): IndicatorItem[] {
  const out: IndicatorItem[] = [];
  for (const id of active) {
    const d = byId[id];
    if (d && (d.status === "downloading" || d.status === "verifying")) out.push(fromDownload(d));
  }
  for (const j of media) if (mediaRunning(j)) out.push(fromMedia(j));
  return out;
}

/** Whether the indicator has anything to show (cheap: no list built). */
export function anyRunning(active: number[], media: MediaJob[]): boolean {
  return active.length > 0 || media.some(mediaRunning);
}
