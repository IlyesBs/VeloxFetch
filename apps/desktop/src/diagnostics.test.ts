// Phase 14: diagnostics stay on this computer. The UI shows what Rust already cleaned of secrets, and has no way
// to send anything anywhere: no network calls of its own, no telemetry.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { formatDuration, formatHttpStatus } from "./format";

const here = dirname(fileURLToPath(import.meta.url));

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((n) => {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) return sources(p);
    return /\.(ts|tsx)$/.test(n) && !/\.test\.tsx?$/.test(n) ? [p] : [];
  });
}

describe("diagnostics formatting", () => {
  it("names common HTTP statuses", () => {
    expect(formatHttpStatus(206)).toBe("206 Partial content");
    expect(formatHttpStatus(404)).toBe("404 Not found");
    expect(formatHttpStatus(418)).toBe("418");
    expect(formatHttpStatus(null)).toBe("—");
  });

  it("says how long a download transferred", () => {
    expect(formatDuration(0.2)).toBe("under 1 s");
    expect(formatDuration(42)).toBe("42 s");
    expect(formatDuration(125)).toBe("2 min 5 s");
    expect(formatDuration(3725)).toBe("1 h 2 min");
  });
});

describe("nothing leaves this computer", () => {
  it("the UI makes no network requests of its own (no telemetry): it only talks to Rust", () => {
    for (const f of sources(here)) {
      const text = readFileSync(f, "utf8");
      for (const [what, re] of [
        ["fetch()", /\bfetch\s*\(/],
        ["XMLHttpRequest", /XMLHttpRequest/],
        ["sendBeacon", /sendBeacon/],
        ["WebSocket", /new\s+WebSocket/],
        ["EventSource", /new\s+EventSource/],
      ] as const) {
        expect(text, `${f} uses ${what}`).not.toMatch(re);
      }
    }
  });

  it("diagnostics types carry no secret field", () => {
    const types = readFileSync(join(here, "types.ts"), "utf8");
    for (const name of ["Diagnostics", "DiagEvent", "LogLine", "Logs"]) {
      const body = types.match(new RegExp(`export interface ${name} \\{([\\s\\S]*?)\\n\\}`))?.[1];
      expect(body, name).toBeDefined();
      expect(body).not.toMatch(/^\s+(cookies?|passwords?|authorization|headers|token|url)\??:/im);
    }
  });

  it("the diagnostics report and log only go to the clipboard, never into storage", () => {
    for (const file of ["components/DiagnosticsCard.tsx", "components/DiagnosticsSettings.tsx"]) {
      const text = readFileSync(join(here, file), "utf8");
      expect(text).not.toMatch(/localStorage|sessionStorage|indexedDB/i);
      expect(text).toMatch(/actions\.copy\(/);
    }
  });
});
