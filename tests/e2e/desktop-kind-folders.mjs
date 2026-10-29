// End-to-end test of "Remember this folder for <kind>", in the real desktop app: a folder picked for one download
// can be remembered for its kind of file (videos, documents, archives, programs...), each kind keeps its own,
// the next file of that kind goes there by itself (and the dialogs show it), a folder picked without ticking
// the box is used once, Settings lists, changes and forgets them, they survive a restart, and a remembered
// folder that is gone is skipped for the usual place without being forgotten.
//
//   cargo build -p velox-integration-tests     (the stand-ins for yt-dlp and ffmpeg)
//   node tests/e2e/desktop-kind-folders.mjs [path-to-debug-velox-desktop]
//
// The folder dialog is answered through a file (`VELOX_TEST_PICK_FILE`, debug builds only).

import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const EXE = process.platform === "win32" ? ".exe" : "";
const YTDLP = path.resolve(`target/debug/fake-ytdlp${EXE}`);
const FFMPEG = path.resolve(`target/debug/fake-ffmpeg${EXE}`);
const VID = `kf${process.pid}x${Date.now() % 100000}`;
const srv = await startFileServer(200 * 1024);
const answerFile = path.join(os.tmpdir(), `velox-e2e-kindpick-${process.pid}.txt`);
const answer = (v) => fs.writeFileSync(answerFile, v);
answer("cancel");
const h = createHarness({
  port: 9363,
  appPath: APP,
  env: { VELOX_TEST_PICK_FILE: answerFile, VELOX_ORGANIZE: "1", VELOX_YTDLP: YTDLP, VELOX_FFMPEG: FFMPEG, VELOX_NO_LAUNCH: "1" },
});
const { page, invoke } = h;

const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const fill = (sel, v) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const nav = async (n) => (await click(`[data-testid=nav-${n}]`), sleep(250));
const real = (p) => fs.realpathSync.native(p);
const files = (dir) => (fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => !f.endsWith(".part")).sort() : []);
const settle = async (id) => waitFor(async () => ["completed", "failed"].includes((await invoke("get_download", { id })).ok?.status), `download ${id}`, 30000);
const where = async (name) => {
  const d = (await invoke("list_downloads", { filter: {} })).ok.find((x) => x.filename === name);
  if (!d) return null;
  await settle(d.id);
  return path.dirname((await invoke("get_download", { id: d.id })).ok.destination);
};
async function plain(name) {
  const d = (await invoke("create_download", { request: { url: `${srv.base}/${name}` } })).ok;
  await settle(d.id);
  return path.dirname((await invoke("get_download", { id: d.id })).ok.destination);
}
const folders = async () => Object.fromEntries((await invoke("get_storage")).ok.folders.map((f) => [f.kind, f]));

const base = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-kinds-"));
const mk = (n) => (fs.mkdirSync(path.join(base, n), { recursive: true }), path.join(base, n));
const videos = mk("D drive/My Videos");
const docs = mk("E drive/Documents");
const once = mk("Just once");
const archives = mk("USB stick/Archives");
const programs = mk("Programs here");

