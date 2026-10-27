// Real-browser test of the "Download video" bar's dragging (apps/browser-extension/src/videobar.js), in a real
// Chromium with real (trusted) mouse and key events from the DevTools protocol.
//
//   node tests/e2e/real-video-bar-drag.mjs [path-to-chromium]
//
// The page runs the shipped videobar.js as it is, with a stand-in for the few extension calls it makes (its state
// from the background script, and the extension's storage). What is checked:
//  * the bar shows over a large video as a small island at its usual place (the video's top right); hovered it
//    grows into the full bar from its middle, left alone it shrinks back; its button still works;
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
// The bar's box, found from the page (its shadow root is closed): the host's place is the bar's middle and top, and its
// edges are where the page's hit test stops landing on it.
const barAt = () =>
  evaluate(`(() => { const h = document.querySelector("velox-video-bar"); const m = /translate\\((-?[\\d.]+)px, ?(-?[\\d.]+)px\\)/.exec(h.style.transform);
    if (!m) return null; const x = +m[1], y = +m[2], my = y + 19; const on = (px) => document.elementsFromPoint(px, my).includes(h);
    let l = x, r = x; while (l > 0 && on(l - 1)) l--; while (r < innerWidth && on(r + 1)) r++;
    let b = y + 2; while (b < innerHeight && document.elementsFromPoint(x, b + 1).includes(h)) b++;
    return { x, y, left: l, w: on(x) ? r - l + 1 : 0, cx: Math.round((l + r) / 2), cy: Math.round((y + b) / 2), shown: h.style.display === "block" }; })()`);

