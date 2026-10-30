// End-to-end test: a video (and a playlist) saved in a folder the user picks, in the real desktop app.
// The video dialog has "Save in ... Change…" like the Add dialog: the native folder dialog gives the UI a
// token, never a path, and the video lands exactly there (a playlist: its own folder inside it).
//
//   cargo build -p velox-integration-tests     (the stand-ins for yt-dlp and ffmpeg)
//   node tests/e2e/desktop-video-folder.mjs [path-to-debug-velox-desktop]
//
// yt-dlp and ffmpeg are stand-ins (no internet). The folder dialog is answered through a file
// (`VELOX_TEST_PICK_FILE`, debug builds only).

import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, fail, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const EXE = process.platform === "win32" ? ".exe" : "";
const YTDLP = path.resolve(`target/debug/fake-ytdlp${EXE}`);
const FFMPEG = path.resolve(`target/debug/fake-ffmpeg${EXE}`);
const VID = `vf${process.pid}x${Date.now() % 100000}`;

for (const f of [YTDLP, FFMPEG]) {
  if (!fs.existsSync(f)) {
    console.error(`missing ${f}: build it first (cargo build -p velox-integration-tests)`);
    process.exit(1);
  }
}

const answerFile = path.join(os.tmpdir(), `velox-e2e-vfolder-${process.pid}.txt`);
const answer = (v) => fs.writeFileSync(answerFile, v);
answer("cancel");
const h = createHarness({
  port: 9361,
  appPath: APP,
  env: { VELOX_TEST_PICK_FILE: answerFile, VELOX_YTDLP: YTDLP, VELOX_FFMPEG: FFMPEG, VELOX_ORGANIZE: "1", VELOX_NO_LAUNCH: "1" },
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
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
const jobs = async () => (await invoke("list_media_jobs")).ok;
const tree = (dir) => (fs.existsSync(dir) ? fs.readdirSync(dir).sort() : []);
const real = (p) => fs.realpathSync(p);

const base = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-vfolder-"));
const folderA = path.join(base, "My videos");
const folderB = path.join(base, "External disk");
for (const f of [folderA, folderB]) fs.mkdirSync(f, { recursive: true });

const openDialog = async (url) => {
  await click("[data-testid=nav-downloads]");
  await click("[data-testid=stream-open]");
  await waitFor(() => has("[data-testid=media-dialog]"), "the video dialog");
  await fill("[data-testid=media-url]", url);
  await click("[data-testid=media-look]");
};

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================== A. one video, into a picked folder
  await openDialog(`https://www.youtube.com/watch?v=${VID}`);
  await waitFor(() => has("[data-testid=media-options]"), "the video's qualities");
  check("the video dialog offers 'Save in … Change…' and shows the usual folder", /usual download folder/.test((await text("[data-testid=media-folder-path]")) ?? ""));
  check("no 'Usual' button while nothing is chosen", !(await has("[data-testid=media-default-folder]")));
  await click("[data-testid=media-change-folder]");
  await sleep(500);
  check("closing the folder dialog keeps the usual folder", /usual download folder/.test((await text("[data-testid=media-folder-path]")) ?? ""));
  answer(folderA);
  await click("[data-testid=media-change-folder]");
  await waitFor(async () => (await text("[data-testid=media-folder-path]")) === real(folderA), "the chosen folder shown");
  check("the sub-folder box is disabled with a chosen folder", (await ev("document.querySelector('[data-testid=media-subdir]').disabled")) === true);
  await click("[data-testid=media-default-folder]");
  check("'Usual' goes back", /usual download folder/.test((await text("[data-testid=media-folder-path]")) ?? "") && (await ev("document.querySelector('[data-testid=media-subdir]').disabled")) === false);
  await click("[data-testid=media-change-folder]");
  await waitFor(async () => (await text("[data-testid=media-folder-path]")) === real(folderA), "the chosen folder again");
  await fill("[data-testid=media-name]", "Picked clip");
  await click("[data-testid=media-start]");
  await waitFor(async () => !(await has("[data-testid=media-dialog]")), "the dialog to close");
  await waitFor(async () => (await jobs()).some((j) => j.status === "completed"), "the video", 30000);
  check("the video is saved exactly in the chosen folder", JSON.stringify(tree(folderA)) === JSON.stringify(["Picked clip.mp4"]), JSON.stringify(tree(folderA)));
  check("not in the usual folder", !fs.existsSync(path.join(h.root, "Video", "Picked clip.mp4")) && !fs.existsSync(path.join(h.root, "Picked clip.mp4")));
  check("the job shows the chosen folder", (await jobs())[0].folder === real(folderA), (await jobs())[0].folder);
  for (const j of await jobs()) await invoke("clear_media_job", { id: j.id });

  // ============================================================== B. a playlist, inside a picked folder
  answer(folderB);
  await openDialog(`https://www.youtube.com/playlist?list=PL${VID}`);
  await waitFor(() => has("[data-testid=media-playlist-header]"), "the playlist");
  await click("[data-testid=media-change-folder]");
  await waitFor(async () => (await text("[data-testid=media-folder-path]")) === real(folderB), "the chosen folder shown");
  check("a playlist keeps its folder name box, inside the chosen folder", (await ev("document.querySelector('[data-testid=media-subdir]').disabled")) === false && /inside the folder you chose/.test(await text("[data-testid=media-dialog]")));
  await click("[data-testid=media-pick-all]");
  await click("[data-testid=media-item-1] input");
  await click("[data-testid=media-start]");
  await waitFor(async () => !(await has("[data-testid=media-dialog]")), "the dialog to close");
  await waitFor(async () => (await jobs()).length === 1 && (await jobs())[0].status === "completed", "the playlist video", 30000);
  const inside = path.join(folderB, "My list_ best_of");
  check("the playlist's folder is made inside the chosen folder", JSON.stringify(tree(inside)) === JSON.stringify(["01 - Clip 1_ part_1.mp4"]), `${JSON.stringify(tree(folderB))} ${JSON.stringify(tree(inside))}`);
  for (const j of await jobs()) await invoke("clear_media_job", { id: j.id });

  // ============================================================== C. the web view cannot choose a folder by itself
  const bad = async (cmd, args, why) => {
    const r = await invoke(cmd, args);
    check(`${cmd}: ${why}`, !!r.err, JSON.stringify(r));
  };
  const url = `https://www.youtube.com/watch?v=${VID}z`;
  await bad("start_media", { request: { url, folderToken: 999999 } }, "a made-up folder token is refused");
  await bad("start_media", { request: { url, folder: base } }, "a path is refused (unknown field)");
  await bad("start_media", { request: { url, folderToken: 1, subdir: "x" } }, "a folder token with a sub-folder is refused");
  await bad("start_playlist", { request: { key: "nope", items: [1], folderToken: 999999 } }, "a playlist with a made-up token is refused");
  check("nothing was started by the refused requests", (await jobs()).length === 0);

  const errs = (await ev("window.__errs")) ?? [];
  check("no script errors in the page", errs.length === 0, JSON.stringify(errs));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  fs.rmSync(answerFile, { force: true });
  fs.rmSync(base, { recursive: true, force: true });
  await h.cleanup();
}
finish();
