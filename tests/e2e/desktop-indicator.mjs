// End-to-end test of the download indicator, in the real desktop app: while something downloads, a small window of
// its own sits on the desktop, over the other programs (also with the main window minimised), with the first
// download (its name, progress and time left). With several, an arrow shows all the others above it (the window
// grows upwards) and hides them again. It can be dragged anywhere and comes back there; a click on a download opens
// its details in the main window; its × hides it until the next downloads; Settings turns it off. It goes once
// nothing downloads, and never keeps the app running.
//
//   cargo build -p velox-desktop -p velox-integration-tests     (the stand-in for yt-dlp)
//   node tests/e2e/desktop-indicator.mjs [path-to-debug-velox-desktop.exe]
//
// Window states are read from X11 on Linux (x11-windows.py); elsewhere only what the pages show is checked.

import fs from "node:fs";
import { spawnSync } from "node:child_process";
import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const YTDLP = path.resolve(exe("target/debug/fake-ytdlp"));
const FFMPEG = path.resolve(exe("target/debug/fake-ffmpeg"));
const SHOTS = path.resolve("docs/screenshots");
const VID = `ind${process.pid}x${Date.now() % 100000}`;
const IND = "download-indicator";
const BAR = "[data-testid=dl-indicator]";
const HERE = path.dirname(new URL(import.meta.url).pathname);

for (const f of [YTDLP, FFMPEG]) {
  if (!fs.existsSync(f)) {
    console.error(`missing ${f}: build it first (cargo build -p velox-integration-tests)`);
    process.exit(1);
  }
}

const srv = await startFileServer(32 * 1024 * 1024);
const h = createHarness({ port: 9377, appPath: APP, env: { VELOX_YTDLP: YTDLP, VELOX_FFMPEG: FFMPEG } });
const ev = (e) => h.page(e).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = async (sel) => {
  await waitFor(() => has(sel), `the element ${sel}`, 10000);
  return ev(`(document.querySelector(${JSON.stringify(sel)}).click(), true)`);
};
const status = async (id) => (await h.invoke("get_download", { id })).ok?.status;
const labels = async () => (await h.windows()).map((w) => w.label).filter(Boolean).sort();
const inMain = () => h.useWindow("main");
const inBar = async () => {
  await waitFor(async () => (await labels()).includes(IND), "the download indicator's window", 15000);
  await h.useWindow(IND);
  await waitFor(() => has(BAR), "the bar on its page", 10000);
};
const add = async (name) => {
  const d = (await h.invoke("create_download", { request: { url: `${srv.base}/${name}` } })).ok;
  await waitFor(async () => (await status(d.id)) === "downloading", `${name} to start`);
  return d;
};

// ---- X11 (Linux): the windows as the screen has them
const x11 = () => {
  if (process.platform !== "linux" || !h.display) return null;
  const r = spawnSync("python3", [path.join(HERE, "x11-windows.py"), String(h.app.pid), "30"], { env: { ...process.env, DISPLAY: h.display }, encoding: "utf8" });
  return r.status === 0 ? JSON.parse(r.stdout) : null;
};
/** The indicator's X11 window: the one about 440 wide (the main window is far wider). */
const barWindow = () => x11()?.find((w) => w.width >= 430 && w.width <= 460) ?? null;
/** Move an X11 window the way a window manager does at the end of a drag. */
const moveX11 = (id, x, y) =>
  spawnSync(
    "python3",
    ["-c", `import ctypes,ctypes.util;x=ctypes.cdll.LoadLibrary(ctypes.util.find_library("X11"));x.XOpenDisplay.restype=ctypes.c_void_p;d=x.XOpenDisplay(None);x.XMoveWindow(ctypes.c_void_p(d),ctypes.c_ulong(${id}),${x},${y});x.XSync(ctypes.c_void_p(d),0)`],
    { env: { ...process.env, DISPLAY: h.display } },
  );

