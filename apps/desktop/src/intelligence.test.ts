import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import ar from "./locales/ar.json";
import { errorLabel, evidence, findingLine, storageLine, strategyLine, warnings } from "./intelligence";
import type { Analysis, Finding, FindingCode } from "./types";

const base: Analysis = {
  kind: "file",
  filename: "a.zip",
  size: 4 * 1024 ** 3,
  contentType: "application/zip",
  https: true,
  redirected: false,
  finalHost: "dl.example.com",
  rangeClaimed: true,
  rangeVerified: true,
  etag: true,
  lastModified: false,
  resume: "supported",
  strategy: "parallel",
  connections: 16,
  access: "public",
  temporary: false,
  expiresAtMs: null,
  storage: "ok",
  freeBytes: 100 * 1024 ** 3,
  duplicate: null,
  health: "good",
  findings: [],
  errorCode: null,
};
const finding = (code: FindingCode, level: Finding["level"] = "warning", over: Partial<Finding> = {}): Finding => ({
  code,
  level,
  detail: null,
  bytes: null,
  free: null,
  atMs: null,
  ...over,
});
const keys = (lines: { key: string }[]) => lines.map((l) => l.key);

describe("Download Intelligence lines", () => {
  it("says only what was observed", () => {
    expect(keys(evidence(base))).toEqual(["intel.ev.https", "intel.ev.size", "intel.ev.rangeVerified", "intel.ev.etag"]);
    const ignored = evidence({ ...base, https: false, rangeVerified: false, rangeClaimed: true, resume: "notSupported", etag: false, size: null });
    expect(keys(ignored)).toEqual(["intel.ev.http", "intel.ev.noSize", "intel.ev.rangeClaimed", "intel.ev.rangeIgnored"]);
    const redirected = evidence({ ...base, redirected: true, finalHost: "cdn.example.net" });
    expect(redirected[1]).toEqual({ key: "intel.ev.redirected", vars: { host: "cdn.example.net" } });
  });

  it("a failed check says so, and nothing about a server answer it did not get", () => {
    const failed: Analysis = { ...base, kind: "unknown", size: null, rangeClaimed: false, rangeVerified: false, etag: false, errorCode: "http_503" };
    expect(evidence(failed)[0]).toEqual({ key: "intel.ev.noAnswer", vars: { code: "HTTP 503" } });
    expect(keys(evidence(failed))).not.toContain("intel.ev.noSize");
    expect(errorLabel("network")).toBe("network");
  });

  it("warnings show at once, information only under Why?", () => {
    const a = { ...base, findings: [finding("noSpace", "problem"), finding("insecure"), finding("unknownSize", "info")] };
    expect(warnings(a).map((f) => f.code)).toEqual(["noSpace", "insecure"]);
    expect(keys(evidence(a))).toContain("intel.f.unknownSize");
    expect(keys(evidence(a))).not.toContain("intel.f.insecure");
  });

  it("fills in the values of each sentence", () => {
    expect(findingLine(finding("serverError", "problem", { detail: "http_404" }))).toEqual({ key: "intel.f.serverError", vars: { code: "HTTP 404" } });
    expect(findingLine(finding("temporaryLink")).key).toBe("intel.f.temporaryLink");
    expect(findingLine(finding("temporaryLink", "warning", { atMs: 1_700_000_000_000 })).key).toBe("intel.f.temporaryLinkUntil");
    expect(findingLine(finding("noSpace", "problem", { bytes: 2048, free: 1024 })).vars).toEqual({ size: "2.0 KB", free: "1.0 KB" });
    expect(strategyLine(base)).toEqual({ key: "intel.strategy.parallel", vars: { n: 16 } });
    expect(strategyLine({ ...base, strategy: "conservative", connections: null }).key).toBe("intel.strategy.conservative");
    expect(storageLine({ ...base, storage: "cannotVerify", size: null }).key).toBe("intel.storage.cannotVerify");
  });

  it("every sentence exists in every language", () => {
    const codes: FindingCode[] = [
      "insecure", "httpsDowngrade", "otherSite", "authRequired", "sessionRequired", "sessionSent", "accessRefused", "serverError",
      "unreachable", "slowServer", "temporaryLink", "expiredLink", "webPage", "noSpace", "tooLargeForDrive", "lowSpace",
      "storageUnverified", "noResume", "resumeUnverified", "unknownSize", "duplicate", "stream",
    ];
    const needed = new Set<string>(["intel.f.temporaryLinkUntil"]);
    for (const c of codes) needed.add(findingLine(finding(c)).key);
    for (const s of ["parallel", "single", "conservative", "stream"] as const) needed.add(strategyLine({ ...base, strategy: s }).key);
    for (const s of ["ok", "low", "insufficient", "tooLargeForDrive", "cannotVerify", "unavailable"] as const) needed.add(storageLine({ ...base, storage: s }).key);
    for (const r of ["supported", "likelySupported", "notSupported", "unknown"]) needed.add(`intel.resume.${r}`);
    for (const h of ["good", "caution", "problem", "unknown"]) needed.add(`intel.health.${h}`);
    for (const k of ["webPage", "stream", "videoPage"]) needed.add(`intel.kind.${k}`);
    for (const r of ["sameLink", "sameFinalLink", "sameFile"]) needed.add(`intel.dup.${r}`);
    for (const file of [en, fr, ar] as unknown as Record<string, string>[]) {
      expect([...needed].filter((k) => !(k in file))).toEqual([]);
    }
  });
});
