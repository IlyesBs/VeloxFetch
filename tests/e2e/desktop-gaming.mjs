// End-to-end test of the gaming policy in the real desktop app: a stand-in "game" (a copy of this test's own
// runtime, renamed and left idle; nothing else on the computer is touched) is started and stopped while a download
// runs, so the real process scan sees it. Checks: Settings offers the three policies and the low speed; "pause"
// pauses the running download (progress kept), says so in the queue and on the card, starts nothing new, and when
// the game closes resumes only what it paused (a download the user paused stays paused); "low" holds the download
// to the chosen speed; turned off, nothing is held.
//
//   node tests/e2e/desktop-gaming.mjs [path-to-debug-velox-desktop]

import { spawn } from "node:child_process";
import os from "node:os";
import { check, createHarness, exe, fail, finish, fs, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const MB = 1024 * 1024;
const GAME = "VeloxE2EGame";

const srv = await startFileServer(200 * MB);
srv.rateBps = 2 * MB;
const h = createHarness({ port: 9375, appPath: APP });
const { page, invoke } = h;
const ev = async (expr) => (await page(expr)).ok;
const q = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = async (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = (sel) =>
  page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no element ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
const choose = (sel, value) =>
  page(`(() => { const el = document.querySelector(${JSON.stringify(sel)});
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(el, ${JSON.stringify(value)});
    el.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
const state = async () => (await invoke("get_smart_throttle")).ok;
const queue = async () => (await invoke("get_queue_state")).ok;
const dl = async (id) => (await invoke("get_download", { id })).ok;

// The stand-in game: this runtime copied under the game's name, waiting. Killed in `finally`.
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "velox-game-"));
const gameExe = path.join(dir, exe(GAME));
fs.copyFileSync(process.execPath, gameExe);
fs.chmodSync(gameExe, 0o755);
let game = null;
const startGame = () => {
  game = spawn(gameExe, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore" });
};
const stopGame = () => {
  game?.kill();
  game = null;
};

try {
  console.log(`app: ${APP}\nserver: ${srv.base}\n`);
  await h.launch();
  await waitFor(() => q("[data-testid=throttle-hud]"), "the interface");
  await invoke("set_smart_throttle", { request: { enabled: true, auto: true, apps: [GAME] } });

  // ---- Settings: the policy and the low speed ------------------------------------------------------------------
  await click("[data-testid=nav-settings]");
  await waitFor(() => q("[data-testid=smart-policy]"), "Settings > Smart throttle > policy");
  check("the default policy is adaptive (as before)", (await ev(`document.querySelector('[data-testid=smart-policy]').value`)) === "adaptive");
  check("three policies are offered", (await ev(`document.querySelectorAll('[data-testid=smart-policy] option').length`)) === 3);
  check("no low speed shown unless the low policy is chosen", !(await q("[data-testid=smart-policy-limit]")));
  await choose("[data-testid=smart-policy]", "pause");
  await waitFor(async () => (await state()).gamePolicy === "pause", "the pause policy to be saved");
  check("the policy is saved from Settings", true);

  // ---- a running download, a download the user paused, then the game starts -------------------------------------
  const a = (await invoke("create_download", { request: { url: `${srv.base}/big/a.bin` } })).ok;
  const mine = (await invoke("create_download", { request: { url: `${srv.base}/big/b.bin` } })).ok;
  await waitFor(async () => (await dl(a.id)).downloaded > 0 && (await dl(mine.id)).downloaded > 0, "the downloads to run");
  await invoke("pause_download", { id: mine.id });
  startGame();
  await waitFor(async () => (await dl(a.id)).status === "paused", "the game to pause the running download", 20000);
  const qs = await queue();
  check("the queue is held for the game, not paused by the user", qs.gameHold === true && qs.paused === false && qs.held === 1, JSON.stringify(qs));
  const s = await state();
  check("the program is detected by the real scan", s.activeApp === GAME && s.gameHold === true, JSON.stringify(s));
  const kept = (await dl(a.id)).downloaded;
  check("progress is kept", kept > 0, String(kept));
  const late = (await invoke("create_download", { request: { url: `${srv.base}/big/late.bin` } })).ok;
  await sleep(1500);
  check("nothing new starts during the game", (await dl(late.id)).downloaded === 0);
  await click("[data-testid=nav-downloads]");
  await waitFor(async () => /paused until it closes/i.test((await text("[data-testid=throttle-hud]")) ?? ""), "the card to say why");
  check("the card says downloads are paused for the game", true);
  await click("[data-testid=nav-queue]");
  await waitFor(() => q("[data-testid=queue-status]"), "the queue view");
  check("the queue says it waits for the game", /game or call/i.test((await text("[data-testid=queue-status]")) ?? ""), await text("[data-testid=queue-status]"));

  // ---- the game closes: only what it paused resumes --------------------------------------------------------------
  stopGame();
  await waitFor(async () => (await dl(a.id)).status === "downloading", "the download to resume after the game", 20000);
  check("the download the game paused resumes, from where it was", (await dl(a.id)).downloaded >= kept);
  await waitFor(async () => (await dl(late.id)).downloaded > 0, "the download added during the game to start", 20000);
  check("the one added during the game starts", true);
  check("the user's own pause is kept", (await dl(mine.id)).status === "paused");
  const after = await queue();
  check("nothing is held any more", after.gameHold === false && after.held === 0 && after.gateOpen === true, JSON.stringify(after));
  await invoke("cancel_download", { id: late.id });

  // ---- low: the chosen speed -------------------------------------------------------------------------------------
  await click("[data-testid=nav-settings]");
  await waitFor(() => q("[data-testid=smart-policy]"), "Settings");
  await choose("[data-testid=smart-policy]", "low");
  await waitFor(() => q("[data-testid=smart-policy-limit]"), "the low speed choice");
  await choose("[data-testid=smart-policy-limit]", String(256 * 1024));
  await waitFor(async () => (await state()).gameLimitBps === 256 * 1024, "the low speed to be saved");
  startGame();
  await waitFor(async () => (await state()).limitBps === 256 * 1024, "the low limit to apply", 20000);
  await sleep(1000);
  const from = (await dl(a.id)).downloaded;
  await sleep(3000);
  const got = (await dl(a.id)).downloaded - from;
  check("low: held near the chosen speed (256 KB/s)", got > 0 && got < 256 * 1024 * 3 * 1.6, `${got} bytes in 3 s`);
  check("low: nothing paused", (await dl(a.id)).status === "downloading" && (await queue()).gameHold === false);

  // ---- turned off during the game: full speed, nothing held ---------------------------------------------------
  await invoke("set_smart_throttle", { request: { enabled: false } });
  const off = await state();
  check("off during the game: no limit and nothing held at once", off.limitBps === null && off.gameHold === false, JSON.stringify(off));
  stopGame();
  await invoke("cancel_download", { id: a.id });
  await invoke("cancel_download", { id: mine.id });
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  stopGame();
  srv.close();
  await h.cleanup();
  fs.rmSync(dir, { recursive: true, force: true });
}

finish();
