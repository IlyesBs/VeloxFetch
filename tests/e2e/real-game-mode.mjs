// By hand, with the internet: Game mode (feature #7) on a real download from a real server, in the real desktop app
// (no test switch). A Linux kernel archive from cdn.kernel.org runs at the line's speed; one click on the status bar's
// Game mode button slows it to the gaming policy (give way: a quarter of what it had; then a fixed low speed; then
// paused), the banner says so, and turning it off gives the full speed back. Smart throttle itself stays off.
//
//   node tests/e2e/real-game-mode.mjs [path-to-debug-velox-desktop]

import { check, createHarness, exe, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const URL = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz";
const KB = 1024;
const h = createHarness({ port: 9399, appPath: APP });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = (sel) => page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
const dl = async (id) => (await invoke("get_download", { id })).ok;
const state = async () => (await invoke("get_smart_throttle")).ok;
// The average speed over `secs` seconds (bytes/s).
const speed = async (id, secs) => {
  const a = (await dl(id)).downloaded;
  await sleep(secs * 1000);
  return ((await dl(id)).downloaded - a) / secs;
};
const kbs = (v) => `${(v / KB).toFixed(0)} KB/s`;

try {
  await h.launch();
  await waitFor(() => has("[data-testid=statusbar]"), "the interface");
  await invoke("set_smart_throttle", { request: { enabled: false, gamePolicy: "adaptive" } });
  const d = (await invoke("create_download", { request: { url: URL } })).ok;
  await waitFor(async () => (await dl(d.id)).downloaded > 2 * 1024 * 1024, "the download to get going", 60000);
  await sleep(4000); // the learned speed settles
  const full = await speed(d.id, 8);
  console.log(`full speed: ${kbs(full)}`);
  check("the real download runs at the line's speed", full > 600 * KB, kbs(full));

  // ---- give way (the default): a quarter of what downloads had, about at once
  await click("[data-testid=status-game]");
  await waitFor(() => has("[data-testid=game-banner]"), "the banner");
  const s = await state();
  console.log(`game mode: cap ${kbs(s.limitBps)} (reference ${kbs(s.referenceBps ?? 0)})`);
  check("one click: Game mode on, with Smart throttle off", s.gameMode.on && !s.enabled && s.reason === "game" && s.limitBps > 0, JSON.stringify(s));
  const first = await speed(d.id, 4);
  await sleep(4000); // what was already on its way drains first
  const given = await speed(d.id, 10);
  console.log(`in Game mode: ${kbs(first)} in the first seconds, then ${kbs(given)}`);
  check("give way: the real download keeps about a quarter of its speed, held to the cap", given < full * 0.45 && given > 0 && given < s.limitBps * 1.25, `${kbs(given)} of ${kbs(full)}, cap ${kbs(s.limitBps)}`);
  check("the banner says what downloads keep", /downloads keep/.test(await ev(`document.querySelector('[data-testid=game-banner]').innerText`)));

  // ---- a fixed low speed
  await invoke("set_smart_throttle", { request: { gamePolicy: "low", gameLimitBps: 256 * KB } });
  await sleep(3000);
  const low = await speed(d.id, 8);
  console.log(`low (256 KB/s): ${kbs(low)}`);
  check("low: held near 256 KB/s", low > 100 * KB && low < 256 * KB * 1.35, kbs(low));

  // ---- pause
  await invoke("set_smart_throttle", { request: { gamePolicy: "pause" } });
  await waitFor(async () => (await dl(d.id)).status === "paused", "Game mode to pause the download", 20000);
  const kept = (await dl(d.id)).downloaded;
  await sleep(3000);
  check("pause: the download is paused, its progress kept", (await dl(d.id)).downloaded === kept && kept > 0);

  // ---- off: full speed again
  await click("[data-testid=game-off]");
  await waitFor(async () => (await dl(d.id)).status === "downloading", "the download to resume", 30000);
  await sleep(5000);
  const back = await speed(d.id, 8);
  console.log(`after Game mode: ${kbs(back)}`);
  check("off: the real download runs at full speed again", back > full * 0.6, `${kbs(back)} (was ${kbs(full)})`);
  check("off: nothing held, no limit", !(await state()).gameMode.on && (await state()).limitBps === null);
  await invoke("cancel_download", { id: d.id });
  await invoke("set_smart_throttle", { request: { gamePolicy: "adaptive" } });
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  finish();
}
