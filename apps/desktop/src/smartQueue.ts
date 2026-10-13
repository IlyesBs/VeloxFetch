// Smart Download Queue: what the queue page shows, worked out from the store (pure functions, unit-tested).
//
//  * the state of each download in plain words: Downloading, Waiting, Scheduled, Paused, Completed, Failed, Retrying
//  * why a waiting download waits (a free slot, its queue's time, the network, the computer being used...)
//  * the order the queue will run them in (the engine's order: priority, then the queue order, then the place)
//  * the "Next download" line and the 24-hour timeline

import { t, type Key } from "./i18n";
import type { Download, Priority, QueueGate, QueueInfo, QueueRules, Queues } from "./types";

export const MAIN_QUEUE = 1;

/** The three priorities the queue offers (the two extremes still show when a download has one). */
export const QUEUE_PRIORITIES: Priority[] = ["high", "normal", "low"];

const RANK: Record<Priority, number> = { lowest: 0, low: 1, normal: 2, high: 3, highest: 4 };

export type VisualState = "downloading" | "retrying" | "waiting" | "scheduled" | "paused" | "completed" | "failed";

export type WaitReason = "slot" | "later" | "at" | "givingWay" | Exclude<QueueGate, "open">;

export const NO_RULES: QueueRules = { when: { kind: "anytime" }, onlyWhenIdle: false, connection: "any", pauseAboveBps: null, largeFilesOver: null };

export function isPlain(r: QueueRules): boolean {
  return r.when.kind === "anytime" && !r.onlyWhenIdle && r.connection === "any" && r.pauseAboveBps === null && r.largeFilesOver === null;
}

export function queueName(q: Pick<QueueInfo, "id" | "name">): string {
  return q.id === MAIN_QUEUE || !q.name ? t("sq.main") : q.name;
}

/** The queue a download is in (the main queue when its queue is gone). */
export function queueOf(d: Download, queues: Queues | null): QueueInfo | undefined {
  const all = queues?.queues ?? [];
  return all.find((q) => q.id === d.queueId) ?? all.find((q) => q.id === MAIN_QUEUE);
}

export function visualState(d: Download, q?: QueueInfo): VisualState {
  switch (d.status) {
    case "completed":
      return "completed";
    case "failed":
      // Tried again on its own (no network, a server's passing trouble): not given up.
      return d.autoRetry ? "retrying" : "failed";
    case "cancelled":
      return "failed";
    case "downloading":
    case "verifying":
      return d.retrying ? "retrying" : "downloading";
    case "paused":
      if (d.startAt) return "scheduled";
      // Paused by its queue's rules: it carries on by itself.
      if (d.held) return q?.gate === "time" ? "scheduled" : "waiting";
      return "paused";
    case "queued":
      if (d.startAt) return "scheduled";
      return q?.gate === "time" ? "scheduled" : "waiting";
  }
}

/** Why a waiting or scheduled download is not running (null for the others). */
export function waitReason(d: Download, q: QueueInfo | undefined, queues: Queues | null): WaitReason | null {
  const v = visualState(d, q);
  if (v !== "waiting" && v !== "scheduled") return null;
  if (d.startAt) return "at";
  if (q && q.gate !== "open") return q.gate;
  if (d.idle) return isPlain(q?.rules ?? NO_RULES) ? "later" : "slot";
  if (queues?.givingWay && RANK[d.priority] <= RANK.low) return "givingWay";
  return "slot";
}

const GROUP: Record<VisualState, number> = { downloading: 0, retrying: 0, waiting: 1, scheduled: 2, paused: 3, failed: 4, completed: 5 };

/**
 * The downloads of one queue in the order they will run: running, then waiting (priority, then place), then
 * scheduled (soonest first), paused, failed, and the finished ones last (newest first).
 */
export function queueOrder(ids: number[], byId: Record<number, Download>, q: QueueInfo | undefined): number[] {
  const list = ids.map((id) => byId[id]).filter((d): d is Download => !!d && d.status !== "cancelled");
  const key = (d: Download) => GROUP[visualState(d, q)];
  return list
    .sort((a, b) => {
      const g = key(a) - key(b);
      if (g) return g;
      const group = key(a);
      if (group === 2) return (a.startAt ?? q?.opensAt ?? "").localeCompare(b.startAt ?? q?.opensAt ?? "") || a.position - b.position;
      if (group === 5) return b.id - a.id;
      return RANK[b.priority] - RANK[a.priority] || a.position - b.position;
    })
    .map((d) => d.id);
}

/** Can this download trade places with the one above / below (same priority, both still to run)? */
export function canMove(order: number[], byId: Record<number, Download>, id: number, dir: -1 | 1, q?: QueueInfo): boolean {
  const i = order.indexOf(id);
  const d = byId[id];
  const other = byId[order[i + dir] ?? -1];
  if (!d || !other) return false;
  const movable = (x: Download) => ["queued", "paused"].includes(x.status) && !x.startAt;
  return movable(d) && movable(other) && d.priority === other.priority && visualState(d, q) === visualState(other, q);
}

