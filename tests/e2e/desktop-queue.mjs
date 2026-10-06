// End-to-end test of Phase 6 in the real desktop app: queue limits, pause/resume queue, the
// time-window scheduler, the opt-in power action (dry run only) and persistence across a restart.
//
//   node tests/e2e/desktop-queue.mjs [path-to-debug-velox-desktop.exe]
//
// SAFETY: the power part runs the app with VELOX_DRY_RUN_POWER=1, which only debug builds honour
// (it makes the "power action" print a line instead of touching the computer). It therefore runs
// only against a debug binary and is skipped for anything else.

import { execFileSync } from "node:child_process";
import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const DEBUG_BUILD = /[\\/]debug[\\/]/i.test(APP);
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

const srv = await startFileServer(96 * MB);
const h = createHarness({
  port: 9336,
  appPath: APP,
  env: { VELOX_DRY_RUN_POWER: "1", VELOX_POWER_GRACE_SECS: "5" },
  captureStderr: true,
});
const { page, invoke } = h;

// ---- helpers ------------------------------------------------------------------------------------
const q = (sel) => page(`!!document.querySelector(${JSON.stringify(sel)})`).then((r) => r.ok);
const text = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`)).ok;
const value = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.value ?? null`)).ok;
const click = (sel) =>
  page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no element ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
const disabled = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.disabled ?? null`)).ok;
const fill = (sel, v) =>
  page(`(() => {
    const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const checked = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.checked ?? null`)).ok;
const theme = (t) => page(`(document.documentElement.setAttribute('data-theme', ${JSON.stringify(t)}), true)`);
const nav = async (name) => {
  await click(`[data-testid=nav-${name}]`);
  await sleep(150);
};
const state = async () => (await invoke("get_queue_state")).ok;
const dl = async (id) => (await invoke("get_download", { id })).ok;
const status = async (id) => (await dl(id))?.status;
const untilStatus = (id, s, ms = 30000) => waitFor(async () => (await status(id)) === s, `download ${id} -> ${s}`, ms);
const untilState = (pred, what, ms = 15000) => waitFor(async () => pred(await state()), what, ms);
const hhmm = (offsetMin) => {
  const d = new Date(Date.now() + offsetMin * 60000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
};
const launch = async () => {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => {
    window.__csp = []; window.__errs = [];
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI));
    window.addEventListener('error', (e) => window.__errs.push(e.message));
    window.addEventListener('unhandledrejection', (e) => window.__errs.push(String(e.reason)));
    return true;
  })()`);
};
const dryRuns = () => (h.stderr.match(/\[dry-run\] power action/g) ?? []).length;

