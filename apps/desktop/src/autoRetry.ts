// "Never lose a download", UI side: what a failed download waiting to be tried again on its own says
// (crates/download-manager/src/manager/recovery.rs). Pure: `now` is passed in for tests.

import { waitText } from "./doctor";
import type { Key } from "./i18n";
import type { Download } from "./types";

export type Line = { key: Key; vars?: Record<string, string | number> };

/** The card's line for a failed download tried again on its own, or null. */
export function autoRetryLine(d: Download, now: number): Line | null {
  const a = d.status === "failed" ? d.autoRetry : null;
  if (!a) return null;
  if (a.waitingForNetwork || a.atMs === null) return { key: "card.waitingNetwork" };
  const left = (a.atMs - now) / 1000;
  return left > 0.5 ? { key: "card.autoRetryIn", vars: { s: waitText(left) } } : { key: "card.autoRetryNow" };
}
