// End-to-end test of the everyday-use features in the real desktop app:
// file-type icons, Open / Show in folder (dry run), search / filters / bulk actions, the "already in your list"
// prompt, sign in and retry, the Add dialog's sign-in and headers, network settings (proxy / user agent),
// speed limits by kind of file, history (search, remove, clear, download again), paste / drop a link anywhere,
// keyboard shortcuts, the sectioned Settings page, and the confirmation popup following a rename.
// Writes screenshots to docs/screenshots.
//
//   node tests/e2e/desktop-usability.mjs [path-to-debug-velox-desktop.exe]

import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const srv = await startFileServer(400 * 1024);
const h = createHarness({ port: 9351, appPath: APP, env: { VELOX_ORGANIZE: "1" } });
const { page, invoke } = h;

const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const count = (sel) => ev(`document.querySelectorAll(${JSON.stringify(sel)}).length`);
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const clickText = async (sel, label) => {
  await waitFor(() => ev(`[...document.querySelectorAll(${JSON.stringify(sel)})].some((b) => b.innerText.trim() === ${JSON.stringify(label)})`), `${label}`);
  return page(`(() => { [...document.querySelectorAll(${JSON.stringify(sel)})].find((b) => b.innerText.trim() === ${JSON.stringify(label)}).click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
const key = (k, mods = {}) =>
  page(`(() => { document.body.dispatchEvent(new KeyboardEvent('keydown', { key: ${JSON.stringify(k)}, ctrlKey: ${!!mods.ctrl}, shiftKey: ${!!mods.shift}, bubbles: true })); return true; })()`);
const nav = async (n) => (await click(`[data-testid=nav-${n}]`), sleep(250));
const toasts = () => ev(`[...document.querySelectorAll('[role=alert],[role=status]')].map((e) => e.innerText).join(' | ')`);
const clearToasts = () => page(`(() => { document.querySelectorAll('[aria-label=Dismiss]').forEach((b) => b.click()); return true; })()`);
const settle = async (id) => waitFor(async () => ["completed", "failed"].includes((await invoke("get_download", { id })).ok?.status), `download ${id}`, 30000);
async function get(name, extra = {}) {
  const d = (await invoke("create_download", { request: { url: `${srv.base}/${name}`, ...extra } })).ok;
  await settle(d.id);
  return (await invoke("get_download", { id: d.id })).ok;
}
const dark = () => page(`(() => { document.documentElement.dataset.theme = 'dark'; return true; })()`);
const light = () => page(`(() => { document.documentElement.dataset.theme = 'light'; return true; })()`);

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================ icons, open, show in folder
  const pdf = await get("report.pdf");
  const exe = await get("setup.exe");
  const song = await get("song.mp3");
  await nav("downloads");
  await waitFor(() => has(`[data-testid=download-${song.id}] [data-kind=audio]`), "icons");
  check("each download shows the icon of its kind", (await has(`[data-testid=download-${pdf.id}] [data-kind=document]`)) && (await has(`[data-testid=download-${exe.id}] [data-kind=program]`)));
  check("a finished download shows a full, green progress bar", await has(`[data-testid=download-${pdf.id}] [role=progressbar][aria-valuenow='100'] .bar-ok`));
  check("a finished document offers Open", await has(`[data-testid=download-${pdf.id}] [data-testid=open-file]`));
  check("a finished program never offers Open (only Show in folder)", !(await has(`[data-testid=download-${exe.id}] [data-testid=open-file]`)) && (await has(`[data-testid=download-${exe.id}] [data-testid=show-in-folder]`)));
  await clearToasts();
  await click(`[data-testid=download-${pdf.id}] [data-testid=open-file]`);
  await click(`[data-testid=download-${exe.id}] [data-testid=show-in-folder]`);
  await sleep(300);
  check("Open and Show in folder work (no error)", !/error|not/i.test(await toasts()), await toasts());
  const refused = await invoke("open_file", { target: { kind: "download", id: exe.id } });
  check("Rust refuses to open a program even when asked directly", refused.err?.code === "invalid_input" && /does not open programs/.test(refused.err.message), JSON.stringify(refused));
  const outside = await invoke("show_in_folder", { target: { kind: "download", id: 424242 } });
  check("an unknown download cannot be shown", outside.err?.code === "not_found", JSON.stringify(outside));
  const pathArg = await invoke("show_in_folder", { target: { kind: "path", path: "C:\\Windows" } });
  check("show_in_folder takes no path, only an id", !!pathArg.err, JSON.stringify(pathArg));
  await page(`(() => { document.querySelector('[data-testid=download-${pdf.id}]').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 200, clientY: 200 })); return true; })()`);
  await waitFor(() => has("[role=menu]"), "menu");
  const items = await ev(`[...document.querySelectorAll('[role=menu] [role=menuitem]')].map((b) => b.innerText.trim()).join('|')`);
  check("a finished file's menu starts with Open and Show in folder", items.startsWith("Open|Show in folder|Download again|Details|Copy link"), items);
  await key("Escape");
  await page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); return true; })()`);

  // ============================================================ duplicate prompt
  await fill("[data-testid=url-input]", `${srv.base}/report.pdf`);
  await click("[data-testid=add-button]");
  await waitFor(() => has("[data-testid=duplicate-dialog]"), "duplicate prompt");
  check("adding a link that is already in the list asks first", (await text("[data-testid=duplicate-dialog]")).includes("report.pdf"));
  check(
    "the prompt knows the file is finished and still there: Open, Show in folder, Download again",
    (await ev(`document.querySelector('[data-testid=duplicate-dialog]').dataset.file`)) === "present" &&
      (await has("[data-testid=duplicate-open]")) &&
      (await has("[data-testid=duplicate-folder]")) &&
      (await has("[data-testid=duplicate-again]")),
  );
  const before = (await invoke("list_downloads", { filter: {} })).ok.length;
  await h.screenshot(path.join(SHOTS, "duplicate-dark.png"), { width: 1000, height: 640 });
  await click("[data-testid=duplicate-again]");
  await waitFor(async () => (await invoke("list_downloads", { filter: {} })).ok.length === before + 1, "second copy");
  check("'Download again' adds it anyway", true);
  const second = (await invoke("list_downloads", { filter: {} })).ok.at(-1);
  await settle(second.id);
  const again = (await invoke("get_download", { id: second.id })).ok;
  check("... under a new name, never over the first file", again.status === "completed" && again.destination !== pdf.destination, again.destination);

  // ============================================================ sign in and retry
  const locked = (await invoke("create_download", { request: { url: `${srv.base}/auth/members.zip` } })).ok;
  await settle(locked.id);
  const lockedNow = (await invoke("get_download", { id: locked.id })).ok;
  check("a server asking for a password is explained plainly", lockedNow.error === "The server asks for a user name and password." && lockedNow.needs === "signIn", JSON.stringify(lockedNow.error));
  await waitFor(() => has(`[data-testid=download-${locked.id}] [data-testid=signin-open]`), "sign-in button");
  check("the card offers Sign in instead of a bare Retry", true);
  check("the plain explanation is on the card, the technical one in its tooltip", (await text(`[data-testid=download-${locked.id}] [data-testid=card-error]`)).includes("user name and password") && (await ev(`document.querySelector('[data-testid=download-${locked.id}] [data-testid=card-error]').title`)).includes("401"));
  await click(`[data-testid=download-${locked.id}] [data-testid=signin-open]`);
  await waitFor(() => has("[data-testid=signin-dialog]"), "sign-in dialog");
  check("the sign-in dialog does not open the details behind it", (await has("[data-testid=signin-dialog]")) && !(await has("[data-testid=detail-view]")));
  await fill("[data-testid=signin-user]", "user");
  await fill("[data-testid=signin-pass]", "pass");
  await dark();
  await h.screenshot(path.join(SHOTS, "signin-dark.png"), { width: 1000, height: 640 });
  await click("[data-testid=signin-submit]");
  await settle(locked.id);
  await waitFor(async () => (await invoke("get_download", { id: locked.id })).ok.status === "completed", "signed-in download to finish", 20000);
  check("after signing in the download finishes", (await invoke("get_download", { id: locked.id })).ok.status === "completed");
  check("the password never comes back to the UI", !JSON.stringify((await invoke("get_download_detail", { id: locked.id })).ok).includes("pass\""));

  // ============================================================ Add dialog: sign-in and headers
  await clickText("button", "Options");
  await waitFor(() => ev("document.querySelector('dialog[open]') !== null"), "add dialog");
  await page(`(() => { document.querySelector('[data-testid=add-advanced]').open = true; return true; })()`);
  await fill("dialog[open] input[required]", `${srv.base}/auth/private.pdf`);
  await fill("[data-testid=add-username]", "user");
  await fill("[data-testid=add-password]", "pass");
  await fill("[data-testid=add-referrer]", "https://members.example/page");
  await fill("[data-testid=add-cookies]", "session=e2e");
  await h.screenshot(path.join(SHOTS, "add-advanced-dark.png"), { width: 1000, height: 820 });
  srv.headers.length = 0;
  await page(`(() => { document.querySelector('dialog[open] button[type=submit]').click(); return true; })()`);
  const priv = await waitFor(async () => (await invoke("list_downloads", { filter: {} })).ok.find((d) => d.filename === "private.pdf"), "private.pdf");
  await settle(priv.id);
  check("the Add dialog's sign-in works", (await invoke("get_download", { id: priv.id })).ok.status === "completed");
  check("its referrer and cookies reach the server", srv.headers.some((hd) => hd.referer === "https://members.example/page" && hd.cookie === "session=e2e"));

  // ============================================================ search, filters, bulk
  await nav("downloads");
  await fill("[data-testid=list-search]", "song");
  await waitFor(() => has("[data-testid=filtered-list]"), "filtered list");
  check("search narrows the list", (await count("[data-testid=filtered-list] > article")) === 1 && (await text("[data-testid=filtered-list]")).includes("song.mp3"));
  await fill("[data-testid=list-search]", "nothing-like-this");
  await waitFor(() => has("[data-testid=filtered-empty]"), "empty result");
  check("no match says so and offers to show everything", (await text("[data-testid=filtered-empty]")).includes("Show everything"));
  await fill("[data-testid=list-search]", "");
  await click("[data-testid=filter-completed]");
  await waitFor(() => has("[data-testid=filtered-list]"), "finished filter");
  const finishedCount = (await invoke("list_downloads", { filter: {} })).ok.filter((d) => d.status === "completed").length;
  check("the Finished filter shows the finished downloads", (await count("[data-testid=filtered-list] > article")) === finishedCount);
  await click("[data-testid=filter-all]");
  await light();
  await sleep(400);
  await h.screenshot(path.join(SHOTS, "dashboard-icons-light.png"), { width: 1000, height: 900 });
  await dark();
  await h.screenshot(path.join(SHOTS, "dashboard-icons-dark.png"), { width: 1000, height: 900 });

  // ============================================================ network settings
  await nav("settings");
  await waitFor(() => has("[data-testid=settings-nav]"), "settings nav");
  check("Settings has a side menu with every section", (await count("[data-testid=settings-nav] button")) === 9); // incl. Diagnostics (Phase 14)
  await clickText("[data-testid=settings-nav] button", "Network");
  await waitFor(async () => (await ev("document.querySelector('[data-testid=settings-nav] [aria-current=true]')?.innerText.trim()")) === "Network", "Network highlighted", 5000).catch(() => null);
  check("the side menu highlights the section on screen", (await ev("document.querySelector('[data-testid=settings-nav] [aria-current=true]')?.innerText.trim()")) === "Network");
  await click("[data-testid=proxy-manual]");
  await fill("[data-testid=proxy-url]", "ftp://nope:21");
  await clearToasts();
  await click("[data-testid=network-save]");
  await waitFor(async () => /proxy must start/.test(await toasts()), "proxy refused");
  check("a bad proxy address is refused with a clear reason", true);
  check("... and nothing was changed", (await invoke("get_network")).ok.proxyMode === "system");
  await click("[data-testid=proxy-system]");
  await fill("[data-testid=user-agent]", "VeloxE2E/9.9");
  await click("[data-testid=network-save]");
  await waitFor(async () => (await invoke("get_network")).ok.userAgent === "VeloxE2E/9.9", "user agent saved");
  srv.headers.length = 0;
  await get("ua-check.bin");
  check("the chosen user agent is what servers see", srv.headers.length > 0 && srv.headers.every((hd) => hd["user-agent"] === "VeloxE2E/9.9"), JSON.stringify(srv.headers.map((x) => x["user-agent"])));
  await h.screenshot(path.join(SHOTS, "settings-network-dark.png"), { width: 1000, height: 760 });
  await invoke("set_network", { request: { proxyMode: "system", userAgent: "" } });

  // ============================================================ speed limit by kind
  await clickText("[data-testid=settings-nav] button", "Speed");
  await click("[data-testid=type-limits-toggle]");
  await waitFor(() => has("[data-testid=type-limit-video]"), "type limits");
  await fill("[data-testid=type-limit-video] input", "2M");
  await clickText("[data-testid=type-limit-video] button", "Set");
  await waitFor(async () => (await invoke("get_speed_limits")).ok.byType.video === 2 * 1024 * 1024, "video limit");
  check("a speed limit for videos is set from Settings", true);
  check("... and shown next to Videos", !!(await waitFor(async () => (await text("[data-testid=type-limit-video]"))?.includes("2.0 MB/s"), "limit shown", 5000).catch(() => false)), await text("[data-testid=type-limit-video]"));
  await h.screenshot(path.join(SHOTS, "settings-speed-dark.png"), { width: 1000, height: 760 });
  await invoke("set_speed_limit", { request: { target: { kind: "category", name: "video" }, bytesPerSec: null } });

  // ============================================================ keyboard shortcuts
  await key("1", { ctrl: true });
  await waitFor(() => has("[data-testid=url-input]"), "downloads view");
  check("Ctrl+1 opens Downloads", true);
  await key(",", { ctrl: true });
  await waitFor(() => has("[data-testid=settings-nav]"), "settings view");
  check("Ctrl+, opens Settings", true);
  await key("3", { ctrl: true });
  await waitFor(() => has("[data-testid=history-list]"), "history view");
  check("Ctrl+3 opens History", true);
  await key("n", { ctrl: true });
  await waitFor(() => ev("document.querySelector('dialog[open]') !== null"), "add dialog from Ctrl+N");
  check("Ctrl+N opens the Add dialog (from any view)", await has("[data-testid=url-input]"));
  await page(`(() => { document.querySelector('dialog[open]').close(); return true; })()`);
  await key("l", { ctrl: true });
  await sleep(150);
  check("Ctrl+L puts the cursor in the link bar", await ev("document.activeElement?.dataset.testid === 'url-input'"));

  // ============================================================ paste and drop anywhere
  await page(`(() => { document.activeElement?.blur(); return true; })()`);
  await fill("[data-testid=url-input]", "");
  await page(`(() => { const dt = new DataTransfer(); dt.setData('text/plain', 'look: ${srv.base}/pasted.iso'); document.body.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true })); return true; })()`);
  await waitFor(async () => (await ev("document.querySelector('[data-testid=url-input]').value")) === `${srv.base}/pasted.iso`, "pasted link");
  check("pasting a link anywhere puts it in the link bar (nothing starts by itself)", !(await invoke("list_downloads", { filter: {} })).ok.some((d) => d.filename === "pasted.iso"));
  await fill("[data-testid=url-input]", "");
  await page(`(() => { const dt = new DataTransfer(); dt.setData('text/uri-list', '# comment\\r\\n${srv.base}/dropped.zip'); document.querySelector('main').dispatchEvent(new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true })); return true; })()`);
  await waitFor(async () => (await ev("document.querySelector('[data-testid=url-input]').value")) === `${srv.base}/dropped.zip`, "dropped link");
  check("dropping a link on the window puts it in the link bar", true);
  await fill("[data-testid=url-input]", "");
  await page(`(() => { const dt = new DataTransfer(); dt.setData('text/plain', '${srv.base}/a1.zip ${srv.base}/a2.zip'); document.body.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true })); return true; })()`);
  await waitFor(() => has("[data-testid=batch-dialog]"), "batch dialog from a paste of several links");
  check("pasting several links opens 'Add several' with them", (await ev("document.querySelector('[data-testid=batch-links]').value")).split("\n").length === 2);
  await click("[data-testid=batch-cancel]");
  await page(`(() => { const dt = new DataTransfer(); dt.setData('text/plain', 'javascript:alert(1)'); document.body.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true })); return true; })()`);
  await sleep(200);
  check("pasting something that is not an http(s) link does nothing", (await ev("document.querySelector('[data-testid=url-input]').value")) === "" && !(await has("[data-testid=batch-dialog]")));

  // ============================================================ history
  await nav("history");
  await waitFor(() => has("[data-testid=history-row]"), "history rows");
  check("history rows show the icon of their kind", await has("[data-testid=history-row] [data-kind=document]"));
  check("history is grouped by day", (await text("[data-testid=history-list]")).includes("TODAY") || (await text("[data-testid=history-list]")).includes("Today"));
  await fill("[data-testid=history-search]", "song");
  await sleep(150);
  check("history search narrows the list", (await count("[data-testid=history-row]")) === 1);
  await fill("[data-testid=history-search]", "");
  await sleep(150);
  await h.screenshot(path.join(SHOTS, "history-dark.png"), { width: 1000, height: 800 });
  const rows = await count("[data-testid=history-row]");
  const firstRowMenu = `[data-testid=history-row] button[aria-label^='More actions']`;
  await click(firstRowMenu);
  await clickText("[role=menu] [role=menuitem]", "Remove from history");
  await waitFor(async () => (await count("[data-testid=history-row]")) === rows - 1, "row removed");
  check("a history line can be removed", true);
  const total = (await invoke("get_metrics")).ok.totalDownloaded;
  await click("[data-testid=history-clear]");
  await click("[data-testid=history-clear-confirm]");
  await waitFor(() => ev("!document.querySelector('[data-testid=history-row]')"), "history cleared");
  check("history can be cleared (after a confirmation)", (await invoke("list_history", { limit: 50 })).ok.length === 0);
  check("the files are still there and the lifetime total is kept", (await invoke("get_metrics")).ok.totalDownloaded >= total);

  // ============================================================ confirmation popup follows a rename
  await nav("downloads");
  await invoke("review_link", { url: `${srv.base}/incoming.bin` });
  await waitFor(() => has("[data-testid=pending-dialog]"), "confirmation popup");
  await waitFor(async () => (await text("[data-testid=pending-dir-path]"))?.includes("Other"), "sorted folder");
  await fill("[data-testid=pending-name]", "renamed-track.mp3");
  await waitFor(async () => (await text("[data-testid=pending-dir-path]"))?.endsWith("Audio"), "folder follows the new name", 5000);
  check("renaming in the popup updates where the file will go", (await text("[data-testid=pending-dir-path]")).endsWith("Audio"));
  await click("[data-testid=pending-cancel]");

  // ============================================================ detail view
  const failed = (await invoke("create_download", { request: { url: `${srv.base}/404/gone.iso` } })).ok;
  await settle(failed.id);
  await click(`[data-testid=download-${failed.id}]`);
  await waitFor(() => has("[data-testid=detail-error]"), "detail error");
  check("the detail view explains a failure and keeps the technical details", (await text("[data-testid=detail-error]")).includes("not on the server") && (await has("[data-testid=detail-error] details")));
  await h.screenshot(path.join(SHOTS, "detail-error-dark.png"), { width: 1000, height: 700 });

  const errs = await ev("window.__errs");
  check("no JavaScript errors", errs.length === 0, errs.join(" | "));
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  srv.close();
  finish();
}
