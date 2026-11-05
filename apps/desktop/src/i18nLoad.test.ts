import { afterEach, describe, expect, it } from "vitest";
import fs from "node:fs";
import path from "node:path";
import fr from "./locales/fr.json";
import ar from "./locales/ar.json";

// setLang marks <html>: a bare stand-in is enough here (no browser in these tests).
(globalThis as unknown as { document: unknown }).document = { documentElement: {} };

const { LANGS, getLang, loadLang, setLang, t } = await import("./i18n");

afterEach(() => setLang("en"));

describe("languages loaded when used", () => {
  it("still lists every language file, English first, with its own name and direction", () => {
    const files = fs.readdirSync(path.join(__dirname, "locales")).filter((f) => f.endsWith(".json")).map((f) => f.slice(0, -5));
    expect(LANGS.map((l) => l.lang).sort()).toEqual(files.sort());
    expect(LANGS[0]!.lang).toBe("en");
    expect(LANGS.find((l) => l.lang === "fr")).toMatchObject({ label: fr._meta.name, dir: "ltr" });
    expect(LANGS.find((l) => l.lang === "ar")).toMatchObject({ label: ar._meta.name, dir: "rtl" });
  });

  it("switches once the texts are there, never to a language with English texts in between", async () => {
    await setLang("fr");
    expect(getLang()).toBe("fr");
    expect(t("nav.settings")).toBe(fr["nav.settings"]);
  });

  it("the last choice wins when two are made at once", async () => {
    const a = setLang("ar");
    const b = setLang("fr");
    await Promise.all([a, b]);
    expect(getLang()).toBe("fr");
  });

  it("a language without a file leaves the texts in English", async () => {
    await loadLang("xx");
    expect(t("nav.settings")).toBe("Settings");
  });
});
