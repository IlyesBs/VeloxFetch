// Download Intelligence, UI side: turns the analysis Rust sends (crates/download-manager/src/intelligence.rs) into
// the lines the panel shows. Pure functions (no React, no network): every line maps to one field or finding of the
// analysis, so "Why?" never says more than what was observed.

import { formatBytes } from "./format";
import type { Key } from "./i18n";
import type { Analysis, Finding, Health } from "./types";

export type Line = { key: Key; vars?: Record<string, string | number> };

const when = (ms: number) => new Date(ms).toLocaleString();

/** `http_404` -> `HTTP 404`; other codes as they are. */
export function errorLabel(code: string | null): string {
  if (!code) return "";
  const m = /^http_(\d{3})$/.exec(code);
  return m ? `HTTP ${m[1]}` : code;
}

/** The sentence for one finding. */
export function findingLine(f: Finding): Line {
  switch (f.code) {
    case "otherSite":
      return { key: "intel.f.otherSite", vars: { host: f.detail ?? "" } };
    case "serverError":
    case "unreachable":
      return { key: `intel.f.${f.code}`, vars: { code: errorLabel(f.detail) } };
    case "temporaryLink":
      return f.atMs !== null ? { key: "intel.f.temporaryLinkUntil", vars: { time: when(f.atMs) } } : { key: "intel.f.temporaryLink" };
    case "expiredLink":
      return { key: "intel.f.expiredLink", vars: { time: f.atMs !== null ? when(f.atMs) : "" } };
    case "noSpace":
      return { key: "intel.f.noSpace", vars: { size: formatBytes(f.bytes), free: formatBytes(f.free) } };
    case "duplicate":
      return { key: "intel.f.duplicate", vars: { name: f.detail ?? "" } };
    default:
      return { key: `intel.f.${f.code}` };
  }
}

/** Warnings and problems: shown at once. */
export const warnings = (a: Analysis): Finding[] => a.findings.filter((f) => f.level !== "info");

/** What was observed, for "Why?": the server's answer first, then the remarks that are only information. */
export function evidence(a: Analysis): Line[] {
  const out: Line[] = [];
  const checked = a.kind !== "unknown" && a.kind !== "stream" && a.kind !== "videoPage";
  if (a.errorCode && !checked) out.push({ key: "intel.ev.noAnswer", vars: { code: errorLabel(a.errorCode) } });
  out.push({ key: a.https ? "intel.ev.https" : "intel.ev.http" });
  if (a.redirected && a.finalHost) out.push({ key: "intel.ev.redirected", vars: { host: a.finalHost } });
  if (checked) {
    out.push(a.size !== null ? { key: "intel.ev.size", vars: { size: formatBytes(a.size) } } : { key: "intel.ev.noSize" });
    if (a.rangeVerified) out.push({ key: "intel.ev.rangeVerified" });
    else {
      if (a.rangeClaimed) out.push({ key: "intel.ev.rangeClaimed" });
      if (a.resume === "notSupported") out.push({ key: "intel.ev.rangeIgnored" });
    }
    if (a.etag) out.push({ key: "intel.ev.etag" });
    if (a.lastModified) out.push({ key: "intel.ev.lastModified" });
  }
  for (const f of a.findings) if (f.level === "info") out.push(findingLine(f));
  return out;
}

export function strategyLine(a: Analysis): Line {
  return a.strategy === "parallel" ? { key: "intel.strategy.parallel", vars: { n: a.connections ?? 1 } } : { key: `intel.strategy.${a.strategy}` };
}

export function storageLine(a: Analysis): Line {
  return { key: `intel.storage.${a.storage}`, vars: { free: formatBytes(a.freeBytes), size: formatBytes(a.size) } };
}

/** The destination conflict, if the file name is taken in its folder (never a claim that the contents are the same). */
export function nameLine(a: Analysis): Line | null {
  const c = a.nameConflict;
  if (!c) return null;
  const vars = { name: a.filename, saved: c.savedAs ?? "" };
  if (c.sameResource) return { key: c.savedAs ? "intel.name.own" : "intel.name.ownNoAlt", vars };
  return { key: c.savedAs ? "intel.name.taken" : "intel.name.takenNoAlt", vars };
}

export const HEALTH_CLASS: Record<Health, string> = {
  good: "bg-ok/15 text-ok",
  caution: "bg-warn/15 text-warn",
  problem: "bg-bad/15 text-bad",
  unknown: "bg-line text-dim",
};
