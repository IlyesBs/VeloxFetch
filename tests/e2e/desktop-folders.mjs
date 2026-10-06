// End-to-end test of the download folder features in the real desktop app:
// the default folder (change / reset / restart), sorting by type (on / off / renamed folders), a folder chosen
// for one download in the Add dialog and in the browser confirmation popup, and the security boundaries
// (the web view never sends a path; forbidden folders are refused; made-up tokens are refused).
//
//   node tests/e2e/desktop-folders.mjs [path-to-debug-velox-desktop.exe]
//
// The native folder dialog cannot be driven by a script: debug builds read the answer from the file named by
// VELOX_TEST_PICK_FILE (a folder, or the word `cancel`); release builds ignore that variable and always ask the person.

import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const srv = await startFileServer(300 * 1024);
const answerFile = path.join(os.tmpdir(), `velox-e2e-pick-${process.pid}.txt`);
const answer = (v) => fs.writeFileSync(answerFile, v);
answer("cancel");
const h = createHarness({ port: 9347, appPath: APP, env: { VELOX_TEST_PICK_FILE: answerFile, VELOX_ORGANIZE: "1" } });
const { page, invoke } = h;

const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
// A click waits for the element first: the UI trails the backend by an event.
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); e.click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
const nav = async (n) => (await click(`[data-testid=nav-${n}]`), sleep(250));
const toasts = () => ev(`[...document.querySelectorAll('[role=alert],[role=status]')].map((e) => e.innerText).join(' | ')`);
const clearToasts = () => page(`(() => { document.querySelectorAll('[aria-label=Dismiss]').forEach((b) => b.click()); return true; })()`);
const real = (p) => fs.realpathSync.native(p);
const tree = (dir) => {
  const out = [];
  const walk = (d, pre) => {
    for (const e of fs.existsSync(d) ? fs.readdirSync(d, { withFileTypes: true }) : []) {
      const rel = pre ? `${pre}/${e.name}` : e.name;
      if (e.isDirectory()) walk(path.join(d, e.name), rel);
      else out.push(rel);
    }
  };
  walk(dir, "");
  return out.sort();
};
const settle = async (id) => waitFor(async () => ["completed", "failed"].includes((await invoke("get_download", { id })).ok?.status), `download ${id}`, 30000);
async function get(name) {
  const d = (await invoke("create_download", { request: { url: `${srv.base}/${name}` } })).ok;
  await settle(d.id);
  return (await invoke("get_download", { id: d.id })).ok;
}

