import { describe, expect, it } from "vitest";
import fs from "node:fs";
import path from "node:path";
import { drawAurora, LIGHTS, transparentOf } from "./aurora";

const css = fs.readFileSync(path.join(__dirname, "styles.css"), "utf8");
const app = fs.readFileSync(path.join(__dirname, "App.tsx"), "utf8");

describe("the backdrop drawn as a picture", () => {
  it("has exactly the lights of `.aurora` in styles.css (the fallback), in the same order", () => {
    const blocks = [...css.matchAll(/\n\.aurora \{\n  background:\n([\s\S]*?);\n\}/g)];
    const last = blocks.at(-1)?.[1] ?? ""; // the later rule is the one in force
    const fromCss = [...last.matchAll(/radial-gradient\(([\d.]+)rem ([\d.]+)rem at (-?[\d.]+)% (-?[\d.]+)%, var\((--[\w-]+)\), transparent ([\d.]+)%\)/g)].map((m) => ({
      w: Number(m[1]),
      h: Number(m[2]),
      x: Number(m[3]) / 100,
      y: Number(m[4]) / 100,
      color: m[5],
      stop: Number(m[6]) / 100,
    }));
    expect(fromCss.length).toBe(5);
    expect(LIGHTS).toEqual(fromCss);
  });

  it("fades each light to its own colour at zero alpha (as CSS does), not to black", () => {
    expect(transparentOf("rgba(0, 200, 245, 0.16)")).toBe("rgba(0, 200, 245, 0)");
    expect(transparentOf("rgb(155 28 245 / 0.22)")).toBe("rgba(155, 28, 245, 0)");
    expect(transparentOf("nonsense")).toBe("rgba(0, 0, 0, 0)");
  });

  it("paints the lights bottom first (the first CSS layer ends on top), each as a scaled ellipse", () => {
    const calls: string[] = [];
    const stops: [number, string][] = [];
    const ctx = {
      clearRect: () => calls.push("clear"),
      save: () => {},
      restore: () => {},
      translate: (x: number, y: number) => calls.push(`at ${Math.round(x)},${Math.round(y)}`),
      scale: (_x: number, y: number) => calls.push(`squash ${y.toFixed(3)}`),
      createRadialGradient: (_a: number, _b: number, _c: number, _d: number, _e: number, r: number) => {
        calls.push(`r ${r}`);
        return { addColorStop: (o: number, c: string) => stops.push([o, c]) };
      },
      fillRect: () => calls.push("fill"),
      fillStyle: "",
    } as unknown as CanvasRenderingContext2D;
    drawAurora(ctx, 1000, 800, 16, (v) => (v === "--sky-1" ? "rgba(1, 2, 3, 0.5)" : "rgba(9, 9, 9, 0.1)"));
    expect(calls[0]).toBe("clear");
    expect(calls.filter((c) => c === "fill").length).toBe(5);
    // --sky-1 (last in CSS) first: at 0 % / 90 %, 30 x 24 rem.
    expect(calls.slice(1, 4)).toEqual(["at 0,720", `squash ${(24 / 30).toFixed(3)}`, `r ${30 * 16}`]);
    expect(stops.slice(0, 2)).toEqual([
      [0, "rgba(1, 2, 3, 0.5)"],
      [0.72, "rgba(1, 2, 3, 0)"],
    ]);
  });

  it("is used by the app shell", () => {
    expect(app).toMatch(/paintAurora\(auroraRef\.current, onThemeChange\)/);
    expect(app).toMatch(/className="aurora"[^>]*ref=\{auroraRef\}/);
  });
});

describe("the window shows as soon as the interface is on the page", () => {
  it("is asked for at commit (layout effect), not in a later task the hidden page holds back", () => {
    expect(app).toMatch(/useLayoutEffect\(\(\) => showWindowWhenReady\(\), \[\]\)/);
    const feel = fs.readFileSync(path.join(__dirname, "nativeFeel.ts"), "utf8");
    expect(feel).toMatch(/import \{ windowControl \} from "\.\/chrome";/);
    expect(feel).not.toMatch(/import\("\.\/chrome"\)/);
  });
});

describe("Linux: no shadows (the costly part of every repaint there)", () => {
  const L = ':root[data-platform="linux"]';
  it("turns every box shadow off", () => {
    expect(css).toContain(`${L} *,\n${L} *::before,\n${L} *::after {\n  box-shadow: none !important;\n}`);
  });

  it("draws the rings that mean something as outlines instead", () => {
    for (const sel of [
      '.tab[aria-pressed="true"]',
      '.rail-btn[aria-current="page"]',
      '.side-link[aria-current="page"]',
      '.pill[aria-pressed="true"]',
      '.hud-core[aria-pressed="true"]',
      ".input:focus",
      ".add-bar:focus-within",
    ]) {
      const i = css.indexOf(`${L} ${sel} {`);
      expect(i, sel).toBeGreaterThan(0);
      expect(css.slice(i, i + 200), sel).toMatch(/outline: \d+px solid/);
    }
  });

  it("removes drop-shadow filters and keeps the glass panels' top highlight as a border", () => {
    expect(css).toMatch(/:root\[data-platform="linux"\] \.gauge-arc[\s\S]{0,400}filter: none;/);
    expect(css).toMatch(/:where\(:root\[data-platform="linux"\]\) \.task,[\s\S]{0,600}border-top-color:/);
  });
});
