// The pictures for the public download page and the download sites (docs/publishing.md), taken from the real app
// as it ships: its version, its windows, its languages. Downloads look like real ones: the app goes through a local
// proxy, so a link such as http://releases.example.org/... is served from this computer (example.org and its
// cousins are names reserved for examples: nothing real is named or reached). The streamed video is a playlist served the same way.
//
//   cargo build -p velox-desktop -p velox-integration-tests     (debug app + the video stand-in)
//   node tests/e2e/publish-screenshots.mjs [path-to-debug-velox-desktop]
//
// Writes docs/publish/screenshots/*.png (1280x800: the size the browser stores ask for, fine everywhere else).
// Like every e2e test: a throwaway data folder, nothing outside it, no power action, no browser profile.

import fs from "node:fs";
import http from "node:http";
import { check, createHarness, exe, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const YTDLP = path.resolve(exe("target/debug/fake-ytdlp"));
const FFMPEG = path.resolve(exe("target/debug/fake-ffmpeg"));
const SHOTS = path.resolve("docs/publish/screenshots");
const VERSION = JSON.parse(fs.readFileSync("apps/desktop/src-tauri/tauri.conf.json", "utf8")).version;
const MB = 1024 * 1024;
const SIZE = { width: 1280, height: 800 };

for (const tool of [YTDLP, FFMPEG]) {
  if (!fs.existsSync(tool)) {
    console.error(`missing ${tool}: cargo build -p velox-integration-tests`);
    process.exit(1);
  }
}

// ---- the files, by link: size and the server's speed (shared by all connections, like a real line) -----------
const FILES = {
  "http://releases.example.org/24.04/ubuntu-24.04.1-desktop-amd64.iso": { size: 6_114_656_256, rate: 46 * MB },
  "http://cdn.example.com/blender/release/blender-4.3.2-windows-x64.zip": { size: 398_458_880, rate: 21 * MB },
  "http://files.example.net/backup/family-photos-2026.zip": { size: 2_576_980_378, rate: 11 * MB },
  "http://downloads.example.org/devkit/devkit-2026.2-setup.exe": { size: 862_978_048, rate: 30 * MB },
  "http://assets.example.com/packs/texture-pack-hd.7z": { size: 96 * MB, rate: 200 * MB },
  "http://mirror.example.org/linux/debian-13.1.0-amd64-netinst.iso": { size: 754_974_720, rate: 25 * MB },
  "http://downloads.example.org/studio/photo-studio-2026-x64-setup.exe": { size: 1_288_490_189, rate: 40 * MB },
};
const MISSING = "http://archive.example.org/drivers/printer-driver-2019.exe";
const VIDEO = "http://video.example.com/sunset-timelapse/master.m3u8";
/** A streamed video (HLS): three qualities, ten minutes in six-second pieces. */
const media = (height) => ["#EXTM3U", "#EXT-X-VERSION:3", "#EXT-X-TARGETDURATION:6", "#EXT-X-PLAYLIST-TYPE:VOD",
  ...Array.from({ length: 100 }, (_, i) => `#EXTINF:6.0,\n${height}p/${String(i).padStart(3, "0")}.ts`), "#EXT-X-ENDLIST", ""].join("\n");
const PLAYLISTS = {
  [VIDEO]: ["#EXTM3U",
    '#EXT-X-STREAM-INF:BANDWIDTH=6500000,RESOLUTION=1920x1080,CODECS="avc1.640028,mp4a.40.2"', "1080p.m3u8",
    '#EXT-X-STREAM-INF:BANDWIDTH=3200000,RESOLUTION=1280x720,CODECS="avc1.4d401f,mp4a.40.2"', "720p.m3u8",
    '#EXT-X-STREAM-INF:BANDWIDTH=1100000,RESOLUTION=640x360,CODECS="avc1.42001e,mp4a.40.2"', "360p.m3u8", ""].join("\n"),
  ...Object.fromEntries([1080, 720, 360].map((h) => [VIDEO.replace("master", `${h}p`), media(h)])),
};

/** The byte at `pos` of every file: a position-dependent pattern, so any size works without a source file. */
const fillPattern = (buf, pos) => {
  for (let i = 0; i < buf.length; i++) buf[i] = ((pos + i) * 2654435761) >>> 24;
};
const pace = new Map(); // link -> when its line is free again (ms)

// A plain-HTTP proxy: the app asks for absolute links ("GET http://host/path"), they are answered here.
const proxy = http.createServer(async (req, res) => {
  if (PLAYLISTS[req.url]) return void res.writeHead(200, { "Content-Type": "application/vnd.apple.mpegurl" }).end(PLAYLISTS[req.url]);
  const file = FILES[req.url];
  if (!file) return void res.writeHead(404, { "Content-Type": "text/plain" }).end("not found");
  const m = /bytes=(\d+)-(\d*)/.exec(req.headers.range ?? "");
  const start = m ? Number(m[1]) : 0;
  const end = m && m[2] ? Math.min(Number(m[2]), file.size - 1) : file.size - 1;
  const headers = {
    "Accept-Ranges": "bytes",
    ETag: `"${Buffer.from(req.url).toString("base64url").slice(-16)}"`,
    "Last-Modified": "Tue, 06 Oct 2026 08:00:00 GMT",
    "Content-Type": "application/octet-stream",
    "Content-Length": end - start + 1,
  };
  if (m) headers["Content-Range"] = `bytes ${start}-${end}/${file.size}`;
  res.writeHead(m ? 206 : 200, headers);
  if (req.method === "HEAD") return void res.end();
  const chunk = Buffer.alloc(64 * 1024);
  for (let pos = start; pos <= end && !res.destroyed; pos += chunk.length) {
    const n = Math.min(chunk.length, end - pos + 1);
    const now = Date.now();
    const slot = Math.max(now, pace.get(req.url) ?? 0);
    pace.set(req.url, slot + (n / file.rate) * 1000);
    if (slot > now) await sleep(slot - now);
    fillPattern(chunk, pos);
    if (!res.write(n === chunk.length ? chunk : chunk.subarray(0, n))) await new Promise((r) => res.once("drain", r));
  }
  res.end();
});
await new Promise((r) => proxy.listen(0, "127.0.0.1", r));
const PROXY = `http://127.0.0.1:${proxy.address().port}`;

const h = createHarness({ port: 9375, appPath: APP, env: { VELOX_YTDLP: YTDLP, VELOX_FFMPEG: FFMPEG, VELOX_ORGANIZE: "0" } });
const { page, invoke } = h;
const ev = async (e) => (await page(e)).ok;
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = async (sel) => {
  await waitFor(() => has(sel), `the element ${sel}`, 15000);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const key = (k, more = "") => page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: ${JSON.stringify(k)}, bubbles: true ${more} })); return true; })()`);
const theme = (t) => page(`(document.documentElement.setAttribute('data-theme', ${JSON.stringify(t)}), true)`);
const shot = async (name) => {
  // The confirmations of earlier steps ("Queue created"...) are not part of the picture.
  await page(`(() => { document.querySelectorAll('[data-testid=toast-dismiss]').forEach((b) => b.click()); return true; })()`);
  await sleep(500);
  // Every link was answered by the local proxy: its address must never be in a picture.
  check(`${name}: nothing names this computer's address`, !(await ev("document.body.innerText")).includes("127.0.0.1"));
  await h.screenshot(path.join(SHOTS, name), SIZE);
  check(`picture ${name}`, fs.statSync(path.join(SHOTS, name)).size > 20_000);
};
const add = async (url, more = {}) => (await invoke("create_download", { request: { url, ...more } })).ok;
const status = async (id) => (await invoke("get_download", { id })).ok?.status;

