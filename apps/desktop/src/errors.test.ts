import { afterEach, describe, expect, it } from "vitest";

// setLang marks <html>: a bare stand-in is enough here (no browser in these tests).
(globalThis as unknown as { document: unknown }).document = { documentElement: {} };

const { localError, localMediaError } = await import("./errors");
const { setLang, t } = await import("./i18n");

afterEach(() => setLang("en"));

describe("errors in the user's language", () => {
  it("video job failures are said by their code, the engine's text stays for English and unknown codes", () => {
    const raw = "this stream is protected (DRM). Protected streams are not supported";
    expect(localMediaError("media_drm", raw)).toBe(raw);
    setLang("fr");
    expect(localMediaError("media_drm", raw)).toBe(t("e.mediaDrm"));
    expect(localMediaError("media_drm", raw)).not.toMatch(/stream|supported/);
    for (const code of ["media_live", "media_playlist", "media_sign_in", "media_no_video", "media_unsupported", "media_site", "media_invalid", "media_limit", "media_remux", "media_network", "media_file"]) {
      expect(localMediaError(code, "english text"), code).not.toBe("english text");
    }
    // Codes shared with ordinary downloads reuse their sentences.
    expect(localMediaError("http_404", "network error: HTTP status 404")).toBe(t("e.404"));
    expect(localMediaError("something_new", "english text")).toBe("english text");
    expect(localMediaError(null, null)).toBe("");
  });

  it("a failed download on the queue page is said in the user's language", () => {
    setLang("ar");
    expect(localError("http_404", "The file is not on the server")).toBe(t("e.404"));
  });
});
