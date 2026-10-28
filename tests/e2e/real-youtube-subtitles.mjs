// By hand, needs the internet: a real YouTube video with subtitles in many languages, through the real app window and
// the real yt-dlp / Deno / ffmpeg. The dialog lists the languages (people's, the video's own captions, the automatic
// translations), the search box narrows them, "only the sound" hides them, and the chosen ones land next to the video
// as .srt files named after it.
//   node tests/e2e/real-youtube-subtitles.mjs [app] <folder with yt-dlp, deno, ffmpeg>
// The tools folder is copied into the test's own data folder (nothing is downloaded for the tools).
import fs from "node:fs";
import path from "node:path";
import { check, createHarness, exe, finish, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const TOOLS = process.argv[3];
// A talk with people's subtitles in about 50 languages and the site's captions.
const LINK = process.env.VELOX_TEST_REAL_SUBTITLES ?? "https://www.youtube.com/watch?v=arj7oStGLkU";
if (!TOOLS || !fs.existsSync(TOOLS)) {
  console.log("SKIP  give the folder with yt-dlp, deno and ffmpeg");
  process.exit(0);
}

const h = createHarness({ port: 9342, appPath: APP, env: { VELOX_TOOLS: "official" } });
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
const job = async (id) => ((await invoke("list_media_jobs")).ok ?? []).find((j) => j.id === id);

try {
  fs.mkdirSync(path.join(h.dataDir, "tools"), { recursive: true });
  for (const f of fs.readdirSync(TOOLS)) fs.copyFileSync(path.join(TOOLS, f), path.join(h.dataDir, "tools", f));
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText"))?.includes("VeloxFetch"), "UI");

  await click("[data-testid=stream-open]");
  await waitFor(() => has("[data-testid=media-dialog]"), "the dialog");
  await fill("[data-testid=media-url]", LINK);
  await click("[data-testid=media-look]");
  await waitFor(() => has("[data-testid=media-options]"), "the page to be read", 150000);
  const all = await shownSubs();
  check("the subtitle languages are listed", all.length > 40, `${all.length}`);
  check("people's subtitles and the site's automatic translations", all.includes("mar") && all.includes("men") && all.some((id) => id.startsWith("a")), all.slice(0, 8).join(","));
  check("a language people wrote is not offered a second time (not even the video's own captions)", !all.includes("aar") && !all.includes("aen") && !all.includes("aen-orig"));
  const labels = await ev(`[...document.querySelectorAll('input[type=checkbox][data-testid^=media-sub-]')].map(e => e.parentElement.innerText)`);
  check("people's are named plainly, the site's translations say so", labels[0] === "English" && labels.some((l) => /^\S.* · auto-translated$/.test(l)), labels.slice(0, 3).join(" | ").slice(0, 200));
  check("the list scrolls and has a search box", await has("[data-testid=media-sub-find]"));

  await fill("[data-testid=media-sub-find]", "arab");
  await waitFor(async () => (await shownSubs()).length < 5, "the search");
  check("the search narrows the list", JSON.stringify(await shownSubs()) === JSON.stringify(["mar"]), JSON.stringify(await shownSubs()));
  await click("[data-testid=media-sub-mar]");
  await fill("[data-testid=media-sub-find]", "french");
  await waitFor(async () => (await shownSubs()).includes("mfr"), "French");
  await click("[data-testid=media-sub-mfr]");
  const translated = all.find((id) => id.startsWith("a"));
  await fill("[data-testid=media-sub-find]", "");
  await waitFor(async () => (await shownSubs()).length === all.length, "the whole list again");
  await click(`[data-testid=media-sub-${translated}]`);
  await click("[data-testid=media-sub-men]");
  check("the dialog counts the chosen languages", /4 chosen/i.test(await ev(`document.querySelector('[data-testid=media-options]').innerText`)));

  await fill("[data-testid=media-quality]", "audio");
  await waitFor(async () => !(await has("[data-testid=media-sub-find]")), "only the sound");
  check("only the sound: no subtitles offered", (await shownSubs()).length === 0);
  await fill("[data-testid=media-quality]", "h360");
  await waitFor(() => has("[data-testid=media-sub-mar]"), "the subtitles again");
  check("back to a video: the choice is kept", await ev(`document.querySelector('[data-testid=media-sub-mar]').checked`));

  await fill("[data-testid=media-name]", "Talk");
  await click("[data-testid=media-start]");
  await waitFor(async () => ((await invoke("list_media_jobs")).ok ?? []).length > 0, "the job");
  const id = (await invoke("list_media_jobs")).ok[0].id;
  await waitFor(async () => !["preparing", "queued", "downloading", "combining", "waiting"].includes((await job(id))?.status), "the download", 300000);
  const done = await job(id);
  check("the video downloaded", done.status === "completed", JSON.stringify({ s: done.status, e: done.error }));
  const tag = translated.slice(1);
  // YouTube limits its automatic translations (HTTP 429 for a while once a computer asked for many): the translation is
  // then skipped with a note saying so, and the others still come.
  const limited = done.notes.some((n) => n.startsWith("The site is limiting requests for now, so these subtitles were skipped: " + tag + "."));
  const want = ["Talk.mp4", "Talk.ar.srt", "Talk.fr.srt", ...(limited ? [] : [`Talk.${tag}.srt`]), "Talk.en.srt"];
  check("the subtitles are saved next to it, named after it, in the order chosen", JSON.stringify(done.files) === JSON.stringify(want), JSON.stringify(done.files));
  check(limited ? "the translation the site refused for now is named in a note (and nothing else is missing)" : "nothing was skipped", done.notes.length === (limited ? 1 : 0), JSON.stringify(done.notes));
  const dir = done.folder;
  const srt = (n) => fs.readFileSync(path.join(dir, n), "utf8");
  check("they are real SRT files", want.slice(1).every((n) => /^\uFEFF?1\r?\n\d\d:\d\d:\d\d,\d\d\d --> /.test(srt(n))));
  check("the Arabic one is in Arabic", /[\u0600-\u06FF]{3}/.test(srt("Talk.ar.srt")));
  check("no work folder is left behind", !fs.readdirSync(dir).some((n) => n.startsWith(".velox")), fs.readdirSync(dir).join(","));
  console.log(`    ${dir}: ${fs.readdirSync(dir).map((n) => `${n} ${fs.statSync(path.join(dir, n)).size}`).join(", ")}`);
} catch (e) {
  check("no exception", false, e.stack);
} finally {
  await h.cleanup();
}
finish();
