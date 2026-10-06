// End-to-end test of Phase 9 in the real desktop app: video streams (HLS) chosen in the dialog,
// downloaded, listed, cancelled; routing of .m3u8 links; the refusals; and that ordinary downloads
// still work next to it.
//
//   cargo build -p velox-integration-tests      (builds the stand-in ffmpeg used below)
//   node tests/e2e/desktop-media.mjs [path-to-debug-velox-desktop.exe]
//
// ffmpeg is never needed: the first run has none (forced), the second uses a stand-in that only
// concatenates its inputs, so this works on any machine.

import crypto from "node:crypto";
import http from "node:http";
import { check, createHarness, exe, fail, finish, fs, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const FAKE = path.resolve(`target/debug/fake-ffmpeg${process.platform === "win32" ? ".exe" : ""}`);
const SHOTS = path.resolve("docs/screenshots");
const sha = (b) => crypto.createHash("sha256").update(b).digest("hex");

// ---------------------------------------------------------------------------------- a tiny site
const bytes = (n, seed) => Buffer.from(Array.from({ length: n }, (_, i) => (i * 31 + seed * 17) & 255));
const high = Array.from({ length: 5 }, (_, i) => bytes(20_000 + i, 10 + i));
const low = Array.from({ length: 3 }, (_, i) => bytes(9_000, 20 + i));
const audio = Array.from({ length: 4 }, (_, i) => bytes(4_000, 30 + i));
const routes = new Map();
const put = (p, body) => routes.set(p, Buffer.isBuffer(body) ? body : Buffer.from(body));
const playlist = (dir, names) => `#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n${names.map((n) => `#EXTINF:6.0,\n${dir}/${n}\n`).join("")}#EXT-X-ENDLIST\n`;
put(
  "/hls/master.m3u8",
  `#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",LANGUAGE="en",DEFAULT=YES,URI="audio.m3u8"\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",NAME="English",LANGUAGE="en",URI="subs.m3u8"\n` +
    `#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO="aud"\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,AUDIO="aud",SUBTITLES="subs"\nhigh.m3u8\n`,
);
put("/hls/high.m3u8", playlist("high", high.map((_, i) => `s${i}.ts`)));
put("/hls/low.m3u8", playlist("low", low.map((_, i) => `s${i}.ts`)));
put("/hls/audio.m3u8", playlist("audio", audio.map((_, i) => `a${i}.aac`)));
put("/hls/subs.m3u8", playlist("subs", ["p0.vtt", "p1.vtt"]));
high.forEach((b, i) => put(`/hls/high/s${i}.ts`, b));
low.forEach((b, i) => put(`/hls/low/s${i}.ts`, b));
audio.forEach((b, i) => put(`/hls/audio/a${i}.aac`, b));
put("/hls/subs/p0.vtt", "WEBVTT\n\n00:00.000 --> 00:02.000\nHello\n");
put("/hls/subs/p1.vtt", "WEBVTT\n\n00:06.000 --> 00:08.000\nWorld\n");
put("/x/enc.m3u8", '#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI="k"\n#EXTINF:5,\na.ts\n#EXT-X-ENDLIST\n');
put("/x/live.m3u8", "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:5,\na.ts\n");
put("/x/file.m3u8", "#EXTM3U\n#EXTINF:5,\nfile:///etc/passwd\n#EXT-X-ENDLIST\n");
put("/x/drm.mpd", '<MPD type="static"><Period><AdaptationSet mimeType="video/mp4"><ContentProtection schemeIdUri="urn:uuid:x"/><Representation id="1" bandwidth="1"><BaseURL>a.mp4</BaseURL></Representation></AdaptationSet></Period></MPD>');
let slowMs = 0;
const site = http.createServer(async (req, res) => {
  const p = new URL(req.url, "http://x").pathname;
  const body = routes.get(p);
  if (!body) return void res.writeHead(404).end();
  if (slowMs && p.startsWith("/hls/high/")) await sleep(slowMs);
  res.writeHead(200, { "Content-Length": body.length, "Content-Type": "application/octet-stream" }).end(body);
});
await new Promise((r) => site.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${site.address().port}`;
const files = await startFileServer(300 * 1024);

// -------------------------------------------------------------------------------------- the app
const env = { VELOX_FFMPEG: path.join(process.cwd(), "no-such-ffmpeg") };
const h = createHarness({ port: 9346, appPath: APP, env });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
// Wait for the element first: the UI follows the backend through events, so it can lag a moment behind
// (page() reports a missing element as a value, not an exception, and a click on nothing is silent).
const click = async (sel) => {
  await waitFor(() => has(sel), `the element ${sel}`, 10000);
  return page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const jobs = async () => (await invoke("list_media_jobs")).ok;
const downloads = async () => (await invoke("list_downloads", { filter: {} })).ok;
const read = (name) => fs.readFileSync(path.join(h.root, name));
const openDialog = async (url = "") => {
  await click("[data-testid=stream-open]");
  await waitFor(async () => await has("[data-testid=media-dialog]"), "stream dialog");
  if (url) {
    await fill("[data-testid=media-url]", url);
    await click("[data-testid=media-look]");
  }
};
const closeDialog = async () => {
  await click("[data-testid=media-cancel]");
  await waitFor(async () => !(await has("[data-testid=media-dialog]")), "dialog to close");
};

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================= choose, download, see the result
  await openDialog(`${base}/hls/master.m3u8`);
  await waitFor(async () => await has("[data-testid=media-options]"), "the analysis");
  const qualities = await ev(`[...document.querySelectorAll('[data-testid=media-quality] option')].map(o => o.innerText)`);
  check("the dialog lists the qualities, best first", qualities.length === 2 && /720p/.test(qualities[0]) && /360p/.test(qualities[1]), JSON.stringify(qualities));
  check("and an audio choice and a subtitle choice", (await has("[data-testid=media-audio]")) && (await has("[data-testid=media-sub-s0]")));
  check("without ffmpeg it says so and offers no joining", (await has("[data-testid=media-no-ffmpeg]")) && (await ev("document.querySelector('[data-testid=media-combine]').disabled")) === true);
  check("nothing has been downloaded by looking", (await jobs()).length === 0 && (await downloads()).length === 0 && (!fs.existsSync(h.root) || fs.readdirSync(h.root).length === 0));
  await click("[data-testid=media-sub-s0]");
  await h.page("document.documentElement.setAttribute('data-theme','dark')");
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "media-dialog-dark.png"), { width: 1000, height: 800 });
  await click("[data-testid=media-start]");
  await waitFor(async () => !(await has("[data-testid=media-dialog]")), "dialog to close");
  await waitFor(async () => await has("[data-testid=media-jobs]"), "the job in the list");
  await waitFor(async () => (await jobs()).every((j) => j.status === "completed"), "the stream to complete", 30000);
  const done = (await jobs())[0];
  check("the job completes with the video, the audio and the subtitles as files", done.files.join() === "master.ts,master.audio.aac,master.en.vtt", JSON.stringify(done.files));
  check("the video is the best quality, every piece in order", sha(read("master.ts")) === sha(Buffer.concat(high)));
  check("the audio track is complete", sha(read("master.audio.aac")) === sha(Buffer.concat(audio)));
  check("the subtitles have one header and both pieces", (() => { const t = read("master.en.vtt").toString(); return t.split("WEBVTT").length === 2 && t.includes("Hello") && t.includes("World"); })());
  check("it explains that the required dependencies would join them (no program name)", done.notes.some((n) => /required dependencies/.test(n) && !/ffmpeg/i.test(n)));
  check("no working files are left in the folder", fs.readdirSync(h.root).every((n) => !n.startsWith(".velox-media")));
  check("the list shows it as completed with its files", /Completed/.test((await text("[data-testid=media-status]")) ?? "") && /master\.ts/.test((await text("[data-testid=media-files]")) ?? ""));
  await h.screenshot(path.join(SHOTS, "media-jobs-dark.png"), { width: 1000, height: 700 });
  check("a stream is not a row in the ordinary download list", (await downloads()).length === 0);

  // ====================================================================== routing of stream links
  await fill("[data-testid=url-input]", `${base}/hls/master.m3u8`);
  await click("[data-testid=add-button]");
  await waitFor(async () => await has("[data-testid=media-dialog]"), "the stream dialog from the URL bar");
  await waitFor(async () => await has("[data-testid=media-options]"), "analysis from the URL bar");
  check("an .m3u8 link typed in the address bar opens the stream dialog instead of downloading a playlist file", (await downloads()).length === 0);
  await closeDialog();

  await click("[data-testid=batch-open]");
  await fill("[data-testid=batch-links]", `${base}/hls/master.m3u8\n${base}/file-a.bin`);
  await waitFor(async () => (await text("[data-testid=batch-summary]"))?.includes("will be added"), "batch preview");
  check("a batch marks stream links as not addable", /1 not valid/.test((await text("[data-testid=batch-summary]")) ?? "") && /video stream/.test((await text("[data-testid=batch-preview]")) ?? ""));
  await click("[data-testid=batch-cancel]");
  const review = await invoke("review_link", { url: `${base}/hls/master.m3u8` });
  check("the confirmation flow refuses a stream link", !!review.err && /stream/i.test(review.err.message));

  // ================================================================================= the refusals
  for (const [path_, word] of [["/x/enc.m3u8", "encrypted"], ["/x/live.m3u8", "live"], ["/x/file.m3u8", "file"], ["/x/drm.mpd", "DRM"]]) {
    await openDialog(`${base}${path_}`);
    await waitFor(async () => await has("[data-testid=media-error]"), `error for ${path_}`);
    const msg = (await text("[data-testid=media-error]")) ?? "";
    check(`${path_} is refused with a reason (${word})`, msg.toLowerCase().includes(word.toLowerCase()), msg);
    check("and Download stays disabled", (await ev("document.querySelector('[data-testid=media-start]').disabled")) === true);
    await closeDialog();
  }

  // ================================================================ the IPC boundary refuses these
  const r1 = await invoke("analyze_media", { url: "file:///etc/passwd" });
  check("a file:// link cannot be analysed", !!r1.err);
  const r2 = await invoke("start_media", { request: { url: `${base}/hls/master.m3u8`, path: "C:\\evil" } });
  check("an extra field in a start request is refused", !!r2.err);
  const r3 = await invoke("start_media", { request: { url: `${base}/hls/master.m3u8`, subdir: "../../outside" } });
  check("a folder outside the download folder is refused", !!r3.err);
  const r4 = await invoke("start_media", { request: { url: `${base}/hls/master.m3u8`, stream: "v0; calc.exe" } });
  check("a track id that is not a plain id is refused", !!r4.err);
  const r5 = await invoke("start_media", { request: { url: `${base}/hls/master.m3u8`, filename: "a/b" } });
  check("a file name with a separator is refused", !!r5.err);
  const r6 = await invoke("cancel_media_job", { id: 9999 });
  check("cancelling a job that does not exist is an error, not a crash", !!r6.err);
  const before = (await jobs()).length;
  check("none of that started anything", before === 1 && !fs.existsSync(path.join(h.root, "..", "outside")));

  // ============================================================================== cancel a running one
  // Segments are fetched 6 at a time, so the whole job takes about one delay: it must outlast the click by a wide margin.
  slowMs = 8000;
  await invoke("start_media", { request: { url: `${base}/hls/master.m3u8`, audio: "none", filename: "slow one" } });
  await waitFor(async () => (await jobs()).some((j) => j.title === "slow one" && ["preparing", "downloading"].includes(j.status)), "the slow job to run");
  const slow = (await jobs()).find((j) => j.title === "slow one");
  await click(`[data-testid=media-${slow.id}] [data-testid=media-cancel-job]`);
  await waitFor(async () => (await jobs()).find((j) => j.id === slow.id).status === "cancelled", "the job to be cancelled", 10000);
  check("Cancel stops a running stream", true);
  check("and leaves no file or working folder behind", fs.readdirSync(h.root).every((n) => !n.startsWith("slow one") && !n.startsWith(".velox-media")), fs.readdirSync(h.root).join());
  await click(`[data-testid=media-${slow.id}] [data-testid=media-clear-job]`);
  await waitFor(async () => !(await jobs()).some((j) => j.id === slow.id), "the job to be removed");
  check("a finished job can be removed from the list", true);
  slowMs = 0;

  // ================================================================= ordinary downloads still work
  const dl = await invoke("create_download", { request: { url: `${files.base}/plain.bin` } });
  check("an ordinary download is created", !!dl.ok, JSON.stringify(dl.err));
  await waitFor(async () => (await downloads()).every((d) => d.status === "completed"), "the ordinary download", 30000);
  const plain = (await downloads())[0];
  check("and finishes with the right content", sha(fs.readFileSync(plain.destination)) === files.sha);

  // ============================== a second run, with an ffmpeg (the stand-in) to join video and audio
  if (fs.existsSync(FAKE)) {
    h.app.kill();
    await sleep(800);
    env.VELOX_FFMPEG = FAKE;
    await h.launch();
    await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI after restart");
    check("stream jobs are in memory only: the list starts empty after a restart", (await jobs()).length === 0);
    await openDialog(`${base}/hls/master.m3u8`);
    await waitFor(async () => await has("[data-testid=media-options]"), "analysis with ffmpeg");
    check("with ffmpeg the option to join video and audio is available and on", !(await has("[data-testid=media-no-ffmpeg]")) && (await ev("document.querySelector('[data-testid=media-combine]').checked")) === true);
    await fill("[data-testid=media-name]", "Joined movie");
    await click("[data-testid=media-start]");
    await waitFor(async () => (await jobs()).some((j) => j.status === "completed"), "the joined stream", 30000);
    const joined = (await jobs())[0];
    check("video and audio come out as one .mp4", joined.files.join() === "Joined movie.mp4", JSON.stringify(joined));
    const out = read("Joined movie.mp4");
    check("made from the video and then the audio", out.subarray(0, 8).toString() === "FAKEMUX\n" && sha(out.subarray(8)) === sha(Buffer.concat([...high, ...audio])));
  } else {
    console.log(`SKIP  joining with ffmpeg (build the stand-in first: cargo build -p velox-integration-tests; expected ${FAKE})`);
  }

  const errs = (await ev("window.__errs")) ?? [];
  check("no script errors", errs.length === 0, JSON.stringify(errs));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  site.close();
  files.close();
  await h.cleanup();
}

finish();