const folderA = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-folderA-"));
const folderB = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-folderB-"));
const folderC = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-folderC-"));

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================ sorting by type (on by default in the app)
  await nav("settings");
  await waitFor(async () => (await text("[data-testid=storage-root]"))?.includes("downloads"), "storage panel");
  check("Settings shows the download folder", (await text("[data-testid=storage-root]")) === h.root, await text("[data-testid=storage-root]"));
  check("sorting by type is on by default in the app", (await ev("document.querySelector('[data-testid=organize-toggle]').checked")) === true);
  check("'use the usual folder' is disabled while the usual folder is in use", (await ev("document.querySelector('[data-testid=storage-reset]').disabled")) === true);

  for (const n of ["song.mp3", "paper.pdf", "setup.exe", "clip.mkv", "photo.png", "bundle.zip", "mystery.xyz"]) await get(n);
  const sorted = tree(h.root);
  check(
    "new files go into a folder per type",
    JSON.stringify(sorted) === JSON.stringify(["Archives/bundle.zip", "Audio/song.mp3", "Documents/paper.pdf", "Images/photo.png", "Other/mystery.xyz", "Programs/setup.exe", "Video/clip.mkv"]),
    sorted.join(", "),
  );

  await click("[data-testid=organize-toggle]");
  await waitFor(async () => (await ev("document.querySelector('[data-testid=organize-toggle]').checked")) === false, "toggle off");
  await get("flat.mp3");
  check("with sorting off a file goes straight into the folder", tree(h.root).includes("flat.mp3"));

  await click("[data-testid=organize-toggle]");
  await waitFor(async () => has("[data-testid=organize-names]"), "folder names");
  await fill("[data-testid=folder-name-audio]", "Music");
  await fill("[data-testid=folder-name-program]", "Applications");
  await click("[data-testid=folder-names-save]");
  await waitFor(async () => /Folder names saved/.test((await toasts()) ?? ""), "names saved");
  await get("tune.mp3");
  await get("installer.exe");
  check("renamed folders are used", tree(h.root).includes("Music/tune.mp3") && tree(h.root).includes("Applications/installer.exe"), tree(h.root).join(", "));
  await fill("[data-testid=folder-name-video]", "../escape");
  await click("[data-testid=folder-names-save]");
  await waitFor(async () => /invalid/i.test((await toasts()) ?? ""), "hostile name refused");
  check("a folder name with a path in it is refused, and nothing escapes", !fs.existsSync(path.join(h.tmp, "escape")));
  await fill("[data-testid=folder-name-video]", "Video");

  // ============================================================ the default folder
  answer("cancel");
  await clearToasts();
  await click("[data-testid=storage-change]");
  await sleep(500);
  check("closing the folder dialog changes nothing and shows no error", (await text("[data-testid=storage-root]")) === h.root && !/not allowed|error/i.test((await toasts()) ?? ""));

  const forbidden = process.platform === "win32" ? "C:\\Windows" : "/etc";
  answer(forbidden);
  await clearToasts();
  await click("[data-testid=storage-change]");
  await waitFor(async () => /not allowed/.test((await toasts()) ?? ""), "system folder refused");
  check("a system folder is refused with a reason", (await text("[data-testid=storage-root]")) === h.root, await toasts());
  answer(process.platform === "win32" ? "C:\\" : "/");
  await click("[data-testid=storage-change]");
  await sleep(500);
  check("the top of a drive is refused too", (await text("[data-testid=storage-root]")) === h.root);

  answer(folderA);
  await click("[data-testid=storage-change]");
  await waitFor(async () => (await text("[data-testid=storage-root]")) === real(folderA), "new folder shown");
  check("a folder chosen in the dialog becomes the default", true);
  await get("after.mp3");
  check("new downloads use the new folder (sorted there)", tree(folderA).join() === "Music/after.mp3", tree(folderA).join());
  check("nothing already downloaded was moved", tree(h.root).includes("Music/tune.mp3"));
  check("'use the usual folder' is available now", (await ev("document.querySelector('[data-testid=storage-reset]').disabled")) === false);

  // Survives a restart.
  await h.cdp.send("Browser.close").catch(() => {});
  h.app.kill();
  await sleep(1200);
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI after restart");
  await nav("settings");
  await waitFor(async () => (await text("[data-testid=storage-root]")) === real(folderA), "folder remembered");
  check("the chosen folder and the folder names survive a restart", (await ev("document.querySelector('[data-testid=folder-name-audio]').value")) === "Music");

  await click("[data-testid=storage-reset]");
  await waitFor(async () => (await text("[data-testid=storage-root]")) === h.root, "back to the usual folder");
  check("going back to the usual folder works", true);

  // ============================================================ a folder for one download: Add dialog
  await nav("downloads");
  await waitFor(() => ev(`[...document.querySelectorAll('[data-testid=add-form] button')].some((b) => b.innerText.trim() === 'Options')`), "Options button");
  await page(`[...document.querySelectorAll('[data-testid=add-form] button')].find((b) => b.innerText.trim() === 'Options').click()`);
  await waitFor(async () => has("[data-testid=add-change-folder]"), "add dialog");
  check("the Add dialog offers 'Change…' and shows the usual folder", /usual download folder/.test((await text("[data-testid=add-folder-path]")) ?? ""));
  answer(folderB);
  await click("[data-testid=add-change-folder]");
  await waitFor(async () => (await text("[data-testid=add-folder-path]")) === real(folderB), "chosen folder shown");
  check("the chosen folder is shown and the sub-folder box is disabled", (await ev("document.querySelector('dialog input[placeholder*=\"not with\"]')?.disabled")) === true);
  await fill("dialog input[required]", `${srv.base}/special.mp3`);
  await click("dialog button[type=submit]");
  await waitFor(async () => tree(folderB).length === 1, "download into folder B", 20000);
  check("the file is saved exactly in the chosen folder, not sorted", tree(folderB).join() === "special.mp3", tree(folderB).join());
  check("the default folder is unchanged", (await invoke("get_storage")).ok.root === h.root);

  // ============================================================ a folder for one download: browser confirmation popup
  const before = (await invoke("list_downloads", { filter: {} })).ok.length;
  const link = await invoke("review_link", { url: `${srv.base}/movie.mkv` });
  check("a link can be put in front of the user", !!link.ok, JSON.stringify(link));
  await waitFor(async () => has("[data-testid=pending-dialog]"), "confirmation popup");
  await waitFor(async () => (await text("[data-testid=pending-type]")) !== "Checking…", "probe finished");
  check("the popup shows where the file will go (sorted folder)", (await text("[data-testid=pending-dir-path]")) === path.join(h.root, "Video"), await text("[data-testid=pending-dir-path]"));
  answer("cancel");
  await click("[data-testid=pending-change-folder]");
  await sleep(500);
  check("cancelling the dialog keeps the usual folder", (await text("[data-testid=pending-dir-path]")) === path.join(h.root, "Video"));
  answer(folderC);
  await click("[data-testid=pending-change-folder]");
  await waitFor(async () => (await text("[data-testid=pending-dir-path]")) === real(folderC), "chosen folder in the popup");
  check("'Usual' goes back", (await has("[data-testid=pending-default-folder]")) === true);
  await click("[data-testid=pending-start]");
  await waitFor(async () => tree(folderC).join() === "movie.mkv", "download into folder C", 20000).catch(() => {});
  check("the confirmed download lands in the chosen folder", tree(folderC).join() === "movie.mkv", tree(folderC).join());
  check("exactly one download was created", (await invoke("list_downloads", { filter: {} })).ok.length === before + 1);

  // ============================================================ the web view cannot choose a folder by itself
  const bad = async (cmd, args, why) => {
    const r = await invoke(cmd, args);
    check(`${cmd}: ${why}`, !!r.err, JSON.stringify(r));
  };
  await bad("create_download", { request: { url: `${srv.base}/x.bin`, folderToken: 999999 } }, "a made-up folder token is refused");
  await bad("create_download", { request: { url: `${srv.base}/x.bin`, folder: "C:\\Windows" } }, "a path field is refused (unknown field)");
  await bad("create_download", { request: { url: `${srv.base}/x.bin`, folderToken: 1, subdir: "x" } }, "a folder token together with a sub-folder is refused");
  await bad("confirm_pending_download", { request: { token: 424242, folderToken: 1 } }, "an unknown pending download is refused");
  await bad("set_folder_names", { request: { names: { audio: "../../x" } } }, "a traversal folder name is refused");
  await bad("set_folder_names", { request: { names: { audio: "a", document: "A" } } }, "two kinds cannot share a folder");
  await bad("set_folder_names", { request: { names: { nonsense: "x" } } }, "an unknown kind is refused");
  await bad("set_folder_names", { request: { names: {}, path: "C:\\" } }, "unknown fields are refused");
  await bad("set_organize", { enabled: "yes" }, "a non-boolean is refused");
  answer("cancel");
  const r = await invoke("choose_download_folder", { path: "C:\\Windows", folder: "C:\\Windows" });
  check("choose_download_folder ignores any path the web view supplies", r.ok === null && (await invoke("get_storage")).ok.root === h.root, JSON.stringify(r));
  const r2 = await invoke("pick_folder", { path: "C:\\Windows" });
  check("pick_folder ignores any path the web view supplies", r2.ok === null, JSON.stringify(r2));
  check("the storage command works and describes the state", (await invoke("get_storage")).ok.folders.length === 7);

  const errs = (await ev("window.__errs")) ?? [];
  check("no script errors", errs.length === 0, errs.join("; "));
  const csp = h.events.filter((m) => (m.method === "Log.entryAdded" && /Content Security Policy|Refused to/.test(m.params.entry.text)) || m.method === "Runtime.exceptionThrown");
  check("no CSP violations or script exceptions", csp.length === 0, JSON.stringify(csp.slice(0, 2)));
} catch (e) {
  check("run completed", false, e.stack ?? String(e));
} finally {
  srv.close();
  await h.cleanup();
  for (const d of [folderA, folderB, folderC]) fs.rmSync(d, { recursive: true, force: true });
  fs.rmSync(answerFile, { force: true });
}
finish();
