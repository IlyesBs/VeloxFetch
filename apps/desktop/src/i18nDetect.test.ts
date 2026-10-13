import { describe, expect, it } from "vitest";

(globalThis as unknown as { document: unknown }).document = { documentElement: {} };
const { pickLang } = await import("./i18n");

const HAVE = ["en", "ar", "de", "es", "fr", "id", "it", "pt-br", "ru", "tr", "zh-cn"];

describe("the interface starts in the computer's language", () => {
  it("takes the exact language, else the same language from another country", () => {
    expect(pickLang(["fr-FR"], HAVE)).toBe("fr");
    expect(pickLang(["es-MX", "en-US"], HAVE)).toBe("es");
    expect(pickLang(["pt-BR"], HAVE)).toBe("pt-br");
    expect(pickLang(["pt-PT"], HAVE)).toBe("pt-br");
    expect(pickLang(["pt"], HAVE)).toBe("pt-br");
    expect(pickLang(["de_AT"], HAVE)).toBe("de");
    expect(pickLang(["ar-EG"], HAVE)).toBe("ar");
  });

  it("gives Simplified Chinese only to Simplified Chinese readers", () => {
    for (const l of ["zh-CN", "zh", "zh-Hans", "zh-Hans-SG", "zh-SG"]) expect(pickLang([l], HAVE), l).toBe("zh-cn");
    for (const l of ["zh-TW", "zh-HK", "zh-Hant", "zh-Hant-TW", "zh-MO"]) expect(pickLang([l], HAVE), l).toBeNull();
    expect(pickLang(["zh-TW", "en-GB"], HAVE)).toBe("en");
  });

  it("goes down the list of wanted languages, and says nothing when none is there", () => {
    expect(pickLang(["ja-JP", "ko", "ru-RU"], HAVE)).toBe("ru");
    expect(pickLang(["ja-JP", "ko"], HAVE)).toBeNull();
    expect(pickLang([], HAVE)).toBeNull();
  });

  it("ignores anything that is not a language code", () => {
    expect(pickLang(["", "  ", "../../etc", "en US", "x".repeat(40)], HAVE)).toBeNull();
    expect(pickLang([" TR "], HAVE)).toBe("tr");
  });
});
