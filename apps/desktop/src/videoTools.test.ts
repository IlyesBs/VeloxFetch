import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import { initialState, reduce } from "./store";
import type { ToolStatus, VideoTools } from "./types";

const tool = (over: Partial<ToolStatus> = {}): ToolStatus => ({
  installed: true,
  fetching: false,
  unpacking: false,
  canFetch: true,
  error: null,
  received: 0,
  total: 0,
  ...over,
});

const tools = (over: Partial<VideoTools> = {}): VideoTools => ({ ytdlp: tool(), deno: tool(), ffmpeg: tool(), update: null, ...over });

describe("required dependencies", () => {
  it("start unknown, then follow their events (an update offered, then answered)", () => {
    expect(initialState().videoTools).toBeNull();
    const offered = reduce(initialState(), { type: "videoTools", tools: tools({ update: { tools: ["ytdlp"], size: 18_000_000 } }) });
    expect(offered.videoTools?.update).toEqual({ tools: ["ytdlp"], size: 18_000_000 });
    const answered = reduce(offered, { type: "videoTools", tools: tools({ ytdlp: tool({ fetching: true }) }) });
    expect(answered.videoTools?.update).toBeNull();
    expect(answered.videoTools?.ytdlp.fetching).toBe(true);
  });

  it("say an update is only downloaded with the user's agreement, and never name a program", () => {
    const all = en as unknown as Record<string, string>;
    const texts = Object.entries(all).filter(([k]) => k.startsWith("toolsu.") || k.startsWith("deps."));
    expect(texts.length).toBeGreaterThan(5);
    expect(all["toolsu.text"]).toContain("{size}");
    for (const [key, text] of texts) {
      expect(text, key).not.toMatch(/yt-dlp|ffmpeg|deno/i);
    }
  });
});
