// End-to-end test of the download indicator, in the real desktop app: while something downloads a small bar floats
// above the status bar with the first download (its name, progress and time left). With several, an arrow shows all
// the others above it and hides them again; the first one stays in the bar. It rises in and slides away.
//
//   cargo build -p velox-desktop -p velox-integration-tests     (the stand-in for yt-dlp)
//   node tests/e2e/desktop-indicator.mjs [path-to-debug-velox-desktop.exe]

import fs from "node:fs";
import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const YTDLP = path.resolve(exe("target/debug/fake-ytdlp"));
const FFMPEG = path.resolve(exe("target/debug/fake-ffmpeg"));
const SHOTS = path.resolve("docs/screenshots");
const VID = `ind${process.pid}x${Date.now() % 100000}`;

for (const f of [YTDLP, FFMPEG]) {
  if (!fs.existsSync(f)) {
    console.error(`missing ${f}: build it first (cargo build -p velox-integration-tests)`);
    process.exit(1);
  }
}

const srv = await startFileServer(32 * 1024 * 1024);
const h = createHarness({ port: 9377, appPath: APP, env: { VELOX_YTDLP: YTDLP, VELOX_FFMPEG: FFMPEG } });
const { page, invoke } = h;
const ev = (e) => page(e).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = async (sel) => {
  await waitFor(() => has(sel), `the element ${sel}`, 10000);
  return ev(`(document.querySelector(${JSON.stringify(sel)}).click(), true)`);
};
const status = async (id) => (await invoke("get_download", { id })).ok?.status;
const IND = "[data-testid=download-indicator]";
const box = (sel) => ev(`JSON.stringify(document.querySelector(${JSON.stringify(sel)})?.getBoundingClientRect() ?? null)`).then(JSON.parse);
const add = async (name) => {
  const d = (await invoke("create_download", { request: { url: `${srv.base}/${name}` } })).ok;
  await waitFor(async () => (await status(d.id)) === "downloading", `${name} to start`);
  return d;
};

