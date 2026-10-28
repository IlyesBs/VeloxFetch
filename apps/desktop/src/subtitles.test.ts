import { describe, expect, it } from "vitest";
import type { TrackOption } from "./types";

// setLang marks <html>: a bare stand-in is enough here (no browser in these tests).
(globalThis as unknown as { document: unknown }).document = { documentElement: {} };
const { setLang } = await import("./i18n");
const { localLabel, subtitleLabel, subtitlesFor } = await import("./components/MediaDialog");

const track = (id: string, label: string, language: string | null): TrackOption => ({ id, label, language, group: null, default: false });

describe("a video page's subtitles", () => {
  const subs = [
    track("mar", "Arabic", "ar"),
    track("mfr", "French", "fr"),
    track("aen-orig", "English · auto-generated", "en"),
    track("afr-CA", "French (Canada) · auto-translated", "fr-CA"),
    track("ade", "German · auto-translated", "de"),
  ];

  it("puts the app's language first and keeps the page's order otherwise", () => {
    expect(subtitlesFor(subs, "fr").map((s) => s.id)).toEqual(["mfr", "afr-CA", "mar", "aen-orig", "ade"]);
    expect(subtitlesFor(subs, "pt-br").map((s) => s.id)).toEqual(subs.map((s) => s.id));
  });

  it("names each language in the app's language and says how it was made", () => {
    setLang("en");
    expect(subtitleLabel(subs[0]!, "en")).toBe("Arabic");
    expect(subtitleLabel(subs[2]!, "en")).toBe("English · auto-generated");
    setLang("fr");
    expect(subtitleLabel(subs[0]!, "fr").toLowerCase()).toBe("arabe");
    expect(subtitleLabel(subs[4]!, "fr").toLowerCase()).toBe("allemand · traduction automatique");
    setLang("ar");
    expect(subtitleLabel(subs[1]!, "ar")).toBe("الفرنسية");
    expect(localLabel("English · auto-generated")).toBe("English · مُنشأة تلقائيًا");
    setLang("en");
  });

  it("keeps the page's name when the language code is unknown", () => {
    expect(subtitleLabel(track("aqq-zz", "Somewhere", "qq-zz"), "en")).toBe("Somewhere");
    expect(subtitleLabel(track("mxx", "Other", null), "en")).toBe("Other");
  });
});