// VELOX_BAR_SHOTS=<folder>: also saves pictures of the bar (idle, open, while sending), to look at its design.
const shot = async (name) => {
  if (!process.env.VELOX_BAR_SHOTS) return;
  const b = await barAt();
  const clip = { x: Math.max(0, b.x - 260), y: Math.max(0, b.y - 30), width: 520, height: 110, scale: 2 };
  const { data } = await cdp("Page.captureScreenshot", { format: "png", clip });
  fs.writeFileSync(path.join(process.env.VELOX_BAR_SHOTS, `bar-${name}.png`), Buffer.from(data, "base64"));
};

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

  const island = await open();
  // Video: 100..900 x 80..530. Idle: the small island, at the top right of the video.
  await sleep(700);
  await shot("idle");
  check("idle, the bar is a small island at the video's top right", island.w > 20 && island.w <= 48 && island.x > 600 && island.x < 900 && Math.abs(island.y - 90) <= 2, JSON.stringify(island));

  // Hovered: it opens, from its middle.
  await cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x: island.x, y: island.y + 19 });
  await sleep(900);
  const start = await barAt();
  await shot("open");
  check("hovered, it grows into the full bar, from its middle", start.w >= 150 && Math.abs(start.cx - island.cx) <= 3, JSON.stringify({ island, start }));
  // Left alone: back to the island.
  await cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x: 300, y: 400 });
  await sleep(1900);
  const idle = await barAt();
  check("left alone, it shrinks back to the island", idle.shown && idle.w > 20 && idle.w <= 48, JSON.stringify(idle));
  await cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x: island.x, y: island.y + 19 });
  await sleep(900);

  // Its button still works: a click sends the video to the app.
  await mouse("mousePressed", start.left + 80, start.y + 19);
  await mouse("mouseReleased", start.left + 80, start.y + 19);
  await waitFor(async () => (await evaluate("sent.includes('bar:download')")) === true, "the click to send the video", 5000).catch(() => false);
  check("the Download video button still works (it sends the video)", await evaluate("sent.includes('bar:download')"));
  await sleep(600);
  await shot("sent");
  await sleep(700);
  const open1 = await barAt();

  // Drag by the handle (the open bar's first pixels) to the bottom left of the video.
  const gx = open1.left + 13;
  const gy = open1.y + 19;
  await evaluate("pageSaw.length = 0");
  await mouse("mousePressed", gx, gy);
  for (let i = 1; i <= 10; i++) await mouse("mouseMoved", gx - i * 25, gy + i * 34);
  await mouse("mouseReleased", gx - 250, gy + 340);
  await sleep(250);
  const dropped = await barAt();
  check("dragging the handle moves the bar", Math.abs(dropped.x - (open1.x - 250)) <= 3 && Math.abs(dropped.y - (open1.y + 340)) <= 3, JSON.stringify({ open1, dropped }));
  const saw = await evaluate("pageSaw.slice()");
  check("the page (its player) gets no press or click from the drag", saw.length === 0, JSON.stringify(saw));
  await cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x: 500, y: 300 });
  await sleep(300);
  const still = await barAt();
  check("it stays where it was dropped", Math.abs(still.cx - dropped.cx) <= 3 && Math.abs(still.y - dropped.y) <= 2, JSON.stringify(still));
  check("the place is kept as a place on the video, nothing about the page", JSON.stringify(Object.keys(stored)) === '["videoBarSpot"]' && stored.videoBarSpot.fx < 0.45 && stored.videoBarSpot.fy > 0.8, JSON.stringify(stored));

  // Dragged past the video's edge: it stops at the edge, inside the video.
  await mouse("mousePressed", dropped.left + 13, dropped.y + 19);
  for (let i = 1; i <= 6; i++) await mouse("mouseMoved", dropped.left + 13 - i * 100, dropped.y + 19);
  await mouse("mouseReleased", dropped.left + 13 - 600, dropped.y + 19);
  await sleep(250);
  const edge = await barAt();
  check("dragged past the video's edge, it stays inside the video", Math.abs(edge.left - 100) <= 2 && Math.abs(edge.y - dropped.y) <= 2, JSON.stringify(edge));
  await mouse("mousePressed", edge.left + 13, edge.y + 19);
  for (let i = 1; i <= 6; i++) await mouse("mouseMoved", edge.left + 13 + i * ((dropped.x - edge.x) / 6), edge.y + 19);
  await mouse("mouseReleased", edge.left + 13 + (dropped.x - edge.x), edge.y + 19);
  await sleep(250);

  // A new page (a new copy of the script): the bar goes where the user put it.
  const again = await open();
  check("a new page puts it at the same place", Math.abs(again.cx - dropped.cx) <= 3 && Math.abs(again.cy - dropped.cy) <= 3, JSON.stringify({ again, dropped }));

  // The page's own (synthetic) events cannot drag it.
  await evaluate(`(() => { const h = document.querySelector("velox-video-bar"); for (const [t, x, y] of [["pointerover", ${again.x}, ${again.y + 19}], ["pointerenter", ${again.x}, ${again.y + 19}], ["pointerdown", ${again.x}, ${again.y + 19}], ["pointermove", 700, 100], ["pointerup", 700, 100]]) h.dispatchEvent(new PointerEvent(t, { bubbles: true, composed: true, clientX: x, clientY: y, button: 0, pointerId: 1 })); return true; })()`);
  await sleep(300);
  const notMoved = await barAt();
  check("the page's own events can neither open nor drag it", Math.abs(notMoved.x - again.x) <= 1 && Math.abs(notMoved.y - again.y) <= 1 && notMoved.w <= 48, JSON.stringify(notMoved));

  // Keyboard: open it, focus the handle (a real click on it), then the arrow keys.
  await cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x: again.x, y: again.y + 19 });
  await sleep(900);
  const open2 = await barAt();
  await mouse("mousePressed", open2.left + 13, open2.y + 19);
  await mouse("mouseReleased", open2.left + 13, open2.y + 19);
  for (const key of ["ArrowRight", "ArrowRight", "ArrowUp"]) {
    await cdp("Input.dispatchKeyEvent", { type: "rawKeyDown", key, code: key, windowsVirtualKeyCode: { ArrowRight: 39, ArrowUp: 38 }[key] });
    await cdp("Input.dispatchKeyEvent", { type: "keyUp", key, code: key, windowsVirtualKeyCode: { ArrowRight: 39, ArrowUp: 38 }[key] });
  }
  await sleep(250);
  const keyed = await barAt();
  check("the arrow keys on the handle move it", Math.abs(keyed.x - (open2.x + 32)) <= 2 && Math.abs(keyed.y - (open2.y - 16)) <= 2, JSON.stringify({ open2, keyed }));

  // Double click on the handle: back to its usual place, and nothing kept.
  await mouse("mousePressed", keyed.left + 13, keyed.y + 19, { clickCount: 1 });
  await mouse("mouseReleased", keyed.left + 13, keyed.y + 19, { clickCount: 1 });
  await mouse("mousePressed", keyed.left + 13, keyed.y + 19, { clickCount: 2 });
  await mouse("mouseReleased", keyed.left + 13, keyed.y + 19, { clickCount: 2 });
  await sleep(400);
  const back = await barAt();
  check("a double click puts it back at the video's top right", Math.abs(back.x - island.x) <= 3 && Math.abs(back.y - island.y) <= 2 && !("videoBarSpot" in stored), JSON.stringify({ back, island, stored }));
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
