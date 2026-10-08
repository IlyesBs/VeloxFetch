// Download Doctor, UI side: turns the diagnosis Rust sends (crates/download-manager/src/doctor.rs) into the lines and
// buttons the panel shows. Pure (no React, no network): every line comes from one field of the diagnosis.

import { formatBytes } from "./format";
import type { Key } from "./i18n";
import type { Diagnosis, DoctorAction } from "./types";

export type Line = { key: Key; vars?: Record<string, string | number> };

export function headline(d: Diagnosis): Line {
  return { key: `doc.cat.${d.category}`, vars: { code: d.httpStatus ?? "" } };
}

export function sourceLine(d: Diagnosis): Line {
  return { key: `doc.source.${d.source}` };
}

/** "45 s", "2 min", "1 h 5 min". */
export function waitText(secs: number): string {
  if (secs < 60) return `${Math.max(1, Math.round(secs))} s`;
  const m = Math.round(secs / 60);
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h${m % 60 ? ` ${m % 60} min` : ""}`;
}

/** Whether retrying helps, and when. `now` = Date.now() (passed in for tests). */
export function retryLine(d: Diagnosis, now: number): Line {
  if (d.retryAtMs !== null) {
    const left = (d.retryAtMs - now) / 1000;
    return left > 0.5 ? { key: "doc.retryIn", vars: { s: waitText(left) } } : { key: "doc.retryingNow" };
  }
  if (d.retry === "retryableWithDelay" && d.retryAfterSecs !== null) return { key: "doc.retry.waitFor", vars: { s: waitText(d.retryAfterSecs) } };
  return { key: `doc.retry.${d.retry}` };
}

export function factorLines(d: Diagnosis): Line[] {
  return d.factors.map((f) => ({ key: `doc.factor.${f.code}`, vars: { n: f.n ?? 0 } }));
}

/** Disk full with the numbers the engine gave. */
export function spaceLine(d: Diagnosis): Line | null {
  return d.neededBytes !== null && d.freeBytes !== null
    ? { key: "doc.space", vars: { need: formatBytes(d.neededBytes), free: formatBytes(d.freeBytes), more: formatBytes(Math.max(0, d.neededBytes - d.freeBytes)) } }
    : null;
}

type Advice = "reauthInBrowser" | "newLink" | "checkConnection" | "checkProxy" | "checkFolder";
const ADVICE: readonly DoctorAction[] = ["reauthInBrowser", "newLink", "checkConnection", "checkProxy", "checkFolder"];

/** Advice: what the user can do outside the app (everything that is not a button). */
export function advice(d: Diagnosis): Line[] {
  return d.actions.filter((a): a is Advice => ADVICE.includes(a)).map((a) => ({ key: `doc.act.${a}` }));
}

/** The panel's buttons, in order: one retry button (labelled for the case), sign in, open the download folder. */
export function buttons(d: Diagnosis): { action: "retry" | "signIn" | "openFolder"; label: Key }[] {
  const out: { action: "retry" | "signIn" | "openFolder"; label: Key }[] = [];
  for (const a of d.actions) {
    if (a === "signIn") out.push({ action: "signIn", label: "doc.btn.signIn" });
    if (a === "freeSpace") out.push({ action: "openFolder", label: "doc.btn.openFolder" });
  }
  const retry = d.actions.find((a) => a === "retry" || a === "retryLater" || a === "restartSafely");
  if (retry) out.push({ action: "retry", label: retry === "restartSafely" ? "doc.btn.restart" : retry === "retryLater" ? "doc.btn.retryAnyway" : "doc.btn.retry" });
  return out;
}

export const SEVERITY_CLASS: Record<Diagnosis["severity"], string> = {
  info: "bg-line text-dim",
  warning: "bg-warn/15 text-warn",
  recoverable: "bg-accent-soft text-accent",
  blocking: "bg-bad/15 text-bad",
  critical: "bg-bad/15 text-bad",
};
