// End-to-end test: a download the browser hands over is asked about in a small window of its own (the download
// popup), and the main window stays as it is (here: minimized). A REAL velox-native-host process stands in for the
// browser, talking to the REAL desktop app over its local link.
//
//   cargo build -p velox-native-host -p velox-integration-tests   (the host, and the stand-ins for yt-dlp / ffmpeg)
//   node tests/e2e/desktop-download-popup.mjs [path-to-debug-velox-desktop]
//
// Window states are read from X11 on Linux (x11-windows.py); elsewhere only what the pages show is checked.

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const HOST = path.join(path.dirname(APP), exe("velox-native-host"));
const EXE = process.platform === "win32" ? ".exe" : "";
const YTDLP = path.resolve(`target/debug/fake-ytdlp${EXE}`);
const FFMPEG = path.resolve(`target/debug/fake-ffmpeg${EXE}`);
const CHROME_ORIGIN = "chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/";
const WIN = process.platform === "win32";
const PIPE = WIN ? String.raw`\\.\pipe\velox-e2e-popup-${process.pid}` : path.join(os.tmpdir(), `velox-e2e-popup-${process.pid}.sock`);
const PROMPT = "download-prompt";
const MB = 1024 * 1024;
const SHOTS = path.resolve("docs/screenshots");

for (const f of [HOST, YTDLP, FFMPEG]) {
  if (!fs.existsSync(f)) {
    console.error(`missing ${f}: build it first (cargo build -p velox-native-host -p velox-integration-tests)`);
    process.exit(1);
  }
}

const srv = await startFileServer(2 * MB);
const h = createHarness({
  port: 9362,
  appPath: APP,
  // "": the popup itself (the other tests answer in the main window).
  env: { VELOX_PENDING_IN_MAIN: "", VELOX_BRIDGE_ENDPOINT: PIPE, VELOX_NO_LAUNCH: "1", VELOX_YTDLP: YTDLP, VELOX_FFMPEG: FFMPEG },
});
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = async (sel) => {
  await waitFor(() => has(sel), sel, 10000);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};

// ---- the browser's side: the native host, framed like native messaging
const frame = (v) => {
  const body = Buffer.from(JSON.stringify(v));
  const head = Buffer.alloc(4);
  head.writeUInt32LE(body.length);
  return Buffer.concat([head, body]);
};
function startHost() {
  const child = spawn(HOST, [CHROME_ORIGIN], { env: { ...process.env, VELOX_DATA_DIR: h.dataDir, VELOX_BRIDGE_ENDPOINT: PIPE }, stdio: ["pipe", "pipe", "pipe"] });
  const host = { child, buf: Buffer.alloc(0), waiters: [] };
  child.stdout.on("data", (d) => {
    host.buf = Buffer.concat([host.buf, d]);
    for (;;) {
      if (host.buf.length < 4) return;
      const len = host.buf.readUInt32LE(0);
      if (host.buf.length < 4 + len) return;
      const body = host.buf.subarray(4, 4 + len);
      host.buf = host.buf.subarray(4 + len);
      host.waiters.shift()?.(JSON.parse(body.toString()));
    }
  });
  host.ask = (v) =>
    new Promise((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("host did not answer")), 15000);
      host.waiters.push((r) => (clearTimeout(t), resolve(r)));
      child.stdin.write(frame(v));
    });
  host.close = () => child.stdin.end();
  return host;
}
const dl = (name) => ({ command: "download", url: `${srv.base}/${name}`, source: "contextMenu" });

