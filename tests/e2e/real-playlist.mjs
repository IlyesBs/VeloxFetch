// By hand, needs the internet: a real YouTube playlist through the real app window, with the real yt-dlp / Deno /
// ffmpeg. A video link that is part of the playlist is opened in the video dialog, "Download the whole playlist"
// lists the real videos, three are chosen (360p MP4) and downloaded one job each into the playlist's folder,
// numbered in its order; each file is checked with ffprobe (length, picture, sound).
//   node tests/e2e/real-playlist.mjs [app] <folder with yt-dlp, deno, ffmpeg>
// The tools folder is copied into the test's own data folder (nothing is downloaded for the tools).
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { check, createHarness, exe, fail, finish, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const TOOLS = process.argv[3];
// Blender's own channel, "BLENDERHEADS" (13 public videos, 13 to 31 minutes each).
const LIST = process.env.VELOX_TEST_REAL_PLAYLIST ?? "PLa1F2ddGya_8I8QCMCKlQUOQpgOga2nZ-";
const WATCH = `https://www.youtube.com/watch?v=6d5rI5y8hqQ&list=${LIST}&index=3`;
const PICK = [2, 3, 9];
if (!TOOLS || !fs.existsSync(TOOLS)) {
  console.log("SKIP  give the folder with yt-dlp, deno and ffmpeg");
  process.exit(0);
}

const h = createHarness({ port: 9412, appPath: APP, env: { VELOX_TOOLS: "official", VELOX_ORGANIZE: "0" } });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = async (sel, ms = 15000) => {
  await waitFor(() => has(sel), sel, ms);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const jobs = async () => (await invoke("list_media_jobs")).ok ?? [];
// ffprobe next to the tools when there is one, else the system's.
const FFPROBE = TOOLS && fs.existsSync(path.join(TOOLS, exe("ffprobe"))) ? path.join(TOOLS, exe("ffprobe")) : "ffprobe";
const probe = (f) => JSON.parse(execFileSync(FFPROBE, ["-v", "error", "-show_entries", "format=duration:stream=codec_type,height", "-of", "json", f], { encoding: "utf8" }));

try {
  fs.mkdirSync(path.join(h.dataDir, "tools"), { recursive: true });
  for (const f of fs.readdirSync(TOOLS)) fs.copyFileSync(path.join(TOOLS, f), path.join(h.dataDir, "tools", f));
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  await click("[data-testid=nav-downloads]");
  await click("[data-testid=stream-open]");
  await waitFor(() => has("[data-testid=media-dialog]"), "the video dialog");
  await fill("[data-testid=media-url]", WATCH);
  await click("[data-testid=media-look]");
  await waitFor(() => has("[data-testid=media-part-of-playlist]"), "the real video and the playlist offer", 180000);
  check("a real video of a playlist: the dialog offers the whole playlist", /part of a playlist/.test(await text("[data-testid=media-part-of-playlist]")));
  const t0 = Date.now();
  await click("[data-testid=media-whole-playlist]");
  await waitFor(() => has("[data-testid=media-playlist-header]"), "the real playlist", 180000);
  const secs = ((Date.now() - t0) / 1000).toFixed(1);
  const title = await text("[data-testid=media-playlist-title]");
  const rows = await ev(`[...document.querySelectorAll('[data-testid=media-playlist-items] label')].map(l => l.innerText.replace(/\\s+/g, ' ').trim())`);
  console.log(`      playlist "${title}", ${rows.length} videos, read in ${secs} s`);
  check("the real playlist is listed with its name and every video", title === "BLENDERHEADS" && rows.length === 13 && rows.every((r, i) => r.startsWith(`${i + 1} `)), JSON.stringify(rows.slice(0, 3)));
  check("with each video's real title and length", rows[2].includes("The end of an era") && rows[2].endsWith("13:00") && rows[8].includes("BLENDERHEADS - Ep. 05"), `${rows[2]} | ${rows[8]}`);
  await click("[data-testid=media-pick-all]"); // none
  for (const i of PICK) await click(`[data-testid=media-item-${i}] input`);
  await fill("[data-testid=media-quality]", "h360");
  await fill("[data-testid=media-format]", "mp4");
  check("three chosen", (await text("[data-testid=media-start]")) === "Download videos (3)");
  await h.screenshot(path.resolve("docs/screenshots/playlist-real.png"), { width: 1000, height: 860 });
  await click("[data-testid=media-start]");
  await waitFor(async () => (await jobs()).length === 3, "three video jobs");

  const t1 = Date.now();
  await waitFor(async () => (await jobs()).every((j) => !["queued", "preparing", "downloading", "combining"].includes(j.status)), "the three videos", 30 * 60 * 1000);
  const done = (await jobs()).sort((a, b) => a.playlist.index - b.playlist.index);
  console.log(`      downloaded in ${((Date.now() - t1) / 1000).toFixed(0)} s: ${done.map((j) => `${j.status} ${j.files.join(",")}`).join(" | ")}`);
  check("all three completed", done.every((j) => j.status === "completed"), JSON.stringify(done.map((j) => [j.status, j.error])));
  check("each knows its place in the playlist", JSON.stringify(done.map((j) => j.playlist.index)) === JSON.stringify(PICK));
  const folder = path.join(h.root, "BLENDERHEADS");
  const files = fs.existsSync(folder) ? fs.readdirSync(folder).sort() : [];
  check("the files are in the playlist's folder, numbered in its order", files.length === 3 && files[0].startsWith("02 - The personality test") && files[1].startsWith("03 - The end of an era") && files[2].startsWith("09 - BLENDERHEADS - Ep_ 05") && files.every((f) => f.endsWith(".mp4")), JSON.stringify(files));
  const want = { 2: 1000, 3: 780, 9: 984 };
  for (const [k, f] of files.entries()) {
    const p = probe(path.join(folder, f));
    const dur = Number(p.format.duration);
    const v = p.streams.find((s) => s.codec_type === "video");
    const a = p.streams.some((s) => s.codec_type === "audio");
    check(`${f}: a real video of the right length, 360p, with sound`, Math.abs(dur - want[PICK[k]]) < 3 && v?.height === 360 && a, `${dur} s, ${v?.height}p, sound ${a}, ${(fs.statSync(path.join(folder, f)).size / 1048576).toFixed(1)} MB`);
  }
  check("no work folder is left", !files.some((f) => f.startsWith(".velox-media-")));
  const hist = ((await invoke("list_history", { limit: 20 })).ok ?? []).map((x) => x.filename);
  check("they are in the history", files.every((f) => hist.includes(f)), JSON.stringify(hist));
  check("no script errors in the window", (await ev("window.__errs.length")) === 0, JSON.stringify(await ev("window.__errs")));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  await sleep(200);
  await h.cleanup();
}
finish();
