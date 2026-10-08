// End-to-end test of Phase 14 (observability & diagnostics) in the real desktop app:
// the Diagnostics card of a download (HTTP status, error code, attempts, retries, connections, speed, timeline),
// Settings > Diagnostics (detailed logging, recent log, report, log folder in a dry run), the log file on disk,
// and the rule that passwords, cookies, sign-in headers, proxy passwords, link query values and the home folder
// never reach the log, the diagnostics or the reports. Writes screenshots to docs/screenshots.
//
//   node tests/e2e/desktop-diagnostics.mjs [path-to-debug-velox-desktop.exe]

import os from "node:os";
import { check, createHarness, exe, finish, fs, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const srv = await startFileServer(600 * 1024);
const h = createHarness({ port: 9361, appPath: APP });
const { page, invoke } = h;

const SECRETS = ["E2E_QUERY_SECRET", "E2E_COOKIE_SECRET", "E2E_PW_SECRET", "E2E_PROXY_SECRET", "E2E_FRAG_SECRET", Buffer.from("user:E2E_PW_SECRET").toString("base64"), Buffer.from("user:pass").toString("base64")];
const leaks = (text) => SECRETS.filter((s) => text.includes(s));
const home = os.homedir().toLowerCase();

const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const clickText = async (sel, label) => {
  await waitFor(() => ev(`[...document.querySelectorAll(${JSON.stringify(sel)})].some((b) => b.innerText.trim() === ${JSON.stringify(label)})`), `${label}`);
  return page(`(() => { [...document.querySelectorAll(${JSON.stringify(sel)})].find((b) => b.innerText.trim() === ${JSON.stringify(label)}).click(); return true; })()`);
};
const toasts = () => ev(`[...document.querySelectorAll('[role=alert],[role=status]')].map((e) => e.innerText).join(' | ')`);
const clearToasts = () => page(`(() => { document.querySelectorAll('[aria-label=Dismiss]').forEach((b) => b.click()); return true; })()`);
const settle = async (id) => waitFor(async () => ["completed", "failed"].includes((await invoke("get_download", { id })).ok?.status), `download ${id}`, 30000);
async function get(url, extra = {}) {
  const d = (await invoke("create_download", { request: { url, ...extra } })).ok;
  await settle(d.id);
  await sleep(200); // the run's diagnostics are saved just after the final status
  return (await invoke("get_download", { id: d.id })).ok;
}
const attr = async (sel, a) => (await page(`document.querySelector(${JSON.stringify(sel)})?.getAttribute(${JSON.stringify(a)}) ?? null`)).ok;
const openDetail = async (id) => {
  await click("[data-testid=nav-downloads]");
  await click(`[data-testid=download-${id}]`);
  await waitFor(() => has("[data-testid=diagnostics-card] [data-testid=diag-http]"), "diagnostics card");
};

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================ downloads that exercise every path
  const ok = await get(`${srv.base}/files/data.bin`);
  const missing = await get(`${srv.base}/404/missing.zip`);
  const signed = await get(`${srv.base}/auth/private.bin?token=E2E_QUERY_SECRET#E2E_FRAG_SECRET`, { username: "user", password: "pass", cookies: "sid=E2E_COOKIE_SECRET" });
  const wrong = await get(`${srv.base}/auth/other.bin?token=E2E_QUERY_SECRET`, { username: "user", password: "E2E_PW_SECRET" });
  check("the test downloads end as expected", ok.status === "completed" && missing.status === "failed" && signed.status === "completed" && wrong.status === "failed", JSON.stringify([ok.status, missing.status, signed.status, wrong.status]));
  check("a failure carries a short error code", missing.errorCode === "http_404" && wrong.errorCode === "http_401", `${missing.errorCode} ${wrong.errorCode}`);
  // A proxy with a password, then back to the system's (its password must never be written anywhere readable).
  const setProxy = await invoke("set_network", { request: { proxyMode: "manual", proxyUrl: "http://127.0.0.1:9", proxyUsername: "proxyuser", proxyPassword: "E2E_PROXY_SECRET" } });
  const back = await invoke("set_network", { request: { proxyMode: "system" } });
  check("network settings with a proxy password are accepted and reverted", !!setProxy.ok && !!back.ok, JSON.stringify(setProxy.err ?? back.err));

  // ============================================================ the Diagnostics card
  await openDetail(missing.id);
  check("a failed download shows HTTP 404 and its error code", (await text("[data-testid=diag-http]")) === "404 Not found" && (await text("[data-testid=diag-code]")) === "http_404", `${await text("[data-testid=diag-http]")} / ${await text("[data-testid=diag-code]")}`);
  check("attempts and retries are shown", (await text("[data-testid=diag-attempts]")) === "1" && (await text("[data-testid=diag-retries]")) === "0");
  const timeline = await text("[data-testid=diag-events]");
  check("the timeline tells the start and the failure", /Started \(attempt 1\)/.test(timeline) && /Failed \(http_404\): HTTP status 404/.test(timeline), timeline);
  check("the error card keeps the error code with the technical details", await has("[data-testid=detail-error] [data-testid=detail-error-code]"));
  // Download Doctor: what happened, where, whether retrying helps, what to do (all from the evidence).
  check("Download Doctor diagnoses a missing file", (await attr("[data-testid=doctor]", "data-category")) === "notFound" && (await attr("[data-testid=doctor]", "data-retry")) === "notRetryable", `${await attr("[data-testid=doctor]", "data-category")} ${await attr("[data-testid=doctor]", "data-retry")}`);
  check("it says the link is the problem and retrying will not help", (await text("[data-testid=doctor-source]"))?.includes("the link itself") && (await text("[data-testid=doctor-retry]"))?.includes("will not help"), await text("[data-testid=doctor]"));
  check("it advises a fresh link", (await text("[data-testid=doctor-advice]"))?.includes("fresh link"));
  check("the timeline names the diagnosis before the failure", /Doctor: http_404 - problem with: Link - retry: NotRetryable[\s\S]*Failed \(http_404\)/.test(timeline), timeline);
  await h.screenshot(path.join(SHOTS, "doctor.png"), { width: 1100, height: 760 });
  await page(`(() => { document.querySelector('[data-testid=diagnostics-card]').scrollIntoView(); return true; })()`);
  await h.screenshot(path.join(SHOTS, "detail-diagnostics-failed.png"), { width: 1100, height: 760 });

  // Retry: a second attempt, counted live.
  await click("[data-testid=detail-error] button:last-child");
  await waitFor(async () => (await text("[data-testid=diag-attempts]")) === "2", "second attempt", 15000);
  check("Retry counts a second attempt", (await text("[data-testid=diag-events]")).includes("Retried (attempt 2)"));

  await openDetail(ok.id);
  check("a finished download shows 206, no error code, speed and time", (await text("[data-testid=diag-http]")) === "206 Partial content" && (await text("[data-testid=diag-code]")) === "none" && (await text("[data-testid=diag-speed]")) !== "—");
  check("its timeline tells the server's answer and the end", /Server answered 206: 600\.0 KB, parallel download possible/.test(await text("[data-testid=diag-events]")) && /Finished: 600\.0 KB received/.test(await text("[data-testid=diag-events]")), await text("[data-testid=diag-events]"));
  await clearToasts();
  await click("[data-testid=diagnostics-copy]");
  await waitFor(async () => /copied|clipboard/i.test(await toasts()), "copy toast", 5000).catch(() => null);
  check("Copy answers with a notice (copied, or no clipboard in this window)", /copied|clipboard/i.test(await toasts()), await toasts());
  await h.screenshot(path.join(SHOTS, "detail-diagnostics.png"), { width: 1100, height: 760 });

  // Everything diagnostics returns for these downloads, cleaned of secrets.
  let all = "";
  for (const d of [ok, missing, signed, wrong]) {
    const g = (await invoke("get_download_diagnostics", { id: d.id })).ok;
    all += JSON.stringify(g);
  }
  check("the diagnostics and their reports contain no secret", leaks(all).length === 0, leaks(all).join(","));
  check("... and no home folder", !all.toLowerCase().includes(home));
  const unknown = await invoke("get_download_diagnostics", { id: 987654 });
  check("an unknown download has no diagnostics", unknown.err?.code === "not_found", JSON.stringify(unknown));

  // ============================================================ Settings > Diagnostics
  await click("[data-testid=nav-settings]");
  await clickText("[data-testid=settings-nav] button", "Diagnostics");
  await waitFor(() => has("[data-testid=log-lines] li"), "log lines");
  await waitFor(async () => (await text("[data-testid=log-lines]")).includes("download ended"), "the end-of-run line", 8000);
  const shown = await text("[data-testid=log-lines]");
  check("the recent log shows one line per run with id, status, code, retries, HTTP status, connections, speed",
    /download ended id=\d+ status=failed error_code=http_404 retries=\d+ http_status=404 connections=\d+ avg_speed_bps=\d+/.test(shown), shown.slice(0, 400));
  check("the recent log shows no secret", leaks(shown).length === 0, leaks(shown).join(","));
  await click("[data-testid=log-filter-problems]");
  await sleep(300);
  const problems = await ev(`[...document.querySelectorAll('[data-testid=log-lines] li')].map((l) => l.innerText)`);
  check("Problems shows warnings and errors only", problems.length > 0 && problems.every((l) => /\b(WARN|ERROR)\b/.test(l)), problems.slice(0, 3).join(" | "));
  await click("[data-testid=log-filter-all]");
  await click("[data-testid=detailed-logging]");
  await waitFor(async () => (await invoke("get_logs", { limit: 5, problemsOnly: false })).ok?.detailed === true, "detailed on");
  const shownOn = await waitFor(() => ev(`document.querySelector('[data-testid=detailed-logging]').checked`), "checkbox follows", 5000).catch(() => false);
  check("detailed logging turns on", shownOn === true);
  await h.screenshot(path.join(SHOTS, "settings-diagnostics.png"), { width: 1100, height: 900 });
  await click("[data-testid=detailed-logging]");
  await waitFor(async () => (await invoke("get_logs", { limit: 5, problemsOnly: false })).ok?.detailed === false, "detailed off");
  await clearToasts();
  await click("[data-testid=open-log-folder]");
  await sleep(400);
  check("Open log folder works (a dry run in tests)", !/error|cannot/i.test(await toasts()), await toasts());
  await click("[data-testid=copy-report]");
  await waitFor(async () => /copied|clipboard/i.test(await toasts()), "report toast", 5000).catch(() => null);
  check("Copy diagnostics report answers with a notice", /copied|clipboard/i.test(await toasts()), await toasts());

  // ============================================================ the report and the file on disk
  const report = (await invoke("get_diagnostics_report")).ok ?? "";
  check("the report has settings, failures and the recent log", /diagnostics report/.test(report) && /Latest failures/.test(report) && /http_404/.test(report) && /Recent log/.test(report), report.slice(0, 300));
  check("the report names no proxy, user or secret", leaks(report).length === 0 && !report.includes("proxyuser") && !report.includes("127.0.0.1:9\n"), leaks(report).join(","));
  check("the report has no home folder", !report.toLowerCase().includes(home));
  const logFile = path.join(h.dataDir, "logs", "velox.log");
  const onDisk = fs.existsSync(logFile) ? fs.readFileSync(logFile, "utf8") : "";
  check("the log file is in the app's data folder", onDisk.includes("VeloxFetch 0.1.0 started on") && onDisk.includes("download ended"), logFile);
  check("the log file contains no secret", leaks(onDisk).length === 0, leaks(onDisk).join(","));
  check("the log file hides the home folder", !onDisk.toLowerCase().includes(home));
  check("the log file keeps links useful but without their values", /private\.bin\?token=\[redacted\]/.test(onDisk) || /other\.bin\?token=\[redacted\]/.test(onDisk) || !/\?token=/.test(onDisk));

  const errs = await ev("window.__errs");
  check("no JavaScript errors", errs.length === 0, errs.join(" | "));
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  srv.close();
  finish();
}
