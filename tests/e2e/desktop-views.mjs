// End-to-end test of the three download pages in the real desktop app: Downloads (the dashboard), Active
// (work in progress, live numbers) and Completed (the finished files), each its own view; and deleting a
// finished file from the disk (asked first, from the Completed page and from a card's menu) next to removing
// it from the list only. Checks every page at several window sizes for sideways scrolling.
//
//   node tests/e2e/desktop-views.mjs [path-to-debug-velox-desktop.exe]

import { check, createHarness, exe, finish, fs, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const srv = await startFileServer(400 * 1024);
const h = createHarness({ port: 9367, appPath: APP });
const { page, invoke } = h;

const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const count = (sel) => ev(`document.querySelectorAll(${JSON.stringify(sel)}).length`);
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)});
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
const nav = async (n) => (await click(`[data-testid=nav-${n}]`), sleep(250));
const status = async (id) => (await invoke("get_download", { id })).ok?.status;
const finished = (id) => waitFor(async () => (await status(id)) === "completed", `download ${id} to finish`, 30000);
async function get(name) {
  const d = (await invoke("create_download", { request: { url: `${srv.base}/${name}` } })).ok;
  await finished(d.id);
  return (await invoke("get_download", { id: d.id })).ok;
}
const onDisk = (d) => fs.existsSync(path.join(h.root, d.filename));

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  const a = await get("alpha.pdf");
  const b = await get("bravo.zip");
  const c = await get("charlie.mp3");
  check("three finished files are on disk", onDisk(a) && onDisk(b) && onDisk(c));

  // ============================================================ each sidebar row has its own view
  await nav("downloads");
  await waitFor(() => has("[data-testid=list-toolbar]"), "the dashboard list");
  check("Downloads is the dashboard (stat cards, tabbed list, side column)", (await has("[data-testid=bento]")) && (await has("[data-testid=list-toolbar]")) && (await has("[data-testid=dash-side]")), JSON.stringify([await has("[data-testid=bento]"), await has("[data-testid=list-toolbar]"), await has("[data-testid=dash-side]")]));
  // the side panels' live network and disk numbers, measured by the system (also with nothing downloading)
  const speedText = /^\d+(\.\d+)? (B|KB|MB|GB)\/s$/;
  await waitFor(async () => speedText.test((await text("[data-testid=net-up]"))?.trim() ?? ""), "a measured upload speed", 8000).catch(() => {});
  const io = { down: (await text("[data-testid=net-down]"))?.trim(), up: (await text("[data-testid=net-up]"))?.trim(), w: (await text("[data-testid=disk-write]"))?.trim(), r: (await text("[data-testid=disk-read]"))?.trim() };
  check("Network shows the measured download and upload speed, idle too", speedText.test(io.down) && speedText.test(io.up), JSON.stringify(io));
  check("Disk shows the measured write and read speed", /Write \d.*\/s/.test(io.w) && /Read \d.*\/s/.test(io.r), JSON.stringify(io));
  const m = (await invoke("get_metrics")).ok;
  check("the numbers come from the engine's metrics (whole computer, download disk)", [m.netDownBps, m.netUpBps, m.diskReadBps, m.diskWriteBps].every((v) => typeof v === "number" && v >= 0) && m.ioSample > 0, JSON.stringify(m));
  const paths = () => ev(`[...document.querySelectorAll('[data-testid=panel-network] svg path, [data-testid=panel-disk] svg path')].length`);
  await waitFor(async () => (await paths()) >= 6, "the charts to get a few readings", 8000).catch(() => {});
  const charts = await paths();
  check("both panels draw two lines (download / upload, write / read)", charts >= 6, String(charts));

  await nav("active");
  check("Active is its own view, not the dashboard", (await has("[data-testid=active-view]")) && !(await has("[data-testid=bento]")) && !(await has("[data-testid=completed-view]")));
  check("Active is marked as the current page", (await ev(`document.querySelector('[data-testid=nav-active]').getAttribute('aria-current')`)) === "page");
  check("Active shows its live numbers (speed, connections, time left, still to download)", (await has("[data-testid=active-speed]")) && (await has("[data-testid=active-conns]")) && (await has("[data-testid=active-eta]")) && (await has("[data-testid=active-remaining]")));
  check("with nothing running, Active says so (and finished files are not listed there)", /Nothing is downloading/.test(await text("[data-testid=active-view]")) && !(await has(`[data-testid=download-${a.id}]`)));
  await nav("completed");
  check("Completed is its own view, not the dashboard", (await has("[data-testid=completed-view]")) && !(await has("[data-testid=bento]")) && !(await has("[data-testid=active-view]")));
  check("Completed is marked as the current page", (await ev(`document.querySelector('[data-testid=nav-completed]').getAttribute('aria-current')`)) === "page");
  check("Completed lists the three finished files", (await count("[data-testid=done-list] > li")) === 3, String(await count("[data-testid=done-list] > li")));
  check("Completed sums them up (3 files, total size)", /3 files · 1\.2 MB/.test(await text("[data-testid=done-summary]")), await text("[data-testid=done-summary]"));
  check("each finished file offers Open, Show in folder, Remove from list and Delete file", (await has(`[data-testid=done-${a.id}] [data-testid=done-open]`)) && (await has(`[data-testid=done-${a.id}] [data-testid=done-folder]`)) && (await has(`[data-testid=done-${a.id}] [data-testid=done-remove]`)) && (await has(`[data-testid=done-${a.id}] [data-testid=done-delete]`)));

  // search and kinds
  await fill("[data-testid=done-search]", "bravo");
  await sleep(200);
  check("search narrows the finished files", (await count("[data-testid=done-list] > li")) === 1 && (await has(`[data-testid=done-${b.id}]`)));
  await fill("[data-testid=done-search]", "");
  await click("[data-testid=kind-audio]");
  await sleep(200);
  check("the kind tabs narrow to one kind of file", (await count("[data-testid=done-list] > li")) === 1 && (await has(`[data-testid=done-${c.id}]`)));
  await click("[data-testid=kind-all]");
  await sleep(200);

  // ============================================================ delete from disk (asked first) vs remove from list
  await click(`[data-testid=done-${a.id}] [data-testid=done-delete]`);
  await waitFor(() => has("[data-testid=delete-dialog]"), "the delete question");
  check("deleting asks first, naming the file", /alpha\.pdf/.test(await text("[data-testid=delete-dialog]")));
  check("the question starts on Cancel (the safe answer)", await ev(`document.activeElement?.dataset.testid === 'confirm-cancel'`));
  await click("[data-testid=confirm-cancel]");
  await sleep(300);
  check("Cancel deletes nothing", onDisk(a) && (await has(`[data-testid=done-${a.id}]`)) && !(await has("[data-testid=delete-dialog]")));

  await click(`[data-testid=done-${a.id}] [data-testid=done-delete]`);
  await click("[data-testid=confirm-delete]");
  await waitFor(async () => !(await has(`[data-testid=done-${a.id}]`)), "the deleted file to leave the list");
  await waitFor(async () => !onDisk(a), "the file to be deleted from the disk", 5000).catch(() => {});
  check("Delete from disk removes the file from the disk and the list", !onDisk(a) && (await status(a.id)) === undefined);

  await click(`[data-testid=done-${b.id}] [data-testid=done-remove]`);
  await waitFor(async () => !(await has(`[data-testid=done-${b.id}]`)), "the removed file to leave the list");
  check("Remove from list keeps the file on disk", onDisk(b));

  await click(`[data-testid=done-${c.id}] [data-testid=done-delete]`);
  await click("[data-testid=confirm-remove-only]");
  await waitFor(async () => !(await has(`[data-testid=done-${c.id}]`)), "the file to leave the list");
  check("the question's 'Remove from list only' keeps the file on disk", onDisk(c));

  // ============================================================ Download again (a file deleted by mistake)
  const e = await get("echo.pdf");
  fs.rmSync(path.join(h.root, e.filename)); // deleted outside the app
  await page(`(() => { window.dispatchEvent(new Event('focus')); return true; })()`); // back to the app: the list resyncs
  await waitFor(() => has(`[data-testid=done-${e.id}] [data-testid=done-file-missing]`), "the missing file to be shown", 8000);
  check("a finished file deleted outside the app says it is no longer on disk", /no longer on disk/.test(await text(`[data-testid=done-${e.id}]`)));
  check("Download again is its highlighted button, and Open is gone", (await has(`[data-testid=done-${e.id}] [data-testid=done-redownload].round-primary`)) && !(await has(`[data-testid=done-${e.id}] [data-testid=done-open]`)));
  await nav("downloads");
  await waitFor(() => has(`[data-testid=download-${e.id}]`), "the card");
  check("on the dashboard card too: the note, and Download again as the main button", (await has(`[data-testid=download-${e.id}] [data-testid=card-file-missing]`)) && (await has(`[data-testid=download-${e.id}] [data-testid=card-redownload]`)) && !(await has(`[data-testid=download-${e.id}] [data-testid=open-file]`)));
  await click(`[data-testid=download-${e.id}] [data-testid=card-redownload]`);
  await finished(e.id).catch(() => {});
  await waitFor(async () => onDisk(e), "the file to be back", 15000).catch(() => {});
  check("Download again brings the file back, same name and folder, on the same entry", onDisk(e) && (await status(e.id)) === "completed" && (await invoke("get_download", { id: e.id })).ok?.fileMissing === false);
  await waitFor(async () => !(await has(`[data-testid=download-${e.id}] [data-testid=card-file-missing]`)), "the note to go", 5000).catch(() => {});
  check("the note is gone once the file is back", !(await has(`[data-testid=download-${e.id}] [data-testid=card-file-missing]`)));
  const twice = await invoke("redownload", { id: e.id });
  await finished(e.id).catch(() => {});
  const copy = (await invoke("get_download", { id: e.id })).ok;
  check("with the file still there, the new copy takes a free name (the file is never overwritten)", twice.ok && /echo \(1\)\.pdf$/.test(copy?.destination ?? "") && onDisk(e), JSON.stringify(copy?.destination));
  check("an unknown id is refused", !!(await invoke("redownload", { id: 987654 })).err);
  await invoke("delete_download", { id: e.id, deleteFile: false });
  await nav("completed");

  // ...and from a card's menu on the dashboard
  const d = await get("delta.pdf");
  await nav("downloads");
  await waitFor(() => has(`[data-testid=download-${d.id}]`), "the card");
  await page(`(() => { document.querySelector('[data-testid=download-${d.id}]').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 300, clientY: 300 })); return true; })()`);
  await waitFor(() => has("[role=menu]"), "menu");
  const items = await ev(`[...document.querySelectorAll('[role=menu] [role=menuitem]')].map((b) => b.innerText.trim())`);
  check("a finished card's menu offers Remove from list and Remove and delete file", items.includes("Remove from list") && items.includes("Remove and delete file"), items.join("|"));
  await page(`(() => { [...document.querySelectorAll('[role=menu] [role=menuitem]')].find((b) => b.innerText.trim() === 'Remove and delete file').click(); return true; })()`);
  await waitFor(() => has("[data-testid=delete-dialog]"), "the delete question from the menu");
  check("the card's delete asks first too", onDisk(d));
  await click("[data-testid=confirm-delete]");
  await waitFor(async () => !onDisk(d), "the card's file to be deleted", 5000).catch(() => {});
  check("the card's Remove and delete file deletes it from the disk", !onDisk(d));

  // ============================================================ Use a new link (an expired link, a mirror)
  srv.delayMs = 400; // ~2.5 s for the whole file: time to pause it part-way
  const nl = (await invoke("create_download", { request: { url: `${srv.base}/foxtrot.bin?sig=old` } })).ok;
  await waitFor(async () => (await invoke("get_download", { id: nl.id })).ok?.downloaded >= 65536, "some bytes saved", 15000);
  await invoke("pause_download", { id: nl.id });
  await waitFor(async () => (await status(nl.id)) === "paused", "the download to pause");
  srv.delayMs = 0;
  const kept = (await invoke("get_download", { id: nl.id })).ok.downloaded;
  await nav("downloads");
  await waitFor(() => has(`[data-testid=download-${nl.id}]`), "the paused card");
  const openMenu = () => page(`(() => { document.querySelector('[data-testid=download-${nl.id}]').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 300, clientY: 300 })); return true; })()`);
  await openMenu();
  await waitFor(() => has("[role=menu]"), "menu");
  const nlItems = await ev(`[...document.querySelectorAll('[role=menu] [role=menuitem]')].map((b) => b.innerText.trim())`);
  check("a paused download's menu offers Use a new link", nlItems.includes("Use a new link…"), nlItems.join("|"));
  await page(`(() => { [...document.querySelectorAll('[role=menu] [role=menuitem]')].find((b) => b.innerText.trim() === 'Use a new link…').click(); return true; })()`);
  await waitFor(() => has("[data-testid=newlink-dialog]"), "the new link dialog");
  await fill("[data-testid=newlink-url]", `${srv.base}/foxtrot.bin?sig=old`);
  await click("[data-testid=newlink-check]");
  await waitFor(() => has("[data-testid=newlink-result]"), "the check's result", 10000);
  check("the link it already has is not taken for a new one, and nothing changes", (await ev(`document.querySelector('[data-testid=newlink-result]').dataset.reason`)) === "sameSource" && (await status(nl.id)) === "paused");
  await fill("[data-testid=newlink-url]", `${srv.base}/mirror/foxtrot.bin?sig=fresh`);
  await click("[data-testid=newlink-check]");
  await waitFor(async () => (await ev(`document.querySelector('[data-testid=newlink-result]')?.dataset.applied`)) === "true", "the new link to be used", 10000);
  check("a fresh link to the same file is proven and used", (await ev(`document.querySelector('[data-testid=newlink-result]').dataset.reason`)) === "etagMatch", await text("[data-testid=newlink-result]"));
  await finished(nl.id);
  const nlDone = (await invoke("get_download", { id: nl.id })).ok;
  check("it completes from the saved progress, byte for byte", fs.readFileSync(path.join(h.root, nlDone.filename)).equals(srv.FILE) && kept > 0 && nlDone.url.includes("sig=fresh"));
  await waitFor(async () => !(await has("[data-testid=newlink-dialog]")), "the dialog to close", 5000).catch(() => {});
  check("no error was thrown by the page", (await ev("window.__errs.length")) === 0, JSON.stringify(await ev("window.__errs")));
  check("an unknown download is refused", !!(await invoke("use_new_link", { request: { id: 987654, url: `${srv.base}/x.bin` } })).err);
  await invoke("delete_download", { id: nl.id, deleteFile: true });

  // ============================================================ Active: live work, pause / resume all
  srv.delayMs = 1200; // ~8 s for the whole file: long enough to look at it running
  const run = (await invoke("create_download", { request: { url: `${srv.base}/echo.iso` } })).ok;
  await nav("active");
  await waitFor(() => has(`[data-testid=active-list] [data-testid=download-${run.id}]`), "the running download on Active", 15000);
  check("Active lists the running download as a full card (with its connection panel)", await has(`[data-testid=active-list] [data-testid=download-${run.id}] .task-threads`));
  await waitFor(async () => (await text("[data-testid=active-conns]"))?.match(/\d+/)?.[0] !== "0", "connections in use", 10000).catch(() => {});
  check("Active counts the connections in use", Number((await text("[data-testid=active-conns]")).match(/\d+/)?.[0]) >= 1, await text("[data-testid=active-conns]"));
  check("Active shows what is still to download", /KB|MB/.test(await text("[data-testid=active-remaining]")), await text("[data-testid=active-remaining]"));
  await click("[data-testid=active-pause-all]");
  await waitFor(() => has(`[data-testid=active-paused] [data-testid=download-${run.id}]`), "the paused download under Paused");
  check("Pause all moves it under Paused, still on Active", (await status(run.id)) === "paused");
  await click("[data-testid=active-resume-all]");
  await waitFor(async () => ["downloading", "queued", "completed"].includes(await status(run.id)), "resume");
  check("Resume all starts it again", true);
  srv.delayMs = 0;
  await finished(run.id);
  await waitFor(async () => !(await has(`[data-testid=active-list] [data-testid=download-${run.id}]`)), "the finished download to leave Active");
  check("a finished download leaves Active by itself", true);
  await nav("completed");
  check("...and shows on Completed", await has(`[data-testid=done-${run.id}]`));

  // ============================================================ every page fits every window size
  for (const [width, height] of [[1536, 1024], [1024, 700], [800, 600], [640, 480]]) {
    await h.cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
    for (const p of ["active", "completed"]) {
      await nav(p);
      const over = await ev(`(() => { const m = document.querySelector('[data-testid=main]'); return m.scrollWidth - m.clientWidth; })()`);
      check(`${p} at ${width}x${height}: no sideways scrolling`, over <= 1, String(over));
    }
  }
  await h.cdp.send("Emulation.clearDeviceMetricsOverride");
  await page(`(() => { document.documentElement.dataset.theme = 'dark'; return true; })()`);
  await nav("completed");
  await h.screenshot(path.join(SHOTS, "views-completed-dark.png"), { width: 1360, height: 860 });
  await nav("active");
  await h.screenshot(path.join(SHOTS, "views-active-dark.png"), { width: 1360, height: 860 });

  check("no script errors on any page", (await ev("window.__errs.length")) === 0, JSON.stringify(await ev("window.__errs")));
} catch (e) {
  check(`unexpected error: ${e?.stack ?? e}`, false);
} finally {
  srv.close();
  await h.cleanup();
}
finish();
