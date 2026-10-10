import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import ar from "./locales/ar.json";
import { duplicatePrompt, localDuplicate } from "./duplicates";
import type { Download, DuplicateMatch } from "./types";

const match = (over: Partial<DuplicateMatch> = {}): DuplicateMatch => ({
  id: 7,
  historyId: null,
  status: "completed",
  filename: "a.zip",
  reason: "sameLink",
  confidence: "likely",
  changed: false,
  file: "present",
  openable: true,
  ...over,
});

describe("duplicate prompt", () => {
  it("a running download: view it, or download another copy (never a new one on its own)", () => {
    for (const status of ["queued", "downloading", "verifying"] as const) {
      const p = duplicatePrompt(match({ status, file: null }));
      expect(p.title).toBe("dup.t.active");
      expect(p.actions).toEqual(["view", "another"]);
    }
  });

  it("a paused download offers to resume it", () => {
    expect(duplicatePrompt(match({ status: "paused", file: null })).actions).toEqual(["resume", "view", "another"]);
  });

  it("a failed download is never taken for a usable file", () => {
    const p = duplicatePrompt(match({ status: "failed", file: null, openable: false }));
    expect(p.title).toBe("dup.t.failed");
    expect(p.actions).not.toContain("open");
    expect(p.actions).not.toContain("folder");
  });

  it("a finished file that is there: open it (when safe) or download again", () => {
    expect(duplicatePrompt(match()).actions).toEqual(["open", "folder", "view", "again"]);
    expect(duplicatePrompt(match({ openable: false })).actions).toEqual(["folder", "view", "again"]); // a program is only shown, never opened;
    expect(duplicatePrompt(match({ confidence: "exact" })).title).toBe("dup.t.done");
    const likely = duplicatePrompt(match());
    expect(likely.title).toBe("dup.t.doneLikely");
    expect(likely.notes).toContain("dup.n.unsure");
  });

  it("a finished file that is gone or changed is said so, and Open is not offered", () => {
    const gone = duplicatePrompt(match({ file: "missing", openable: false }));
    expect(gone.title).toBe("dup.t.gone");
    expect(gone.actions[0]).toBe("again");
    expect(gone.actions).not.toContain("open");
    const edited = duplicatePrompt(match({ file: "changed", openable: false }));
    expect(edited.title).toBe("dup.t.edited");
    expect(edited.actions).not.toContain("open");
    const unknown = duplicatePrompt(match({ file: "unknown", openable: false }));
    expect(unknown.actions).toEqual(["view", "again"]);
  });

  it("a changed version on the server is pointed out", () => {
    const p = duplicatePrompt(match({ confidence: "possible", changed: true }));
    expect(p.notes[0]).toBe("dup.n.changed");
    expect(p.notes).not.toContain("dup.n.unsure");
  });

  it("a match only in the history has nothing to view in the list", () => {
    const p = duplicatePrompt(match({ id: null, historyId: 3, status: null }));
    expect(p.actions).toEqual(["open", "folder", "again"]);
  });

  it("every prompt can still download, and every sentence exists in every language", () => {
    const keys = new Set<string>(["dup.show", "dup.resume", "dup.retry", "dup.open", "dup.folder", "dup.again", "dup.another"]);
    const statuses = ["queued", "downloading", "verifying", "paused", "failed", "completed", null] as const;
    const files = ["present", "missing", "changed", "unknown", null] as const;
    const reasons = ["sameLink", "sameFinalLink", "sameFile", "sameChecksum"] as const;
    for (const status of statuses)
      for (const file of files)
        for (const reason of reasons)
          for (const confidence of ["exact", "likely", "possible"] as const) {
            const p = duplicatePrompt(match({ status, file, reason, confidence, changed: confidence === "possible" }));
            expect(p.actions.some((a) => a === "again" || a === "another")).toBe(true);
            for (const k of [p.title, p.reason, ...p.notes]) keys.add(k);
          }
    for (const file of [en, fr, ar] as unknown as Record<string, string>[]) expect([...keys].filter((k) => !(k in file))).toEqual([]);
  });
});

describe("local fallback", () => {
  const dl = (id: number, url: string, status: Download["status"]) => ({ id, url, status, filename: `f${id}` }) as Download;
  it("compares the link (without its #fragment), skips cancelled ones and claims no file", () => {
    const byId = { 1: dl(1, "https://x.example/a.zip", "cancelled"), 2: dl(2, "https://x.example/a.zip#p", "completed") };
    const d = localDuplicate(" https://x.example/a.zip ", byId, [1, 2])!;
    expect([d.id, d.confidence, d.file, d.openable]).toEqual([2, "likely", null, false]);
    expect(localDuplicate("https://x.example/a.zip?v=2", byId, [1, 2])).toBeNull();
    expect(localDuplicate("https://x.example/a.zip", { 1: byId[1] }, [1])).toBeNull();
  });
});
