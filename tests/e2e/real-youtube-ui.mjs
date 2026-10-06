// By hand, needs the internet: a real YouTube video through the real app window, with the real yt-dlp / Deno /
// ffmpeg. Checks what the user sees: the dashboard's speed and active cards move, "Watch now" really plays the
// part downloaded so far, Pause / Resume continue (not restart), and the finished file lands in History.
//   node tests/e2e/real-youtube-ui.mjs [app] <folder with yt-dlp, deno, ffmpeg>
// The tools folder is copied into the test's own data folder (nothing is downloaded for the tools).
import fs from "node:fs";
import path from "node:path";
import { check, createHarness, exe, finish, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const TOOLS = process.argv[3];
const LINK = process.env.VELOX_TEST_REAL_YOUTUBE ?? "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
if (!TOOLS || !fs.existsSync(TOOLS)) {
  console.log("SKIP  give the folder with yt-dlp, deno and ffmpeg");
  process.exit(0);
}

const h = createHarness({ port: 9341, appPath: APP, env: { VELOX_TOOLS: "official" } });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = async (sel) => {
  await waitFor(() => has(sel), sel);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const job = async (id) => ((await invoke("list_media_jobs")).ok ?? []).find((j) => j.id === id);

try {
  fs.mkdirSync(path.join(h.dataDir, "tools"), { recursive: true });
  for (const f of fs.readdirSync(TOOLS)) fs.copyFileSync(path.join(TOOLS, f), path.join(h.dataDir, "tools", f));
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText"))?.includes("VeloxFetch"), "UI");
  const a = await invoke("analyze_media", { url: LINK });
  check("the page is read", !!a.ok?.streams?.length, JSON.stringify(a.err));
  const q = a.ok.streams.find((s) => s.id === "h1080") ?? a.ok.streams[0];
  const started = (await invoke("start_media", { request: { url: LINK, stream: q.id, format: "mp4" } })).ok;
  const card = `[data-testid=media-${started.id}]`;
  await waitFor(async () => (await job(started.id))?.status === "downloading" && (await job(started.id)).bytes > 2_000_000, "2 MB downloaded", 120000);

  // The dashboard follows the video.
  const m = (await invoke("get_metrics")).ok;
  check("dashboard: the video is active and has a speed", m.active >= 1 && m.speedBps > 0, JSON.stringify(m));
  check("dashboard: network bytes count it", m.sessionDownloaded > 1_000_000, String(m.sessionDownloaded));
  const j1 = await job(started.id);
  check("the card shows a speed", j1.speedBps > 0, JSON.stringify(j1));

  // Watch now: the player really plays the part downloaded so far.
  await waitFor(() => has(`${card} [data-testid=media-play]`), "Watch now", 60000);
  await click(`${card} [data-testid=media-play]`);
  const playing = await waitFor(
    async () => {
      const s = await ev(`(() => { const v = document.querySelector('${card} video'); return v ? { t: v.currentTime, r: v.readyState, e: v.error && v.error.code, w: v.videoWidth } : null; })()`);
      return s && s.t > 1 && s.r >= 2 ? s : s?.e ? s : null;
    },
    "the video to play",
    60000,
  ).catch((e) => ({ timeout: String(e) }));
  check("Watch now plays the downloaded part (picture moving)", playing.t > 1 && playing.w > 0 && !playing.e, JSON.stringify(playing));
  await h.screenshot(path.resolve("docs/screenshots/video-watch-now.png"), { width: 1000, height: 900 });
  await click(`${card} [data-testid=media-play]`); // hide the player

  // Pause, then Resume: it continues from where it was.
  await click(`${card} [data-testid=media-pause]`);
  await waitFor(async () => (await job(started.id))?.status === "paused", "paused", 30000);
  const atPause = (await job(started.id)).percent ?? 0;
  check("paused, Resume offered", await has(`${card} [data-testid=media-resume]`));
  await sleep(1500);
  await click(`${card} [data-testid=media-resume]`);
  await waitFor(async () => (await job(started.id))?.status === "downloading" && (await job(started.id)).percent > 0, "running again", 120000);
  const afterResume = (await job(started.id)).percent ?? 0;
  check("Resume continued (did not start over)", afterResume >= atPause - 1, `${atPause}% -> ${afterResume}%`);

  const done = await waitFor(async () => {
    const j = await job(started.id);
    return j && !["preparing", "downloading", "combining"].includes(j.status) ? j : null;
  }, "the video to finish", 900000);
  check("the video completes as .mp4", done.status === "completed" && done.files[0]?.endsWith(".mp4"), JSON.stringify(done));
  const hist = (await invoke("list_history", { limit: 10 })).ok ?? [];
  check("it is in the history", hist.some((x) => x.filename === done.files[0]));
  check("a finished video offers Play / Open / Show in folder", (await has(`${card} [data-testid=media-open]`)) && (await has(`${card} [data-testid=media-show]`)));
} catch (e) {
  check(`unexpected error: ${e}`, false);
} finally {
  await h.cleanup();
}
finish();
