// End-to-end test of Smart throttle in the real desktop app: the dashboard card (switch, gauge, modes), the Settings
// panel and the status bar showing one and the same state; quick clicks that end where the last one says, without
// flicking back; a mode chosen while it is off turning it on; the app's own process standing in for "a game" so the
// real process scan runs; a running download slowed to the mode's share of its real speed, moving on every second
// while modes change, and back to full speed at once when it is turned off.
// Writes docs/screenshots/throttle-*.png.
//
//   node tests/e2e/desktop-throttle.mjs [path-to-debug-velox-desktop]

import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

const srv = await startFileServer(200 * MB);
srv.rateBps = 3 * MB; // a 3 MB/s line shared by every connection: long enough to watch, whatever the machine
const h = createHarness({ port: 9371, appPath: APP });
const { page, invoke } = h;

const ev = async (expr) => (await page(expr)).ok;
const q = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = async (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const attr = async (sel, a) => ev(`document.querySelector(${JSON.stringify(sel)})?.getAttribute(${JSON.stringify(a)}) ?? null`);
const click = (sel) =>
  page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no element ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
const theme = (t) => page(`(document.documentElement.setAttribute('data-theme', ${JSON.stringify(t)}), true)`);
const state = async () => (await invoke("get_smart_throttle")).ok;
const HUD = "[data-testid=throttle-hud]";
const SWITCH = `${HUD} [data-testid=hud-switch]`;
const downloaded = async (id) => (await invoke("get_download", { id })).ok.downloaded;

try {
  console.log(`app: ${APP}\nserver: ${srv.base}\n`);
  await h.launch();
  await waitFor(() => q(HUD), "the Smart throttle card");
  await page(`(() => {
    window.__csp = []; window.__errs = [];
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI));
    window.addEventListener('error', (e) => window.__errs.push(e.message));
    window.addEventListener('unhandledrejection', (e) => window.__errs.push(String(e.reason)));
    return true;
  })()`);
  await waitFor(async () => (await attr(SWITCH, "disabled")) === null, "the card to load its state");

  // ---- off by default; a mode chosen while off turns it on in that mode ---------------------------------------
  check("off by default", (await state()).enabled === false && (await attr(HUD, "data-state")) === "off");
  await click(`${HUD} [data-testid=hud-mode-10]`);
  check("the switch says on at once (before the answer)", (await attr(SWITCH, "aria-checked")) === "true");
  await waitFor(async () => (await state()).enabled === true, "the app to turn it on");
  const s1 = await state();
  check("choosing Aggressive while off turned it on in Aggressive", s1.enabled && s1.sharePercent === 10, JSON.stringify(s1));
  check("the toast says so", (await ev(`document.body.innerText.includes('Smart throttle is on · Aggressive')`)) === true);
  check("the status bar shows it", await q("[data-testid=status-throttle]"));
  const status = await text(`${HUD} [data-testid=hud-status]`);
  check("the status line does not call a busy download 'network idle'", !/network idle/i.test(status ?? ""), status);

  // ---- quick clicks: they end where the last one says, and nothing flicks back ---------------------------------
  await page(`(() => { const b = document.querySelector(${JSON.stringify(SWITCH)}); for (let i = 0; i < 5; i++) b.click(); return true; })()`);
  check("five quick clicks from on: shown off at once", (await attr(SWITCH, "aria-checked")) === "false");
  const seen = new Set();
  for (let i = 0; i < 16; i++) {
    seen.add(await attr(SWITCH, "aria-checked")); // across several 3 s polls
    await sleep(250);
  }
  check("the switch never flicked back while the clicks were answered and polled", seen.size === 1 && seen.has("false"), [...seen].join(","));
  check("and the app agrees: off", (await state()).enabled === false);

  // ---- a running download, then "a game" (this app's own process) ----------------------------------------------
  const d = (await invoke("create_download", { request: { url: `${srv.base}/big/game-update.bin` } })).ok;
  await waitFor(async () => (await downloaded(d.id)) > 2 * MB, "the download to run", 30000);
  await sleep(7000); // the watcher learns what downloads get while nothing slows them (it looks every 3 s)
  const free = (await invoke("get_metrics")).ok?.speedBps ?? 0;
  await invoke("set_smart_throttle", { request: { apps: ["velox-desktop", "velox-desktop.exe"], sharePercent: 25 } });
  await click(SWITCH);
  await waitFor(async () => (await state()).reason === "app", "the scan to find the program", 15000);
  const capped = await state();
  check("a listed program caps downloads", capped.limitBps !== null && capped.activeApp !== null, JSON.stringify(capped));
  check("the cap is the mode's share of what downloads really got (not of an old best speed)", capped.referenceBps !== null && Math.abs(capped.limitBps - Math.max(capped.minBps, capped.referenceBps * 0.25)) < 2, JSON.stringify(capped));
  await waitFor(async () => (await attr(HUD, "data-state")) === "limiting", "the card to show the cap");
  const gauge = await text(`${HUD} [data-testid=hud-toggle]`);
  const expect = Math.round((capped.limitBps / Math.max(capped.referenceBps, capped.limitBps)) * 100);
  check("the gauge shows the share downloads keep", gauge?.includes(`${expect}%`), `${gauge} vs ${expect}%`);
  console.log(`  free ${(free / MB).toFixed(1)} MB/s, cap ${(capped.limitBps / MB).toFixed(2)} MB/s`);
  const core = async () =>
    ev(`(() => { const b = document.querySelector('${HUD} [data-testid=hud-toggle]'); const g = b.parentElement.getBoundingClientRect(); const r = b.getBoundingClientRect();
      return { pos: getComputedStyle(b).position, dx: Math.abs((r.left + r.width / 2) - (g.left + g.width / 2)), dy: Math.abs((r.top + r.height / 2) - (g.top + g.height / 2)), w: Math.round(r.width) }; })()`);
  const live = await core();
  check("while slowed, the gauge's core stays in place (centred, its full size)", live.pos === "absolute" && live.dx < 2 && live.dy < 2 && live.w >= 90, JSON.stringify(live));
  const cut = await ev(`[...document.querySelectorAll('${HUD} .mode-label')].filter((e) => e.scrollWidth > e.clientWidth + 1).map((e) => e.textContent)`);
  check("every mode's name fits on its button (none cut short)", cut.length === 0, JSON.stringify(cut));
  check("the gauge's core glows while slowed", (await attr(`${HUD} [data-testid=hud-toggle]`, "class"))?.includes("hud-live"));
  await theme("dark");
  await sleep(1500);
  await h.screenshot(path.join(SHOTS, "throttle-hud-limiting-dark.png"), { width: 1240, height: 860 });

  // Modes change while capped: the cap follows at once, and the download moves on every second (never stops).
  const moving = [];
  let last = await downloaded(d.id);
  for (const share of [50, 10, 25, 50, 25]) {
    await click(`${HUD} [data-testid=hud-mode-${share}]`);
    await waitFor(async () => (await state()).sharePercent === share, `mode ${share}`);
    for (let i = 0; i < 2; i++) {
      await sleep(1000);
      const now = await downloaded(d.id);
      moving.push(now - last);
      last = now;
    }
  }
  check("switching modes never stops the download (bytes every second)", moving.every((b) => b > 0), moving.join(" "));
  const s2 = await state();
  check("the last mode is in force", s2.sharePercent === 25 && s2.limitBps !== null);

  // ---- Settings shows the same state, and a change there shows on the card at once -----------------------------
  await click("[data-testid=nav-settings]");
  await waitFor(() => q("[data-testid=smart-enabled]"), "Settings > Smart throttle");
  check("Settings: the same on/off", (await ev(`document.querySelector('[data-testid=smart-enabled]').checked`)) === true);
  check("Settings: the same mode", (await ev(`document.querySelector('[data-testid=smart-mode]').value`)) === "25");
  await page(`(() => { const el = document.querySelector('[data-testid=smart-mode]');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(el, '50');
    el.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await waitFor(async () => (await state()).sharePercent === 50, "the mode set in Settings");
  check("the card follows a change made in Settings", (await attr(`${HUD} [data-testid=hud-mode-50]`, "aria-checked")) === "true");
  await page(`(() => { const el = document.querySelector('[data-testid=smart-enabled]'); el.scrollIntoView({ block: 'start' }); return true; })()`);
  await sleep(800);
  await h.screenshot(path.join(SHOTS, "throttle-settings-dark.png"), { width: 1240, height: 860 });
  await click("[data-testid=nav-downloads]");

  // ---- off: full speed at once ---------------------------------------------------------------------------------
  const before = await downloaded(d.id);
  await click(SWITCH);
  check("off: no cap shown, at once", (await attr(HUD, "data-state")) === "off");
  await waitFor(async () => (await state()).limitBps === null, "the cap to lift");
  await sleep(2000);
  const after = await downloaded(d.id);
  check("full speed came back (more than the cap allowed)", after - before > s2.limitBps * 2, `${after - before} bytes in 2 s, cap was ${s2.limitBps}`);

  await invoke("cancel_download", { id: d.id });

  // ---- Download Profiles -----------------------------------------------------------------------------------------
  const profiles = async () => (await invoke("get_profiles")).ok;
  await invoke("set_smart_throttle", { request: { enabled: false } });
  const p0 = await profiles();
  check("profiles: Default by default, in force = your own settings", p0.active === "default" && JSON.stringify(p0.effective) === JSON.stringify(p0.base), JSON.stringify(p0));
  await click("[data-testid=nav-settings]");
  await waitFor(() => q("[data-testid=profile-lowResource]"), "Settings > Download profiles");
  check("Settings lists the four built-in profiles", (await ev(`["default","fast","balanced","lowResource"].every((id) => !!document.querySelector('[data-testid=profile-' + id + ']'))`)) === true);
  await click("[data-testid=profile-lowResource] [role=radio]");
  await waitFor(async () => (await profiles()).active === "lowResource", "Low resource selected");
  const low = await profiles();
  check("Low resource: one at a time, few connections; your settings unchanged", low.effective.maxActive === 1 && low.effective.maxConnections === 4 && low.base.maxActive === p0.base.maxActive, JSON.stringify(low));
  await waitFor(() => ev(`document.querySelector('[data-testid=profile-lowResource]').dataset.active === 'true'`), "marked active");
  check("the status bar shows the active profile", (await ev(`document.querySelector('[data-testid=profile-switch]').value`)) === "lowResource");
  await page(`(() => { const el = document.querySelector('[data-testid=profile-switch]'); el.value = 'balanced'; el.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await waitFor(async () => (await profiles()).active === "balanced", "Balanced from the status bar");
  await waitFor(async () => (await state()).profileAdaptive === true, "Smart throttle follows");
  check("Balanced turns Smart throttle's automatic mode on, your switch stays off", (await state()).enabled === false && (await q("[data-testid=smart-profile-note]")));
  const bad = await invoke("profile_action", { request: { kind: "create", name: "Bad", settings: { maxConnections: 999 } } });
  check("an invalid profile is refused by the app", !!bad.err, JSON.stringify(bad));
  const sneaky = await invoke("profile_action", { request: { kind: "delete", id: "fast" } });
  check("a built-in profile cannot be deleted", !!sneaky.err);
  const unknown = await invoke("profile_action", { request: { kind: "create", name: "X", settings: { maxActive: 2, skipTls: true } } });
  check("unknown settings are refused", !!unknown.err);
  await click("[data-testid=profile-dup-fast]");
  await waitFor(async () => (await profiles()).profiles.some((x) => !x.builtIn), "a copy of Fast");
  const copy = (await profiles()).profiles.find((x) => !x.builtIn);
  await h.page("document.documentElement.setAttribute('data-theme','dark')");
  await page(`(() => { document.querySelector('[data-testid=profiles]').scrollIntoView({ block: 'start' }); return true; })()`);
  await sleep(500);
  await h.screenshot(path.join(SHOTS, "profiles-dark.png"), { width: 1240, height: 860 });
  await h.page("document.documentElement.removeAttribute('data-theme')");
  await invoke("profile_action", { request: { kind: "select", id: copy.id } });
  await click(`[data-testid=profile-del-${copy.id}]`);
  await click(`[data-testid=profile-del-${copy.id}]`);
  await waitFor(async () => (await profiles()).active === "default", "back to Default after deleting the active profile");
  check("deleting the active profile falls back to Default", JSON.stringify((await profiles()).effective) === JSON.stringify(p0.base));
  await click("[data-testid=nav-downloads]");

  const csp = (await page("window.__csp")).ok;
  const errs = (await page("window.__errs")).ok;
  check("no CSP violations or script errors", csp.length === 0 && errs.length === 0, JSON.stringify({ csp, errs }));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  srv.close();
  await h.cleanup();
}

finish();
