import { describe, expect, it } from "vitest";
import { anyRunning, indicatorItems } from "./indicator";
import { buildGroups } from "./store";
import type { Download, MediaJob } from "./types";
import app from "./App.tsx?raw";

const locales = import.meta.glob<Record<string, string>>("./locales/*.json", { eager: true, import: "default" });

const dl = (id: number, over: Partial<Download> = {}): Download => ({
  id, url: `https://x.test/${id}`, filename: `f${id}.bin`, destination: "/tmp", status: "downloading", totalSize: 1000, downloaded: 0,
  speedBps: 0, etaSecs: null, connections: 0, priority: "normal", category: null, speedLimit: null, resumable: true, error: null,
  errorDetail: null, needs: null, errorCode: null, kind: "other", idle: false, queueId: 1, position: id * 1024, startAt: null,
  retrying: false, held: false, doctor: null,
  ...over,
});

const job = (id: number, over: Partial<MediaJob> = {}): MediaJob => ({
  id, title: `clip ${id}`, host: "example.com", status: "downloading", bytes: 0, segmentsDone: 0, segmentsTotal: 0, percent: null,
  folder: "/home/u/Downloads/Video", files: [], notes: [], error: null, errorCode: null, site: true, speedBps: 0, etaSecs: null,
  resumable: false, playable: false,
  ...over,
});

/** What the indicator lists for these downloads (the store's groups, as the app has them). */
const items = (downloads: Download[], media: MediaJob[] = []) => {
  const byId = Object.fromEntries(downloads.map((d) => [d.id, d]));
  return indicatorItems(buildGroups(downloads.map((d) => d.id), byId).active, byId, media);
};

describe("download indicator", () => {
  it("lists only what runs now: file downloads first, oldest first, then video downloads", () => {
    const list = items(
      [dl(3), dl(1, { status: "queued" }), dl(2, { status: "paused" }), dl(5, { status: "verifying" }), dl(6, { status: "completed" }), dl(7, { status: "failed" })],
      [job(1, { status: "queued" }), job(2, { status: "preparing" }), job(3, { status: "paused" }), job(4, { status: "combining" }), job(5, { status: "completed" })],
    );
    expect(list.map((i) => i.key)).toEqual(["d3", "d5", "m2", "m4"]);
    expect(items([dl(9, { status: "paused" })], [job(1, { status: "failed" })])).toEqual([]);
  });

  it("keeps the first one first while others start and end", () => {
    const a = dl(4, { downloaded: 100 });
    expect(items([a, dl(8)])[0]!.key).toBe("d4");
    expect(items([a, dl(8), dl(9)], [job(1)])[0]!.key).toBe("d4");
    expect(items([dl(4, { downloaded: 900 }), dl(9, { status: "completed" })])[0]!.key).toBe("d4");
  });

  it("gives the name, progress, time left and speed of each", () => {
    const [d] = items([dl(1, { filename: "movie.mkv", kind: "video", downloaded: 250, totalSize: 1000, etaSecs: 80, speedBps: 4096 })]);
    expect(d).toMatchObject({ kind: "download", id: 1, name: "movie.mkv", fileKind: "video", fraction: 0.25, etaSecs: 80, speedBps: 4096, bytes: 250, phase: null });
    const [m] = items([], [job(2, { title: "Talk", percent: 40, etaSecs: 12, speedBps: 99, bytes: 7 })]);
    expect(m).toMatchObject({ kind: "media", id: 2, name: "Talk", fileKind: "video", fraction: 0.4, etaSecs: 12, speedBps: 99, bytes: 7, phase: null });
  });

  it("has no progress until the size is known, and never more than all of it", () => {
    expect(items([dl(1, { totalSize: null, downloaded: 500 })])[0]).toMatchObject({ fraction: null, bytes: 500 });
    expect(items([dl(1, { totalSize: 0 })])[0]!.fraction).toBeNull();
    expect(items([dl(1, { totalSize: 100, downloaded: 150 })])[0]!.fraction).toBe(1);
    expect(items([], [job(1, { percent: null })])[0]!.fraction).toBeNull();
    expect(items([], [job(1, { percent: 104 })])[0]!.fraction).toBe(1);
  });

  it("says what it does when it is not downloading, without a time left or speed", () => {
    expect(items([dl(1, { status: "verifying", etaSecs: 5, speedBps: 10 })])[0]).toMatchObject({ phase: "status.verifying", etaSecs: null, speedBps: 0 });
    expect(items([], [job(1, { status: "preparing", etaSecs: 5 })])[0]).toMatchObject({ phase: "mj.preparing", etaSecs: null });
    expect(items([], [job(1, { status: "combining", speedBps: 5 })])[0]).toMatchObject({ phase: "mj.finishing", speedBps: 0 });
  });

  it("is shown exactly when there is something to list", () => {
    const cases: [Download[], MediaJob[]][] = [
      [[], []],
      [[dl(1)], []],
      [[dl(1, { status: "paused" })], [job(1, { status: "queued" })]],
      [[], [job(1, { status: "combining" })]],
    ];
    for (const [d, m] of cases) {
      const byId = Object.fromEntries(d.map((x) => [x.id, x]));
      expect(anyRunning(buildGroups(d.map((x) => x.id), byId).active, m)).toBe(items(d, m).length > 0);
    }
  });

  it("is in the main window, with its words in every language", () => {
    expect(app).toContain("<DownloadIndicator onOpen={open} />");
    expect(Object.keys(locales)).toHaveLength(8);
    for (const [f, d] of Object.entries(locales)) {
      for (const k of ["ind.label", "ind.show", "ind.hide"]) expect(d[k], `${f} ${k}`).toBeTruthy();
    }
  });
});