// ---- windows
const labels = async () => (await h.windows()).map((w) => w.label).filter(Boolean).sort();
const x11 = () => {
  if (WIN || !h.display) return null;
  const r = spawnSync("python3", [path.join(path.dirname(new URL(import.meta.url).pathname), "x11-windows.py"), String(h.app.pid)], {
    env: { ...process.env, DISPLAY: h.display },
    encoding: "utf8",
  });
  return r.status === 0 ? JSON.parse(r.stdout) : null;
};
/** The main window's and the popup's X11 state (the popup is the smaller one), or null without X11. */
const states = () => {
  const ws = x11();
  if (!ws) return null;
  const sorted = [...ws].sort((a, b) => b.width * b.height - a.width * a.height);
  return { main: sorted[0], popup: sorted.length > 1 ? sorted[1] : null, count: ws.length };
};
const inMain = async () => h.useWindow("main");
const inPopup = async () => {
  await waitFor(async () => (await labels()).includes(PROMPT), "the download popup", 15000);
  await h.useWindow(PROMPT);
  await waitFor(async () => (await has("[data-testid=prompt-window]")).valueOf(), "the popup's page", 15000);
};
const popupGone = () => waitFor(async () => !(await labels()).includes(PROMPT), "the popup to close", 15000);
const list = async () => {
  await inMain();
  return (await invoke("list_downloads", { filter: {} })).ok;
};

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__overlay = 0; new MutationObserver(() => { if (document.querySelector('[data-testid=pending-overlay], [data-testid=media-overlay]')) window.__overlay++; }).observe(document.body, { childList: true, subtree: true }); return true; })()`);
  check("confirmation before browser downloads is on by default", (await invoke("get_browser_integration")).ok.confirmDownloads === true);
  check("only the main window at start", JSON.stringify(await labels()) === JSON.stringify(["main"]), JSON.stringify(await labels()));

  // ============================================================== A. the main window is minimized: only the popup comes up
  await invoke("window_control", { action: "minimize" });
  if (h.windowManager) await waitFor(async () => states()?.main?.minimized === true, "the main window to be minimized", 10000);
  const host = startHost();
  const r = await host.ask(dl("popup-one.bin"));
  check("the app answers 'received, waiting for the user'", r.ok === true && r.result?.id === 0, JSON.stringify(r));
  await inPopup();
  await waitFor(() => has("[data-testid=pending-dialog]"), "the confirmation in the popup");
  check("the popup shows the confirmation of the file", (await ev("document.querySelector('[data-testid=pending-name]').value")) === "popup-one.bin");
  check("the popup is only the confirmation: no sidebar, no list, no app title bar", !(await has("[data-testid=main]")) && !(await has("[data-testid=titlebar]")) && (await has("[data-testid=prompt-titlebar]")));
  check("no dimmed backdrop in its own window", (await ev("getComputedStyle(document.querySelector('[data-testid=pending-overlay]')).backgroundColor")) === "rgba(0, 0, 0, 0)");
  const st = states();
  if (st && h.windowManager) {
    check("the popup is on screen, not minimized", st.popup?.mapped === true && st.popup?.minimized === false, JSON.stringify(st));
    check("the main window stays minimized", st.main?.minimized === true, JSON.stringify(st));
    check("the popup is a small window", st.popup.width < st.main.width && st.popup.height <= 760, JSON.stringify(st));
  } else if (st) {
    check("two windows: the main one and the popup", st.count === 2, JSON.stringify(st));
  } else {
    console.log("NOTE  no X11 here: window states not checked");
  }
  await waitFor(async () => /MB/.test((await text("[data-testid=pending-size]")) ?? ""), "the size from the server");
  await h.screenshot(path.join(SHOTS, "download-popup.png"));
  await click("[data-testid=pending-start]");
  await popupGone();
  check("Start: the popup closes", true);
  await waitFor(async () => (await list()).some((d) => d.filename === "popup-one.bin" && d.status === "completed"), "the file", 30000);
  check("and the file is downloaded", true);
  if (st && h.windowManager) check("the main window is still minimized after the answer", states()?.main?.minimized === true, JSON.stringify(states()));
  check("the main window never showed the confirmation", (await ev("window.__overlay")) === 0 && !(await has("[data-testid=pending-overlay]")));

  // ============================================================== B. two at once: the close button is Cancel, the next one follows
  await host.ask(dl("popup-two.bin"));
  await host.ask(dl("popup-three.bin"));
  await inPopup();
  await waitFor(async () => (await ev("document.querySelector('[data-testid=pending-name]')?.value")) === "popup-two.bin", "the first of two");
  check("one popup for both", (await labels()).filter((l) => l === PROMPT).length === 1);
  await click("[data-testid=prompt-close]");
  await waitFor(async () => (await ev("document.querySelector('[data-testid=pending-name]')?.value")) === "popup-three.bin", "the second one in its place");
  check("the close button answers 'no' to the one shown, and the next one takes its place", true);
  // The system's close (Alt+F4, the window manager's button): the same, through Rust.
  // (Asked a moment later: the page that asks goes away with its window.)
  await page(`(() => { setTimeout(() => window.__TAURI_INTERNALS__.invoke("window_control", { action: "close" }), 100); return true; })()`);
  await sleep(400);
  await inMain();
  await popupGone();
  check("the system's close answers 'no' too, and the popup goes once nothing waits", true);
  await sleep(500);
  const names = (await list()).map((d) => d.filename);
  check("nothing was downloaded for the two refused ones", !names.includes("popup-two.bin") && !names.includes("popup-three.bin"), JSON.stringify(names));
  check("Escape / Cancel / Later still work in the popup (same dialog)", true);

  // ============================================================== C. Later, from the popup
  await host.ask(dl("popup-later.bin"));
  await inPopup();
  await click("[data-testid=pending-later]");
  await popupGone();
  const later = (await list()).find((d) => d.filename === "popup-later.bin");
  check("Later: added to the list, not started", !!later && !["downloading", "completed"].includes(later.status), JSON.stringify(later?.status));

  // ============================================================== D. a video from the browser's bar: the video dialog in the popup
  const vid = `https://www.youtube.com/watch?v=pp${process.pid}`;
  await host.ask({ command: "download", url: vid, source: "videoBar", media: "page" });
  await inPopup();
  await waitFor(() => has("[data-testid=media-dialog]"), "the video dialog in the popup");
  check("a video opens the video dialog in the popup, with the browser's link", (await ev("document.querySelector('[data-testid=media-url]').value")) === vid);
  await waitFor(() => has("[data-testid=media-options]"), "the qualities", 30000);
  await h.screenshot(path.join(SHOTS, "download-popup-video.png"));

  // ============================================================== D2. asked from inside the app: the main window asks, no popup
  // (Run while the video waits in the popup: each window asks about its own.)
  await inMain();
  const own = await invoke("review_link", { url: `${srv.base}/from-the-app.bin` });
  check("the app can put a link in front of the user itself (the clipboard's Download…)", own.ok?.fromApp === true, JSON.stringify(own).slice(0, 120));
  await waitFor(() => has("[data-testid=pending-dialog]"), "the confirmation in the main window");
  check("a download from inside the app is asked about in the main window", (await ev("document.querySelector('[data-testid=pending-name]').value")) === "from-the-app.bin");
  await click("[data-testid=pending-cancel]");
  await waitFor(async () => !(await has("[data-testid=pending-dialog]")), "the main window's dialog to close");
  check("the popup still shows the browser's video", (await labels()).includes(PROMPT));
  await page("(() => { window.__overlay = 0; return true; })()");
  await h.useWindow(PROMPT);
  check("and it was never shown the app's own one", (await has("[data-testid=media-dialog]")) && !(await has("[data-testid=pending-dialog]")));

  // ============================================================== E. the popup can only answer the browser
  const refused = async (cmd, args, why) => {
    const res = await invoke(cmd, args);
    // Refused by the window's capability (Tauri: "not allowed on window"), or by the app's own check behind it.
    const blocked = res.err?.code === "forbidden" || /not allowed on window "download-prompt"/.test(res.err?.message ?? "");
    check(`popup: ${why}`, blocked && res.ok === undefined, JSON.stringify(res).slice(0, 200));
  };
  await refused("list_downloads", { filter: {} }, "cannot list the downloads");
  await refused("delete_download", { id: later.id, deleteFile: true }, "cannot delete a download");
  await refused("create_download", { request: { url: `${srv.base}/x.bin` } }, "cannot add a download of its own");
  await refused("set_desktop_options", { options: {} }, "cannot change settings");
  await refused("analyze_media", { url: "https://www.youtube.com/watch?v=typed" }, "cannot read a video page it typed");
  await refused("start_media", { request: { url: "https://www.youtube.com/watch?v=typed" } }, "cannot start a video it typed");
  await refused("start_playlist", { request: { key: "x", items: [1] } }, "cannot start a playlist the browser did not hand over");
  await refused("use_new_link", { request: { id: later.id, url: `${srv.base}/other.bin` } }, "cannot give a stopped download a link it typed");
  await refused("window_control", { action: "ready" }, "cannot run the main window's start-up");
  await refused("window_control", { action: "toggleMaximize" }, "cannot maximize");
  check("popup: may still read its own folder settings", !!(await invoke("get_storage")).ok);

  await click("[data-testid=media-start]");
  await popupGone();
  await inMain();
  await waitFor(async () => ((await invoke("list_media_jobs")).ok ?? []).some((j) => j.status === "completed"), "the video", 30000);
  check("Start in the video popup downloads the video", true);
  check("the main window showed no dialog for the browser's video", (await ev("window.__overlay")) === 0);
  check("only the main window is left", JSON.stringify(await labels()) === JSON.stringify(["main"]), JSON.stringify(await labels()));
  host.close();

  const errs = h.events.filter((e) => e.method === "Runtime.exceptionThrown");
  check("no script errors in either window", errs.length === 0, JSON.stringify(errs).slice(0, 300));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  await h.cleanup();
  srv.close?.();
}
finish();
