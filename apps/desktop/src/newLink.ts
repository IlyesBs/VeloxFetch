// "Use a new link", UI side. The rules live in Rust (crates/download-manager/src/relink.rs, `use_new_link`): it
// checks the new link, keeps the saved bytes only when it gives the same file, and changes nothing otherwise.
// These pure helpers decide where the UI offers it and what an outcome says. Never more than the outcome proves.

import type { Key } from "./i18n";
import type { Download, DuplicateMatch, NewLinkOutcome } from "./types";

/** A stopped download (paused or failed) can be continued from a new link. */
export const canUseNewLink = (d: Pick<Download, "status">): boolean => d.status === "paused" || d.status === "failed";

/** The download a link the browser just handed over could continue: a stopped one found as its duplicate. */
export const continuable = (dup: DuplicateMatch | null | undefined): number | null =>
  dup && dup.id !== null && (dup.status === "paused" || dup.status === "failed") ? dup.id : null;

export type NewLinkOffer = "confirm" | "addNew";

export interface NewLinkView {
  tone: "ok" | "warn" | "bad";
  /** What happened, in one sentence. */
  title: Key;
  /** The evidence or the reason. */
  detail: Key;
  /** What the user may do next (the saved bytes are kept in every case). */
  offers: NewLinkOffer[];
}

export function outcomeView(o: NewLinkOutcome): NewLinkView {
  const detail: Key = `nl.r.${o.reason}`;
  if (o.applied) return { tone: "ok", title: o.keptBytes > 0 ? "nl.t.resumed" : "nl.t.started", detail, offers: [] };
  if (o.canConfirm) return { tone: "warn", title: "nl.t.likely", detail, offers: ["confirm", "addNew"] };
  if (o.verdict === "differentResource") return { tone: "bad", title: "nl.t.different", detail, offers: ["addNew"] };
  if (o.reason === "sameSource") return { tone: "warn", title: "nl.t.same", detail, offers: [] };
  if (o.reason === "unreachable") return { tone: "bad", title: "nl.t.unreachable", detail, offers: [] };
  return { tone: "warn", title: "nl.t.kept", detail, offers: ["addNew"] };
}

export const NEW_LINK = "velox:new-link";
/** Open the "Use a new link" dialog for a download (one dialog for the whole app, see `NewLinkHost`). */
export const openNewLink = (id: number) => window.dispatchEvent(new CustomEvent(NEW_LINK, { detail: id }));
