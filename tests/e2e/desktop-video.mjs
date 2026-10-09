// End-to-end test of the browser's "Download video" bar, in the real desktop app: a REAL native host
// hands over a video page (as the bar does), the app opens the video dialog with the qualities, the
// user picks one and the file arrives; DRM services are refused; closing the dialog answers "no";
// and a download uses up to 64 connections by default.
//
//   cargo build -p velox-integration-tests     (the stand-ins for yt-dlp and ffmpeg)
//   node tests/e2e/desktop-video.mjs [path-to-debug-velox-desktop.exe]
//
// yt-dlp and ffmpeg are stand-ins (no internet, no real YouTube); browser registration goes to a scratch
// registry key, so real browser settings are never touched.

import fs from "node:fs";
import os from "node:os";
import { spawn } from "node:child_process";
import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const HOST = path.join(path.dirname(APP), exe("velox-native-host"));
const EXE = process.platform === "win32" ? ".exe" : "";
const YTDLP = path.resolve(`target/debug/fake-ytdlp${EXE}`);
const FFMPEG = path.resolve(`target/debug/fake-ffmpeg${EXE}`);
const SHOTS = path.resolve("docs/screenshots");
const CHROME_ORIGIN = "chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/";
// A named pipe on Windows; elsewhere a socket file in the temp folder (never in the working folder).
const PIPE = process.platform === "win32" ? String.raw`\\.\pipe\velox-e2e-video-${process.pid}` : path.join(os.tmpdir(), `velox-e2e-video-${process.pid}.sock`);
const SCRATCH = String.raw`Software\VeloxFetchE2E-video-${process.pid}`;
const LOG = path.join(os.tmpdir(), "velox-fake-ytdlp.log");
const VID = `e2e${process.pid}x${Date.now() % 100000}`;

for (const f of [HOST, YTDLP, FFMPEG]) {
  if (!fs.existsSync(f)) {
    console.error(`missing ${f}: build it first (cargo build -p velox-native-host -p velox-integration-tests)`);
    process.exit(1);
  }
}

const files = await startFileServer(64 * 1024 * 1024);
const h = createHarness({
  port: 9353,
  appPath: APP,
  env: { VELOX_BRIDGE_ENDPOINT: PIPE, VELOX_NMH_SCRATCH_KEY: SCRATCH, VELOX_NO_LAUNCH: "1", VELOX_YTDLP: YTDLP, VELOX_FFMPEG: FFMPEG, VELOX_ORGANIZE: "0" },
});
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = async (sel) => {
  await waitFor(() => has(sel), `the element ${sel}`, 10000);
  return page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); e.click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const jobs = async () => (await invoke("list_media_jobs")).ok;
const pending = async () => (await invoke("list_pending_downloads")).ok;
const ytdlpCalls = () =>
  (fs.existsSync(LOG) ? fs.readFileSync(LOG, "utf8") : "").split(/\r?\n/).filter((l) => l.includes(VID)).map((l) => l.split("\u001f"));

