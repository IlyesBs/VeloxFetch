import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import ar from "./locales/ar.json";
import { advice, buttons, factorLines, headline, retryLine, sourceLine, spaceLine, waitText } from "./doctor";
import type { Diagnosis, DoctorAction, DoctorCategory, DoctorSource, FactorCode, RetryClass, DoctorSeverity } from "./types";

const base: Diagnosis = {
  category: "serverTemporary",
  code: "http_503",
  source: "server",
  httpStatus: 503,
  retry: "retryableWithDelay",
  retryAfterSecs: null,
  retryAtMs: null,
  severity: "recoverable",
  factors: [],
  actions: ["retryLater", "cancel"],
  neededBytes: null,
  freeBytes: null,
};

describe("Download Doctor lines", () => {
  it("says what happened, where, and whether retrying helps", () => {
    expect(headline(base)).toEqual({ key: "doc.cat.serverTemporary", vars: { code: 503 } });
    expect(sourceLine(base).key).toBe("doc.source.server");
    expect(retryLine(base, 0).key).toBe("doc.retry.retryableWithDelay");
    expect(retryLine({ ...base, retryAfterSecs: 60 }, 0)).toEqual({ key: "doc.retry.waitFor", vars: { s: "1 min" } });
    expect(retryLine({ ...base, retry: "notRetryable" }, 0).key).toBe("doc.retry.notRetryable");
  });

  it("counts down while the engine retries on its own", () => {
    expect(retryLine({ ...base, retryAtMs: 30_000 }, 0)).toEqual({ key: "doc.retryIn", vars: { s: "30 s" } });
    expect(retryLine({ ...base, retryAtMs: 100 }, 0).key).toBe("doc.retryingNow");
    expect(waitText(3900)).toBe("1 h 5 min");
  });

  it("buttons only for what the app can do, the rest is advice", () => {
    expect(buttons(base)).toEqual([{ action: "retry", label: "doc.btn.retryAnyway" }]);
    const disk: Diagnosis = { ...base, category: "diskFull", actions: ["freeSpace", "retry", "cancel"], neededBytes: 8 * 1024 ** 3, freeBytes: 1024 ** 3 };
    expect(buttons(disk).map((b) => b.action)).toEqual(["openFolder", "retry"]);
    expect(spaceLine(disk)?.vars).toEqual({ need: "8.0 GB", free: "1.0 GB", more: "7.0 GB" });
    const lost: Diagnosis = { ...base, category: "notFound", actions: ["newLink", "cancel"] };
    expect(buttons(lost)).toEqual([]);
    expect(advice(lost)).toEqual([{ key: "doc.act.newLink" }]);
    expect(buttons({ ...base, actions: ["restartSafely", "cancel"] })[0]!.label).toBe("doc.btn.restart");
    expect(factorLines({ ...base, factors: [{ code: "repeatedRetries", n: 5 }] })).toEqual([{ key: "doc.factor.repeatedRetries", vars: { n: 5 } }]);
  });

  it("every sentence exists in every language", () => {
    const cats: DoctorCategory[] = [
      "offline", "dns", "timeout", "connectionReset", "connectionFailed", "tls", "authRequired", "proxyAuth", "accessDenied",
      "sessionExpired", "linkExpired", "notFound", "requestTimeout", "conflict", "rateLimited", "serverTemporary", "serverError",
      "httpOther", "redirect", "rangeFailure", "diskFull", "permission", "folderNotAllowed", "writeFailed", "fileExists", "integrity",
      "incomplete", "persistence", "webPage", "invalidResource", "proxyInvalid", "cancelled", "unknown",
    ];
    const sources: DoctorSource[] = ["server", "network", "thisComputer", "link", "account", "content", "unknown"];
    const retries: RetryClass[] = ["retryable", "retryableWithDelay", "retryAfterUserAction", "notRetryable", "unknown"];
    const sevs: DoctorSeverity[] = ["info", "warning", "recoverable", "blocking", "critical"];
    const factors: FactorCode[] = ["repeatedRetries", "unstableConnection", "serverThrottled", "workedBefore", "sessionUsed", "temporaryLink", "sameAsBefore", "offline", "startedOver", "noResume"];
    const acts: DoctorAction[] = ["retry", "retryLater", "restartSafely", "signIn", "reauthInBrowser", "newLink", "freeSpace", "checkConnection", "checkProxy", "checkFolder", "cancel"];
    const needed = new Set<string>(["doc.retry.waitFor", "doc.retryIn", "doc.retryingNow", "doc.space", "doc.btn.retry"]);
    for (const c of cats) needed.add(headline({ ...base, category: c }).key);
    for (const s of sources) needed.add(sourceLine({ ...base, source: s }).key);
    for (const r of retries) needed.add(retryLine({ ...base, retry: r }, 0).key);
    for (const s of sevs) needed.add(`doc.sev.${s}`);
    for (const f of factors) needed.add(`doc.factor.${f}`);
    for (const l of advice({ ...base, actions: acts })) needed.add(l.key);
    for (const b of buttons({ ...base, actions: acts })) needed.add(b.label);
    for (const file of [en, fr, ar] as unknown as Record<string, string>[]) {
      expect([...needed].filter((k) => !(k in file))).toEqual([]);
    }
  });
});
