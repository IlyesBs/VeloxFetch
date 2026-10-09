// End-to-end test of the Smart Download Queue in the real desktop app: queue tabs, the editor and its quick
// setups, a time window that holds downloads back, Start / Pause queue, per-download "Start at..." with the
// timeline, Start now, moving downloads up and down, the Add dialog's queue and start choices, clearing
// completed downloads, deleting a queue, and hostile IPC.
//
//   node tests/e2e/desktop-smart-queue.mjs [path-to-debug-velox-desktop]

import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

const srv = await startFileServer(64 * MB);
const h = createHarness({ port: 9341, appPath: APP });
const { page, invoke } = h;

// ---- helpers ------------------------------------------------------------------------------------
const q = (sel) => page(`!!document.querySelector(${JSON.stringify(sel)})`).then((r) => r.ok);
const count = (sel) => page(`document.querySelectorAll(${JSON.stringify(sel)}).length`).then((r) => r.ok);
const text = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`)).ok;
const attr = async (sel, a) => (await page(`document.querySelector(${JSON.stringify(sel)})?.getAttribute(${JSON.stringify(a)}) ?? null`)).ok;
/** Click an element once it is there (the window follows the app a moment later); a miss fails the test. */
const click = async (sel) => {
  await waitFor(() => q(sel), `the element ${sel}`, 10000);
  const r = await page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no element ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
  if (!r.ok) throw new Error(`could not click ${sel}: ${JSON.stringify(r)}`);
  return r;
};
const disabled = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.disabled ?? null`)).ok;
const fill = (sel, v) =>
  page(`(() => {
    const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
/** Open the ... menu of a row and pick the item whose text starts with `label`. */
const rowMenu = async (id, label) => {
  const btn = `[data-testid=queue-${id}] button[aria-label^="More actions"]`;
  // Open this row's menu (and only it), then pick the item inside that menu.
  await waitFor(async () => (await count("[role=menu]")) === 0, "no other menu open", 5000);
  await click(btn);
  await waitFor(async () => (await attr(btn, "aria-expanded")) === "true" && (await count("[role=menu]")) === 1, "the row's menu", 5000);
  return page(`(() => {
    const menu = document.querySelector('[role=menu]');
    const it = [...menu.querySelectorAll('[role=menuitem]')].find((b) => b.innerText.trim().startsWith(${JSON.stringify(label)}));
    if (!it) throw new Error('no menu item ' + ${JSON.stringify(label)});
    it.click(); return true; })()`);
};
const nav = async (name) => {
  await click(`[data-testid=nav-${name}]`);
  await sleep(150);
};
const theme = (t) => page(`(document.documentElement.setAttribute('data-theme', ${JSON.stringify(t)}), true)`);
const queues = async () => (await invoke("get_queues")).ok;
const dl = async (id) => (await invoke("get_download", { id })).ok;
const status = async (id) => (await dl(id))?.status;
const untilStatus = (id, s, ms = 30000) => waitFor(async () => (await status(id)) === s, `download ${id} -> ${s}`, ms).catch(async (e) => {
  const d = await dl(id); const qs = await invoke("get_queues"); const qst = await invoke("get_queue_state");
  console.log("      state when it timed out:", JSON.stringify({ d: { status: d.status, queueId: d.queueId, startAt: d.startAt, held: d.held, downloaded: d.downloaded }, queues: qs.ok?.queues?.map((x) => ({ id: x.id, gate: x.gate, started: x.started, running: x.running })), qst: qst.ok && { gateOpen: qst.ok.gateOpen, paused: qst.ok.paused, running: qst.ok.running, waiting: qst.ok.waiting, held: qst.ok.held, max: qst.ok.maxActive } }));
  throw e; });
const rowState = (id) => attr(`[data-testid=queue-${id}]`, "data-state");
const untilRowState = (id, s, ms = 15000) => waitFor(async () => (await rowState(id)) === s, `row ${id} shows ${s}`, ms);
const hhmm = (offsetMin) => {
  const d = new Date(Date.now() + offsetMin * 60000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
};

try {
  console.log(`app: ${APP}\nserver: ${srv.base}\n`);
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => {
    window.__csp = []; window.__errs = [];
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI));
    window.addEventListener('error', (e) => window.__errs.push(e.message));
    window.addEventListener('unhandledrejection', (e) => window.__errs.push(String(e.reason)));
    return true;
  })()`);
  // Keep downloads running long enough to look at them.
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 2 * MB } });

  // ---- defaults: one main queue, nothing changes for someone who never sets a rule --------------------------
  await nav("queue");
  check("the queue controls the page always had are still there", await q("[data-testid=queue-controls]"));
  await waitFor(() => q("[data-testid=sq-tabs]"), "queue tabs");
  const q0 = await queues();
  check("one main queue, open, without rules", q0.queues.length === 1 && q0.queues[0].id === 1 && q0.queues[0].gate === "open" && q0.queues[0].rules.when.kind === "anytime", JSON.stringify(q0));
  check("the main queue is named in the user's language", (await text("[data-testid=sq-tab-1]"))?.includes("Main queue"));
  check("no timeline without times", !(await q("[data-testid=sq-timeline]")));

  // ---- a new queue from the editor: a window that is closed right now ---------------------------------------
  await click("[data-testid=sq-new]");
  await waitFor(() => q("[data-testid=sq-editor]"), "the editor");
  check("quick setups are offered", (await count("[data-testid^=sq-preset-]")) === 4);
  await click("[data-testid=sq-preset-night]");
  check("a quick setup fills the name and the rules", (await page("document.querySelector('[data-testid=sq-name]').value")).ok === "Night" && (await attr("[data-testid=sq-when-between]", "aria-checked")) === "true");
  // A window that opens in two hours, so it is closed now whatever the time of day.
  await fill("[data-testid=sq-from]", hhmm(120));
  await fill("[data-testid=sq-to]", hhmm(180));
  await fill("[data-testid=sq-name]", "");
  check("a queue needs a name", (await disabled("[data-testid=sq-save]")) === true);
  await fill("[data-testid=sq-name]", "Tonight");
  await click("[data-testid=sq-save]");
  await waitFor(async () => (await queues()).queues.length === 2, "the queue to be saved");
  const night = (await queues()).queues.find((x) => x.name === "Tonight");
  check("the new queue is saved with its rules", night?.rules.when.kind === "between" && night.rules.largeFilesOver === 500 * MB, JSON.stringify(night));
  await waitFor(async () => (await attr(`[data-testid=sq-tab-${night.id}]`, "aria-pressed")) === "true", "the new tab to be selected");
  check("it is closed by time and says when it opens", night.gate === "time" && !!night.opensAt && /Scheduled · starts/.test((await text("[data-testid=sq-gate]")) ?? ""), await text("[data-testid=sq-gate]"));
  check("its rules show as chips", /–/.test((await text("[data-testid=sq-rules]")) ?? ""));
  check("the timeline shows when it may download", await q("[data-testid=sq-timeline-span]"));

  // ---- a download added to it waits for the window ------------------------------------------------------------
  const a = (await invoke("create_download", { request: { url: `${srv.base}/night.bin`, queueId: night.id } })).ok;
  check("the download is in that queue", a.queueId === night.id, JSON.stringify(a));
  await waitFor(() => q(`[data-testid=queue-${a.id}]`), "its row");
  await sleep(800);
  check("it does not start outside the window", (await status(a.id)) === "queued");
  check("it shows as Scheduled with when it starts", (await rowState(a.id)) === "scheduled" && /Starts/.test((await text(`[data-testid=queue-${a.id}] [data-testid=sq-why]`)) ?? ""));
  check("the next-download line names it", (await text("[data-testid=sq-next]"))?.includes("night.bin"));
  await theme("dark");
  await sleep(250);
  await h.screenshot(path.join(SHOTS, "smart-queue-dark.png"), { width: 1100, height: 760 });

  // ---- Start queue runs it now; Pause pauses it (resumed by Start) ----------------------------------------------
  await click("[data-testid=sq-start]");
  await untilStatus(a.id, "downloading");
  check("Start queue runs it whatever the schedule", true);
  await untilRowState(a.id, "downloading");
  check("the row shows Downloading", true);
  check("a started queue says it runs until done", /Started by hand/.test((await text("[data-testid=sq-gate]")) ?? ""));
  await click("[data-testid=sq-pause]");
  await untilStatus(a.id, "paused");
  check("Pause queue pauses its running download", true);
  check("the queue shows Paused", (await queues()).queues.find((x) => x.id === night.id).gate === "paused");
  await click("[data-testid=sq-start]");
  await untilStatus(a.id, "downloading");
  check("Start brings it back, with its progress", (await dl(a.id)).downloaded > 0);

  // ---- per download: Start at... (timeline dot), Start now --------------------------------------------------
  await click("[data-testid=sq-tab-1]");
  await waitFor(async () => (await attr("[data-testid=sq-tab-1]", "aria-pressed")) === "true", "main tab");
  const b = (await invoke("create_download", { request: { url: `${srv.base}/later.bin`, start: false } })).ok;
  await waitFor(() => q(`[data-testid=queue-${b.id}]`), "row b");
  check("added for later: waits for the user", (await rowState(b.id)) === "waiting" && /press Start/.test((await text(`[data-testid=queue-${b.id}] [data-testid=sq-why]`)) ?? ""));
  await rowMenu(b.id, "Start at");
  await waitFor(() => q("[data-testid=sq-at-dialog]"), "the start-at dialog");
  await click("[data-testid=sq-at-dialog] .pill"); // "In 1 hour"
  await click("[data-testid=sq-at-save]");
  await waitFor(async () => !!(await dl(b.id))?.startAt, "start time saved");
  await untilRowState(b.id, "scheduled");
  check("a start time makes it Scheduled", true);
  check("the timeline shows when it starts", await q("[data-testid=sq-timeline-start]"));
  await theme("light");
  await sleep(250);
  await h.screenshot(path.join(SHOTS, "smart-queue-light.png"), { width: 1100, height: 760 });
  await rowMenu(b.id, "Start now");
  await untilStatus(b.id, "downloading");
  check("Start now starts it at once and forgets the start time", (await dl(b.id)).startAt === null);

  // ---- order: one slot, three waiting downloads, moved with the arrows -------------------------------------
  await invoke("set_queue_config", { request: { maxActive: 1 } });
  await invoke("pause_download", { id: b.id });
  await invoke("pause_download", { id: a.id });
  const c1 = (await invoke("create_download", { request: { url: `${srv.base}/c1.bin` } })).ok;
  await untilStatus(c1.id, "downloading");
  await untilRowState(c1.id, "downloading"); // the window follows at the next progress tick
  const ws = [];
  for (const n of ["w1", "w2", "w3"]) ws.push((await invoke("create_download", { request: { url: `${srv.base}/${n}.bin` } })).ok);
  await waitFor(async () => (await count("[data-testid=queue-list] li")) >= 5, "rows");
  const orderOf = () => page("[...document.querySelectorAll('[data-testid=queue-list] li')].map((li) => Number(li.dataset.testid.slice(6)))").then((r) => r.ok);
  const before = await orderOf();
  check("waiting downloads are listed in the order they were added", before.indexOf(ws[0].id) < before.indexOf(ws[1].id) && before.indexOf(ws[1].id) < before.indexOf(ws[2].id), JSON.stringify(before));
  check("the first waiting one cannot move up past the running one", (await disabled(`[data-testid=queue-${ws[0].id}] [data-testid=sq-up]`)) === true);
  await click(`[data-testid=queue-${ws[2].id}] [data-testid=sq-up]`);
  await waitFor(async () => {
    const o = await orderOf();
    return o.indexOf(ws[2].id) < o.indexOf(ws[1].id);
  }, "w3 to move above w2");
  check("the up arrow moves a download up", true);
  await click(`[data-testid=queue-${ws[0].id}] [data-testid=sq-down]`);
  await waitFor(async () => {
    const o = await orderOf();
    return o.indexOf(ws[0].id) > o.indexOf(ws[2].id);
  }, "w1 to move below w3");
  check("the down arrow moves a download down", true);
  await invoke("cancel_download", { id: c1.id });
  await waitFor(async () => (await status(ws[2].id)) === "downloading", "the new first one to start", 20000);
  check("the slot goes to the download the user put first", (await status(ws[0].id)) === "queued" && (await status(ws[1].id)) === "queued");
  check("its row says it waits for a free slot", /free slot/.test((await text(`[data-testid=queue-${ws[0].id}] [data-testid=sq-why]`)) ?? ""));
  for (const w of ws) await invoke("cancel_download", { id: w.id });
  await invoke("set_queue_config", { request: { maxActive: 3 } });

  // ---- the Add dialog offers the queue and when to start --------------------------------------------------
  await nav("downloads");
  await page("(window.dispatchEvent(new CustomEvent('velox:open-add')), true)");
  await waitFor(() => q("dialog[open]"), "the Add dialog");
  check("the Add dialog lets the user pick a queue when there are several", await q("[data-testid=add-queue]"));
  check("and when to start (now, later, at a time)", (await page("[...document.querySelectorAll('[data-testid=add-when] option')].map((o) => o.value).join(',')")).ok === "now,later,at");
  await page("(document.querySelector('dialog[open]')?.close(), true)");
  await nav("queue");

  // ---- clear completed, delete a queue ------------------------------------------------------------------------
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: null } });
  const done = (await invoke("create_download", { request: { url: `${srv.base}/done.bin`, queueId: night.id } })).ok;
  await invoke("queue_action", { id: night.id, action: { kind: "start" } });
  await untilStatus(done.id, "completed", 60000);
  await click(`[data-testid=sq-tab-${night.id}]`);
  await waitFor(() => q("[data-testid=sq-clear]"), "clear completed button");
  await click("[data-testid=sq-clear]");
  await waitFor(async () => !(await dl(done.id)), "the finished download to leave the list");
  check("Clear completed removes the queue's finished downloads", true);
  // Something left in the queue, to see where it goes.
  await invoke("place_download", { request: { id: b.id, queueId: night.id } });
  check("a download can be moved to another queue", (await dl(b.id)).queueId === night.id);
  await click("[data-testid=sq-edit]");
  await waitFor(() => q("[data-testid=sq-delete]"), "delete button");
  await click("[data-testid=sq-delete]");
  await waitFor(async () => (await queues()).queues.length === 1, "the queue to be deleted");
  check("a deleted queue's downloads go to the main queue", (await dl(b.id)).queueId === 1);

  // ---- hostile IPC ------------------------------------------------------------------------------------------
  const rejected = async (cmd, args, why) => {
    const r = await invoke(cmd, args);
    check(`rejected: ${why}`, !!r.err && r.ok === undefined, JSON.stringify(r));
  };
  await rejected("save_queue", { request: { id: null, name: "x", rules: {}, run: "calc.exe" } }, "extra field on a queue");
  await rejected("save_queue", { request: { id: null, name: "x", rules: { when: { kind: "between", start: "25:00", stop: "07:00", days: [true, true, true, true, true, true, true] } } } }, "bad time in a rule");
  await rejected("save_queue", { request: { id: null, name: "", rules: {} } }, "a queue without a name");
  await rejected("delete_queue", { id: 1 }, "deleting the main queue");
  await rejected("queue_action", { id: 1, action: { kind: "shutdown" } }, "unknown queue action");
  await rejected("queue_action", { id: 999, action: { kind: "start" } }, "unknown queue");
  await rejected("place_download", { request: { id: b.id, start: { kind: "at", at: "tomorrow" } } }, "a start time that is not a time");
  await rejected("place_download", { request: { id: b.id, queueId: 999 } }, "a queue that does not exist");
  await rejected("place_download", { request: { id: b.id, step: "sideways" } }, "an unknown step");
  await rejected("create_download", { request: { url: `${srv.base}/x.bin`, startAt: "2026-13-40T99:00" } }, "a bad start time on a new download");

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
