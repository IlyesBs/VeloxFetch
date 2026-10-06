// Every translation file must be complete and safe to show: same keys as en.json, the same {placeholders},
// a _meta entry, and only <b>…</b> as markup (the UI renders nothing else).

import fs from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";

const dir = path.join(__dirname, "locales");
const load = (f: string) => JSON.parse(fs.readFileSync(path.join(dir, f), "utf8")) as Record<string, unknown>;
const en = load("en.json");
const keys = Object.keys(en).filter((k) => k !== "_meta");
const holes = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

describe("locale files", () => {
  const files = fs.readdirSync(dir).filter((f) => f.endsWith(".json"));

  it("ships English, French and Arabic", () => {
    expect(files).toEqual(expect.arrayContaining(["en.json", "fr.json", "ar.json"]));
  });

  for (const f of files) {
    describe(f, () => {
      const d = load(f);
      it("names the language", () => {
        const meta = d._meta as { name?: string; dir?: string } | undefined;
        expect(meta?.name).toBeTruthy();
        expect(["ltr", "rtl", undefined]).toContain(meta?.dir);
      });
      it("has every key of en.json and no unknown key", () => {
        expect(keys.filter((k) => typeof d[k] !== "string" || d[k] === "")).toEqual([]);
        expect(Object.keys(d).filter((k) => k !== "_meta" && !(k in en))).toEqual([]);
      });
      it("keeps the same placeholders", () => {
        expect(keys.filter((k) => JSON.stringify(holes(String(d[k]))) !== JSON.stringify(holes(String(en[k]))))).toEqual([]);
      });
      it("uses no markup other than <b>", () => {
        expect(keys.filter((k) => /<(?!\/?b>)[a-z/]/i.test(String(d[k])))).toEqual([]);
      });
      it("never names a helper program", () => {
        expect(keys.filter((k) => /yt-dlp|youtube-dl|\bdeno\b|ffmpeg|ffprobe/i.test(String(d[k])))).toEqual([]);
      });
    });
  }
});
