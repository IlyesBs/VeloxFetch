// Real-browser test of the "Download video" bar's dragging (apps/browser-extension/src/videobar.js), in a real
// Chromium with real (trusted) mouse and key events from the DevTools protocol.
//
//   node tests/e2e/real-video-bar-drag.mjs [path-to-chromium]
//
// The page runs the shipped videobar.js as it is, with a stand-in for the few extension calls it makes (its state
// from the background script, and the extension's storage). What is checked:
//  * the bar shows over a large video, at its usual place (the video's top right), and its button still works;
//  * dragging its handle moves it, and it stays where it was dropped; the page sees none of the drag's events;
//  * the place is kept as a place on the video: a fresh page (a new copy of the script) puts it there again;
//  * a page's synthetic events cannot drag it; the arrow keys on the handle move it; a double click puts it back.
// Uses a throwaway profile in the temp folder; nothing is installed.

import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { check, finish, sleep, waitFor } from "./harness.mjs";

const BROWSER =
  process.argv[2] ??
  ["/snap/bin/chromium", "/usr/bin/chromium", "/usr/bin/chromium-browser", "/usr/bin/google-chrome", "/usr/bin/brave-browser"].find((p) => fs.existsSync(p));
if (!BROWSER) {
  console.log("SKIP  no Chromium-based browser found");
  process.exit(0);
}
const BAR = fs.readFileSync(path.resolve("apps/browser-extension/src/videobar.js"), "utf8");

// The page: a large video (a canvas stream, nothing to download) and the stand-in extension API. The storage
// lives in the server, so a new page (a new copy of the script) finds what the previous one saved.
let stored = {};
const PAGE = `<!doctype html><meta charset="utf-8"><style>body{margin:0;background:#222}video{position:absolute;left:100px;top:80px;width:800px;height:450px;background:#000}</style>
<video id="v" muted></video>
<script>
  window.pageSaw = [];
  // As a player listens: on the page, as the events bubble up (a page's capturing listeners see every event first).
  for (const t of ["pointerdown", "pointerup", "click", "dblclick", "mousedown"]) addEventListener(t, (e) => pageSaw.push(t));
  const stored = ${"__STORED__"};
  window.sent = [];
  window.chrome = { runtime: { sendMessage: async (m) => (sent.push(m.type), m.type === "bar:state" ? { ok: true, page: "Example", items: [] } : { ok: true }) },
    storage: { local: {
      get: async (k) => (k in stored ? { [k]: stored[k] } : {}),
      set: async (o) => { Object.assign(stored, o); await fetch("/store", { method: "POST", body: JSON.stringify(stored) }); },
      remove: async (k) => { delete stored[k]; await fetch("/store", { method: "POST", body: JSON.stringify(stored) }); } } } };
</script>
<script src="/videobar.js"></script>`;

