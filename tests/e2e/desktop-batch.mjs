// End-to-end test of Phase 8 in the real desktop app: batch downloads (a list of links, a pattern,
// an imported .txt file) and clipboard detection.
//
//   node tests/e2e/desktop-batch.mjs [path-to-debug-velox-desktop.exe]
//
// Safety: the clipboard is never touched. Debug builds can read "the clipboard" from a file
// (VELOX_CLIPBOARD_FILE), which is what this test writes to; release builds ignore that variable.

import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

const srv = await startFileServer(512 * 1024);
const clipFile = path.join(os.tmpdir(), `velox-e2e-clipboard-${process.pid}.txt`);
fs.writeFileSync(clipFile, "");
const h = createHarness({ port: 9345, appPath: APP, env: { VELOX_CLIPBOARD_FILE: clipFile } });
const { page, invoke } = h;

const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = (sel) => page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
const setChecked = (sel, want) => page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (e.checked !== ${want}) e.click(); return e.checked; })()`);
const nav = async (n) => (await click(`[data-testid=nav-${n}]`), sleep(200));
const downloads = async () => (await invoke("list_downloads", { filter: {} })).ok;
const previewText = () => text("[data-testid=batch-summary]");
const clip = (s) => fs.writeFileSync(clipFile, s);
const okCount = async () => Number(await text("[data-testid=batch-ok]"));

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ===================================================================================== the dialog
  await click("[data-testid=batch-open]");
  await waitFor(async () => await has("[data-testid=batch-dialog]"), "batch dialog");
  check("the dialog opens from the Downloads page", true);
  check("Add is disabled while there is nothing to add", (await ev("document.querySelector('[data-testid=batch-add]').disabled")) === true);

  const base = srv.base;
  const list = [
    "# my links",
    `${base}/a.bin`,
    `${base}/b.bin`,
    `${base}/b.bin#again`,
    "",
    "javascript:alert(1)",
    "not a link",
    `${base}/c.bin`,
  ].join("\n");
  await fill("[data-testid=batch-links]", list);
  await waitFor(async () => (await previewText())?.includes("will be added"), "preview of the list");
  const summary = await previewText();
  check("the preview counts what will be added, repeated and not valid", (await okCount()) === 3 && /1 repeated/.test(summary) && /2 not valid/.test(summary), summary);
  check("problems are listed with the reason", /Repeated in this list/.test((await text("[data-testid=batch-preview]")) ?? "") && /(unsupported|bad URL|relative)/i.test((await text("[data-testid=batch-preview]")) ?? ""));
  check("nothing has been added yet", (await downloads()).length === 0);
  await click("[data-testid=batch-tab-pattern]");
  await click("[data-testid=batch-tab-links]");
  await h.page("document.documentElement.setAttribute('data-theme','dark')");
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "batch-links-dark.png"), { width: 1000, height: 800 });
  check("the button says how many it will add", /Add 3 downloads/.test((await text("[data-testid=batch-add]")) ?? ""));
  await click("[data-testid=batch-add]");
  await waitFor(async () => !(await has("[data-testid=batch-dialog]")), "dialog to close");
  await waitFor(async () => (await downloads()).length === 3, "3 downloads");
  await waitFor(async () => (await downloads()).every((d) => d.status === "completed"), "all completed", 30000);
  check("the 3 good links were downloaded (list, comments, repeats and junk skipped)", (await downloads()).map((d) => d.filename).sort().join() === "a.bin,b.bin,c.bin");

  // Links already in the list are recognised the next time.
  await click("[data-testid=batch-open]");
  await fill("[data-testid=batch-links]", `${base}/a.bin\n${base}/d.bin`);
  await waitFor(async () => (await previewText())?.includes("will be added"), "second preview");
  check("a link already in the downloads is flagged and not added again", (await okCount()) === 1 && /1 already in your downloads/.test((await previewText()) ?? ""));
  await click("[data-testid=batch-cancel]");

  // ============================================================================== import a .txt file
  await click("[data-testid=batch-open]");
  await ev(`(() => { const input = document.querySelector('[data-testid=batch-file]'); const dt = new DataTransfer();
    dt.items.add(new File([${JSON.stringify(`${base}/e.bin\n${base}/f.bin\n`)}], 'links.txt', { type: 'text/plain' })); input.files = dt.files;
    input.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await waitFor(async () => (await ev("document.querySelector('[data-testid=batch-links]').value"))?.includes("e.bin"), "file contents in the box");
  await waitFor(async () => (await okCount()) === 2, "preview of the imported file");
  check("a .txt file can be imported and previewed", true);
  await ev(`(() => { const input = document.querySelector('[data-testid=batch-file]'); const dt = new DataTransfer();
    dt.items.add(new File(['x'.repeat(600 * 1024)], 'big.txt', { type: 'text/plain' })); input.files = dt.files;
    input.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await waitFor(async () => (await has("[data-testid=batch-error]")), "size error");
  check("a file over the size limit is refused", /too large/.test((await text("[data-testid=batch-error]")) ?? ""));
  await click("[data-testid=batch-cancel]");

  // ===================================================================================== the pattern
  await click("[data-testid=batch-open]");
  await click("[data-testid=batch-tab-pattern]");
  await fill("[data-testid=batch-pattern]", `${base}/file_[001-012].bin`);
  await fill("[data-testid=batch-subdir]", "series/one");
  await waitFor(async () => (await previewText())?.includes("will be added"), "pattern preview");
  check("the pattern [001-012] generates 12 links, zero-padded", (await okCount()) === 12 && /file_001\.bin/.test((await text("[data-testid=batch-preview]")) ?? "") && /file_012\.bin/.test((await text("[data-testid=batch-preview]")) ?? ""));
  await h.screenshot(path.join(SHOTS, "batch-pattern-dark.png"), { width: 1000, height: 800 });

  // Limits, refused by the backend and shown in the dialog.
  for (const [pat, what] of [
    [`${base}/f[1-1001].bin`, "more than 1000 links"],
    [`${base}/f[1-100][1-100][1-100].bin`, "a product of ranges over the limit"],
    [`${base}/f[1-999999999].bin`, "a huge range"],
    [`${base}/f[9-1].bin`, "a backwards range"],
    [`${base}/nothing.bin`, "no range"],
  ]) {
    await fill("[data-testid=batch-pattern]", pat);
    await waitFor(async () => (await has("[data-testid=batch-error]")), `error for ${what}`);
    check(`refused: ${what}`, true, (await text("[data-testid=batch-error]")) ?? "");
    check(`and Add is disabled (${what})`, (await ev("document.querySelector('[data-testid=batch-add]').disabled")) === true);
  }
  await fill("[data-testid=batch-pattern]", `${base}/file_[001-012].bin`);
  await waitFor(async () => (await previewText())?.includes("will be added"), "pattern preview again");
  await fill("[data-testid=batch-subdir]", "..\\..\\evil");
  await waitFor(async () => (await has("[data-testid=batch-error]")), "folder error");
  check("a sub-folder that leaves the download folder is refused", /sub-folder|folder|invalid/i.test((await text("[data-testid=batch-error]")) ?? ""));
  await fill("[data-testid=batch-subdir]", "series/one");
  await waitFor(async () => (await has("[data-testid=batch-summary]")), "preview back");
  await click("[data-testid=batch-add]");
  await waitFor(async () => (await downloads()).filter((d) => d.filename.startsWith("file_")).length === 12, "12 pattern downloads", 30000);
  await waitFor(async () => (await downloads()).every((d) => d.status === "completed"), "everything completed", 40000);
  const patternDl = (await downloads()).filter((d) => d.filename.startsWith("file_"));
  check("all 12 files were downloaded into the chosen sub-folder", patternDl.every((d) => d.destination.replace(/\\/g, "/").includes("/series/one/") && fs.existsSync(d.destination)));

  // Wire-level checks: the IPC boundary refuses what the dialog would never send.
  const extra = await invoke("preview_batch", { request: { links: "https://a.test/x.zip", path: "C:\\evil" } });
  check("an extra field in a batch request is refused at the IPC boundary", !!extra.err);
  const both = await invoke("add_batch", { request: { links: `${base}/z.bin`, pattern: `${base}/[1-2]` } });
  check("both a list and a pattern at once is refused", !!both.err);
  const nothing = await invoke("add_batch", { request: {} });
  check("an empty request is refused", !!nothing.err);
  const before = (await downloads()).length;
  const evil = await invoke("add_batch", { request: { links: `${base}/y.bin`, subdir: "../../outside" } });
  check("a bad folder adds nothing", !!evil.err && (await downloads()).length === before);

  // ============================================================================ clipboard detection
  await nav("settings");
  await waitFor(async () => await has("[data-testid=clipboard-toggle]"), "clipboard settings");
  check("clipboard detection is OFF by default", (await ev("document.querySelector('[data-testid=clipboard-toggle]').checked")) === false);
  const off = await invoke("get_clipboard_watch");
  check("the backend agrees", off.ok?.enabled === false && off.ok?.available === true);
  await nav("downloads");

  clip(`${base}/while-off.zip`);
  await sleep(2500);
  check("with detection off, a copied link is not looked at", !(await has("[data-testid=clipboard-banner]")));

  // What is already on the clipboard when detection is turned on is the baseline: never reported.
  clip(`${base}/baseline.zip`);
  await nav("settings");
  await setChecked("[data-testid=clipboard-toggle]", true);
  await waitFor(async () => (await invoke("get_clipboard_watch")).ok?.enabled === true, "watch enabled");
  await nav("downloads");
  await sleep(2500);
  check("the link that was already copied when it was turned on is not reported", !(await has("[data-testid=clipboard-banner]")));

  clip(`${base}/new-link.zip`);
  await waitFor(async () => await has("[data-testid=clipboard-banner]"), "clipboard banner", 8000);
  check("a newly copied download link shows a notice with its name and host", /new-link\.zip/.test((await text("[data-testid=clipboard-text]")) ?? "") && /127\.0\.0\.1/.test((await text("[data-testid=clipboard-text]")) ?? ""));
  await h.screenshot(path.join(SHOTS, "clipboard-banner-dark.png"), { width: 1000, height: 360 });
  check("nothing was downloaded by the notice itself", !(await downloads()).some((d) => d.filename === "new-link.zip"));
  await click("[data-testid=clipboard-review]");
  await waitFor(async () => await has("[data-testid=pending-dialog]"), "confirmation dialog");
  check("Download… opens the usual confirmation with the file's details", (await ev("document.querySelector('[data-testid=pending-name]').value")) === "new-link.zip");
  check("and the notice went away", !(await has("[data-testid=clipboard-banner]")));
  await click("[data-testid=pending-cancel]");
  await waitFor(async () => !(await has("[data-testid=pending-dialog]")), "dialog closed");
  check("Cancel downloads nothing", !(await downloads()).some((d) => d.filename === "new-link.zip"));

  for (const [what, value] of [
    ["ordinary text", "please download the file I sent you"],
    ["a password", "Tr0ub4dor&3-correct-horse"],
    ["a web page link", "https://example.com/some/page.html"],
    ["a link with a password in it", "https://user:hunter2@example.com/secret.zip"],
    ["a link inside a sentence", `get it at ${base}/x.zip now`],
    ["a link and other text", `${base}/y.zip\nand some words`],
  ]) {
    clip(value);
    await sleep(2200);
    check(`${what} is ignored`, !(await has("[data-testid=clipboard-banner]")));
  }

  clip(`${base}/one.zip\n${base}/two.mp4\n${base}/three.pdf\n`);
  await waitFor(async () => await has("[data-testid=clipboard-banner]"), "banner for several links", 8000);
  check("several copied links give one notice", /3 download links/.test((await text("[data-testid=clipboard-text]")) ?? ""));
  await click("[data-testid=clipboard-review]");
  await waitFor(async () => await has("[data-testid=batch-dialog]"), "batch dialog from the banner");
  await waitFor(async () => (await okCount()) === 3, "the 3 copied links previewed");
  check("Review… opens the batch dialog with those links", true);
  await click("[data-testid=batch-add]");
  await waitFor(async () => (await downloads()).some((d) => d.filename === "three.pdf"), "the copied links added");
  check("and adding works", true);

  clip(`${base}/one.zip`);
  await sleep(2500);
  check("a link that is already in the list is not offered again", !(await has("[data-testid=clipboard-banner]")));

  clip(`${base}/dismiss-me.zip`);
  await waitFor(async () => await has("[data-testid=clipboard-banner]"), "banner to dismiss", 8000);
  await click("[data-testid=clipboard-dismiss]");
  check("Dismiss closes the notice", !(await has("[data-testid=clipboard-banner]")));

  // Turning it off stops it, and the choice is remembered across a restart.
  await nav("settings");
  await setChecked("[data-testid=clipboard-toggle]", false);
  await waitFor(async () => (await invoke("get_clipboard_watch")).ok?.enabled === false, "watch disabled");
  clip(`${base}/after-off.zip`);
  await sleep(2500);
  check("after turning it off nothing is reported", !(await has("[data-testid=clipboard-banner]")));
  await setChecked("[data-testid=clipboard-toggle]", true);
  await waitFor(async () => (await invoke("get_clipboard_watch")).ok?.enabled === true, "enabled again");

  const errs = (await ev("window.__errs")) ?? [];
  check("no script errors", errs.length === 0, JSON.stringify(errs));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  try {
    fs.rmSync(clipFile, { force: true });
  } catch {}
  srv.close();
  await h.cleanup();
}

finish();