try {
  console.log(`app: ${APP}\nserver: ${srv.base}\n`);
  await launch();
  // Hold downloads at 3 MB/s so they stay running while each step is checked.
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 3 * MB } });

  // ---- defaults: nothing is restricted until the user configures it ----------------------------
  await nav("queue");
  check("queue controls are shown", await q("[data-testid=queue-controls]"));
  const s0 = await state();
  check("defaults: gate open, not paused, schedule off, power off",
    s0.gateOpen && !s0.paused && !s0.schedule.enabled && s0.scheduleOpen && s0.power.phase === "off" && s0.power.action === null, JSON.stringify(s0));
  check("defaults: 3 downloads at once, 64 connections", s0.maxActive === 3 && s0.maxConnections === 64, JSON.stringify(s0));
  check("controls show the defaults", (await value("[data-testid=queue-max-active]")) === "3" && (await value("[data-testid=queue-max-connections]")) === "64");
  check("status says the queue is running", (await text("[data-testid=queue-status]"))?.startsWith("Queue running"), await text("[data-testid=queue-status]"));

  // ---- max active, from the UI --------------------------------------------------------------------
  await fill("[data-testid=queue-max-active]", "1");
  await untilState((s) => s.maxActive === 1, "max active = 1");
  check("max active can be changed from the UI", (await state()).maxActive === 1);
  const a = (await invoke("create_download", { request: { url: `${srv.base}/a.bin` } })).ok;
  await sleep(200);
  const b = (await invoke("create_download", { request: { url: `${srv.base}/b.bin` } })).ok;
  await untilStatus(a.id, "downloading");
  check("with one slot the second download waits", (await status(b.id)) === "queued");
  await waitFor(async () => (await text("[data-testid=queue-status]"))?.includes("1 running · 1 waiting"), "running/waiting counts in the UI");
  check("the UI shows 1 running and 1 waiting", true);
  // ---- menus of a card must be drawn ON TOP of the cards below it (they used to hide under them) ----------
  await nav("downloads");
  await waitFor(async () => (await page("document.querySelectorAll('[data-testid^=download-]').length")).ok >= 2, "two cards in the list");
  const probeMenu = async (open, what) => {
    await page(open);
    await sleep(200);
    const r = (await page(`(() => {
      const menu = document.querySelector('[role=menu]');
      if (!menu) return { error: 'no menu' };
      const b = menu.getBoundingClientRect();
      const items = [...menu.querySelectorAll('[role=menuitem]')];
      const covered = items.filter((it) => { const r = it.getBoundingClientRect(); const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !top || top.closest('[role=menu]') !== menu; }).length;
      const inside = b.top >= 0 && b.left >= 0 && b.bottom <= innerHeight && b.right <= innerWidth;
      return { items: items.length, covered, inside };
    })()`)).ok;
    check(`${what}: every item is visible on top (none hidden under a card below) and inside the window`, r.items >= 3 && r.covered === 0 && r.inside, JSON.stringify(r));
    await page("document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))");
    await sleep(150);
  };
  await probeMenu("document.querySelectorAll('button[aria-label^=\"More actions\"]')[0].click()", "the ... menu of the first card");
  await probeMenu("(() => { const c = document.querySelectorAll('button[aria-label^=\"More actions\"]'); c[c.length - 1].click(); })()", "the ... menu of the last card");
  await probeMenu("(() => { const c = document.querySelectorAll('[data-testid^=download-]')[0]; const r = c.getBoundingClientRect(); c.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r.left + 40, clientY: r.top + r.height - 10 })); })()", "right-click on the first card");
  await page("document.querySelectorAll('button[aria-label^=\"More actions\"]')[0].click()");
  await sleep(150);
  await page("document.querySelectorAll('button[aria-label^=\"More actions\"]')[0].click()");
  await sleep(150);
  check("clicking the ... button again closes its menu", (await page("document.querySelectorAll('[role=menu]').length")).ok === 0);
  await nav("queue");
  await theme("light");
  await sleep(350);
  await h.screenshot(path.join(SHOTS, "queue-controls-light.png"), { width: 1100, height: 760 });

  // ---- pause queue (with running) / resume ----------------------------------------------------------
  await waitFor(async () => ((await dl(a.id)).downloaded ?? 0) > 2 * MB, "a is transferring");
  await click("[data-testid=queue-pause]");
  await untilStatus(a.id, "paused");
  const before = (await dl(a.id)).downloaded;
  check("pause queue also paused the running download and kept its progress", before > 0);
  await untilState((s) => s.paused && s.held === 1, "queue paused, 1 held");
  await sleep(1200);
  check("nothing new starts while the queue is paused", (await status(b.id)) === "queued" && (await status(a.id)) === "paused");
  check("status says the queue is paused",
    (await text("[data-testid=queue-status]")) === "Queue paused · 1 paused by the queue", await text("[data-testid=queue-status]"));
  check("the button turns into Resume queue", (await q("[data-testid=queue-resume]")) && !(await q("[data-testid=queue-pause]")));
  await click("[data-testid=queue-resume]");
  await untilStatus(a.id, "downloading");
  check("resume queue continues the same download (progress not reset)", (await dl(a.id)).downloaded >= before);
  check("queue state is back to running", (await state()).gateOpen && (await state()).held === 0);

  // ---- soft pause: running finishes, nothing new starts ---------------------------------------------
  await click("[data-testid=queue-pause-running]"); // untick "also pause running downloads"
  check("the 'also pause running' choice is a real checkbox", (await checked("[data-testid=queue-pause-running]")) === false);
  await click("[data-testid=queue-pause]");
  await untilState((s) => s.paused, "soft pause");
  await sleep(1000);
  check("soft pause leaves the running download alone", (await status(a.id)) === "downloading" && (await state()).held === 0);
  await click("[data-testid=queue-resume]");
  await untilState((s) => !s.paused, "resumed");

  // ---- scheduler --------------------------------------------------------------------------------------
  await nav("settings");
  check("scheduler panel is present and off by default", (await q("[data-testid=schedule-enabled]")) && (await checked("[data-testid=schedule-enabled]")) === false);
  await click("[data-testid=schedule-enabled]");
  await fill("[data-testid=schedule-start]", "22:00");
  await fill("[data-testid=schedule-stop]", "22:00");
  await sleep(150);
  check("equal start and stop cannot be saved, with a reason", (await disabled("[data-testid=schedule-save]")) === true && /differ/.test((await text("main")) ?? ""));
  // A window that is definitely not now: starts in 2 h, ends in 3 h.
  await fill("[data-testid=schedule-start]", hhmm(120));
  await fill("[data-testid=schedule-stop]", hhmm(180));
  await sleep(150);
  check("a valid window can be saved", (await disabled("[data-testid=schedule-save]")) === false);
  await click("[data-testid=schedule-save]");
  await untilState((s) => s.schedule.enabled && !s.scheduleOpen && !s.gateOpen && s.held === 1, "schedule closed the gate and held a");
  await untilStatus(a.id, "paused");
  const [sa, sb] = [await status(a.id), await status(b.id)];
  check("outside the window: the running download is paused and nothing starts", sa === "paused" && sb === "queued", `a=${sa} b=${sb}`);
  check("settings show that the window is closed", /Outside the window/.test((await text("[data-testid=schedule-status]")) ?? ""), await text("[data-testid=schedule-status]"));
  await theme("dark");
  await sleep(350);
  await h.screenshot(path.join(SHOTS, "settings-scheduler-dark.png"), { width: 1100, height: 900 });
  await nav("queue");
  check("the Queue view explains it is waiting for the window",
    /Outside the schedule window/.test((await text("[data-testid=queue-status]")) ?? ""), await text("[data-testid=queue-status]"));

  await nav("settings");
  await click("[data-testid=schedule-enabled]"); // turn the schedule off again
  await click("[data-testid=schedule-save]");
  await untilStatus(a.id, "downloading");
  check("turning the schedule off resumes what it paused", (await state()).gateOpen && (await state()).held === 0);

  // ---- persistence across a restart -------------------------------------------------------------------
  await invoke("set_queue_config", { request: { maxActive: 2, maxConnections: 6 } }); // b gets a slot too
  await untilStatus(b.id, "downloading");
  await invoke("pause_queue", { request: { pauseRunning: true } });
  await untilStatus(a.id, "paused");
  await untilStatus(b.id, "paused");
  const csp = (await page("window.__csp")).ok;
  const errs = (await page("window.__errs")).ok;
  check("no CSP violations or script errors in this session so far", csp.length === 0 && errs.length === 0, JSON.stringify({ csp, errs }));
  const gone = h.exited();
  await h.closeWindow();
  check("app exits cleanly", (await Promise.race([gone, sleep(20000).then(() => "timeout")])) === 0);
  h.app = null;
  await launch();
  const s1 = await state();
  check("queue settings survived the restart", s1.maxActive === 2 && s1.maxConnections === 6 && s1.paused && s1.held === 2 && !s1.gateOpen, JSON.stringify(s1));
  await nav("queue");
  check("the UI shows the queue as paused after the restart",
    (await text("[data-testid=queue-status]")) === "Queue paused · 2 paused by the queue", await text("[data-testid=queue-status]"));
  check("selects show the saved values", (await value("[data-testid=queue-max-active]")) === "2" && (await value("[data-testid=queue-max-connections]")) === "6");
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 3 * MB } }); // restored anyway; explicit
  await click("[data-testid=queue-resume]");
  await untilStatus(a.id, "downloading");
  await untilStatus(b.id, "downloading");
  check("resuming after a restart continues every held download", (await state()).held === 0);

  // ---- validation at the IPC boundary --------------------------------------------------------------------
  const rejected = async (cmd, args, why) => {
    const r = await invoke(cmd, args);
    check(`rejected: ${why}`, !!r.err && r.ok === undefined, JSON.stringify(r));
  };
  const snapshot = JSON.stringify(await state());
  const before_rejected = snapshot;
  await rejected("set_queue_config", { request: { maxActive: 0 } }, "max active 0");
  await rejected("set_queue_config", { request: { maxActive: 33 } }, "max active 33");
  await rejected("set_queue_config", { request: { maxConnections: 99 } }, "max connections 99");
  await rejected("set_queue_config", { request: { maxActive: 2, evil: true } }, "unknown field");
  await rejected("pause_queue", { request: {} }, "pause_queue without an explicit choice");
  await rejected("set_schedule", { schedule: { enabled: true, start: "22:00", stop: "22:00", days: [true, true, true, true, true, true, true] } }, "empty schedule window");
  await rejected("set_schedule", { schedule: { enabled: true, start: "25:99", stop: "07:00", days: [true, true, true, true, true, true, true] } }, "bad time");
  await rejected("set_schedule", { schedule: { enabled: true, start: "22:00", stop: "07:00", days: [false, false, false, false, false, false, false] } }, "no days");
  await rejected("set_power_action", { request: { action: "format-disk" } }, "unknown power action");
  await rejected("set_power_action", { request: { action: "shutdown", now: true } }, "extra field on power action");
  const bad = (await invoke("set_queue_config", { request: { maxActive: 0 } })).err;
  check("errors carry a code and no internal detail", bad?.code === "invalid_input" && !/[A-Z]:\\|panicked|stack/.test(bad.message ?? ""), JSON.stringify(bad));
  check("rejected requests changed nothing", JSON.stringify(await state()) === before_rejected);

  // ---- power action (dry run) ------------------------------------------------------------------------------
  if (!DEBUG_BUILD) {
    console.log("SKIP  power action checks: they need the debug build's dry-run switch");
  } else {
    await nav("settings");
    await theme("light");
    check("power panel: not armed by default", /Not armed/.test((await text("[data-testid=power-status]")) ?? ""), await text("[data-testid=power-status]"));
    check("nothing is armed in the backend", (await state()).power.phase === "off");
    await click("[data-testid=power-option-sleep]");
    await click("[data-testid=power-arm]");
    await untilState((s) => s.power.phase === "armed" && s.power.action === "sleep", "armed");
    await waitFor(async () => /Armed: sleep/.test((await text("[data-testid=power-status]")) ?? ""), "the UI to show 'Armed'", 5000)
      .then(() => check("arming from the UI shows an explicit 'Armed' status", true))
      .catch(async () => check("arming from the UI shows an explicit 'Armed' status", false, await text("[data-testid=power-status]")));

    // Let everything finish: cancel the waiting one, lift the speed limit.
    await invoke("cancel_download", { id: b.id });
    await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: null } });
    await untilStatus(a.id, "completed", 60000);
    await waitFor(async () => await q("[data-testid=power-banner]"), "countdown banner", 15000);
    check("a visible countdown appears when all downloads have finished", true);
    const secs = Number(await text("[data-testid=power-countdown]"));
    check("the countdown shows seconds left (<= the 5 s grace)", secs >= 0 && secs <= 5, String(secs));
    await h.screenshot(path.join(SHOTS, "power-countdown-light.png"), { width: 1100, height: 700 });
    await nav("queue"); // the banner must follow the user to every screen
    check("the banner is on every screen", await q("[data-testid=power-banner]"));
    await click("[data-testid=power-cancel]");
    await untilState((s) => s.power.phase === "off" && s.power.action === null, "disarmed");
    await sleep(7000);
    check("cancelling during the countdown prevents the action", dryRuns() === 0 && !(await q("[data-testid=power-banner]")), `dry runs: ${dryRuns()}`);

    // Arm again, then finish another download: this time it runs, exactly once.
    await invoke("set_power_action", { request: { action: "sleep" } });
    const c = (await invoke("create_download", { request: { url: `${srv.base}/c.bin` } })).ok;
    await untilStatus(c.id, "completed", 60000);
    await waitFor(async () => dryRuns() >= 1, "dry-run power action", 20000);
    await sleep(1500);
    check("the armed action ran exactly once (dry run)", dryRuns() === 1, `dry runs: ${dryRuns()}`);
    check("it names the action that was armed", /power action Sleep/.test(h.stderr));
    await untilState((s) => s.power.phase === "off", "one-shot: disarmed");
    check("it is disarmed after firing (one-shot)", (await state()).power.phase === "off");
  }

  const csp2 = (await page("window.__csp")).ok;
  const errs2 = (await page("window.__errs")).ok;
  check("no CSP violations or script errors after the restart either", csp2.length === 0 && errs2.length === 0, JSON.stringify({ csp2, errs2 }));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  srv.close();
  await h.cleanup();
}

finish();