const server = http.createServer((req, res) => {
  if (req.url === "/videobar.js") return res.end(BAR);
  if (req.url === "/store" && req.method === "POST") {
    let body = "";
    req.on("data", (d) => (body += d));
    return req.on("end", () => ((stored = JSON.parse(body)), res.end("ok")));
  }
  res.setHeader("content-type", "text/html");
  res.end(PAGE.replace('${"__STORED__"}', JSON.stringify(stored)).replace("__STORED__", JSON.stringify(stored)));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

// A snap browser can only use folders in the home folder.
const profile = fs.mkdtempSync(path.join(BROWSER.startsWith("/snap/") ? path.join(os.homedir(), "snap/chromium/common") : os.tmpdir(), "velox-bar-"));
const port = 9520 + (process.pid % 200);
const browser = spawn(BROWSER, [`--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, "--headless=new", "--no-first-run", "--no-default-browser-check", "--window-size=1200,800", "about:blank"], { stdio: "ignore" });

let ws;
let seq = 0;
const pending = new Map();
const cdp = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const id = ++seq;
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });
const evaluate = async (expr) => (await cdp("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true })).result.value;
const mouse = (type, x, y, extra = {}) => cdp("Input.dispatchMouseEvent", { type, x, y, button: "left", buttons: type === "mouseReleased" ? 0 : 1, clickCount: 1, ...extra });
const barAt = () => evaluate(`(() => { const m = /translate\\((-?[\\d.]+)px, ?(-?[\\d.]+)px\\)/.exec(document.querySelector("velox-video-bar").style.transform); const r = document.querySelector("velox-video-bar").getBoundingClientRect(); return m ? { x: +m[1], y: +m[2], cx: Math.round(r.left + r.width / 2), cy: Math.round(r.top + r.height / 2), shown: document.querySelector("velox-video-bar").style.display === "block" } : null; })()`);

async function open() {
  await cdp("Page.navigate", { url: `${base}/?${Date.now()}` });
  await waitFor(async () => (await evaluate("!!document.querySelector('velox-video-bar') && document.readyState === 'complete'")) === true, "the page and the bar");
  // Hover the video: the bar shows.
  await cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x: 500, y: 300 });
  await waitFor(async () => (await barAt())?.shown, "the bar over the video");
  await sleep(150);
  return barAt();
}

try {
  const list = await waitFor(async () => {
    try {
      return await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    } catch {
      return null;
    }
  }, "the browser", 30000);
  const target = list.find((t) => t.type === "page");
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    const p = d.id && pending.get(d.id);
    if (p) (pending.delete(d.id), d.error ? p.reject(new Error(d.error.message)) : p.resolve(d.result));
  };
  await cdp("Page.enable");

  const start = await open();
  // Video: 100..900 x 80..530. The usual place: its top right.
  check("the bar shows over the video at its top right", start.x > 600 && start.x < 900 && Math.abs(start.y - 90) <= 2, JSON.stringify(start));

  // Its button still works: a click sends the video to the app.
  await mouse("mousePressed", start.x + 60, start.y + 18);
  await mouse("mouseReleased", start.x + 60, start.y + 18);
  await waitFor(async () => (await evaluate("sent.includes('bar:download')")) === true, "the click to send the video", 5000).catch(() => false);
  check("the Download video button still works (it sends the video)", await evaluate("sent.includes('bar:download')"));

  // Drag by the handle (the bar's first few pixels) to the bottom left of the video.
  const gx = start.x + 12;
  const gy = start.y + 18;
  await evaluate("pageSaw.length = 0");
  await mouse("mousePressed", gx, gy);
  for (let i = 1; i <= 10; i++) await mouse("mouseMoved", gx - i * 50, gy + i * 38);
  await mouse("mouseReleased", gx - 500, gy + 380);
  await sleep(200);
  const dropped = await barAt();
  check("dragging the handle moves the bar", Math.abs(dropped.x - (start.x - 500)) <= 2 && Math.abs(dropped.y - (start.y + 380)) <= 2, JSON.stringify({ start, dropped }));
  const saw = await evaluate("pageSaw.slice()");
  check("the page (its player) gets no press or click from the drag", saw.length === 0, JSON.stringify(saw));
  await cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x: 500, y: 300 });
  await sleep(300);
  const still = await barAt();
  check("it stays where it was dropped", Math.abs(still.x - dropped.x) <= 2 && Math.abs(still.y - dropped.y) <= 2, JSON.stringify(still));
  check("the place is kept as a place on the video, nothing about the page", JSON.stringify(Object.keys(stored)) === '["videoBarSpot"]' && stored.videoBarSpot.fx < 0.45 && stored.videoBarSpot.fy > 0.8, JSON.stringify(stored));

  // A new page (a new copy of the script): the bar goes where the user put it.
  const again = await open();
  check("a new page puts it at the same place", Math.abs(again.cx - dropped.cx) <= 3 && Math.abs(again.cy - dropped.cy) <= 3, JSON.stringify({ again, dropped }));

  // The page's own (synthetic) events cannot drag it.
  await evaluate(`(() => { const h = document.querySelector("velox-video-bar"); for (const [t, x, y] of [["pointerdown", ${again.x + 12}, ${again.y + 18}], ["pointermove", 700, 100], ["pointerup", 700, 100]]) h.dispatchEvent(new PointerEvent(t, { bubbles: true, composed: true, clientX: x, clientY: y, button: 0, pointerId: 1 })); return true; })()`);
  await sleep(200);
  const notMoved = await barAt();
  check("the page's own events cannot drag it", Math.abs(notMoved.x - again.x) <= 1 && Math.abs(notMoved.y - again.y) <= 1, JSON.stringify(notMoved));

  // Keyboard: focus the handle (a real click on it), then the arrow keys.
  await mouse("mousePressed", again.x + 12, again.y + 18);
  await mouse("mouseReleased", again.x + 12, again.y + 18);
  for (const key of ["ArrowRight", "ArrowRight", "ArrowUp"]) {
    await cdp("Input.dispatchKeyEvent", { type: "rawKeyDown", key, code: key, windowsVirtualKeyCode: { ArrowRight: 39, ArrowUp: 38 }[key] });
    await cdp("Input.dispatchKeyEvent", { type: "keyUp", key, code: key, windowsVirtualKeyCode: { ArrowRight: 39, ArrowUp: 38 }[key] });
  }
  await sleep(200);
  const keyed = await barAt();
  check("the arrow keys on the handle move it", Math.abs(keyed.x - (again.x + 32)) <= 2 && Math.abs(keyed.y - (again.y - 16)) <= 2, JSON.stringify({ again, keyed }));

  // Double click on the handle: back to its usual place, and nothing kept.
  await mouse("mousePressed", keyed.x + 12, keyed.y + 18, { clickCount: 1 });
  await mouse("mouseReleased", keyed.x + 12, keyed.y + 18, { clickCount: 1 });
  await mouse("mousePressed", keyed.x + 12, keyed.y + 18, { clickCount: 2 });
  await mouse("mouseReleased", keyed.x + 12, keyed.y + 18, { clickCount: 2 });
  await sleep(300);
  const back = await barAt();
  check("a double click puts it back at the video's top right", Math.abs(back.x - start.x) <= 2 && Math.abs(back.y - start.y) <= 2 && !("videoBarSpot" in stored), JSON.stringify({ back, start, stored }));
} catch (e) {
  check(`unexpected error: ${e?.stack ?? e}`, false);
} finally {
  try {
    ws?.close();
  } catch {}
  browser.kill();
  server.close();
  await sleep(500);
  fs.rmSync(profile, { recursive: true, force: true });
}
finish();
