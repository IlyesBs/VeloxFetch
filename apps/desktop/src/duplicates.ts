// Smart Duplicate Detection, UI side: turns the match Rust found (crates/download-manager/src/intelligence.rs,
// `find_duplicate`) into what the "already have it" prompt says and offers. Pure functions (no React, no network):
// every sentence maps to a field of the match, an action is only offered when it can work, and nothing ever claims
// two files are the same beyond what the match proves. A duplicate is advice: "Download again" is always there.

import type { Key } from "./i18n";
import type { Download, DownloadStatus, DuplicateMatch } from "./types";

/** What the prompt may offer. */
export type DupAction = "view" | "resume" | "retry" | "open" | "folder" | "again" | "another";

export interface DupPrompt {
  title: Key;
  /** Why it is taken for the same download (the evidence). */
  reason: Key;
  /** What the user should know before choosing (version changed, file gone...). */
  notes: Key[];
  /** Most useful first; the first one is the default button. */
  actions: DupAction[];
}

const RUNNING: DownloadStatus[] = ["queued", "downloading", "verifying"];

export function duplicatePrompt(d: DuplicateMatch): DupPrompt {
  const inList = d.id !== null;
  const notes: Key[] = [];
  if (d.changed) notes.push("dup.n.changed");
  const reason: Key = `dup.r.${d.reason}`;
  if (d.status !== null && RUNNING.includes(d.status)) {
    return { title: "dup.t.active", reason, notes, actions: ["view", "another"] };
  }
  if (d.status === "paused") {
    return { title: "dup.t.paused", reason, notes, actions: ["resume", "view", "another"] };
  }
  if (d.status === "failed") {
    // A failed download never counts as a usable file.
    return { title: "dup.t.failed", reason, notes, actions: ["retry", "view", "again"] };
  }
  // Completed in the list, or only in the history.
  const view: DupAction[] = inList ? ["view"] : [];
  notes.push("dup.n.newName");
  switch (d.file) {
    case "missing":
      return { title: "dup.t.gone", reason, notes, actions: ["again", ...view] };
    case "changed":
      return { title: "dup.t.edited", reason, notes, actions: ["folder", ...view, "again"] };
    case "present": {
      if (d.confidence === "likely" && !d.changed) notes.unshift("dup.n.unsure");
      const title: Key = d.confidence === "exact" ? "dup.t.done" : "dup.t.doneLikely";
      return { title, reason, notes, actions: [...(d.openable ? (["open"] as DupAction[]) : []), "folder", ...view, "again"] };
    }
    default:
      // The file could not be looked at: never claim it is there.
      return { title: "dup.t.doneLikely", reason, notes, actions: [...view, "again"] };
  }
}

/** The link without its `#fragment` (never sent to the server, so not part of the resource). */
const withoutFragment = (u: string) => u.trim().replace(/#.*$/, "");

/**
 * The same link already in the list (not cancelled), from what the window has. Only used when Rust cannot be
 * asked: a plain link comparison, told as "likely" and with no claim about the file.
 */
export function localDuplicate(url: string, byId: Record<number, Download>, ids: number[]): DuplicateMatch | null {
  const u = withoutFragment(url);
  for (let i = ids.length - 1; i >= 0; i--) {
    const d = byId[ids[i]!];
    if (d && d.status !== "cancelled" && withoutFragment(d.url) === u) {
      return { id: d.id, historyId: null, status: d.status, filename: d.filename, reason: "sameLink", confidence: "likely", changed: false, file: null, openable: false };
    }
  }
  return null;
}