async function addWithOptions(link) {
  await nav("downloads");
  await waitFor(() => ev(`[...document.querySelectorAll('[data-testid=add-form] button')].some((b) => b.innerText.trim() === 'Options')`), "Options button");
  await page(`[...document.querySelectorAll('[data-testid=add-form] button')].find((b) => b.innerText.trim() === 'Options').click()`);
  await waitFor(() => has("[data-testid=add-change-folder]"), "add dialog");
  await fill("dialog input[required]", link);
}

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================ nothing remembered: as before
  check("nothing is remembered at first", Object.values(await folders()).every((f) => f.remembered === null));
  check("a video goes to the usual place", (await plain("first.mp4")) === path.join(h.root, "Video"));

  // ============================================================ A. Add dialog: pick a folder for a video and remember it
  await addWithOptions(`${srv.base}/movie.mp4`);
  check("no 'remember' box before a folder is picked", !(await has("[data-testid=add-remember]")));
  answer(videos);
  await click("[data-testid=add-change-folder]");
  await waitFor(async () => (await text("[data-testid=add-folder-path]")) === real(videos), "the picked folder");
  check("the 'remember' box appears next to the picked folder, not ticked", (await has("[data-testid=add-remember-box]")) && (await ev("document.querySelector('[data-testid=add-remember-box]').checked")) === false);
  await click("[data-testid=add-remember-box]");
  await waitFor(async () => (await ev("document.querySelector('[data-testid=add-remember-kind]').value")) === "video", "the kind found by the check", 15000);
  check("ticking it looks at the link and finds the kind: video", true);
  check("it says what remembering does", /From now on, files of this kind are saved there/.test(await text("[data-testid=add-remember]")));
  await click("dialog button[type=submit]");
  check("the video is saved in the picked folder", (await where("movie.mp4")) === real(videos));
  check("the folder is remembered for videos only", (await folders()).video.remembered === real(videos) && (await folders()).document.remembered === null);

  // the next video goes there by itself; other kinds do not
  check("the next video goes to the remembered folder by itself", (await plain("next.mp4")) === real(videos));
  check("a document still goes to the usual place", (await plain("paper.pdf")) === path.join(h.root, "Documents"));
  check("an archive still goes to the usual place", (await plain("bundle.zip")) === path.join(h.root, "Archives"));

  // the Add dialog shows it once it knows the kind
  await addWithOptions(`${srv.base}/shown.mp4`);
  await click("[data-testid=add-check]");
  await waitFor(() => has("[data-testid=add-remembered]"), "the remembered note");
  check("the Add dialog shows the remembered folder for a video", (await text("[data-testid=add-folder-path]")) === real(videos) && /remembered for Video/.test(await text("[data-testid=add-remembered]")));
  await click("dialog button[type=submit]");
  check("and the video is saved exactly there", (await where("shown.mp4")) === real(videos));

  // ============================================================ B. browser confirmation: documents get their own
  const review = async (name) => {
    const r = await invoke("review_link", { url: `${srv.base}/${name}` });
    await waitFor(() => has("[data-testid=pending-dialog]"), "confirmation");
    await waitFor(async () => (await text("[data-testid=pending-type]")) !== "Checking…", "probe finished");
    return r;
  };
  await review("report.pdf");
  answer(docs);
  await click("[data-testid=pending-change-folder]");
  await waitFor(async () => (await text("[data-testid=pending-dir-path]")) === real(docs), "picked in the confirmation");
  check("the confirmation names the kind it will remember", /Remember this folder for Documents/.test(await text("[data-testid=pending-remember]")));
  await click("[data-testid=pending-remember-box]");
  await click("[data-testid=pending-start]");
  check("the document is saved in the picked folder", (await where("report.pdf")) === real(docs));
  const f1 = await folders();
  check("documents remember their folder; videos keep theirs", f1.document.remembered === real(docs) && f1.video.remembered === real(videos));

  await review("letter.pdf");
  check("the next document's confirmation shows the remembered folder", (await text("[data-testid=pending-dir-path]")) === real(docs), await text("[data-testid=pending-dir-path]"));
  check("and says it is the one remembered for documents", /remembered for Documents/.test(await text("[data-testid=pending-remembered]")));
  await click("[data-testid=pending-start]");
  check("and saves it there", (await where("letter.pdf")) === real(docs));

  // ============================================================ C. another folder once, without ticking
  await review("clip.mp4");
  check("a video's confirmation shows the folder remembered for videos", (await text("[data-testid=pending-dir-path]")) === real(videos));
  answer(once);
  await click("[data-testid=pending-change-folder]");
  await waitFor(async () => (await text("[data-testid=pending-dir-path]")) === real(once), "picked once");
  await click("[data-testid=pending-start]");
  check("a folder picked without ticking is used for that download", (await where("clip.mp4")) === real(once));
  check("and the remembered folder is unchanged", (await folders()).video.remembered === real(videos));
  check("the next video still goes to the remembered folder", (await plain("again.mp4")) === real(videos));

  // ============================================================ D. Settings: list, change, forget
  await nav("settings");
  await waitFor(() => has("[data-testid=kind-folders]"), "the per-kind folders");
  check("Settings lists the folder remembered for videos", (await text("[data-testid=kind-folder-video-path]")) === real(videos));
  check("and the usual place for kinds without one", /usual place/.test(await text("[data-testid=kind-folder-program-path]")));
  answer(archives);
  await click("[data-testid=kind-folder-archive-change]");
  await waitFor(async () => (await text("[data-testid=kind-folder-archive-path]")) === real(archives), "archives chosen in Settings");
  check("a folder can be chosen for a kind in Settings", true);
  answer("cancel");
  await click("[data-testid=kind-folder-program-change]");
  await sleep(500);
  check("closing the dialog changes nothing", /usual place/.test(await text("[data-testid=kind-folder-program-path]")));
  answer(programs);
  await click("[data-testid=kind-folder-program-change]");
  await waitFor(async () => (await text("[data-testid=kind-folder-program-path]")) === real(programs), "programs chosen");
  check("an archive goes to its folder", (await plain("pack.zip")) === real(archives));
  check("a program goes to its folder", (await plain("setup.exe")) === real(programs));
  await click("[data-testid=kind-folder-program-forget]");
  await waitFor(async () => /usual place/.test(await text("[data-testid=kind-folder-program-path]")), "programs forgotten");
  check("'Forget' sends that kind back to the usual place", (await plain("setup2.exe")) === path.join(h.root, "Programs"));
  check("the other kinds are untouched", (await plain("pack2.zip")) === real(archives) && (await plain("more.mp4")) === real(videos));

  // ============================================================ E. a restart
  await h.cdp?.send("Browser.close").catch(() => {});
  h.app.kill();
  await sleep(1200);
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI after restart");
  const f2 = await folders();
  check("the remembered folders survive a restart", f2.video.remembered === real(videos) && f2.document.remembered === real(docs) && f2.archive.remembered === real(archives) && f2.program.remembered === null);
  check("and are used after it", (await plain("after.mp4")) === real(videos) && (await plain("after.pdf")) === real(docs));

  // ============================================================ F. a remembered folder that is gone (an unplugged drive)
  const archivesKept = (await folders()).archive.remembered;
  fs.rmSync(archives, { recursive: true, force: true });
  await nav("settings");
  await waitFor(() => has("[data-testid=kind-folder-archive-path]"), "settings");
  await nav("downloads");
  await nav("settings");
  await waitFor(async () => /not available/.test(await text("[data-testid=kind-folder-archive-path]")), "shown as not available");
  check("Settings says the folder is not available, and keeps it", (await folders()).archive.remembered === archivesKept && (await folders()).archive.rememberedAvailable === false);
  check("an archive goes to the usual place meanwhile", (await plain("gone.zip")) === path.join(h.root, "Archives"));
  check("the missing folder is not made again", !fs.existsSync(archives));
  fs.mkdirSync(archives, { recursive: true });
  check("plugged in again, it is used again", (await plain("back.zip")) === real(archives));

  // ============================================================ G. the video dialog
  await nav("downloads");
  await click("[data-testid=stream-open]");
  await waitFor(() => has("[data-testid=media-dialog]"), "the video dialog");
  await fill("[data-testid=media-url]", `https://www.youtube.com/watch?v=${VID}`);
  await click("[data-testid=media-look]");
  await waitFor(() => has("[data-testid=media-options]"), "the video's qualities");
  check("the video dialog shows the folder remembered for videos", (await text("[data-testid=media-folder-path]")) === real(videos) && /remembered for Video/.test(await text("[data-testid=media-remembered]")));
  await fill("[data-testid=media-name]", "From the dialog");
  await click("[data-testid=media-start]");
  await waitFor(async () => (await invoke("list_media_jobs")).ok.some((j) => j.status === "completed"), "the video", 30000);
  check("the video goes to the remembered folder", files(videos).includes("From the dialog.mp4"), JSON.stringify(files(videos)));

  // ============================================================ H. the web view cannot remember a folder by itself
  const bad = async (cmd, args, why) => {
    const r = await invoke(cmd, args);
    check(`${cmd}: ${why}`, !!r.err, JSON.stringify(r));
  };
  await bad("create_download", { request: { url: `${srv.base}/x.mp4`, rememberFor: "video" } }, "remembering without a picked folder is refused");
  await bad("create_download", { request: { url: `${srv.base}/x.mp4`, folderToken: 999999, rememberFor: "video" } }, "a made-up folder token is refused");
  await bad("create_download", { request: { url: `${srv.base}/x.mp4`, rememberFor: "system" } }, "an unknown kind is refused");
  await bad("forget_kind_folder", { kind: "everything" }, "forgetting an unknown kind is refused");
  answer("cancel");
  const r = await invoke("choose_kind_folder", { kind: "video", path: base, folder: base });
  check("choose_kind_folder ignores any path the web view supplies", r.ok === null && (await folders()).video.remembered === real(videos), JSON.stringify(r));
  check("nothing changed by the refused requests", (await folders()).video.remembered === real(videos));

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
