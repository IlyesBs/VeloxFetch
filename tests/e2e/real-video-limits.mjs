// By hand, needs the internet: real YouTube videos through the real app, with the real yt-dlp / Deno / ffmpeg.
// Checks the video pipeline end to end:
//  * only the sound as MP3: the bar keeps moving while ffmpeg converts (no "99 %" for minutes), and the file is a
//    real MP3;
//  * a video as MP4: the bar only goes forward, and the file has both a picture and sound (ffprobe);
//  * the user's global speed limit and the Smart throttle slow a running video download down, and lifting them
//    brings the speed back (yt-dlp is restarted with the new limit and continues its partial file).
//   node tests/e2e/real-video-limits.mjs [app] <folder with yt-dlp, deno, ffmpeg, ffprobe>
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { check, createHarness, exe, finish, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const TOOLS = process.argv[3];
const LINK = process.env.VELOX_TEST_REAL_YOUTUBE ?? "https://www.youtube.com/watch?v=aqz-KE-bpKQ"; // Big Buck Bunny, 10 min
if (!TOOLS || !fs.existsSync(TOOLS)) {
  console.log("SKIP  give the folder with yt-dlp, deno, ffmpeg and ffprobe");
  process.exit(0);
}
const EXE = process.platform === "win32" ? ".exe" : "";
const FFPROBE = path.join(TOOLS, `ffprobe${EXE}`);
const KB = 1024;

const h = createHarness({ port: 9343, appPath: APP, env: { VELOX_TOOLS: "official" } });
const { invoke } = h;
const job = async (id) => ((await invoke("list_media_jobs")).ok ?? []).find((j) => j.id === id);
const ended = (j) => j && !["preparing", "downloading", "combining"].includes(j.status);

/** Follow a job until it ends: every percent seen, by status. */
async function follow(id, timeout) {
  const seen = [];
  const t0 = Date.now();
  for (;;) {
    const j = await job(id);
    if (j) seen.push({ s: j.status, p: j.percent ?? 0, t: Date.now() - t0 });
    if (ended(j)) return { j, seen };
    if (Date.now() - t0 > timeout) throw new Error(`job ${id} did not end in ${timeout} ms`);
    await sleep(250);
  }
}

/** Streams in a file, by ffprobe: e.g. ["video", "audio"]. */
function streams(file) {
  const r = spawnSync(FFPROBE, ["-v", "error", "-show_entries", "stream=codec_type", "-of", "csv=p=0", file], { encoding: "utf8" });
  return (r.stdout || "").split(/\r?\n/).map((s) => s.trim()).filter(Boolean);
}

/** yt-dlp processes running now (a one-file yt-dlp is two: its starter and Python). */
function ytdlps() {
  if (process.platform !== "win32") return Number(spawnSync("pgrep", ["-c", "yt-dlp"], { encoding: "utf8" }).stdout.trim() || 0);
  const r = spawnSync("tasklist", ["/FI", "IMAGENAME eq yt-dlp.exe", "/FO", "CSV", "/NH"], { encoding: "utf8" });
  return (r.stdout || "").split(/\r?\n/).filter((l) => l.startsWith('"yt-dlp.exe')).length;
}

/** Bytes per second of a running job over `ms`. */
async function rate(id, ms) {
  const a = await job(id);
  await sleep(ms);
  const b = await job(id);
  return ((b.bytes - a.bytes) * 1000) / ms;
}

try {
  fs.mkdirSync(path.join(h.dataDir, "tools"), { recursive: true });
  for (const f of fs.readdirSync(TOOLS)) {
    if (/^(yt-dlp|deno|ffmpeg|ffprobe)(\.exe)?$/.test(f)) fs.copyFileSync(path.join(TOOLS, f), path.join(h.dataDir, "tools", f));
  }
  await h.launch();
  await waitFor(async () => (await invoke("get_metrics")).ok, "the app", 30000);

  // ============================================================ only the sound, as MP3
  const audio = (await invoke("start_media", { request: { url: LINK, stream: "audio", format: "mp3" } })).ok;
  check("an MP3 (sound only) download starts", !!audio?.id);
  const a = await follow(audio.id, 600000);
  const dl = a.seen.filter((x) => x.s === "downloading").map((x) => x.p);
  const fin = a.seen.filter((x) => x.s === "combining").map((x) => x.p);
  check("MP3: downloading fills the bar up to the converting share (85 %), not to 99 %", dl.length > 0 && Math.max(...dl) <= 85.1, JSON.stringify(dl.slice(-5)));
  check("MP3: while ffmpeg converts the bar keeps moving (no frozen 99 %)", fin.length === 0 || new Set(fin.map((p) => p.toFixed(1))).size > 1 || Math.max(...fin) < 99, JSON.stringify(fin.slice(0, 12)));
  check("MP3: completes as .mp3", a.j.status === "completed" && a.j.files[0]?.endsWith(".mp3"), JSON.stringify(a.j));
  const mp3 = path.join(a.j.folder, a.j.files[0] ?? "");
  check("MP3: the file is real sound (ffprobe: an audio stream, several MB)", streams(mp3).includes("audio") && fs.statSync(mp3).size > 2_000_000, `${streams(mp3)} ${fs.existsSync(mp3) && fs.statSync(mp3).size}`);
  check("MP3: no work files are left next to it", !fs.readdirSync(a.j.folder).some((n) => n.startsWith(".velox-media-")), fs.readdirSync(a.j.folder).join(","));

  // ============================================================ a video, as MP4
  const video = (await invoke("start_media", { request: { url: LINK, stream: "h480", format: "mp4" } })).ok;
  check("an MP4 (480p) download starts", !!video?.id);
  const v = await follow(video.id, 900000);
  const ps = v.seen.map((x) => x.p);
  check("MP4: the bar only goes forward", ps.every((p, i) => i === 0 || p >= ps[i - 1] - 0.05), JSON.stringify(ps.filter((_, i) => i % 20 === 0)));
  check("MP4: completes as .mp4", v.j.status === "completed" && v.j.files[0]?.endsWith(".mp4"), JSON.stringify(v.j));
  const mp4 = path.join(v.j.folder, v.j.files[0] ?? "");
  const kinds = streams(mp4);
  check("MP4: the file has a picture AND sound (ffprobe)", kinds.includes("video") && kinds.includes("audio"), kinds.join(","));

  // ============================================================ speed limits on a running video
  const big = (await invoke("start_media", { request: { url: LINK, stream: "h1080", format: "mp4" } })).ok;
  await waitFor(async () => (await job(big.id))?.status === "downloading" && (await job(big.id)).bytes > 3_000_000, "the 1080p video to run", 120000);
  const free = await rate(big.id, 4000);
  check("unlimited, the video downloads fast", free > 600 * KB, `${Math.round(free / KB)} KB/s`);

  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 300 * KB } });
  await sleep(9000); // seen within 2 s, then yt-dlp restarts and continues
  const limited = await rate(big.id, 8000);
  check("the global speed limit slows the running video (300 KB/s)", limited < 450 * KB, `${Math.round(limited / KB)} KB/s`);
  const kept = await job(big.id);
  check("...with one yt-dlp, the one before the restart was stopped", ytdlps() <= 2, String(ytdlps()));
  check("...and it continued (not started over)", kept.status === "downloading" && kept.bytes > 3_000_000, JSON.stringify({ s: kept.status, b: kept.bytes }));
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: null } });
  await sleep(9000);
  const back = await rate(big.id, 4000);
  check("removing the limit brings the speed back", back > 600 * KB, `${Math.round(back / KB)} KB/s`);

  // Smart throttle: "node" (this test) counts as a game, so the cap applies at once.
  await invoke("set_smart_throttle", { request: { enabled: true, apps: ["node"], sharePercent: 10 } });
  const on = await waitFor(async () => {
    const s = (await invoke("get_smart_throttle")).ok;
    return s?.activeApp ? s : null;
  }, "the Smart throttle to notice the listed program", 20000);
  await sleep(9000);
  const throttled = await rate(big.id, 8000);
  check("Smart throttle caps the running video", throttled < (on.limitBps ?? 0) * 1.5 + 64 * KB, `${Math.round(throttled / KB)} KB/s, cap ${Math.round((on.limitBps ?? 0) / KB)} KB/s`);
  await invoke("set_smart_throttle", { request: { enabled: false } });
  await sleep(9000);
  const free2 = await rate(big.id, 4000);
  check("Smart throttle off: full speed again", free2 > 600 * KB, `${Math.round(free2 / KB)} KB/s`);
  await invoke("cancel_media_job", { id: big.id });
  const gone = await waitFor(async () => ytdlps() === 0, "yt-dlp to stop", 15000).then(() => true, () => false);
  check("cancelling stops yt-dlp completely (no program left running)", gone, String(ytdlps()));
} catch (e) {
  check(`unexpected error: ${e?.stack ?? e}`, false);
} finally {
  await h.cleanup();
}
finish();