try {
  fs.mkdirSync(SHOTS, { recursive: true });
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText ?? ''")).includes("VeloxFetch"), "the window");
  await theme("dark");
  check("the window shows the version that ships", (await ev("document.querySelector('header').innerText")).includes(VERSION), VERSION);
  const net = await invoke("set_network", { request: { proxyMode: "manual", proxyUrl: PROXY } });
  check("the app goes through the local example server", !!net.ok, JSON.stringify(net.err));
  await invoke("set_queue_config", { request: { maxActive: 3 } });

  // ---- a finished one, then three running and one waiting ------------------------------------------------
  const done = await add("http://assets.example.com/packs/texture-pack-hd.7z");
  await waitFor(async () => (await status(done.id)) === "completed", "the finished download", 60000);
  const iso = await add("http://releases.example.org/24.04/ubuntu-24.04.1-desktop-amd64.iso");
  await add("http://cdn.example.com/blender/release/blender-4.3.2-windows-x64.zip");
  await add("http://files.example.net/backup/family-photos-2026.zip");
  await add("http://downloads.example.org/devkit/devkit-2026.2-setup.exe");
  await sleep(9000); // connections climb, the speed settles
  await click("[data-testid=nav-downloads]");
  await page(`(document.querySelector('[data-testid=main]').scrollTop = 0, true)`);
  check("three downloads run at once, the fourth waits", (await ev(`document.querySelectorAll('[data-testid^=download-][data-status=downloading]').length`)) === 3);
  await shot("dashboard-dark.png");
  await theme("light");
  await shot("dashboard-light.png");
  await theme("dark");

  // ---- one download in detail -------------------------------------------------------------------------
  await click(`[data-testid=download-${iso.id}]`);
  await waitFor(() => has("[data-testid=diagnostics-card]"), "the details page");
  await shot("details-dark.png");

  // ---- Smart throttle on -------------------------------------------------------------------------------
  await click("[data-testid=nav-downloads]");
  await click("[data-testid=hud-toggle]");
  await waitFor(async () => !!(await ev("document.documentElement.dataset.throttle")), "the Smart throttle");
  await sleep(2500);
  await shot("smart-throttle-dark.png");
  await click("[data-testid=hud-switch]");
  await waitFor(async () => !(await ev("document.documentElement.dataset.throttle")), "the Smart throttle off");

  // ---- the queue with a night window ---------------------------------------------------------------------
  await click("[data-testid=nav-queue]");
  await click("[data-testid=sq-new]");
  await waitFor(() => has("[data-testid=sq-editor]"), "the queue editor");
  await click("[data-testid=sq-preset-night]");
  await fill("[data-testid=sq-name]", "Night");
  // Only the window: the preset's "big files go here" would also move the running downloads into it.
  await fill("[data-testid=sq-big]", "0");
  await click("[data-testid=sq-save]");
  const night = await waitFor(async () => (await invoke("get_queues")).ok.queues.find((x) => x.name === "Night"), "the Night queue");
  await add("http://mirror.example.org/linux/debian-13.1.0-amd64-netinst.iso", { queueId: night.id });
  await sleep(1200);
  await shot("queue-dark.png");

  // ---- a failed download: the Doctor explains ----------------------------------------------------------------
  const lost = await add(MISSING);
  await waitFor(async () => (await status(lost.id)) === "failed", "the failed download", 30000);
  await click("[data-testid=nav-downloads]");
  await click(`[data-testid=download-${lost.id}]`);
  await waitFor(() => has("[data-testid=doctor]"), "the Doctor");
  await shot("doctor-dark.png");

  // ---- before a download: the analysis -------------------------------------------------------------------
  await click("[data-testid=nav-downloads]");
  await page(`(window.dispatchEvent(new Event('velox:open-add')), true)`);
  await waitFor(() => has("[data-testid=add-check]"), "the Add dialog");
  await fill("dialog[aria-labelledby=add-title] input.input", "http://downloads.example.org/studio/photo-studio-2026-x64-setup.exe");
  await click("[data-testid=add-check]");
  await waitFor(() => has("dialog[aria-labelledby=add-title] [data-testid=intel]"), "the analysis");
  await shot("add-analysis-dark.png");
  await page(`(document.querySelector('dialog[aria-labelledby=add-title]').close(), true)`);
  await waitFor(async () => !(await ev("document.querySelector('dialog[aria-labelledby=add-title]').open")), "the Add dialog to close");
  await fill("[data-testid=add-form] input", ""); // the link bar keeps a link the dialog was given

  // ---- many at once: a numbered series ---------------------------------------------------------------------
  await click("[data-testid=batch-open]");
  await click("[data-testid=batch-tab-pattern]");
  await fill("[data-testid=batch-pattern]", "http://files.example.net/course/lesson-[01-24].mp4");
  await waitFor(async () => /lesson-24/.test((await ev("document.querySelector('[data-testid=batch-preview]')?.innerText ?? ''")) ?? ""), "the preview");
  await shot("batch-dark.png");
  await click("[data-testid=batch-cancel]");

  // ---- a video page ------------------------------------------------------------------------------------
  await click("[data-testid=qa-video]");
  await waitFor(() => has("[data-testid=media-url]"), "the video dialog");
  await fill("[data-testid=media-url]", VIDEO);
  await click("[data-testid=media-look]");
  await waitFor(async () => (await has("[data-testid=media-options]")) || (await has("[data-testid=media-error]")), "the qualities");
  const videoError = await ev("document.querySelector('[data-testid=media-error]')?.innerText ?? null");
  if (videoError) throw new Error(`the video dialog says: ${videoError}`);
  await fill("[data-testid=media-name]", "Sunset timelapse");
  await shot("video-dark.png");
  await click("[data-testid=media-cancel]");

  // ---- Arabic, right to left -----------------------------------------------------------------------------------
  await click("[data-testid=lang-ar]");
  await waitFor(async () => (await ev("document.documentElement.dir")) === "rtl", "Arabic");
  await page(`(document.querySelector('[data-testid=main]').scrollTop = 0, true)`);
  await shot("arabic-dark.png");
  await click("[data-testid=lang-en]");
  await waitFor(async () => (await ev("document.documentElement.dir")) === "ltr", "English again");

  // ---- Settings: speed (everything stopped first and the proxy removed: its address is in Settings) -----------
  await invoke("pause_queue", { request: { pauseRunning: true } });
  await waitFor(async () => (await invoke("list_downloads", { filter: {} })).ok.every((d) => d.status !== "downloading"), "every download paused", 20000);
  await invoke("set_network", { request: { proxyMode: "system" } });
  await click("[data-testid=nav-settings]");
  await page(`(() => { document.getElementById('set-speed').scrollIntoView(); return true; })()`);
  await shot("settings-dark.png");
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  proxy.close();
  finish();
}
