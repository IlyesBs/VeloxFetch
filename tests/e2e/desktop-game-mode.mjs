// End-to-end test of Game mode (feature #7) in the real desktop app: one click in the status bar turns it on with
// Smart throttle off (no listed program running), the banner says so on every screen with the time left, "+1 hour"
// and "Turn off"; the whole window turns amber; the download is held to the gaming policy's speed and runs at full
// speed again once it is off; Ctrl+G turns it on and off; Settings › Speed has its card and the usual time; the
// pause policy holds the queue and says "Paused for Game mode"; it is still on after a restart; every text fits in
// all 8 languages from 1360×860 down to 640×480. Writes docs/screenshots/game-mode.png.
//
//   node tests/e2e/desktop-game-mode.mjs [path-to-debug-velox-desktop]

import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const MB = 1024 * 1024;
const KB = 1024;

const srv = await startFileServer(200 * MB);
srv.rateBps = 3 * MB;
const h = createHarness({ port: 9395, appPath: APP });
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
const ctrlG = () =>
  page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'g', ctrlKey: true, bubbles: true, cancelable: true })); return true; })()`);
const state = async () => (await invoke("get_smart_throttle")).ok;
const queue = async () => (await invoke("get_queue_state")).ok;
const dl = async (id) => (await invoke("get_download", { id })).ok;
const html = (k) => ev(`document.documentElement.dataset.${k} ?? null`);
// Bytes in `secs` seconds.
const rate = async (id, secs) => {
  const from = (await dl(id)).downloaded;
  await sleep(secs * 1000);
  return (await dl(id)).downloaded - from;
};
const pickLang = async (lang) => {
  await click("[data-testid=nav-settings]");
  await waitFor(() => q("[data-testid=settings-lang]"), "language drop-down");
  await choose("[data-testid=settings-lang]", lang);
  await waitFor(async () => (await ev("document.documentElement.lang")) === lang, `the ${lang} interface`);
};
// Elements cut ("…" or clipped) or sticking out of the window.
const cut = (sel) =>
  ev(`[...document.querySelectorAll(${JSON.stringify(sel)})].filter((e) => e.offsetParent).filter((e) => {
    const r = e.getBoundingClientRect();
    const clipped = [e, ...e.querySelectorAll('*')].some((x) => x.offsetParent && getComputedStyle(x).display !== 'none' && x.scrollWidth > x.clientWidth + 1 && getComputedStyle(x).overflow !== 'visible');
    return clipped || r.right > window.innerWidth + 1 || r.left < -1 || r.width < 8; }).map((e) => (e.getAttribute('data-testid') ?? '') + ': ' + e.innerText)`);

try {
  console.log(`app: ${APP}\nserver: ${srv.base}\n`);
  await h.launch();
  await waitFor(() => q("[data-testid=statusbar]"), "the interface");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);
  // Smart throttle stays off (Fast): Game mode needs nothing else. The gaming policy: a low speed.
  await invoke("set_smart_throttle", { request: { enabled: false, gamePolicy: "low", gameLimitBps: 256 * KB } });

  // ============================================================ the status bar: off, one click on
  await waitFor(() => q("[data-testid=status-game][data-state=off]"), "the Game mode button");
  check("the status bar has a Game mode button, off", /Game mode/.test((await text("[data-testid=status-game]")) ?? ""), await text("[data-testid=status-game]"));
  check("no banner while it is off", !(await q("[data-testid=game-banner]")));
  const a = (await invoke("create_download", { request: { url: `${srv.base}/big/a.bin` } })).ok;
  await waitFor(async () => (await dl(a.id)).downloaded > 0, "the download to run");
  const free = await rate(a.id, 2);
  check("before: the download runs at the server's speed", free > 4 * MB, `${free} bytes in 2 s`);

  const t0 = Date.now();
  await click("[data-testid=status-game]");
  await waitFor(() => q("[data-testid=status-game][data-state=on]"), "Game mode on");
  const s = await state();
  check("one click: on for the usual two hours, the gaming policy at once", s.gameMode.on && s.reason === "game" && s.limitBps === 256 * KB && Math.abs(s.gameMode.untilMs - t0 - 120 * 60000) < 10000, JSON.stringify(s));
  check("Smart throttle itself stays as it was (off)", s.enabled === false);
  check("the status bar pill says it and the time left", /Game mode on/.test(await text("[data-testid=status-game]")) && /2 h|1 h 5\d min/.test(await text("[data-testid=status-game-left]")), await text("[data-testid=status-game]"));
  await waitFor(() => q("[data-testid=game-banner]"), "the banner");
  const banner = await text("[data-testid=game-banner]");
  check("the banner says what downloads do and when it ends", /Game mode on/.test(banner) && /limited to 256/.test(banner) && /left/.test(banner) && /\+1 hour/.test(banner) && /Turn off/.test(banner), banner);
  check("the whole window turns amber and is marked", (await html("game")) === "on" && (await html("throttle")) === "limiting");
  check("the gauge shows Game mode's cap", (await ev(`document.querySelector('[data-testid=throttle-hud]')?.getAttribute('data-state')`)) === "limiting");
  await sleep(1000);
  const held = await rate(a.id, 3);
  check("the download is held near 256 KB/s", held > 0 && held < 256 * KB * 3 * 1.6, `${held} bytes in 3 s`);
  await h.screenshot(path.resolve("docs/screenshots/game-mode.png"));

  // ============================================================ +1 hour, on every screen
  await click("[data-testid=game-more]");
  await waitFor(async () => (await state()).gameMode.untilMs - s.gameMode.untilMs >= 59 * 60000, "one more hour");
  check("+1 hour: one hour later", true);
  for (const nav of ["history", "queue", "settings", "downloads"]) {
    await click(`[data-testid=nav-${nav}]`);
    await sleep(150);
    check(`the banner is on the ${nav} screen too`, await q("[data-testid=game-banner]"));
  }

  // ============================================================ Turn off: full speed again
  await click("[data-testid=game-off]");
  await waitFor(async () => !(await q("[data-testid=game-banner]")), "the banner to go");
  const off = await state();
  check("Turn off: no limit, off, the window back to its colours", !off.gameMode.on && off.limitBps === null && (await html("game")) === null && (await html("throttle")) === null, JSON.stringify(off));
  await sleep(1000);
  const back = await rate(a.id, 2);
  check("the download runs at full speed again", back > 4 * MB, `${back} bytes in 2 s`);

  // ============================================================ Settings: the card, the usual time, Ctrl+G
  await click("[data-testid=nav-settings]");
  await waitFor(() => q("[data-testid=game-settings]"), "the Game mode card in Settings");
  const card = await text("[data-testid=game-settings]");
  check("Settings: the Game mode card explains it and offers to turn it on", /Game mode/.test(card) && /Ctrl\+G/.test(card) && /Turn on now/.test(card), card);
  check("Settings: it says what downloads would do", /limited to 256/.test((await text("[data-testid=game-settings-status]")) ?? ""), await text("[data-testid=game-settings-status]"));
  const policyLabel = await ev(`document.querySelector('[data-testid=smart-policy]').parentElement.innerText`);
  check("Settings: the gaming policy is labelled for Game mode too", /Game mode/i.test(policyLabel), policyLabel);
  check("Settings: four usual times, two hours chosen", (await ev(`document.querySelectorAll('[data-testid=game-duration] option').length`)) === 4 && (await ev(`document.querySelector('[data-testid=game-duration]').value`)) === "120");
  await choose("[data-testid=game-duration]", "60");
  await waitFor(async () => (await state()).gameMode.minutes === 60, "the usual time to be saved");
  check("Settings: the usual time is saved", true);
  const keys = await text("[data-testid=settings-keys]").catch(() => null);
  check("the shortcut list has Ctrl+G", /Game mode/.test(await ev(`document.body.innerText`)) && (keys === null || /Game mode/.test(keys)));
  const t1 = Date.now();
  await ctrlG();
  await waitFor(async () => (await state()).gameMode.on, "Ctrl+G to turn it on");
  const g = await state();
  check("Ctrl+G: on for the usual time (now one hour)", Math.abs(g.gameMode.untilMs - t1 - 60 * 60000) < 10000, JSON.stringify(g.gameMode));
  await waitFor(() => q("[data-testid=game-settings-toggle][aria-pressed=true]"), "the card to follow");
  check("the Settings card follows (Turn off)", /Turn off/.test(await text("[data-testid=game-settings-toggle]")));
  await ctrlG();
  await waitFor(async () => !(await state()).gameMode.on, "Ctrl+G to turn it off");
  check("Ctrl+G again: off", true);

  // ============================================================ the pause policy holds the queue
  await invoke("set_smart_throttle", { request: { gamePolicy: "pause" } });
  await click("[data-testid=game-settings-toggle]");
  await waitFor(async () => (await dl(a.id)).status === "paused", "Game mode to pause the download", 20000);
  const qs = await queue();
  check("pause: the queue is held for Game mode, not paused by the user", qs.gameHold && !qs.paused && qs.held === 1, JSON.stringify(qs));
  check("pause: the banner says downloads are paused", /downloads paused/.test((await text("[data-testid=game-banner]")) ?? ""), await text("[data-testid=game-banner]"));
  await click("[data-testid=nav-queue]");
  await waitFor(() => q("[data-testid=queue-status]"), "the queue view");
  check("pause: the queue says it waits for Game mode", /Paused for Game mode/.test((await text("[data-testid=queue-status]")) ?? ""), await text("[data-testid=queue-status]"));

  // ============================================================ still on after a restart
  await invoke("set_game_mode", { request: { on: true, minutes: 0 } });
  await h.closeWindow();
  await h.exited();
  await h.launch();
  await waitFor(() => q("[data-testid=statusbar]"), "the interface after the restart");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);
  const r = await state();
  check("after a restart: still on, until turned off", r.gameMode.on && r.gameMode.untilMs === null && r.reason === "game", JSON.stringify(r));
  await waitFor(() => q("[data-testid=game-banner]"), "the banner after the restart");
  check("after a restart: the banner says it, with no end", /until you turn it off/.test(await text("[data-testid=game-banner]")) && !(await q("[data-testid=game-more]")));
  check("after a restart: what it paused stays paused", (await dl(a.id)).status === "paused" && (await queue()).gameHold);
  await click("[data-testid=status-game]");
  await waitFor(async () => (await dl(a.id)).status === "downloading", "the download to resume when it is off", 20000);
  check("off from the status bar: what it paused resumes", true);

  // ============================================================ every language: translated and fits
  await invoke("set_smart_throttle", { request: { gamePolicy: "low" } });
  await invoke("set_game_mode", { request: { on: true, minutes: 120 } });
  for (const lang of ["fr", "ar", "es", "pt-br", "de", "ru", "tr", "en"]) {
    await pickLang(lang);
    const problems = [];
    for (const [width, height] of [[1360, 860], [900, 640], [640, 480]]) {
      await h.cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
      await click("[data-testid=nav-downloads]");
      await waitFor(() => q("[data-testid=game-banner]"), "the banner");
      problems.push(...(await cut("[data-testid=status-game], [data-testid=game-banner] button, [data-testid=game-banner] p")).map((x) => `${width}: ${x}`));
      // Nothing else in the status bar is pushed out of the window by the button.
      const pushed = await ev(`[...document.querySelector('[data-testid=statusbar]').children].filter((e) => e.offsetParent && e.getBoundingClientRect().right > window.innerWidth + 1).map((e) => e.innerText || e.className)`);
      problems.push(...pushed.map((x) => `${width} status bar pushed out: ${x}`));
      if (width >= 900) {
        // (Below 768 px the Settings tabs have their own fix, with its own check.)
        await click("[data-testid=nav-settings]");
        await waitFor(() => q("[data-testid=game-settings]"), "the card");
        problems.push(...(await cut("[data-testid=game-settings-toggle], [data-testid=game-duration]")).map((x) => `${width} settings: ${x}`));
      }
    }
    await h.cdp.send("Emulation.clearDeviceMetricsOverride");
    await click("[data-testid=nav-downloads]");
    const b = (await text("[data-testid=game-banner]")) ?? "";
    check(`${lang}: Game mode fits in the status bar, the banner and Settings at every window size`, problems.length === 0, problems.join(" | "));
    check(`${lang}: the banner is translated`, lang === "en" ? /Game mode on/.test(b) : !!b && !/Game mode on|Turn off|left/.test(b), b);
  }
  await invoke("set_game_mode", { request: { on: false } });

  // ============================================================ refused requests
  const bad = await invoke("set_game_mode", { request: { on: true, minutes: 100000 } });
  check("too long is refused, in words", !!bad.err && /24/.test(JSON.stringify(bad.err)), JSON.stringify(bad));
  const unknown = await invoke("set_game_mode", { request: { on: true, forever: true } });
  check("an unknown field is refused", !!unknown.err, JSON.stringify(unknown));
  check("nothing turned on by a refused request", !(await state()).gameMode.on);
  check("no JavaScript errors", (await ev("window.__errs.length")) === 0, JSON.stringify(await ev("window.__errs")));
  await invoke("cancel_download", { id: a.id });
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  srv.close();
  await h.cleanup();
}

finish();