export type NextWhen =
  | { kind: "now" }
  | { kind: "slot"; etaSecs: number | null }
  | { kind: "at"; at: string }
  | { kind: "reason"; reason: WaitReason };

/** The download this queue starts next, and when (null: nothing waits). */
export function nextDownload(order: number[], byId: Record<number, Download>, q: QueueInfo | undefined, queues: Queues | null, all: Download[]): { id: number; when: NextWhen } | null {
  const candidates = order.map((id) => byId[id]).filter((d): d is Download => !!d);
  const waiting = candidates.find((d) => {
    const v = visualState(d, q);
    return (v === "waiting" || v === "scheduled") && waitReason(d, q, queues) !== "later";
  });
  if (!waiting) return null;
  const why = waitReason(waiting, q, queues);
  if (why === "at") return { id: waiting.id, when: { kind: "at", at: waiting.startAt! } };
  if (why === "time") return { id: waiting.id, when: q?.opensAt ? { kind: "at", at: q.opensAt } : { kind: "reason", reason: "time" } };
  if (why && why !== "slot") return { id: waiting.id, when: { kind: "reason", reason: why } };
  // Waiting for a free slot: when the first running download is expected to finish.
  const etas = all.filter((d) => d.status === "downloading" && d.etaSecs !== null).map((d) => d.etaSecs!);
  const running = all.some((d) => d.status === "downloading" || d.status === "verifying");
  if (!running) return { id: waiting.id, when: { kind: "now" } };
  return { id: waiting.id, when: { kind: "slot", etaSecs: etas.length ? Math.min(...etas) : null } };
}

/** Short words for a queue's rules, for the chips under its name. */
export function ruleChips(r: QueueRules): string[] {
  const out: string[] = [];
  if (r.when.kind === "between") out.push(t("sq.chipBetween", { from: r.when.start, to: r.when.stop }));
  if (r.when.kind === "startAt") out.push(t("sq.chipStartAt", { time: r.when.time }));
  if (r.onlyWhenIdle) out.push(t("sq.chipIdle"));
  if (r.connection !== "any") out.push(t(`sq.conn.${r.connection}` as Key));
  if (r.pauseAboveBps !== null) out.push(t("sq.chipBusy"));
  if (r.largeFilesOver !== null) out.push(t("sq.chipBig"));
  return out;
}

/** `YYYY-MM-DDTHH:MM` to minutes since the epoch (local wall-clock time; only differences are used). */
export function minutesOf(local: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(local);
  if (!m) return null;
  const [, y, mo, d, h, mi] = m.map(Number) as [number, number, number, number, number, number];
  return Date.UTC(y, mo - 1, d, h, mi) / 60000;
}

export interface TimelineBar {
  left: number;
  width: number;
}

/** Spans (and points) on a timeline that starts at `now` and lasts `hours`, as percentages. */
export function timelineBars(spans: { from: string; to: string }[], now: string, hours = 24): TimelineBar[] {
  const start = minutesOf(now);
  if (start === null) return [];
  const len = hours * 60;
  return spans
    .map((s) => {
      const a = Math.max(0, (minutesOf(s.from) ?? start) - start);
      const b = Math.min(len, (minutesOf(s.to) ?? start) - start);
      return { left: (a / len) * 100, width: Math.max(0.6, ((b - a) / len) * 100) };
    })
    .filter((b) => b.left < 100 && b.width > 0);
}

/** Where a time falls on the timeline (0..100), or null when it is outside. */
export function timelinePoint(at: string, now: string, hours = 24): number | null {
  const a = minutesOf(at);
  const start = minutesOf(now);
  if (a === null || start === null) return null;
  const x = ((a - start) / (hours * 60)) * 100;
  return x >= 0 && x <= 100 ? x : null;
}

/** `YYYY-MM-DDTHH:MM` in local time, for a `Date`. */
export function localStamp(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** Quick start times: in an hour, tonight at 23:00, tomorrow at 02:00 / 08:00 (never in the past). */
export function quickTimes(now: Date): { key: Key; at: string }[] {
  const inHour = new Date(now.getTime() + 3600_000);
  const at = (days: number, h: number) => {
    const d = new Date(now);
    d.setDate(d.getDate() + days);
    d.setHours(h, 0, 0, 0);
    return d;
  };
  const tonight = at(now.getHours() >= 23 ? 1 : 0, 23);
  return [
    { key: "sq.inHour", at: localStamp(inHour) },
    { key: "sq.tonight", at: localStamp(tonight) },
    { key: "sq.tomorrow2", at: localStamp(at(1, 2)) },
    { key: "sq.tomorrow8", at: localStamp(at(1, 8)) },
  ];
}
