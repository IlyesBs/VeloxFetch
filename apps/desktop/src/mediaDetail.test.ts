import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import ar from "./locales/ar.json";
import { averageBps, elapsedMs, qualityText, savedPaths, sizeOf, typeKey } from "./mediaDetail";
import type { MediaJob } from "./types";

const job = (over: Partial<MediaJob> = {}): MediaJob => ({
  id: 1, title: "clip", host: "example.com", status: "downloading", bytes: 0, segmentsDone: 0, segmentsTotal: 0, percent: null,
  folder: "/home/u/Downloads/Video", files: [], notes: [], error: null, errorCode: null, site: true, speedBps: 0, etaSecs: null, resumable: false, playable: false,
  ...over,
});

describe("video download details", () => {
  it("counts running time only, and averages over it", () => {
    const j = job({ activeMs: 4000, runningSinceMs: 10_000, bytes: 8 * 1024 * 1024 });
    expect(elapsedMs(j, 14_000)).toBe(8000);
    expect(averageBps(j, 14_000)).toBe(1024 * 1024);
    expect(elapsedMs(job({ activeMs: 5000, runningSinceMs: null }), 99_999)).toBe(5000); // paused: the clock stops
    expect(averageBps(job({ bytes: 0, activeMs: 9000 }))).toBe(0);
    expect(elapsedMs(job())).toBe(0); // an older answer without the clock
  });

  it("gives the size: the saved files once finished, an estimate before", () => {
    expect(sizeOf(job({ status: "completed", bytes: 900, fileSize: 1000 }))).toEqual({ bytes: 1000, estimate: false });
    expect(sizeOf(job({ percent: 25, bytes: 250 }))).toEqual({ bytes: 1000, estimate: true });
    expect(sizeOf(job({ percent: null, bytes: 250 }))).toBeNull();
    expect(sizeOf(job({ percent: 0.2, bytes: 1 }))).toBeNull(); // too early to guess
  });

  it("shows full paths on every system, the type and the quality", () => {
    expect(savedPaths(job({ files: ["a.mp4", "a.en.vtt"] }))).toEqual(["/home/u/Downloads/Video/a.mp4", "/home/u/Downloads/Video/a.en.vtt"]);
    expect(savedPaths(job({ folder: "C:\\Users\\u\\Downloads\\", files: ["a.mp4"] }))).toEqual(["C:\\Users\\u\\Downloads\\a.mp4"]);
    expect(typeKey(job({ audioOnly: true }))).toBe("md.audioOnly");
    expect(typeKey(job({ site: false }))).toBe("md.typeStream");
    expect(typeKey(job())).toBe("md.typeSite");
    expect(qualityText("1080")).toBe("1080p");
    expect(qualityText("hd720")).toBe("hd720");
    expect(qualityText(null)).toBeNull();
  });

  it("has every text in every language", () => {
    const keys = Object.keys(en).filter((k) => k.startsWith("md."));
    expect(keys.length).toBeGreaterThan(20);
    for (const l of [fr, ar] as unknown as Record<string, string>[]) for (const k of keys) expect(l[k]?.length, k).toBeGreaterThan(0);
  });
});
