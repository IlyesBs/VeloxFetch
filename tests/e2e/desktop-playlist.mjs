// End-to-end test of playlist downloads in the real desktop app (media-engine `playlist_from_json`,
// manager `start_playlist` and its queue, MediaDialog's playlist mode, the job list's playlist chip).
//
//  A. From the browser: a REAL native host hands over a video that is part of a playlist (`watch?v=...&list=...`),
//     as the extension's bar does. The video dialog offers the whole playlist; the playlist shows its name, its
//     videos (an unavailable one cannot be chosen), one quality and file type for all; the chosen videos download
//     into a folder named after the playlist, numbered in its order; the browser's request is answered.
//  B. A long playlist typed in the dialog: three videos run at once, the others say "Waiting its turn" and can be
//     set aside or cancelled; a finished one lets the next start.
//  C. Hostile requests: no link can be given, only positions of the playlist that was read.
//  D. Every language, with long titles: the playlist dialog fits, nothing is cut.
//
//   cargo build -p velox-integration-tests     (the stand-ins for yt-dlp and ffmpeg)
//   node tests/e2e/desktop-playlist.mjs [path-to-debug-velox-desktop]
//
// yt-dlp and ffmpeg are stand-ins (no internet); browser registration goes to a scratch place.

import fs from "node:fs";
import os from "node:os";
import { spawn } from "node:child_process";
import { check, createHarness, exe, fail, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const HOST = path.join(path.dirname(APP), exe("velox-native-host"));
const EXE = process.platform === "win32" ? ".exe" : "";
const YTDLP = path.resolve(`target/debug/fake-ytdlp${EXE}`);
const FFMPEG = path.resolve(`target/debug/fake-ffmpeg${EXE}`);
const SHOTS = path.resolve("docs/screenshots");
const CHROME_ORIGIN = "chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/";
const PIPE = process.platform === "win32" ? String.raw`\\.\pipe\velox-e2e-playlist-${process.pid}` : path.join(os.tmpdir(), `velox-e2e-playlist-${process.pid}.sock`);
const SCRATCH = String.raw`Software\VeloxFetchE2E-playlist-${process.pid}`;
const LOG = path.join(os.tmpdir(), "velox-fake-ytdlp.log");
const VID = `e2epl${process.pid}x${Date.now() % 100000}`;
const LIST = `PL${VID}`;
const LANGS = ["en", "fr", "ar", "es", "pt-br", "de", "ru", "tr"];

for (const f of [HOST, YTDLP, FFMPEG]) {
  if (!fs.existsSync(f)) {
    console.error(`missing ${f}: build it first (cargo build -p velox-native-host -p velox-integration-tests)`);
    process.exit(1);
  }
}

const h = createHarness({
  port: 9411,
  appPath: APP,
  env: { VELOX_BRIDGE_ENDPOINT: PIPE, VELOX_NMH_SCRATCH_KEY: SCRATCH, VELOX_NMH_SCRATCH_HOME: path.join(os.tmpdir(), `velox-e2e-playlist-home-${process.pid}`), VELOX_NO_LAUNCH: "1", VELOX_YTDLP: YTDLP, VELOX_FFMPEG: FFMPEG, VELOX_ORGANIZE: "0" },
});
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = async (sel) => {
  await waitFor(() => has(sel), `the element ${sel}`, 10000);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const jobs = async () => (await invoke("list_media_jobs")).ok;
const pending = async () => (await invoke("list_pending_downloads")).ok;
const calls = (tag) => (fs.existsSync(LOG) ? fs.readFileSync(LOG, "utf8") : "").split(/\r?\n/).filter((l) => l.includes(tag)).map((l) => l.split("\u001f"));
const options = (sel) => ev(`[...document.querySelectorAll(${JSON.stringify(sel + " option")})].map(o => o.value)`);
// Elements clipped or sticking out of the window.
const cut = (sel) =>
  ev(`[...document.querySelectorAll(${JSON.stringify(sel)})].filter((e) => e.offsetParent).filter((e) => {
    const r = e.getBoundingClientRect();
    const clipped = [...e.querySelectorAll('button, label > span, p, h2')].some((x) => x.offsetParent && x.scrollWidth > x.clientWidth + 1 && !x.classList.contains('truncate') && getComputedStyle(x).overflow !== 'visible');
    // A text box scrolls its own long link by design.
    return clipped || r.right > window.innerWidth + 1 || r.left < -1 || (e.tagName !== 'INPUT' && e.scrollWidth > e.clientWidth + 1); }).map((e) => (e.getAttribute('data-testid') ?? '') + ': ' + e.innerText.slice(0, 80))`);

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

let host = null;
try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================== A. a video of a playlist, from the browser
  host = startHost();
  const watch = `https://www.youtube.com/watch?v=${VID}v&list=${LIST}&index=2`;
  const r = await host.ask({ command: "download", url: watch, source: "videoBar", media: "page", referrer: "https://www.youtube.com/", userAgent: "Mozilla/5.0 E2E", cookies: "SID=e2e-playlist-secret" });
  check("the app accepts the video and keeps it waiting for the user", r.ok === true, JSON.stringify(r));
  await waitFor(() => has("[data-testid=media-options]"), "the video dialog with the video's qualities");
  check("the video itself is offered first", JSON.stringify(await options("[data-testid=media-quality]")) === JSON.stringify(["h1080", "h720", "h360", "audio"]));
  await waitFor(() => has("[data-testid=media-part-of-playlist]"), "the playlist offer");
  check("the dialog says the video is part of a playlist and offers it all", /part of a playlist/.test(await text("[data-testid=media-part-of-playlist]")) && /Download the whole playlist/.test(await text("[data-testid=media-whole-playlist]")));
  await click("[data-testid=media-whole-playlist]");
  await waitFor(() => has("[data-testid=media-playlist-header]"), "the playlist");
  check("the dialog's title says it is a playlist", (await text("#media-title")) === "Download a playlist");
  check("the playlist's name and size are shown up front", (await text("[data-testid=media-playlist-title]")) === "My list_ best_of" && /Videos: 5/.test(await text("[data-testid=media-playlist-count]")), await text("[data-testid=media-playlist-header]"));
  const rows = await ev(`[...document.querySelectorAll('[data-testid=media-playlist-items] label')].map(l => [l.querySelector('input').checked, l.querySelector('input').disabled, l.innerText.replace(/\\s+/g, ' ').trim()])`);
  check("every video is listed in order, all chosen, with its length", rows.length === 5 && rows.slice(0, 4).every((x, i) => x[0] && !x[1] && x[2].startsWith(`${i + 1} Clip ${i + 1}: part/${i + 1}`)) && rows[0][2].endsWith("0:10"), JSON.stringify(rows));
  check("a private video is shown but cannot be chosen", rows[4][1] === true && rows[4][0] === false && /Unavailable/.test(rows[4][2]), JSON.stringify(rows[4]));
  check("the link to another host is not listed", !(await text("[data-testid=media-playlist-items]")).includes("local"));
  check("4 of the 4 available are chosen", (await text("[data-testid=media-picked]")) === "Chosen: 4 of 4");
  check("one quality for all, from best to audio only", JSON.stringify(await options("[data-testid=media-quality]")) === JSON.stringify(["best", "h2160", "h1440", "h1080", "h720", "h480", "h360", "audio"]));
  check("the file type can be chosen, sound-only types too", JSON.stringify(await options("[data-testid=media-format]")) === JSON.stringify(["mp4", "mkv", "webm", "mp3", "m4a", "opus"]));
  check("the button says how many videos", (await text("[data-testid=media-start]")) === "Download videos (4)");
  check("the folder is named after the playlist unless the user says otherwise", (await ev("document.querySelector('[data-testid=media-subdir]').placeholder")) === "My list_ best_of");
  check("'Only this video' goes back to the one video", await has("[data-testid=media-only-this]"));
  check("the playlist was read once, from its own page, with the browser's headers, never its cookies", (() => {
    const c = calls(LIST).filter((x) => x.some((a) => a.includes("playlist?list=")));
    return c.length === 1 && c[0].at(-1) === `https://www.youtube.com/playlist?list=${LIST}` && c[0].includes("--flat-playlist") && c[0].includes("Mozilla/5.0 E2E") && !c[0].some((a) => a.includes("e2e-playlist-secret"));
  })());
  check("nothing was downloaded by looking", (await jobs()).length === 0);
  await click("[data-testid=media-pick-all]");
  check("'All' clears every box", (await text("[data-testid=media-picked]")) === "Chosen: 0 of 4" && (await ev("document.querySelector('[data-testid=media-start]').disabled")) === true);
  await click("[data-testid=media-pick-all]");
  await click("[data-testid=media-item-3] input");
  check("one video left out: 3 chosen", (await text("[data-testid=media-picked]")) === "Chosen: 3 of 4" && (await text("[data-testid=media-start]")) === "Download videos (3)");
  await fill("[data-testid=media-quality]", "h720");
  await page("document.querySelector('[data-testid=media-playlist-items]').scrollTop = 0");
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "playlist-dialog.png"), { width: 1000, height: 860 });
  await h.page("document.documentElement.setAttribute('data-theme','dark')");
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "playlist-dialog-dark.png"), { width: 1000, height: 860 });
  await h.page("document.documentElement.removeAttribute('data-theme')");

  await click("[data-testid=media-start]");
  await waitFor(async () => !(await has("[data-testid=media-dialog]")), "the dialog to close");
  await waitFor(async () => (await jobs()).length === 3 && (await jobs()).every((j) => j.status === "completed"), "the three videos", 30000);
  const done = (await jobs()).sort((a, b) => a.playlist.index - b.playlist.index);
  check("each video knows its playlist and position", JSON.stringify(done.map((j) => [j.playlist.title, j.playlist.index, j.playlist.count])) === JSON.stringify([["My list_ best_of", 1, 3], ["My list_ best_of", 2, 3], ["My list_ best_of", 4, 3]]));
  const folder = path.join(h.root, "My list_ best_of");
  const saved = fs.existsSync(folder) ? fs.readdirSync(folder).sort() : [];
  check("the videos are in the playlist's folder, numbered in its order", JSON.stringify(saved) === JSON.stringify(["01 - Clip 1_ part_1.mp4", "02 - Clip 2_ part_2.mp4", "04 - Clip 4_ part_4.mp4"]), JSON.stringify(saved));
  check("the chosen quality was asked for", fs.readFileSync(path.join(folder, "01 - Clip 1_ part_1.mp4"), "utf8").startsWith("FAKEVIDEO bv*[height<=720]+ba"));
  check("the browser's request is answered", (await pending()).length === 0);
  await waitFor(() => has(`[data-testid=media-${done[0].id}] [data-testid=media-playlist]`), "the playlist chip");
  const chip = (await text(`[data-testid=media-${done[0].id}] [data-testid=media-playlist]`))?.trim(); // WebKitGTK ends it with a line break
  check("the list shows which playlist each video comes from", chip === "My list_ best_of · no. 1", JSON.stringify(chip));
  const dl = calls(LIST).filter((x) => !x.includes("--dump-single-json"));
  check("each video was downloaded by its own link, never the playlist's, never with cookies", dl.length === 3 && dl.every((x) => x.at(-2) === "--" && x.at(-1).startsWith(`https://www.youtube.com/watch?v=${LIST}-`) && !x.some((a) => a.includes("secret"))));
  for (const j of done) await invoke("clear_media_job", { id: j.id });

  // ============================================================== B. a long playlist: a few at a time
  const SLOW = `PLslow${VID}`;
  await click("[data-testid=nav-downloads]");
    await click("[data-testid=stream-open]");
  await waitFor(() => has("[data-testid=media-dialog]"), "the video dialog");
  await fill("[data-testid=media-url]", `https://www.youtube.com/playlist?list=${SLOW}`);
  await click("[data-testid=media-look]");
  await waitFor(() => has("[data-testid=media-playlist-header]"), "the typed playlist");
  check("a playlist link typed in the dialog lists its videos directly", (await text("[data-testid=media-picked]")) === "Chosen: 5 of 5" && !(await has("[data-testid=media-only-this]")));
  await click("[data-testid=media-start]");
  await waitFor(async () => (await jobs()).length === 5, "five video jobs");
  const by = async () => (await jobs()).sort((a, b) => a.playlist.index - b.playlist.index);
  await waitFor(async () => (await by()).slice(0, 3).every((j) => ["preparing", "downloading"].includes(j.status)), "three to run");
  let list = await by();
  check("three run at once, the others wait their turn", list.slice(3).every((j) => j.status === "queued"), JSON.stringify(list.map((j) => j.status)));
  await waitFor(() => has(`[data-testid=media-${list[3].id}][data-status=queued]`), "the waiting row");
  check("a waiting video says so and can be paused or cancelled", (await text(`[data-testid=media-${list[3].id}] [data-testid=media-status]`)) === "Waiting its turn" && (await has(`[data-testid=media-${list[3].id}] [data-testid=media-pause]`)) && (await has(`[data-testid=media-${list[3].id}] [data-testid=media-cancel-job]`)));
  await page(`document.querySelector('[data-testid=media-${list[3].id}]').scrollIntoView({ block: 'center' })`);
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "playlist-jobs.png"), { width: 1200, height: 860 });
  await click(`[data-testid=media-${list[3].id}] [data-testid=media-pause]`);
  await waitFor(async () => (await by())[3].status === "paused", "the waiting one set aside");
  await invoke("cancel_media_job", { id: list[0].id });
  await waitFor(async () => ["preparing", "downloading"].includes((await by())[4].status), "the next one to take the free place");
  check("a free place goes to the next one in line; the one set aside stays aside", (await by())[3].status === "paused");
  for (let i = 0; i < 3; i++) {
    for (const j of await jobs()) await invoke("cancel_media_job", { id: j.id });
    await sleep(150);
  }
  await waitFor(async () => (await jobs()).every((j) => !["queued", "preparing", "downloading", "combining"].includes(j.status)), "everything to stop", 20000);
  check("nothing is left running or waiting", true);
  for (const j of await jobs()) await invoke("clear_media_job", { id: j.id });

  // ============================================================== C. hostile requests
  const read = (await invoke("analyze_media", { url: `https://www.youtube.com/playlist?list=${LIST}`, pending: null, playlist: null })).ok;
  check("reading gives a key, never the videos' links", !!read.playlistKey && !JSON.stringify(read).includes("watch?v="), JSON.stringify(read).slice(0, 200));
  const bad = [
    { key: read.playlistKey, items: [1], url: "https://evil.test/x.exe" },
    { key: read.playlistKey, items: [1], subdir: "../../outside" },
    { key: read.playlistKey, items: [99] },
    { key: "p999999", items: [1] },
    { key: read.playlistKey, items: [5] },
    { key: read.playlistKey, items: [1], quality: "-f bestvideo --exec calc" },
  ];
  for (const request of bad) {
    const res = await invoke("start_playlist", { request });
    check(`refused: ${JSON.stringify(request).slice(0, 90)}`, !!res.err && !res.ok);
  }
  check("not part of a playlist: refused", !!(await invoke("analyze_media", { url: `https://www.youtube.com/watch?v=${VID}z`, pending: null, playlist: true })).err);
  check("nothing was started by the refused requests", (await jobs()).length === 0 && !fs.existsSync(path.join(h.root, "..", "outside")));

  // ============================================================== D. every language fits
  for (const lang of LANGS) {
    await invoke("set_language", { lang }).catch(() => {});
    await click("[data-testid=nav-settings]");
    await waitFor(() => has("[data-testid=settings-lang]"), "language drop-down");
    await fill("[data-testid=settings-lang]", lang);
    await waitFor(async () => (await ev("document.documentElement.lang")) === lang, `the ${lang} interface`);
    await click("[data-testid=nav-downloads]");
    await click("[data-testid=stream-open]");
    await waitFor(() => has("[data-testid=media-dialog]"), "the video dialog");
    // Long titles, like real ones: the list must not push the dialog sideways.
    await fill("[data-testid=media-url]", `https://www.youtube.com/watch?v=${VID}v&list=PLlong${VID}`);
    await click("[data-testid=media-look]");
    await click("[data-testid=media-whole-playlist]");
    await waitFor(() => has("[data-testid=media-playlist-header]"), `the playlist in ${lang}`);
    const bad = await cut("[data-testid=media-dialog], [data-testid=media-dialog] *[data-testid]");
    check(`${lang}: the playlist dialog fits, nothing is cut`, bad.length === 0, JSON.stringify(bad));
    if (lang !== "en") check(`${lang}: no English left in the playlist parts`, !/Download the whole|Only this video|Chosen:|Download videos|part of a playlist/.test(await text("[data-testid=media-dialog]")));
    await click("[data-testid=media-cancel]");
    await waitFor(async () => !(await has("[data-testid=media-dialog]")), "the dialog to close");
  }
  await fill("[data-testid=settings-lang]", "en").catch(() => {});
  check("no script errors in the window", (await ev("window.__errs.length")) === 0, JSON.stringify(await ev("window.__errs")));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  host?.child.kill();
  await h.cleanup();
}
finish();
