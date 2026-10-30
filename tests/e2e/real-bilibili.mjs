// By hand, needs the internet: real Bilibili videos through the real app window and the real yt-dlp / Deno / ffmpeg.
//  - A link to Bilibili's translated page (`bilibili.com/ar/video/BV...`) downloads like the video page itself.
//  - A bilibili.tv episode (Bilibili's international site) opens the video dialog, lists its translated subtitles,
//    and the chosen ones land next to the video as .srt files.
//   node tests/e2e/real-bilibili.mjs [app] <folder with yt-dlp, deno, ffmpeg>
// The tools folder is copied into the test's own data folder (nothing is downloaded for the tools).
import fs from "node:fs";
import path from "node:path";
import { check, createHarness, exe, finish, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const TOOLS = process.argv[3];
// A short video, opened from the Arabic version of the site (as a user sent it).
const TRANSLATED = process.env.VELOX_TEST_BILIBILI ?? "https://www.bilibili.com/ar/video/BV1b5nh6dECE/?spm_id_from=333.1387.homepage.video_card.click";
// An episode with subtitles in eight languages.
const INTL = process.env.VELOX_TEST_BILIBILI_TV ?? "https://www.bilibili.tv/en/play/1006209/10218310";
if (!TOOLS || !fs.existsSync(TOOLS)) {
  console.log("SKIP  give the folder with yt-dlp, deno and ffmpeg");
  process.exit(0);
}

const h = createHarness({ port: 9343, appPath: APP, env: { VELOX_TOOLS: "official" } });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = async (sel) => {
  await waitFor(() => has(sel), sel);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const shownSubs = () => ev(`[...document.querySelectorAll('[data-testid^=media-sub-]')].map(e => e.dataset.testid.slice(10)).filter(id => id !== 'find')`);
const jobs = async () => (await invoke("list_media_jobs")).ok ?? [];
const busy = (s) => ["preparing", "queued", "downloading", "combining", "waiting"].includes(s);

const open = async (link) => {
  await click("[data-testid=stream-open]");
  await waitFor(() => has("[data-testid=media-dialog]"), "the dialog");
  await fill("[data-testid=media-url]", link);
  await click("[data-testid=media-look]");
  await waitFor(() => has("[data-testid=media-options]"), "the page to be read", 150000);
};
const download = async (name) => {
  const before = (await jobs()).map((j) => j.id);
  await fill("[data-testid=media-name]", name);
  await click("[data-testid=media-start]");
  await waitFor(async () => (await jobs()).some((j) => !before.includes(j.id)), "the job");
  const id = (await jobs()).find((j) => !before.includes(j.id)).id;
  await waitFor(async () => !busy((await jobs()).find((j) => j.id === id)?.status), "the download", 600000);
  return (await jobs()).find((j) => j.id === id);
};

try {
  fs.mkdirSync(path.join(h.dataDir, "tools"), { recursive: true });
  for (const f of fs.readdirSync(TOOLS)) fs.copyFileSync(path.join(TOOLS, f), path.join(h.dataDir, "tools", f));
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText"))?.includes("VeloxFetch"), "UI");

  // ============================================================== A. Bilibili's translated page
  await open(TRANSLATED);
  check("the translated page is read as the video (qualities offered)", (await ev(`document.querySelectorAll('[data-testid=media-quality] option').length`)) > 1);
  await fill("[data-testid=media-quality]", "h360");
  const a = await download("Translated page");
  check("the video downloaded", a.status === "completed", JSON.stringify({ s: a.status, e: a.error }));
  check("one video file", JSON.stringify(a.files) === JSON.stringify(["Translated page.mp4"]), JSON.stringify(a.files));

  // ============================================================== B. bilibili.tv, translated subtitles
  await open(INTL);
  const all = await shownSubs();
  check("bilibili.tv: the subtitle languages are listed", ["men", "mar", "mth"].every((id) => all.includes(id)), JSON.stringify(all));
  await click("[data-testid=media-sub-men]");
  await click("[data-testid=media-sub-mar]");
  await fill("[data-testid=media-quality]", "h360");
  const b = await download("Episode");
  check("the episode downloaded", b.status === "completed", JSON.stringify({ s: b.status, e: b.error }));
  check("the subtitles are saved next to it, named after it", JSON.stringify(b.files) === JSON.stringify(["Episode.mp4", "Episode.en.srt", "Episode.ar.srt"]), JSON.stringify(b.files));
  const srt = (n) => fs.readFileSync(path.join(b.folder, n), "utf8");
  check("they are real SRT files", ["Episode.en.srt", "Episode.ar.srt"].every((n) => /^\uFEFF?1\r?\n\d\d:\d\d:\d\d,\d\d\d --> /.test(srt(n))));
  check("the Arabic one is in Arabic", /[\u0600-\u06FF]{3}/.test(srt("Episode.ar.srt")));
  console.log(`    ${b.folder}: ${fs.readdirSync(b.folder).map((n) => `${n} ${fs.statSync(path.join(b.folder, n)).size}`).join(", ")}`);
} catch (e) {
  check("no exception", false, e.stack);
} finally {
  await h.cleanup();
}
finish();
