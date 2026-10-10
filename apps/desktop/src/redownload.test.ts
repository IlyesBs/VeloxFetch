import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import ar from "./locales/ar.json";
import { canRedownload, redownloadIsPrimary } from "./redownload";
import type { Download, DownloadStatus } from "./types";

const dl = (status: DownloadStatus, fileMissing?: boolean) => ({ id: 1, status, fileMissing }) as unknown as Download;

describe("Download again", () => {
  it("is offered for finished, cancelled and failed downloads only", () => {
    for (const s of ["completed", "cancelled", "failed"] as DownloadStatus[]) expect(canRedownload(dl(s))).toBe(true);
    for (const s of ["queued", "downloading", "paused"] as DownloadStatus[]) expect(canRedownload(dl(s))).toBe(false);
  });

  it("is the main button when the file is gone or the download was cancelled", () => {
    expect(redownloadIsPrimary(dl("completed", true))).toBe(true);
    expect(redownloadIsPrimary(dl("cancelled"))).toBe(true);
    expect(redownloadIsPrimary(dl("completed", false))).toBe(false); // the file is there: Open stays first
    expect(redownloadIsPrimary(dl("completed"))).toBe(false); // an older answer without the flag
    expect(redownloadIsPrimary(dl("failed"))).toBe(false); // Retry (continues) stays first
    expect(redownloadIsPrimary(dl("paused", true))).toBe(false);
  });

  it("has its texts in every language", () => {
    for (const l of [en, fr, ar] as unknown as Record<string, string>[]) {
      for (const k of ["card.redownload", "card.fileMissing", "act.redownloading"]) expect(l[k]?.length).toBeGreaterThan(0);
      expect(l["act.redownloading"]).toContain("{name}");
    }
  });
});
