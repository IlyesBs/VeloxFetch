import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import ar from "./locales/ar.json";
import { canUseNewLink, continuable, outcomeView } from "./newLink";
import type { DownloadStatus, DuplicateMatch, NewLinkOutcome, NewLinkReason } from "./types";

const out = (o: Partial<NewLinkOutcome>): NewLinkOutcome => ({
  verdict: "unknown",
  reason: "sizeOnly",
  applied: false,
  canConfirm: false,
  httpStatus: null,
  keptBytes: 1000,
  download: null,
  ...o,
});

const REASONS: NewLinkReason[] = [
  "etagMatch", "bytesMatch", "dateMatch", "nothingToReuse", "webPage", "sizeChanged", "etagChanged", "dateChanged",
  "bytesDiffer", "sizeUnknown", "sizeOnly", "noRanges", "noValidator", "sameSource", "unreachable",
];

describe("Use a new link", () => {
  it("is offered for stopped downloads only", () => {
    for (const s of ["paused", "failed"] as DownloadStatus[]) expect(canUseNewLink({ status: s })).toBe(true);
    for (const s of ["queued", "downloading", "verifying", "completed", "cancelled"] as DownloadStatus[]) expect(canUseNewLink({ status: s })).toBe(false);
  });

  it("lets a browser link continue only a stopped download found as its duplicate", () => {
    const dup = (status: DownloadStatus | null, id: number | null = 7) => ({ id, status }) as DuplicateMatch;
    expect(continuable(dup("failed"))).toBe(7);
    expect(continuable(dup("paused"))).toBe(7);
    expect(continuable(dup("completed"))).toBeNull();
    expect(continuable(dup("downloading"))).toBeNull();
    expect(continuable(dup("failed", null))).toBeNull(); // only in the history
    expect(continuable(null)).toBeNull();
  });

  it("never claims more than the outcome proves", () => {
    expect(outcomeView(out({ applied: true, verdict: "verifiedSame", reason: "etagMatch" }))).toMatchObject({ tone: "ok", title: "nl.t.resumed", offers: [] });
    expect(outcomeView(out({ applied: true, keptBytes: 0, reason: "nothingToReuse" })).title).toBe("nl.t.started");
    // Likely: asked first, and the user can still add it as a new file instead.
    expect(outcomeView(out({ verdict: "likelySame", reason: "bytesMatch", canConfirm: true }))).toMatchObject({ tone: "warn", offers: ["confirm", "addNew"] });
    const other = outcomeView(out({ verdict: "differentResource", reason: "bytesDiffer" }));
    expect(other).toMatchObject({ tone: "bad", title: "nl.t.different", offers: ["addNew"] });
    expect(other.offers).not.toContain("confirm");
    expect(outcomeView(out({ reason: "sameSource" })).offers).toEqual([]);
    expect(outcomeView(out({ reason: "unreachable", httpStatus: 403 })).title).toBe("nl.t.unreachable");
    expect(outcomeView(out({ verdict: "verifiedSame", reason: "noRanges" }))).toMatchObject({ title: "nl.t.kept", offers: ["addNew"] });
  });

  it("has every text in every language", () => {
    const keys = [
      ...REASONS.map((r) => `nl.r.${r}`),
      ...["resumed", "started", "likely", "different", "same", "unreachable", "kept"].map((k) => `nl.t.${k}`),
      "nl.menu", "nl.title", "nl.intro", "nl.link", "nl.check", "nl.checking", "nl.confirm", "nl.addNew", "nl.added", "nl.kept", "nl.status", "nl.pendingAsk", "nl.pendingUse",
    ];
    for (const l of [en, fr, ar] as unknown as Record<string, string>[]) {
      for (const k of keys) expect(l[k]?.length, k).toBeGreaterThan(0);
      expect(l["nl.kept"]).toContain("{size}");
      expect(l["nl.status"]).toContain("{code}");
    }
  });
});