try {
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText ?? ''")).includes("VeloxFetch"), "UI to render");
  await ev(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);
  // The bar's animations, seen as it comes and as it goes (the classes it gets while there).
  await ev(`(() => {
    window.__anims = [];
    const seen = (el) => el.getAnimations().forEach((a) => window.__anims.includes(a.animationName) || window.__anims.push(a.animationName));
    new MutationObserver((ms) => { for (const m of ms) {
      for (const n of m.addedNodes) if (n.nodeType === 1 && n.matches('.dl-ind')) seen(n);
      if (m.type === 'attributes' && m.target.matches?.('.dl-ind')) seen(m.target);
    } }).observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['class'] });
    return true; })()`);
  await sleep(600);
  check("nothing downloading: no indicator", !(await has(IND)));

  // ---- one download: the bar with its name, progress and time left
  srv.rateBps = 384 * 1024; // the whole server: long enough to look at everything
  const one = await add("first-file.zip");
  await waitFor(() => has(IND), "the indicator to appear", 5000);
  await sleep(500); // its entrance is over
  check("it rises in", (await ev("JSON.stringify(window.__anims)")).includes("ind-in"), await ev("JSON.stringify(window.__anims)"));
  check("it shows the download's name", (await text(`${IND} [data-testid=ind-first] [data-testid=ind-name]`)) === "first-file.zip", await text(IND));
  await waitFor(async () => /\d+%/.test((await text(`${IND} [data-testid=ind-first] [data-testid=ind-numbers]`)) ?? "") && / left/.test((await text(`${IND} [data-testid=ind-numbers]`)) ?? ""), "the progress and time left", 10000);
  const numbers = await text(`${IND} [data-testid=ind-numbers]`);
  check("it shows the progress, the time left and the speed", /\d+% · \d+:\d\d left · [\d.]+ [KM]B\/s/.test(numbers), numbers);
  check("its progress bar follows the download", Number(await ev(`document.querySelector('${IND} [role=progressbar]').getAttribute('aria-valuenow')`)) >= 0);
  check("one download: no arrow", !(await has(`${IND} [data-testid=ind-toggle]`)));
  const bar = await box(IND);
  const foot = await box("[data-testid=statusbar]");
  const vw = await ev("window.innerWidth");
  check("it floats just above the status bar", bar && foot && bar.bottom <= foot.top && foot.top - bar.bottom < 24, JSON.stringify({ bar, foot }));
  check("it is centred and small", Math.abs(bar.left + bar.width / 2 - vw / 2) < 2 && bar.width <= 482 && bar.height < 90, JSON.stringify(bar));
  check("the page leaves room for it at the bottom", await ev(`!!document.querySelector('[data-testid=main] > .pb-28')`));

  // ---- toasts go above it
  const toastBottom = await ev(`getComputedStyle(document.querySelector('.toast-stack')).bottom`);
  check("toasts sit above the bar, never on it", parseFloat(toastBottom) >= bar.height + (foot.height ?? 40), toastBottom);

  // ---- several: the arrow, the list above, the first stays first
  await add("second-file.iso");
  await add("third-file.pdf");
  const video = (await invoke("start_media", { request: { url: `https://www.youtube.com/watch?v=slow${VID}`, filename: "A long clip" } })).ok;
  await waitFor(async () => (await ev(`document.querySelector('${IND}')?.dataset.count`)) === "4", "four downloads in the indicator", 15000);
  check("several: an arrow with how many more", (await text(`${IND} [data-testid=ind-toggle]`))?.trim() === "+3", await text(`${IND} [data-testid=ind-toggle]`));
  check("the first download stays in the bar", (await text(`${IND} [data-testid=ind-first] [data-testid=ind-name]`)) === "first-file.zip");
  check("the list starts folded and out of reach", await ev(`(() => { const l = document.querySelector('${IND} [data-testid=ind-list]'); return l.getBoundingClientRect().height < 1 && l.inert && l.getAttribute('aria-hidden') === 'true'; })()`));
  check("the arrow says what it does", (await ev(`document.querySelector('${IND} [data-testid=ind-toggle]').getAttribute('aria-label')`)) === "Show all 4 downloads" && (await ev(`document.querySelector('${IND} [data-testid=ind-toggle]').getAttribute('aria-expanded')`)) === "false");
  const closedHeight = (await box(IND)).height;

  await click(`${IND} [data-testid=ind-toggle]`);
  await sleep(450); // the unfolding
  const listed = await ev(`JSON.stringify([...document.querySelectorAll('${IND} [data-testid=ind-list] [data-testid^=ind-] [data-testid=ind-name]')].map((n) => n.innerText))`).then(JSON.parse);
  check("the arrow shows all the others, with their information", JSON.stringify(listed) === JSON.stringify(["second-file.iso", "third-file.pdf", "A long clip"]), JSON.stringify(listed));
  check("each listed download has its progress bar and numbers", await ev(`[...document.querySelectorAll('${IND} [data-testid=ind-list] .dl-ind-line')].every((l) => l.querySelector('[role=progressbar]') && l.querySelector('[data-testid=ind-numbers]').innerText.length > 0)`));
  const openBox = await box(IND);
  check("the list unfolds upwards, the bar stays where it was", openBox.height > closedHeight + 100 && Math.abs(openBox.bottom - bar.bottom) < 2, JSON.stringify({ closedHeight, openBox }));
  check("the first download is still in the bar under the list", (await text(`${IND} [data-testid=ind-first] [data-testid=ind-name]`)) === "first-file.zip" && (await ev(`document.querySelector('${IND} .dl-ind-bar').getBoundingClientRect().top >= document.querySelector('${IND} [data-testid=ind-list]').getBoundingClientRect().bottom - 1`)));
  check("the arrow turns to fold it again", (await ev(`document.querySelector('${IND} [data-testid=ind-toggle]').getAttribute('aria-label')`)) === "Hide the list" && (await ev(`getComputedStyle(document.querySelector('${IND} .dl-ind-chevron')).transform`)) === "none");
  check("the list's lines can be reached", await ev(`!document.querySelector('${IND} [data-testid=ind-list]').inert`));
  check("nothing behind shows through it", (await ev(`getComputedStyle(document.querySelector('${IND}')).backgroundImage`)).match(/rgba?\([^)]*\)/g).every((c) => !/rgba\([^)]*, 0?\.\d+\)/.test(c)), await ev(`getComputedStyle(document.querySelector('${IND}')).backgroundImage`));
  await ev(`(document.documentElement.setAttribute('data-theme', 'dark'), true)`);
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "download-indicator-dark.png"), { width: 1280, height: 800 });
  await ev(`(document.documentElement.setAttribute('data-theme', 'light'), true)`);
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "download-indicator-light.png"), { width: 1280, height: 800 });
  await ev(`(document.documentElement.setAttribute('data-theme', 'dark'), true)`);

  await click(`${IND} [data-testid=ind-toggle]`);
  await sleep(450);
  check("clicking again hides them", await ev(`document.querySelector('${IND} [data-testid=ind-list]').getBoundingClientRect().height < 1`) && (await box(IND)).height === closedHeight);
  check("hidden, the first download is still shown", (await text(`${IND} [data-testid=ind-first] [data-testid=ind-name]`)) === "first-file.zip");
  const live = await text(`${IND} [data-testid=ind-first] [data-testid=ind-numbers]`);
  check("its numbers stay live", /\d+% · /.test(live ?? ""), live);

  // ---- a line opens its details page
  await click(`${IND} [data-testid=ind-toggle]`);
  await sleep(400);
  await click(`${IND} [data-testid=ind-m${video.id}]`);
  await waitFor(() => has("[data-testid=media-detail-view]"), "the video's details page", 5000);
  check("a video's line opens its details page", (await text("[data-testid=media-detail-title]")) === "A long clip", await text("[data-testid=media-detail-title]"));
  await click(`${IND} [data-testid=ind-first]`);
  await waitFor(() => has("[data-testid=detail-view]"), "the first download's details page", 5000);
  check("the bar's download opens its details page", (await text("[data-testid=detail-title]")) === "first-file.zip", await text("[data-testid=detail-title]"));
  check("the list stays open while it is used", await ev(`document.querySelector('${IND}').classList.contains('is-open')`));

  // ---- downloads end: the list goes, then the bar slides away
  await invoke("cancel_media_job", { id: video.id });
  for (const d of (await invoke("list_downloads", { filter: {} })).ok.filter((x) => x.id !== one.id)) await invoke("pause_download", { id: d.id });
  await waitFor(async () => (await ev(`document.querySelector('${IND}')?.dataset.count`)) === "1", "one download left", 10000);
  await sleep(300);
  check("one left: no arrow, the list is gone", !(await has(`${IND} [data-testid=ind-toggle]`)) && !(await has(`${IND} [data-testid=ind-list]`)) && !(await ev(`document.querySelector('${IND}').classList.contains('is-open')`)));
  await ev(`window.__anims = []`);
  await invoke("pause_download", { id: one.id });
  await waitFor(async () => (await ev("JSON.stringify(window.__anims)")).includes("ind-out"), "the bar to slide away", 5000);
  await waitFor(async () => !(await has(IND)), "the bar to go", 3000);
  check("the last download ends: the bar slides away and goes", true);
  check("the page takes its room back", await ev(`!document.querySelector('[data-testid=main] > .pb-28')`));
  check("toasts are back at their place", parseFloat(await ev(`getComputedStyle(document.querySelector('.toast-stack')).bottom`)) === 40);

  // ---- it comes back with the next download
  await invoke("resume_download", { id: one.id });
  await waitFor(() => has(IND), "the indicator to come back", 5000);
  check("a download starts again: the bar is back", (await text(`${IND} [data-testid=ind-name]`)) === "first-file.zip");
  check("no page errors", (await ev(`JSON.stringify(window.__errs ?? [])`)) === "[]", await ev(`JSON.stringify(window.__errs ?? [])`));
} catch (e) {
  check("the run completed", false, String(e?.stack ?? e));
} finally {
  srv.close();
  await h.cleanup();
}
finish();
