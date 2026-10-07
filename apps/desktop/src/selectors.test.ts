import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url)) + "/";

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((n) => {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) return sources(p);
    return /\.tsx?$/.test(n) && !/\.test\.tsx?$/.test(n) ? [p] : [];
  });
}

describe("store selectors", () => {
  // useSyncExternalStore re-renders whenever the selector's answer is a new object: a selector that builds an
  // array or object (`?? []`, `.filter(...)` returned as is, `({ ... })`) makes React loop until it gives up and
  // the window shows "Something went wrong". Select the stored value and derive from it in the component.
  it("never build a new array or object on every read", () => {
    const bad: string[] = [];
    for (const f of sources(here)) {
      const text = readFileSync(f, "utf8");
      for (const m of text.matchAll(/useStore\(\(s\) => ([^;]*?)\);/g)) {
        const body = m[1]!;
        const fresh = /\?\? \[\]\s*$|\?\? \{\}\s*$|^\(\{|^\[/.test(body.trim()) || (/\.(filter|map|slice|concat)\(/.test(body) && !/\.(length|join\([^)]*\))\s*$/.test(body.trim()));
        if (fresh) bad.push(`${f.slice(here.length)}: ${body.trim()}`);
      }
    }
    expect(bad).toEqual([]);
  });
});
