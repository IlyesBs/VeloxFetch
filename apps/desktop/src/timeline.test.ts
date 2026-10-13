import { afterEach, describe, expect, it } from "vitest";

(globalThis as unknown as { document: unknown }).document = { documentElement: {} };

const { localEvent } = await import("./timeline");
const { setLang } = await import("./i18n");

afterEach(() => setLang("en"));

// One line of every kind the manager writes (crates/download-manager/src/manager/diag.rs, relink.rs, manager.rs).
const LINES = [
  "Started (attempt 1)",
  "Started again (attempt 3)",
  "Resumed (attempt 2)",
  "Retried (attempt 2)",
  "Finished: 600.0 KB received at 1.2 MB/s",
  "Paused (progress kept)",
  "Cancelled",
  "Downloading again from the start",
  "Doctor: http_404 - problem with: Link - retry: NotRetryable - Blocking",
  "Failed (http_404): HTTP status 404",
  "Server answered 206: 600.0 KB, parallel download possible",
  "Server answered 200: size unknown, one connection only (no verified ranges)",
  "Part 3 failed (try 2): HTTP status 503. Trying again in 2 s",
  "Request failed (try 1): HTTP status 503. Trying again in 500 ms",
  "The server is limiting connections: using 4 from now on",
  "Continuing from 1.5 MB",
  "Cannot continue the saved part (remote resource changed or cannot be validated (ETag / Last-Modified)): starting over",
  "Cannot continue the saved part (.part file missing): starting over",
  "Cannot continue the saved part (the saved bytes differ from the file on the server): starting over",
  "Cannot continue the saved part (size changed: Some(1) -> Some(2)): starting over",
  "New transfer in 16 parts over several connections",
  "New transfer over one connection",
  "The name was taken meanwhile: saved as video (1).mp4",
  "The server is preparing the file (it answers when it is ready): waiting for it",
  "The saved part changed since it was last saved (a crash or an outside change): 12 finished parts checked again, all intact",
  "The saved part changed since it was last saved (a crash or an outside change): 12 finished parts checked again, all intact; 2 older parts could not be checked",
  "The saved part changed since it was last saved: 1 of 12 finished parts no longer match their check and are downloaded again (1.0 MB)",
  "New link: the download starts from it (nothing was saved yet)",
  "New link: the same file (confirmed by the server and by the saved bytes). Continuing from 1048576 bytes",
  "New link: likely the same file (the saved bytes match; chosen by the user). Continuing from 1048576 bytes",
  "New link not used: it gives a file of another size. The saved bytes are kept",
  "New link not used: it did not answer or refused (HTTP 403). The saved bytes are kept",
];

describe("the diagnostics timeline in the user's language", () => {
  it("is shown exactly as written in English", () => {
    for (const l of LINES) expect(localEvent(l)).toBe(l);
  });

  it("says every kind of line in French and Arabic, without English words left", () => {
    for (const lang of ["fr", "ar"] as const) {
      setLang(lang);
      for (const l of LINES) {
        const said = localEvent(l);
        expect(said, l).not.toBe(l);
        expect(said, `${lang}: ${l} -> ${said}`).not.toMatch(/\b(the|and|failed|started|server|parts|trying|again|link|file|attempt|retry|problem)\b/i);
        expect(said, l).not.toMatch(/\{\w+\}/);
      }
    }
  });

  it("leaves a line it does not know as it was", () => {
    setLang("fr");
    expect(localEvent("Something new from a later version")).toBe("Something new from a later version");
  });
});
