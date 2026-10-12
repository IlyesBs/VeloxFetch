import { beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { BASE_PULSE_S, retime } from "./components/ThreadMatrix";
import { ENTRANCE_MS, forgetShown, markShown, wasShown } from "./ui/entrance";

// Issue #20: cards flashing when a download starts (e2e: tests/e2e/desktop-card-flash.mjs).
const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
const src = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

describe("entrance plays once per item", () => {
  beforeEach(forgetShown);

  it("an item seen once never gets its entrance again (a moved or re-created card stays visible)", () => {
    expect(wasShown("download:7")).toBe(false);
    markShown("download:7");
    expect(wasShown("download:7")).toBe(true);
    expect(wasShown("download:8")).toBe(false);
    expect(wasShown("queue:7")).toBe(false); // each list keeps its own
  });

  it("the fallback outlasts the animation", () => {
    const ms = Number(/\.rise\s*\{\s*animation:\s*rise\s+(\d+)ms/.exec(css)?.[1]);
    expect(ms).toBeGreaterThan(0);
    expect(ENTRANCE_MS).toBeGreaterThan(ms * 2);
  });

  it("cards and queue rows take the class from the hook, never as a fixed class", () => {
    for (const f of ["./components/DownloadCard.tsx", "./components/SmartQueue.tsx"]) {
      const s = src(f);
      expect(s).toMatch(/useEntrance\(`(download|queue):\$\{id\}`\)/);
      expect(s).not.toMatch(/className=\{`[^`]*(?<!\$\{)\brise\b/);
    }
  });
});

describe("the grid's pulse changes pace without a jump", () => {
  it("sets the speed of the pulse only, from the stylesheet's base", () => {
    const pulse = { playbackRate: 1, animationName: "flicker" };
    const fade = { playbackRate: 1 }; // the colour transition (no animation name)
    const other = { playbackRate: 1, animationName: "sweep" };
    expect(retime([pulse, fade, other], 0.8)).toBe(1);
    expect(pulse.playbackRate).toBeCloseTo(BASE_PULSE_S / 0.8);
    expect(fade.playbackRate).toBe(1);
    expect(other.playbackRate).toBe(1);
    expect(retime([pulse], 0.8)).toBe(0); // same pace: untouched
  });

  it("the duration never changes any more: the stylesheet's base is the only one", () => {
    expect(css).toMatch(new RegExp(`--pulse:\\s*${BASE_PULSE_S}s`));
    expect(src("./components/ThreadMatrix.tsx")).not.toMatch(/"--pulse"/);
  });
});