// ---- the native host, driven like a browser drives it -----------------------------------------------
function startHost() {
  const child = spawn(HOST, [CHROME_ORIGIN], { env: { ...process.env, VELOX_DATA_DIR: h.dataDir, VELOX_BRIDGE_ENDPOINT: PIPE }, stdio: ["pipe", "pipe", "pipe"] });
  const host = { child, buf: Buffer.alloc(0), waiters: [] };
  child.stdout.on("data", (d) => {
    host.buf = Buffer.concat([host.buf, d]);
    while (host.buf.length >= 4 && host.buf.length >= 4 + host.buf.readUInt32LE(0)) {
      const len = host.buf.readUInt32LE(0);
      const body = host.buf.subarray(4, 4 + len);
      host.buf = host.buf.subarray(4 + len);
      host.waiters.shift()?.(JSON.parse(body.toString()));
    }
  });
  host.ask = (v, ms = 15000) =>
    new Promise((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("host did not answer")), ms);
      host.waiters.push((r) => (clearTimeout(t), resolve(r)));
      const body = Buffer.from(JSON.stringify(v));
      const head = Buffer.alloc(4);
      head.writeUInt32LE(body.length);
      child.stdin.write(Buffer.concat([head, body]));
    });
  return host;
}

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================== 64 connections by default
  const q = (await invoke("get_queue_state")).ok;
  check("each download may use up to 64 connections by default", q.maxConnections === 64, JSON.stringify(q.maxConnections));
  check("65 is refused (64 is the engine's ceiling)", !!(await invoke("set_queue_config", { request: { maxConnections: 65 } })).err);
  files.delayMs = 15; // ~4 MB/s per connection, like a real server: more connections make it faster
  const big = (await invoke("create_download", { request: { url: `${files.base}/big.bin` } })).ok;
  await waitFor(async () => (await invoke("get_download", { id: big.id })).ok?.status === "completed", "a 64 MB download", 60000);
  const peak = (await invoke("get_download_diagnostics", { id: big.id })).ok?.peakConnections ?? 0;
  check("a 64 MB file is fetched over many connections at once", peak >= 8, `peak ${peak}`);
  files.delayMs = 0;

  // ============================================================== a video page from the bar
  const host = startHost();
  const url = `https://www.youtube.com/watch?v=${VID}`;
  const r = await host.ask({ command: "download", url, source: "videoBar", media: "page", referrer: "https://www.youtube.com/", userAgent: "Mozilla/5.0 E2E", cookies: "SID=e2e-video-secret" });
  check("the app accepts the video and keeps it waiting for the user", r.ok === true && r.result?.id === 0, JSON.stringify(r));
  await waitFor(() => has("[data-testid=media-dialog]"), "the video dialog to open by itself");
  check("it is the video dialog, not the file confirmation", (await text("#media-title")) === "Download this video" && !(await has("[data-testid=pending-dialog]")));
  check("the link is the one the browser sent and cannot be edited", (await ev("document.querySelector('[data-testid=media-url]').value")) === url && (await ev("document.querySelector('[data-testid=media-url]').readOnly")) === true);
  await waitFor(() => has("[data-testid=media-options]"), "the qualities");
  const qualities = await ev(`[...document.querySelectorAll('[data-testid=media-quality] option')].map(o => o.value)`);
  check("the qualities come from the page, best first, plus audio only", JSON.stringify(qualities) === JSON.stringify(["h1080", "h720", "h360", "audio"]), JSON.stringify(qualities));
  check("it says this is a video page", /Video page/.test((await text("[data-testid=media-options]")) ?? ""));
  check("nothing was downloaded by looking", (await jobs()).length === 0);
  await h.page("document.documentElement.setAttribute('data-theme','dark')");
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "video-dialog-dark.png"), { width: 1000, height: 780 });
  await h.page("document.documentElement.removeAttribute('data-theme')");

  const types = await ev(`[...document.querySelectorAll('[data-testid=media-format] option')].map(o => o.value)`);
  check("the file type can be chosen, MP4 first and chosen by default, sound-only types too", JSON.stringify(types) === JSON.stringify(["mp4", "mkv", "webm", "mp3", "m4a", "opus"]) && (await ev("document.querySelector('[data-testid=media-format]').value")) === "mp4", JSON.stringify(types));
  await fill("[data-testid=media-quality]", "audio");
  const audioTypes = await ev(`[...document.querySelectorAll('[data-testid=media-format] option')].map(o => o.value)`);
  check("audio only offers sound types, MP3 by default", JSON.stringify(audioTypes) === JSON.stringify(["mp3", "m4a", "opus"]) && (await ev("document.querySelector('[data-testid=media-format]').value")) === "mp3", JSON.stringify(audioTypes));
  await fill("[data-testid=media-quality]", "h720");
  check("back to a video quality, MP4 is chosen again", (await ev("document.querySelector('[data-testid=media-format]').value")) === "mp4");
  await fill("[data-testid=media-name]", "Holiday clip");
  await click("[data-testid=media-start]");
  await waitFor(async () => !(await has("[data-testid=media-dialog]")), "the dialog to close");
  await waitFor(async () => (await jobs()).some((j) => j.status === "completed"), "the video to arrive", 30000);
  const job = (await jobs())[0];
  check("the job completes with one file", job.site === true && JSON.stringify(job.files) === '["Holiday clip.mp4"]', JSON.stringify(job));
  await waitFor(() => has(`[data-testid=media-${job.id}] [data-testid=media-open]`), "the finished video's buttons");
  check("a finished video offers Play, Open and Show in folder", (await has(`[data-testid=media-${job.id}] [data-testid=media-play]`)) && (await has(`[data-testid=media-${job.id}] [data-testid=media-show]`)));
  const hist = (await invoke("list_history", { limit: 20 })).ok ?? [];
  check("the finished video is in the history", hist.some((x) => x.filename === "Holiday clip.mp4"), JSON.stringify(hist.map((x) => x.filename)));
  const body = fs.readFileSync(path.join(h.root, "Holiday clip.mp4"), "utf8");
  check("the chosen quality was asked for (720p, video + audio joined)", body.startsWith("FAKEVIDEO bv*[height<=720]+ba"), body);
  check("the waiting request is answered", (await pending()).length === 0);
  const calls = ytdlpCalls();
  check("yt-dlp ran twice (look, download), without its configuration files, the link last after --", calls.length === 2 && calls.every((a) => a[0] === "--ignore-config" && a.at(-2) === "--" && a.at(-1) === url), JSON.stringify(calls.map((a) => a.slice(0, 2))));
  const dl = calls[1] ?? [];
  check("the download was asked for as .mp4 (never .mkv)", dl.includes("--remux-video") && dl[dl.indexOf("--merge-output-format") + 1] === "mp4" && !dl.join(" ").includes("mkv"), JSON.stringify(dl));
  check("yt-dlp got the browser's referrer and user agent but never its cookies", calls.every((a) => a.includes("--referer") && a.includes("Mozilla/5.0 E2E") && !a.join(" ").includes("e2e-video-secret")));
  check("no working folder is left behind", fs.readdirSync(h.root).every((n) => !n.startsWith(".velox-media")));

  // ============================================================== pause, resume, watch while downloading
  const slow = (await invoke("start_media", { request: { url: `https://www.youtube.com/watch?v=slow${VID}`, filename: "Long clip" } })).ok;
  check("a long video starts", !!slow?.id, JSON.stringify(slow));
  const card = `[data-testid=media-${slow.id}]`;
  await waitFor(async () => (await ev(`document.querySelector('${card}')?.dataset.status`)) === "downloading", "the long video to download");
  await waitFor(() => has(`${card} [data-testid=media-play]`), "Watch now to appear");
  check("a running video has Pause, Watch now and Show in folder", (await has(`${card} [data-testid=media-pause]`)) && (await has(`${card} [data-testid=media-show]`)));
  const m = (await invoke("get_metrics")).ok;
  check("the dashboard counts the running video as active", m.active >= 1, JSON.stringify(m));
  await click(`${card} [data-testid=media-play]`);
  await waitFor(() => has(`${card} [data-testid=media-player]`), "the player");
  const src = await ev(`document.querySelector('${card} [data-testid=media-player]').src`);
  // Linux: WebKitGTK cannot play from a custom protocol, so the app's private local address (this computer only, a secret).
  const viaApp = process.platform === "linux" ? /^http:\/\/127\.0\.0\.1:\d+\/[0-9a-f]{64}\/m\d+$/.test(src) : /velox-preview[^/]*\/+m\d+$/.test(src);
  check("the player reads the job through the app's own preview address, by id", viaApp, src);
  await click(`${card} [data-testid=media-pause]`);
  await waitFor(async () => (await ev(`document.querySelector('${card}')?.dataset.status`)) === "paused", "the video to pause");
  check("a paused video offers Resume and Cancel", (await has(`${card} [data-testid=media-resume]`)) && (await has(`${card} [data-testid=media-cancel-job]`)));
  check("the paused video keeps its pieces", fs.readdirSync(path.join(h.root)).some((n) => n.startsWith(".velox-media-")) || fs.readdirSync(h.root, { recursive: true }).some((n) => String(n).includes(".velox-media-")));
  await click(`${card} [data-testid=media-resume]`);
  await waitFor(async () => (await ev(`document.querySelector('${card}')?.dataset.status`)) === "downloading", "the video to resume");
  check("Resume continues the same video", true);
  await click(`${card} [data-testid=media-cancel-job]`);
  await waitFor(async () => (await ev(`document.querySelector('${card}')?.dataset.status`)) === "cancelled", "the video to be cancelled");
  check("Cancel removes the pieces", !fs.readdirSync(h.root, { recursive: true }).some((n) => String(n).includes(".velox-media-")));
  await click(`${card} [data-testid=media-clear-job]`);
  await waitFor(async () => (await jobs()).length === 1, "the cancelled video to leave the list");

  // ============================================================== delete a finished video from the disk (asked first)
  const doneCard = `[data-testid=media-${job.id}]`;
  check("a finished video offers Remove and delete file", await has(`${doneCard} [data-testid=media-delete-job]`));
  await click(`${doneCard} [data-testid=media-delete-job]`);
  await waitFor(() => has("[data-testid=delete-dialog]"), "the delete question");
  await click("[data-testid=confirm-cancel]");
  await sleep(300);
  check("Cancel keeps the video on disk and in the list", fs.existsSync(path.join(h.root, "Holiday clip.mp4")) && (await jobs()).length === 1);
  await click(`${doneCard} [data-testid=media-delete-job]`);
  await click("[data-testid=confirm-delete]");
  await waitFor(async () => (await jobs()).length === 0, "the deleted video to leave the list");
  check("Delete from disk removes the finished video's file", !fs.existsSync(path.join(h.root, "Holiday clip.mp4")), fs.readdirSync(h.root).join(","));

  // ============================================================== DRM services
  for (const drm of ["https://www.netflix.com/watch/81234567", "https://shahid.mbc.net/ar/player/episodes/x", "https://www.stctv.com/watch/1"]) {
    const d = await host.ask({ command: "download", url: drm, source: "videoBar", media: "page" });
    check(`refused with a reason: ${new URL(drm).hostname}`, d.ok === false && d.error?.code === "invalid_input" && /DRM/.test(d.error.message), JSON.stringify(d));
  }
  check("nothing waits and no dialog opened", (await pending()).length === 0 && !(await has("[data-testid=media-dialog]")));
  const typed = await invoke("analyze_media", { url: "https://www.netflix.com/watch/1" });
  check("a DRM link typed in the app is refused too", /DRM/.test(typed.err?.message ?? ""), JSON.stringify(typed));

  // ============================================================== closing the dialog answers "no"
  const s = await host.ask({ command: "download", url: `${files.base}/not-a-stream`, source: "videoBar", media: "stream" });
  check("a stream hint opens the video dialog too", s.ok === true);
  await waitFor(() => has("[data-testid=media-dialog]"), "the dialog");
  await waitFor(() => has("[data-testid=media-error]"), "the analysis to fail");
  const why = (await text("[data-testid=media-error]")) ?? "";
  check("a link that is not a stream is explained, not downloaded", /neither an HLS|larger than 4 MiB/.test(why) && (await jobs()).length === 0, why);
  await click("[data-testid=media-cancel]");
  await waitFor(async () => !(await has("[data-testid=media-dialog]")), "the dialog to close");
  await waitFor(async () => (await pending()).length === 0, "the request to be dismissed");
  check("Cancel dismisses the browser's request", true);

  // ============================================================== a YouTube link typed in the app
  await fill("[data-testid=url-input]", `https://youtu.be/${VID}typed`);
  await click("[data-testid=add-button]");
  await waitFor(() => has("[data-testid=media-options]"), "the qualities for a typed YouTube link");
  check("a YouTube link typed in the address bar opens the video dialog", (await ev("document.querySelector('[data-testid=media-url]').readOnly")) === false);
  await click("[data-testid=media-cancel]");
  check("no ordinary download was made from it", (await invoke("list_downloads", { filter: {} })).ok.length === 1);

  // ---- Settings: the browser's video bar switch and the video tools panel
  await click("[data-testid=nav-settings]");
  await waitFor(() => has("[data-testid=video-bar-toggle]"), "the video bar switch");
  check("the Download video bar is on by default", (await ev(`document.querySelector('[data-testid=video-bar-toggle]').checked`)) === true &&
    (await invoke("get_browser_integration")).ok.videoBar === true);
  await click("[data-testid=video-bar-toggle]");
  await waitFor(async () => (await invoke("get_browser_integration")).ok.videoBar === false, "the switch to be saved");
  check("it can be turned off in Settings (the extension follows it through its ping)", true);
  // the switch shows the saved state before it is clicked again (a click during the save was lost)
  await waitFor(async () => (await ev(`document.querySelector('[data-testid=video-bar-toggle]').checked`)) === false && !(await ev(`document.querySelector('[data-testid=video-bar-toggle]').disabled`)), "the switch to show off");
  await click("[data-testid=video-bar-toggle]");
  await waitFor(async () => (await invoke("get_browser_integration")).ok.videoBar === true, "the switch back on");
  await waitFor(() => has("[data-testid=tool-ytdlp]"), "the video tools panel");
  const tools = (await invoke("get_video_tools")).ok;
  check("Settings shows the video tools (yt-dlp, Deno, ffmpeg)", !!tools && "ytdlp" in tools && "deno" in tools && "ffmpeg" in tools && (await has("[data-testid=tool-ffmpeg]")), JSON.stringify(tools));
  check("a debug build fetches nothing by itself (no source in tests)", !tools.deno.canFetch && !tools.deno.fetching);

  check("no page errors", ((await ev("window.__errs")) ?? []).length === 0, JSON.stringify(await ev("window.__errs")));
  host.child.stdin.end();
} catch (e) {
  console.error(e);
  fail();
} finally {
  await h.cleanup();
  files.close();
}
finish();
