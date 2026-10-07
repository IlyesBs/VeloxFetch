// Phase 11: the UI must never keep secrets (or anything but its theme) in the browser's storage,
// and must never receive cookies / credentials from the backend.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((n) => {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) return sources(p);
    return /\.(ts|tsx)$/.test(n) && !/\.test\.tsx?$/.test(n) ? [p] : [];
  });
}

const here = dirname(fileURLToPath(import.meta.url));

describe("browser storage and secrets", () => {
  const files = sources(join(here));

  it("only the theme and language preferences use localStorage; nothing uses sessionStorage, IndexedDB or cookies", () => {
    for (const f of files) {
      const text = readFileSync(f, "utf8");
      expect(text, `${f} uses sessionStorage`).not.toMatch(/sessionStorage/);
      expect(text, `${f} uses indexedDB`).not.toMatch(/indexedDB/i);
      expect(text, `${f} touches document.cookie`).not.toMatch(/document\.cookie/);
      if (/localStorage/.test(text)) {
        // cosmetic, per-user choices only: the theme, the interface language and the layout (prefs.ts)
        expect(f.endsWith("theme.ts") || f.endsWith("i18n.ts") || f.endsWith("prefs.ts"), `${f} uses localStorage`).toBe(true);
      }
    }
  });

  it("the layout preferences keep only the four cosmetic choices", () => {
    const text = readFileSync(join(here, "prefs.ts"), "utf8");
    expect(text.match(/setItem\(/g)?.length).toBe(1);
    expect(text).toMatch(/const KEY = "velox\.ui"/);
    expect(text).toMatch(/interface UiPrefs \{[^}]*sideFolded: boolean;[^}]*density: Density;[^}]*sort: SortOrder;[^}]*queue: number;\s*\}/);
  });

  it("the theme module stores exactly one non-secret key", () => {
    const text = readFileSync(join(here, "theme.ts"), "utf8");
    expect(text.match(/setItem\(/g)?.length).toBe(1);
    expect(text).toMatch(/const KEY = "velox\.theme"/);
  });

  it("the UI never receives cookies, passwords or authorization headers", () => {
    // A secret may only appear in a request the UI sends once (typed by the user in a form) and never gets back.
    const WRITE_ONLY = ["NewDownload", "SetNetwork"];
    const types = readFileSync(join(here, "types.ts"), "utf8");
    const secret = /^\s+(cookies?|passwords?|proxyPassword|authorization|credentials?)\??:/im;
    const blocks = [...types.matchAll(/export interface (\w+)[^{]*\{([\s\S]*?)\n\}/g)];
    expect(blocks.length).toBeGreaterThan(10);
    for (const [, name, body] of blocks) {
      if (secret.test(body!)) expect(WRITE_ONLY, `${name} carries a secret field`).toContain(name);
    }
    // ... and no command returns one of them.
    const api = readFileSync(join(here, "api.ts"), "utf8");
    for (const name of WRITE_ONLY) expect(api).not.toMatch(new RegExp(`call<[^>]*\\b${name}\\b`));
    // Received types say only whether a password exists.
    expect(types).toMatch(/proxyHasPassword: boolean/);
  });
});