try {
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText ?? ''")).includes("VeloxFetch"), "UI to render");
  await sleep(600);
  check("nothing downloading: only the main window, no indicator", JSON.stringify(await labels()) === JSON.stringify(["main"]), JSON.stringify(await labels()));
  check("the app itself shows no download bar inside its window", !(await has(BAR)));

  // ---- one download: its own window on the desktop, over the others, not in the taskbar
  srv.rateBps = 384 * 1024; // the whole server: long enough to look at everything
  const one = await add("first-file.zip");
  await inBar();
  await ev(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);
  await sleep(500); // its entrance is over
  check("a download starts: the indicator opens as a window of its own", (await labels()).includes(IND));
  check("it shows the download's name", (await text(`${BAR} [data-testid=ind-first] [data-testid=ind-name]`)) === "first-file.zip", await text(BAR));
  await waitFor(async () => / left/.test((await text(`${BAR} [data-testid=ind-numbers]`)) ?? ""), "the progress and time left", 10000);
  const numbers = await text(`${BAR} [data-testid=ind-numbers]`);
  check("it shows the progress, the time left and the speed", /\d+% · \d+:\d\d left · [\d.]+ [KM]B\/s/.test(numbers), numbers);
  check("one download: no arrow, a × to hide it", !(await has("[data-testid=ind-toggle]")) && (await has("[data-testid=ind-dismiss]")));
  check("only the bar is drawn: the page around it is see-through", (await ev("getComputedStyle(document.body).backgroundColor")) === "rgba(0, 0, 0, 0)" && (await ev("getComputedStyle(document.documentElement).backgroundColor")) === "rgba(0, 0, 0, 0)");
  const barH = await ev(`Math.round(document.querySelector('${BAR}').getBoundingClientRect().height)`);
  check("the window is exactly the bar's size", Math.abs((await ev("window.innerHeight")) - barH) <= 2 && (await ev("window.innerWidth")) === 440, JSON.stringify({ barH, h: await ev("window.innerHeight"), w: await ev("window.innerWidth") }));
  const screen = await ev("JSON.stringify({ w: screen.width, h: screen.height })").then(JSON.parse);
  let w0 = barWindow();
  if (w0) {
    check("X11: on screen, kept above the other programs, not in the taskbar", w0.mapped && w0.above && w0.skipTaskbar, JSON.stringify(w0));
    check("X11: the first time, in the bottom corner of the screen", w0.x + w0.width === screen.w - 16 && w0.y + w0.height === screen.h - 16, JSON.stringify({ w0, screen }));
  }

  // ---- it stays when the main window is minimised
  await inMain();
  await h.invoke("window_control", { action: "minimize" });
  await sleep(600);
  if (x11()) check("the main window minimised: the bar is still on the desktop", barWindow()?.mapped === true, JSON.stringify(x11()));
  const refused = await h.invoke("window_control", { action: "fit", height: 300 });
  check("the main window may not use the bar's controls", !!refused.err, JSON.stringify(refused));

  // ---- several: the arrow, the list above, the first stays first, the window grows upwards
  await add("second-file.iso");
  await add("third-file.pdf");
  const video = (await h.invoke("start_media", { request: { url: `https://www.youtube.com/watch?v=slow${VID}`, filename: "A long clip" } })).ok;
  await inBar();
  await waitFor(async () => (await ev(`document.querySelector('${BAR}')?.dataset.count`)) === "4", "four downloads in the indicator", 15000);
  check("several: an arrow with how many more", (await text("[data-testid=ind-toggle]"))?.trim() === "+3", await text("[data-testid=ind-toggle]"));
  check("the first download stays in the bar", (await text("[data-testid=ind-first] [data-testid=ind-name]")) === "first-file.zip");
  check("the list starts folded and out of reach", await ev(`(() => { const l = document.querySelector('[data-testid=ind-list]'); return l.getBoundingClientRect().height < 1 && l.inert && l.getAttribute('aria-hidden') === 'true'; })()`));
  check("the arrow says what it does", (await ev(`document.querySelector('[data-testid=ind-toggle]').getAttribute('aria-label')`)) === "Show all 4 downloads");
  await sleep(400);
  const closedH = await ev("window.innerHeight");
  w0 = barWindow();

  await click("[data-testid=ind-toggle]");
  await sleep(500); // the unfolding
  const listed = await ev(`JSON.stringify([...document.querySelectorAll('[data-testid=ind-list] [data-testid=ind-name]')].map((n) => n.innerText))`).then(JSON.parse);
  check("the arrow shows all the others, with their information", JSON.stringify(listed) === JSON.stringify(["second-file.iso", "third-file.pdf", "A long clip"]), JSON.stringify(listed));
  check("each has its progress bar and numbers", await ev(`[...document.querySelectorAll('[data-testid=ind-list] .dl-ind-line')].every((l) => l.querySelector('[role=progressbar]') && l.querySelector('[data-testid=ind-numbers]').innerText.length > 0)`));
  const openH = await ev("window.innerHeight");
  check("the window grows to show them all", openH > closedH + 100 && Math.abs(openH - (await ev(`document.querySelector('${BAR}').getBoundingClientRect().height`))) <= 2, JSON.stringify({ closedH, openH }));
  const w1 = barWindow();
  if (w0 && w1) check("X11: it grows upwards, its bottom edge stays put", w1.height > w0.height + 100 && w1.y + w1.height === w0.y + w0.height && w1.x === w0.x, JSON.stringify({ w0, w1 }));
  check("the first download is still at the bottom, in the bar", await ev(`document.querySelector('.dl-ind-bar').getBoundingClientRect().bottom >= window.innerHeight - 2`));
  check("the arrow turns to fold it again", (await ev(`document.querySelector('[data-testid=ind-toggle]').getAttribute('aria-label')`)) === "Hide the list" && (await ev(`getComputedStyle(document.querySelector('.dl-ind-chevron')).transform`)) === "none");
  await h.screenshot(path.join(SHOTS, "download-indicator-open.png"));

  await click("[data-testid=ind-toggle]");
  await sleep(800); // folding, then the window gets smaller
  check("clicking again hides them and the window shrinks back", Math.abs((await ev("window.innerHeight")) - closedH) <= 2, JSON.stringify({ closedH, now: await ev("window.innerHeight") }));
  const w2 = barWindow();
  if (w0 && w2) check("X11: back to its size, at the same place", w2.height === w0.height && w2.y === w0.y && w2.x === w0.x, JSON.stringify({ w0, w2 }));
  check("folded, the first download is still shown", (await text("[data-testid=ind-first] [data-testid=ind-name]")) === "first-file.zip");

  // ---- dragging: a press that moves moves the window (the system drags it), and is not a click; a press that does not
  // move is a click. (The move itself needs a window manager: checked on a real desktop.)
  const press = (sel, dx) => ev(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); const r = el.getBoundingClientRect(); const at = (x, y, b) => ({ bubbles: true, clientX: r.left + x, clientY: r.top + y, button: 0, buttons: b, pointerId: 1 });
    el.dispatchEvent(new PointerEvent('pointerdown', at(20, 20, 1))); el.dispatchEvent(new PointerEvent('pointermove', at(20 + ${dx}, 22, 1))); el.dispatchEvent(new PointerEvent('pointerup', at(20 + ${dx}, 22, 0))); el.click(); return true; })()`);
  await press("[data-testid=ind-first]", 40);
  await sleep(800);
  await inMain();
  check("a press that moves is a drag, not a click (no details page opens)", !(await has("[data-testid=detail-view]")));
  await inBar();
  // ---- a click opens the download's details in the main window, brought back
  await press("[data-testid=ind-first]", 0);
  await inMain();
  await waitFor(() => has("[data-testid=detail-view]"), "the first download's details page", 8000);
  check("a click on the bar's download opens its details in the main window", (await text("[data-testid=detail-title]")) === "first-file.zip", await text("[data-testid=detail-title]"));
  if (x11()) check("X11: the main window came back from minimised", x11().some((w) => w.width > 900 && w.mapped && !w.minimized), JSON.stringify(x11()));
  await inBar();
  await click("[data-testid=ind-toggle]");
  await sleep(400);
  await click(`[data-testid=ind-m${video.id}]`);
  await inMain();
  await waitFor(() => has("[data-testid=media-detail-view]"), "the video's details page", 8000);
  check("a video's line opens the video's details page", (await text("[data-testid=media-detail-title]")) === "A long clip");

  // ---- the place: moved somewhere (as at the end of a drag), it is remembered
  const before = barWindow();
  if (before) {
    moveX11(before.id, 200, 300);
    await sleep(1200); // remembered once it stops moving
  }

  // ---- the × hides it until the next downloads
  await inBar();
  await click("[data-testid=ind-dismiss]");
  await sleep(500);
  if (x11()) check("the × hides the bar", !barWindow()?.mapped, JSON.stringify(x11()));
  await inMain();
  await h.invoke("create_download", { request: { url: `${srv.base}/fourth-file.bin` } }); // running or waiting for a place
  await sleep(800);
  if (x11()) check("hidden by hand, it stays hidden while those downloads run", !barWindow()?.mapped, JSON.stringify(x11()));

  // ---- nothing downloads: the window goes; the next download brings it back, where it was left
  await h.invoke("cancel_media_job", { id: video.id });
  for (const d of (await h.invoke("list_downloads", { filter: {} })).ok) await h.invoke("pause_download", { id: d.id });
  await waitFor(async () => !(await labels()).includes(IND), "the indicator's window to close once nothing downloads", 8000);
  check("nothing downloads: the window closes (nothing kept in memory)", JSON.stringify(await labels()) === JSON.stringify(["main"]));
  await h.invoke("resume_download", { id: one.id });
  await inBar();
  await waitFor(async () => (await text("[data-testid=ind-first] [data-testid=ind-name]")) !== null, "the bar's first line", 5000).catch(() => null);
  check("the next download brings it back (the × only lasted until then)", (await text("[data-testid=ind-first] [data-testid=ind-name]")) === "first-file.zip", await text(BAR));
  await sleep(500);
  const after = barWindow();
  if (before && after) check("X11: it comes back where it was moved", after.x === 200 && after.y + after.height === 300 + before.height, JSON.stringify({ before, after }));

  // ---- the theme and the language of the main window apply to it at once
  await inMain();
  await ev(`(localStorage.setItem('velox.theme', 'light'), true)`);
  await inBar();
  await waitFor(async () => (await ev("document.documentElement.dataset.theme")) === "light", "the bar to follow the theme", 5000).catch(() => null);
  check("the bar follows the main window's theme", (await ev("document.documentElement.dataset.theme")) === "light");
  await h.screenshot(path.join(SHOTS, "download-indicator-light.png"));
  await inMain();
  await ev(`(localStorage.setItem('velox.theme', 'dark'), true)`);
  await inBar();
  await waitFor(async () => (await ev("document.documentElement.dataset.theme")) === "dark", "the bar to follow the theme", 5000).catch(() => null);
  await h.screenshot(path.join(SHOTS, "download-indicator-dark.png"));
  await inMain();
  await ev(`(localStorage.setItem('velox.lang', 'fr'), true)`);
  await inBar();
  await waitFor(async () => (await ev("document.documentElement.lang")) === "fr", "the bar to follow the language", 5000).catch(() => null);
  await sleep(600);
  check("the bar follows the main window's language", (await ev("document.documentElement.lang")) === "fr" && /restant/.test((await text("[data-testid=ind-numbers]")) ?? ""), await text("[data-testid=ind-numbers]"));
  await inMain();
  await ev(`(localStorage.setItem('velox.lang', 'en'), true)`);

  // ---- Settings turns it off (and on)
  const off = (await h.invoke("set_desktop_options", { request: { indicator: false } })).ok;
  check("Settings: the switch is kept", off?.indicator === false);
  await sleep(500);
  if (x11()) check("turned off: the bar goes at once", !barWindow()?.mapped, JSON.stringify(x11()));
  await h.invoke("set_desktop_options", { request: { indicator: true } });
  await sleep(800);
  if (x11()) check("turned on again: it is back while downloads run", barWindow()?.mapped === true, JSON.stringify(x11()));
  await inBar();
  const tried = [await h.invoke("pause_download", { id: one.id }), await h.invoke("window_control", { action: "minimize" })];
  check("the indicator window may not change downloads nor other window controls", tried.every((r) => !!r.err), JSON.stringify(tried));
  check("no page errors in the bar", (await ev(`JSON.stringify(window.__errs ?? [])`)) === "[]");

  // ---- closing the app: the bar never keeps it running
  await inMain();
  const closing = h.exited();
  await h.closeWindow();
  const code = await Promise.race([closing, sleep(20000).then(() => "still running")]);
  check("closing the main window ends the app, bar included", code !== "still running", String(code));
} catch (e) {
  check("the run completed", false, String(e?.stack ?? e));
} finally {
  srv.close();
  await h.cleanup();
}
finish();
