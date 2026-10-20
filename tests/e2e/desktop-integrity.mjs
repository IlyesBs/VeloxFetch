// End-to-end test of the publisher's checksum (feature #6) in the real desktop app: a site that publishes a checksum
// list next to its file (SHA256SUMS, as Ubuntu and Debian do). The Add dialog says the list was found; the card says
// "Will be verified" then "Verified"; the Completed list, the History and the details page say so too; a file that
// does not match its list is not kept and the card says it arrived damaged; a file with no list is finished as before
// ("Not verified"); the switch in Settings turns it off and on. Every text in every language fits. The local test
// server is plain http, so `VELOX_PUBLISHER_HTTP` lets the app look next to it (debug builds only; a release looks
// next to https links only).
// Writes docs/screenshots/integrity-dark.png.
//
//   node tests/e2e/desktop-integrity.mjs [path-to-debug-velox-desktop]

import http from "node:http";
import { check, createHarness, crypto, exe, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const FILE = crypto.randomBytes(6 * 1024 * 1024);
const SHA = crypto.createHash("sha256").update(FILE).digest("hex");
// Paths of the site: `/ok/` publishes the right list, `/bad/` a wrong one, `/none/` none.
const lists = {
  "/ok/SHA256SUMS": `${"a".repeat(64)}  other.iso\n${SHA}  ubuntu-desktop.iso\n`,
  "/bad/SHA256SUMS": `${"0".repeat(64)}  app-setup.zip\n`,
};
const site = { delayMs: 0, asked: [] };
const server = http.createServer(async (req, res) => {
  const url = req.url.split("?")[0];
  if (url in lists) {
    site.asked.push({ url, headers: req.headers });
    return void res.writeHead(200, { "Content-Type": "text/plain" }).end(lists[url]);
  }
  if (!/\.(iso|zip|bin)$/.test(url)) {
    site.asked.push({ url, headers: req.headers });
    return void res.writeHead(404).end();
  }
  const m = /bytes=(\d+)-(\d*)/.exec(req.headers.range ?? "");
  const total = FILE.length;
  let [start, end, status] = [0, total - 1, 200];
  const headers = { "Accept-Ranges": "bytes", ETag: '"pub-1"', "Content-Type": "application/octet-stream" };
  if (m && req.method === "GET") {
    start = Number(m[1]);
    end = m[2] ? Math.min(Number(m[2]), total - 1) : total - 1;
    status = 206;
    headers["Content-Range"] = `bytes ${start}-${end}/${total}`;
  }
  headers["Content-Length"] = end - start + 1;
  res.writeHead(status, headers);
  if (req.method === "HEAD") return void res.end();
  for (let pos = start; pos <= end; pos += 65536) {
    if (res.destroyed) return;
    res.write(FILE.subarray(pos, Math.min(pos + 65536, end + 1)));
    if (site.delayMs) await sleep(site.delayMs);
  }
  res.end();
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

const h = createHarness({ port: 9391, appPath: APP, env: { VELOX_PUBLISHER_HTTP: "1" } });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const get = async (id) => (await invoke("get_download", { id })).ok;
const until = async (id, status, ms = 30000) => waitFor(async () => (await get(id))?.status === status, `download ${id} ${status}`, ms);
const pickLang = async (lang) => {
  await click("[data-testid=nav-settings]");
  await waitFor(() => has("[data-testid=settings-lang]"), "language drop-down");
  await page(`(() => { const s = document.querySelector('[data-testid=settings-lang]');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(s, ${JSON.stringify(lang)});
    s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await waitFor(async () => (await ev("document.documentElement.lang")) === lang, `the ${lang} interface`);
};
// Badges whose words are cut ("…") or that stick out of their line. A badge may show only its shield in a tight list
// row (the words are then in its tooltip and accessible name).
const cut = (sel) =>
  ev(`[...document.querySelectorAll(${JSON.stringify(sel)})].filter((e) => e.offsetParent).filter((e) => {
    const words = e.lastElementChild; const shown = getComputedStyle(words).display !== 'none';
    const r = e.getBoundingClientRect(), p = e.parentElement.getBoundingClientRect();
    return (shown && words.scrollWidth > words.clientWidth + 1) || (!shown && !e.getAttribute('aria-label')) || r.right > p.right + 1 || r.left < p.left - 1; }).map((e) => e.innerText || e.getAttribute('aria-label'))`);
// The badge's words are shown (not only its shield).
const worded = (sel) => ev(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); return !!e && getComputedStyle(e.lastElementChild).display !== 'none'; })()`);

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================ Settings: on by default, explained
  await click("[data-testid=nav-settings]");
  await waitFor(() => has("[data-testid=integrity-settings]"), "the File integrity panel");
  check("Settings: File integrity is on by default", await ev(`document.querySelector('[data-testid=integrity-toggle]').checked`));
  const panel = await text("[data-testid=integrity-settings]");
  check("Settings: it says what it does and what it does not prove", /SHA256SUMS/.test(panel) && /not that the site can be trusted/.test(panel), panel);

  // ============================================================ the Add dialog says it beforehand
  const a = (await invoke("analyze_link", { request: { url: `${base}/ok/ubuntu-desktop.iso` } })).ok;
  check("the check before the download finds the publisher's list", a?.publisherChecksum?.list === "SHA256SUMS" && a.publisherChecksum.algo === "SHA-256", JSON.stringify(a?.publisherChecksum));
  await click("[data-testid=nav-downloads]");
  await sleep(300);
  await page(`(window.dispatchEvent(new Event('velox:open-add')), true)`);
  await waitFor(() => has("[data-testid=add-check]"), "the Add dialog");
  await page(`(() => { const i = document.querySelector('dialog[aria-labelledby=add-title] input.input'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(i, ${JSON.stringify(`${base}/ok/ubuntu-desktop.iso`)}); i.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
  await click("[data-testid=add-check]");
  await waitFor(() => has("[data-testid=intel-integrity] [data-found=true]"), "the Integrity row of the check", 15000);
  const row = await text("[data-testid=intel-integrity]");
  check("the Add dialog: \"Publisher's checksum found (SHA256SUMS, SHA-256): the file will be verified\"", row?.includes("Publisher's checksum found (SHA256SUMS, SHA-256)"), row);
  await page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); return true; })()`);
  await waitFor(async () => !(await has("[data-testid=add-check]")), "the Add dialog to close", 5000).catch(() => null);

  // ============================================================ a matching file: Will be verified, then Verified
  site.delayMs = 25; // long enough to see it while it downloads
  const ok = (await invoke("create_download", { request: { url: `${base}/ok/ubuntu-desktop.iso`, cookies: "session=SECRET" } })).ok;
  await click("[data-testid=nav-downloads]");
  await waitFor(() => ev(`document.querySelector('[data-testid=download-${ok.id}] [data-testid=verify-badge][data-state=ready]') !== null`), "Will be verified on the card", 15000);
  check("while it downloads, the card says \"Will be verified\"", (await text(`[data-testid=download-${ok.id}] [data-testid=verify-badge]`))?.trim() === "Will be verified");
  site.delayMs = 0;
  await until(ok.id, "completed");
  await waitFor(() => ev(`document.querySelector('[data-testid=download-${ok.id}] [data-testid=verify-badge][data-state=verified]') !== null`), "Verified on the card", 10000);
  const badge = `[data-testid=download-${ok.id}] [data-testid=verify-badge]`;
  check("finished: the card says \"Verified\" in green", (await text(badge))?.trim() === "Verified" && (await ev(`document.querySelector(${JSON.stringify(badge)}).className.includes('text-ok')`)));
  check("its tooltip says against what", (await ev(`document.querySelector(${JSON.stringify(badge)}).title`))?.includes("SHA256SUMS, SHA-256"));
  check("the lists were asked with no cookie", site.asked.filter((x) => x.url.startsWith("/ok/")).every((x) => !x.headers.cookie && !x.headers.authorization && !x.headers.referer));
  await click("[data-testid=nav-completed]");
  await waitFor(() => has(`[data-testid=done-${ok.id}] [data-testid=verify-badge][data-state=verified]`), "Verified in Completed");
  check("Completed: \"Verified\"", true);
  await click("[data-testid=nav-history]");
  await waitFor(() => has("[data-testid=history-row] [data-testid=verify-badge][data-by=publisher]"), "Verified in the History");
  check("History: \"Verified\"", true);
  await click("[data-testid=nav-downloads]");
  await click(`[data-testid=download-${ok.id}]`);
  await waitFor(() => has("[data-testid=integrity-card]"), "the Integrity card");
  check("details: the Integrity card says Verified", (await text("[data-testid=integrity-title]")) === "Verified");
  check("details: against the publisher's list, by name", (await text("[data-testid=integrity-text]"))?.includes("Matches the checksum its publisher lists next to it (SHA256SUMS, SHA-256)"));
  check("details: the checksum itself", (await text("[data-testid=integrity-checksum]")) === `sha256:${SHA}`);
  const tl = (await invoke("get_download_diagnostics", { id: ok.id })).ok.events.map((e) => e.text);
  check("the timeline says it was checked and matched", tl.includes("The file matches the publisher's checksum (SHA256SUMS)"), JSON.stringify(tl.slice(-4)));
  await page(`(document.documentElement.setAttribute('data-theme', 'dark'), true)`);
  await sleep(400);
  await h.screenshot(path.join(SHOTS, "integrity-dark.png"));
  await page(`(document.documentElement.removeAttribute('data-theme'), true)`);

  // ============================================================ a damaged file is not kept
  const bad = (await invoke("create_download", { request: { url: `${base}/bad/app-setup.zip` } })).ok;
  await until(bad.id, "failed");
  const b = await get(bad.id);
  check("a file that does not match is not kept", b.errorCode === "publisher_checksum_mismatch", JSON.stringify(b.errorCode));
  await click("[data-testid=nav-downloads]");
  await waitFor(() => has(`[data-testid=download-${bad.id}] [data-testid=card-error]`), "the error on the card");
  check("the card says it arrived damaged", (await text(`[data-testid=download-${bad.id}] [data-testid=card-error]`))?.includes("arrived damaged"));
  await click(`[data-testid=download-${bad.id}]`);
  await waitFor(() => ev(`document.querySelector('[data-testid=integrity-card] [data-state]')?.dataset.state === 'damaged'`), "Damaged on the details page");
  check("details: Damaged", (await text("[data-testid=integrity-title]")) === "Damaged");

  // ============================================================ no list: finished as before
  const none = (await invoke("create_download", { request: { url: `${base}/none/plain.bin` } })).ok;
  await until(none.id, "completed");
  check("no list: finished, no badge", !(await get(none.id)).verify);
  await click("[data-testid=nav-downloads]");
  check("no list: nothing on the card", !(await has(`[data-testid=download-${none.id}] [data-testid=verify-badge]`)));
  await click(`[data-testid=download-${none.id}]`);
  await waitFor(() => has("[data-testid=integrity-card]"), "the Integrity card");
  check("details: Not verified, only the size was checked", (await text("[data-testid=integrity-title]")) === "Not verified" && (await text("[data-testid=integrity-text]"))?.includes("only its size"));

  // ============================================================ turned off in Settings
  await click("[data-testid=nav-settings]");
  await click("[data-testid=integrity-toggle]");
  await waitFor(async () => (await invoke("get_publisher_check")).ok === false, "turned off");
  check("Settings: turned off", true);
  site.asked = [];
  const off = (await invoke("create_download", { request: { url: `${base}/ok/ubuntu-desktop.iso`, filename: "second.iso" } })).ok;
  await until(off.id, "completed");
  check("off: no list is asked and nothing is verified", !(await get(off.id)).verify && site.asked.length === 0, JSON.stringify(site.asked.map((x) => x.url)));
  await click("[data-testid=nav-settings]");
  await click("[data-testid=integrity-toggle]");
  await waitFor(async () => (await invoke("get_publisher_check")).ok === true, "turned on again");
  check("Settings: on again", true);
  check("a bad request is refused", !!(await invoke("set_publisher_check", { enabled: "yes" })).err);

  // ============================================================ every language: translated and fits
  for (const lang of ["fr", "ar", "es", "pt-br", "de", "ru", "tr", "en"]) {
    await pickLang(lang);
    const problems = [];
    for (const [width, height] of [[1360, 860], [900, 640], [640, 480]]) {
      await h.cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
      await click("[data-testid=nav-downloads]");
      await waitFor(() => has(`[data-testid=download-${ok.id}] [data-testid=verify-badge]`), "the badge");
      problems.push(...(await cut(`[data-testid=download-${ok.id}] [data-testid=verify-badge]`)).map((x) => `${width} card: ${x}`));
      await click("[data-testid=nav-completed]");
      await waitFor(() => has(`[data-testid=done-${ok.id}] [data-testid=verify-badge]`), "the badge in Completed");
      problems.push(...(await cut(`[data-testid=done-${ok.id}] [data-testid=verify-badge]`)).map((x) => `${width} completed: ${x}`));
      await click("[data-testid=nav-history]");
      await waitFor(() => has("[data-testid=history-row] [data-testid=verify-badge]"), "the badge in History");
      problems.push(...(await cut("[data-testid=history-row] [data-testid=verify-badge]")).map((x) => `${width} history: ${x}`));
    }
    await h.cdp.send("Emulation.clearDeviceMetricsOverride");
    await click("[data-testid=nav-completed]");
    await waitFor(() => has(`[data-testid=done-${ok.id}] [data-testid=verify-badge]`), "the badge in Completed");
    const inCompleted = await worded(`[data-testid=done-${ok.id}] [data-testid=verify-badge]`);
    await click("[data-testid=nav-history]");
    await waitFor(() => has("[data-testid=history-row] [data-testid=verify-badge]"), "the badge in History");
    const inHistory = await worded("[data-testid=history-row] [data-testid=verify-badge]");
    check(`${lang}: at the usual window size Completed and History show the word, not only the shield`, inCompleted && inHistory, JSON.stringify([inCompleted, inHistory]));
    await click("[data-testid=nav-downloads]");
    const word = (await text(`[data-testid=download-${ok.id}] [data-testid=verify-badge]`))?.trim();
    check(`${lang}: the badges fit on the card, in Completed and in History, at every window size`, problems.length === 0, problems.join(" | "));
    check(`${lang}: "Verified" is translated`, lang === "en" ? word === "Verified" : !!word && word !== "Verified", word);
    await click(`[data-testid=download-${ok.id}]`);
    await waitFor(() => has("[data-testid=integrity-card]"), "the Integrity card");
    const card = await text("[data-testid=integrity-card]");
    check(`${lang}: the Integrity card is translated and names the list`, card.includes("SHA256SUMS") && (lang === "en" || !card.includes("Matches the checksum")), card);
  }
  check("no JavaScript errors", (await ev("window.__errs.length")) === 0, JSON.stringify(await ev("window.__errs")));
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  server.close();
  await h.cleanup();
  finish();
}
