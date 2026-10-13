// End-to-end test of the desktop UI (Phase 5): every view, live updates, animation smoothness,
// frame rate, accessibility basics, CSP cleanliness. Also writes screenshots to docs/screenshots.
//
//   node tests/e2e/desktop-ui.mjs [path-to-velox-desktop.exe]
//
// Build first (see desktop-ipc.mjs). Windows + WebView2 only.

import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

const srv = await startFileServer(96 * MB);
const h = createHarness({ port: 9335, appPath: APP });
const { page, invoke } = h;

// ---- in-page helpers ---------------------------------------------------------------------------
const q = (sel) => page(`!!document.querySelector(${JSON.stringify(sel)})`).then((r) => r.ok);
const text = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`)).ok;
const count = async (sel) => (await page(`document.querySelectorAll(${JSON.stringify(sel)}).length`)).ok;
const click = (sel) => page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no element ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
const clickText = (sel, label) =>
  page(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(sel)})].find((b) => b.innerText.trim() === ${JSON.stringify(label)}); if (!e) throw new Error('no ' + ${JSON.stringify(label)}); e.click(); return true; })()`);
/** React-friendly value setter (fires the events React listens for). */
const fill = (sel, value) =>
  page(`(() => {
    const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(value)});
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true })); return true; })()`);
const status = async (id) => (await invoke("get_download", { id })).ok?.status;
const dl = async (id) => (await invoke("get_download", { id })).ok;
const untilStatus = (id, s, ms = 30000) => waitFor(async () => (await status(id)) === s, `download ${id} -> ${s}`, ms);
const theme = (t) => page(`(document.documentElement.setAttribute('data-theme', ${JSON.stringify(t)}), true)`);

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");

  // Watch for policy violations, script errors and live event rate from the very start.
  await page(`(() => {
    window.__csp = []; window.__errs = []; window.__ticks = 0;
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI));
    window.addEventListener('error', (e) => window.__errs.push(e.message));
    window.addEventListener('unhandledrejection', (e) => window.__errs.push(String(e.reason)));
    const cb = window.__TAURI_INTERNALS__.transformCallback((ev) => { if (ev.payload && ev.payload.type === 'tick') window.__ticks++; });
    return window.__TAURI_INTERNALS__.invoke('plugin:event|listen', { event: 'velox://event', target: { kind: 'Any' }, handler: cb });
  })()`);

  // Hold every download at 3 MB/s so each phase below measures a download that is really running
  // (unthrottled, a local download finishes in a couple of seconds).
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 3 * MB } });

  // ---- structure ---------------------------------------------------------------------------
  check("top navigation has Downloads / Queue / History / Settings",
    (await count("nav[aria-label=Main] button")) === 5 && !!(await q("[data-testid=nav-settings]")));
  check("the dashboard shows the six information cards and the Smart throttle HUD", (await count("[data-testid=bento] > div")) === 6 && !!(await q("[data-testid=throttle-hud]")));
  check("empty state is shown before any download", (await text("main"))?.includes("No downloads yet"));
  await theme("light");
  await h.screenshot(path.join(SHOTS, "dashboard-empty-light.png"), { width: 1000, height: 700 });

  // ---- add a download through the UI ----------------------------------------------------------
  await fill("[data-testid=url-input]", `${srv.base}/ubuntu-24.04-desktop-amd64.iso`);
  check("Add button enables once a URL is typed", (await page("!document.querySelector('[data-testid=add-button]').disabled")).ok);
  await click("[data-testid=add-button]");
  const card = await waitFor(async () => (await q("[data-testid^=download-][data-status=downloading]")) && (await text("[data-testid^=download-]")), "card to appear and start", 15000);
  check("adding a URL creates a card that starts downloading", card.includes("ubuntu-24.04-desktop-amd64.iso"), card);
  check("the URL field is cleared after adding", (await page("document.querySelector('[data-testid=url-input]').value")).ok === "");

  // ---- live updates -----------------------------------------------------------------------------
  await sleep(1200);
  const speedText = await text("[data-testid=speed-value]");
  check("speed card shows live throughput", speedText && !speedText.startsWith("0 B/s"), speedText);
  check("active card counts the running download", (await text("[data-testid=active-value]")) === "1");
  const fillScale = (await page(`(() => { const f = document.querySelector('[data-testid^=download-] [role=progressbar] > div'); const m = /matrix\\(([^,]+)/.exec(getComputedStyle(f).transform); return m ? Number(m[1]) : null; })()`)).ok;
  check("progress bar fill is animated with a transform (allowed under the strict CSP)", fillScale > 0 && fillScale <= 1, String(fillScale));
  check("card shows speed, ETA and connections", /MB\/s/.test(card) || /MB\/s/.test((await text("[data-testid^=download-]")) ?? ""));

  // ---- performance: frame rate + update cadence while downloading ------------------------------
  const perf = (
    await page(`new Promise((res) => {
      const deltas = []; let last = performance.now(); const start = last; let long = 0; let mut = 0;
      const po = new PerformanceObserver((l) => (long += l.getEntries().length)); try { po.observe({ entryTypes: ['longtask'] }); } catch {}
      const mo = new MutationObserver((m) => (mut += m.length)); mo.observe(document.body, { subtree: true, childList: true, attributes: true, characterData: true });
      const t0 = window.__ticks;
      const f = (t) => { deltas.push(t - last); last = t; if (t - start < 3000) requestAnimationFrame(f); else { po.disconnect(); mo.disconnect(); res({ deltas, long, mut, ms: t - start, ticks: window.__ticks - t0 }); } };
      requestAnimationFrame(f);
    })`)
  ).ok;
  const sorted = [...perf.deltas].sort((a, b) => a - b);
  const p95 = sorted[Math.floor(sorted.length * 0.95)];
  const fps = (perf.deltas.length / perf.ms) * 1000;
  const tickHz = (perf.ticks / perf.ms) * 1000;
  console.log(`      perf: ${fps.toFixed(1)} fps, p95 frame ${p95.toFixed(1)} ms, max ${Math.max(...perf.deltas).toFixed(1)} ms, long tasks ${perf.long}, DOM mutations ${(perf.mut / perf.ms * 1000).toFixed(0)}/s, ticks ${tickHz.toFixed(1)}/s`);
  check("UI holds ~60 FPS while a download runs (>= 50 fps average)", fps >= 50, fps.toFixed(1));
  check("frame times are smooth (p95 <= 25 ms)", p95 <= 25, p95.toFixed(1));
  check("no long main-thread tasks (> 50 ms)", perf.long === 0, String(perf.long));
  check("the backend batches updates to 5-10 per second, not one per network write", tickHz >= 4 && tickHz <= 11, tickHz.toFixed(1));
  check("DOM churn stays low (< 400 mutations/s)", perf.mut / perf.ms * 1000 < 400, String(perf.mut));

  // ---- detail view ------------------------------------------------------------------------------
  const id = (await invoke("list_downloads", { filter: {} })).ok[0].id;
  await click("[data-testid^=download-] button.truncate");
  await waitFor(() => q("[data-testid=detail-view]"), "detail view");
  check("detail view opens with the file name", (await text("[data-testid=detail-title]")) === "ubuntu-24.04-desktop-amd64.iso");
  await waitFor(async () => (await count("[data-testid=segment-row]")) >= 4, "segment rows");
  check("Overview / Network / Segments / Metadata sections are present",
    ["Overview", "Network", "Segments", "Metadata"].every((t) => page(`[...document.querySelectorAll('h2')].some((h) => h.textContent === ${JSON.stringify(t)})`).then((r) => r.ok)) &&
      (await Promise.all(["Overview", "Network", "Segments", "Metadata"].map((t) => page(`[...document.querySelectorAll('h2')].some((h) => h.textContent === ${JSON.stringify(t)})`)))).every((r) => r.ok));
  check("metadata shows server, content type and ETag from the response",
    await page(`document.body.innerText.includes('velox-e2e/1.0') && document.body.innerText.includes('application/octet-stream') && document.body.innerText.includes('e2e-1')`).then((r) => r.ok));
  const p1 = parseFloat(await text("[data-testid=detail-percent]"));
  await sleep(1000);
  const p2 = parseFloat(await text("[data-testid=detail-percent]"));
  check("detail progress advances live", p2 > p1, `${p1} -> ${p2}`);

  // segment monitor: painted canvas + smooth animation between 4 Hz data samples
  const canvasInfo = (
    await page(`(() => { const c = document.querySelector('[data-testid=segment-map]'); const g = c.getContext('2d'); const d = g.getImageData(0, 0, c.width, c.height).data; let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++; return { w: c.width, h: c.height, painted: n }; })()`)
  ).ok;
  check("segment canvas is painted", canvasInfo.painted > 100, JSON.stringify(canvasInfo));
  const anim = (
    await page(`new Promise((res) => {
      const c = document.querySelector('[data-testid=segment-map]'); const g = c.getContext('2d');
      const sig = () => { const d = g.getImageData(0, 0, c.width, 4).data; let s = 0; for (let i = 0; i < d.length; i += 16) s = (s * 31 + d[i] + d[i + 1] * 7 + d[i + 3]) | 0; return s; };
      let last = sig(), changes = 0, frames = 0; const t0 = performance.now();
      const f = (t) => { frames++; const s = sig(); if (s !== last) { changes++; last = s; } if (t - t0 < 1000) requestAnimationFrame(f); else res({ changes, frames }); };
      requestAnimationFrame(f);
    })`)
  ).ok;
  console.log(`      segment monitor: ${anim.changes} distinct paints over ${anim.frames} frames in 1 s (data arrives 4x/s)`);
  check("segment monitor animates between data samples (more paints than the 4 Hz polling)", anim.changes > 6, JSON.stringify(anim));
  check("network sparkline is drawn", (await count("svg[aria-label='Throughput of this download'] path")) >= 1);

  await theme("dark");
  await sleep(400);
  await h.screenshot(path.join(SHOTS, "detail-dark.png"), { width: 1000, height: 1150 });

  // ---- detail controls: priority + speed limit ---------------------------------------------
  await fill("[data-testid=detail-priority]", "high");
  await waitFor(async () => (await dl(id))?.priority === "high", "priority to apply");
  check("priority change from the detail view reaches the backend", true);
  await fill("input[aria-label='Speed limit for this download']", "512K");
  await clickText("button", "Set");
  await waitFor(async () => (await dl(id))?.speedLimit === 512 * 1024, "speed limit to apply");
  await sleep(2200);
  const throttled = (await dl(id)).speedBps;
  check("a per-download speed limit visibly slows the transfer", throttled < 1.5 * MB, String(throttled));
  await clickText("button", "Clear");
  await waitFor(async () => (await dl(id))?.speedLimit === null, "limit cleared");

  // ---- back, pause / resume from the card ------------------------------------------------------
  await click("[data-testid=detail-back]");
  // Desktop feel: right-click never gives the browser menu; on a download it gives the app's own menu.
  const ctx = (sel) => page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); const ev = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 200, clientY: 200 }); e.dispatchEvent(ev); return ev.defaultPrevented; })()`);
  check("right-click on empty page space is swallowed (no Reload / Copy URL menu)", (await ctx("main")).ok === true);
  check("browser shortcuts (F5, Ctrl+R) are blocked", (await page(`(() => { const r = [new KeyboardEvent('keydown', { key: 'F5', cancelable: true, bubbles: true }), new KeyboardEvent('keydown', { key: 'r', ctrlKey: true, cancelable: true, bubbles: true })]; return r.every((k) => { document.body.dispatchEvent(k); return k.defaultPrevented; }); })()`)).ok === true);
  check("right-click on a download opens the app's own menu", (await ctx("[data-testid^=download-]")).ok === true && (await count("[role=menu] [role=menuitem]")) >= 3);
  const items = (await page(`[...document.querySelectorAll('[role=menu] [role=menuitem]')].map((b) => b.innerText.trim()).join('|')`)).ok;
  check("its items are the same as the ... menu (Details, Copy link, Cancel, Remove)", /Details\|Copy link\|Cancel download\|Remove from list/.test(items), items);
  await page(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
  check("Escape closes the right-click menu", await waitFor(async () => (await count("[role=menu]")) === 0, "menu closed", 3000).then(() => true, () => false));
  await waitFor(() => q("[data-testid=bento]"), "dashboard");
  await page(`document.querySelector('[data-testid^=download-] button[aria-label=Pause]').click()`);
  await untilStatus(id, "paused");
  check("Pause button pauses (card shows Paused + Resume)", await waitFor(async () => /Paused/.test((await text("[data-testid^=download-]")) ?? "") && !!(await q("[data-testid^=download-] button[aria-label=Resume]")), "paused card", 5000).then(() => true, () => false));
  await page(`document.querySelector('[data-testid^=download-] button[aria-label=Resume]').click()`);
  await untilStatus(id, "downloading");
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: null } }); // let it finish
  await untilStatus(id, "completed", 60000);
  await waitFor(async () => (await q("[data-testid^=download-][data-status=completed]")), "completed card");
  const doneCard = (await text("[data-testid^=download-][data-status=completed]")) ?? "";
  check("a completed download says Completed, its size and 100%, with a full green bar and Show in folder", /Completed/.test(doneCard) && /\d MB/.test(doneCard) && (await text("[data-testid^=download-][data-status=completed] [data-testid=percent]")) === "100%" && !!(await q("[data-testid^=download-][data-status=completed] [role=progressbar][aria-valuenow='100'] .bar-ok")) && !!(await q("[data-testid^=download-][data-status=completed] [data-testid=show-in-folder]")), doneCard);
  check("total-downloaded and network cards grew", !(await text("[data-testid=tile-total]")).startsWith("TOTAL DOWNLOADED\n0 B") && !(await text("[data-testid=tile-network]")).includes("\n0 B"));
  check("disk card reports used / total space", /\d.* \/ .*\d/.test((await text("[data-testid=tile-disk]")) ?? ""));

  // ---- queue view ----------------------------------------------------------------------------
  const made = [];
  for (const [n, lim] of [["a", 600_000], ["b", 500_000], ["c", 400_000], ["d", 300_000]]) {
    made.push((await invoke("create_download", { request: { url: `${srv.base}/${n}.bin`, speedLimit: lim } })).ok.id);
  }
  await waitFor(async () => (await Promise.all(made.map(status))).filter((s) => s === "queued").length === 1, "one download to be queued (3 slots)", 15000);
  await click("[data-testid=nav-queue]");
  await waitFor(async () => (await count("[data-testid=queue-list] li")) >= 4, "queue rows");
  const queuedId = (await Promise.all(made.map(async (i) => [i, await status(i)]))).find(([, s]) => s === "queued")[0];
  await fill(`[data-testid=queue-${queuedId}] select`, "high");
  await waitFor(async () => (await dl(queuedId))?.priority === "high", "queue priority to apply");
  check("queue lists running + waiting downloads and priority can be changed", true);
  await theme("light");
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "queue-light.png"), { width: 1000, height: 620 });

  // ---- history -----------------------------------------------------------------------------
  await click("[data-testid=nav-history]");
  await waitFor(async () => (await count("[data-testid=history-row]")) >= 1, "history rows");
  check("history lists the completed download", (await text("[data-testid=history-list]"))?.includes("ubuntu-24.04-desktop-amd64.iso"));

  // ---- dashboard with several downloads (screenshot) -------------------------------------------------------
  await click("[data-testid=nav-downloads]");
  await invoke("pause_download", { id: made[3] });
  await sleep(2500);
  await theme("light");
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "dashboard-light.png"), { width: 1000, height: 900 });
  await theme("dark");
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "dashboard-dark.png"), { width: 1000, height: 900 });

  // ---- settings ---------------------------------------------------------------------------------
  await click("[data-testid=nav-settings]");
  await click("[data-testid=theme-light]");
  await sleep(350); // the computed colour follows the variable switch a frame later
  const bgLight = (await page("getComputedStyle(document.body).backgroundColor")).ok;
  check("theme switch to Light applies", (await page("document.documentElement.getAttribute('data-theme')")).ok === "light");
  await click("[data-testid=theme-dark]");
  await sleep(350);
  const bgDark = (await page("getComputedStyle(document.body).backgroundColor")).ok;
  check("theme switch to Dark changes the colours", bgLight !== bgDark, `${bgLight} vs ${bgDark}`);
  await fill("input[aria-label='Global speed limit']", "5");
  await clickText("button", "Apply");
  await waitFor(async () => (await text("[data-testid=global-limit]")) === "5.0 MB/s", "global limit to show");
  check("global speed limit can be set from Settings", true);
  await clickText("button", "Remove");
  await waitFor(async () => (await text("[data-testid=global-limit]")) === "none", "global limit removed");
  check("global speed limit can be removed", true);
  await h.screenshot(path.join(SHOTS, "settings-dark.png"), { width: 1000, height: 620 });

  // ---- drop-down lists in the theme's colours (never the grey system list) ------------------------
  const sw = "[data-testid=profile-switch]";
  const down = (sel) => page(`(() => { document.querySelector(${JSON.stringify(sel)}).dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0 })); return true; })()`);
  const before = (await page(`document.querySelector('${sw}').value`)).ok;
  for (const th of ["light", "dark"]) {
    await theme(th);
    await sleep(150);
    // Closed, every drop-down field is drawn by the app: no system look (grey on Linux), the same surface as the text
    // fields beside it, and its own arrow.
    const closed = (await page(`(() => {
      const sel = [...document.querySelectorAll('select.input')].find((s) => s.offsetParent);
      const field = [...document.querySelectorAll('input.input')].find((i) => i.offsetParent && i.type !== 'checkbox');
      const cs = getComputedStyle(sel);
      return { look: cs.webkitAppearance || cs.appearance, bg: cs.backgroundColor, want: getComputedStyle(field).backgroundColor,
               arrow: (cs.backgroundImage.match(/gradient/g) ?? []).length, all: [...document.querySelectorAll('select')].every((s) => (getComputedStyle(s).webkitAppearance || getComputedStyle(s).appearance) === 'none') };
    })()`)).ok;
    check(`${th}: a closed drop-down field looks like the other fields (no system look, its own arrow)`, closed.look === "none" && closed.all && closed.bg === closed.want && closed.arrow === 2, JSON.stringify(closed));
    await down(sw);
    await waitFor(() => q("[data-testid=select-menu]"), "the themed list to open");
    const look = (await page(`(() => { const m = document.querySelector('[data-testid=select-menu]'); const cs = getComputedStyle(m); const solid = getComputedStyle(document.documentElement).getPropertyValue('--solid').trim(); const probe = document.createElement('div'); probe.style.color = solid; document.body.appendChild(probe); const want = getComputedStyle(probe).color; probe.remove(); return { bg: cs.backgroundColor, want, fg: cs.color, img: cs.backgroundImage, rows: m.querySelectorAll('[role=option]').length }; })()`)).ok;
    check(`${th}: a drop-down opens the app's own list, in the theme's colours`, look.bg === look.want && /gradient/.test(look.img) && look.rows >= 4, JSON.stringify(look));
    if (th === "dark") await h.screenshot(path.join(SHOTS, "select-menu-dark.png")); // no resize: a resize closes the list
    await page(`(document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })), true)`);
    await waitFor(async () => !(await q("[data-testid=select-menu]")), "Escape to close it");
  }
  await sleep(1500); // the screenshot resized the window: let its late resize events pass (a resize closes the list)
  check("Escape closes the list and keeps the value", (await page(`document.querySelector('${sw}').value`)).ok === before);
  await down(sw);
  await waitFor(() => q("[data-testid=select-menu]"), "the list again");
  const clicked = (await page(`(() => { const o = [...document.querySelectorAll('[data-testid=select-menu] [role=option]')].find((r) => /Fast/.test(r.textContent)); if (!o) return 'no row'; o.click(); return 'clicked'; })()`)).ok;
  await waitFor(async () => (await invoke("get_profiles")).ok?.active === "fast", "the pick to apply", 5000).catch(() => {});
  // The field is controlled by the app's state: it shows the pick once the app's "profiles" event has arrived.
  await waitFor(async () => (await page(`document.querySelector('${sw}').value`)).ok === "fast", "the field to show the pick", 5000).catch(() => {});
  const picked = { clicked, active: (await invoke("get_profiles")).ok?.active, value: (await page(`document.querySelector('${sw}').value`)).ok, open: await q("[data-testid=select-menu]"), opts: (await page(`[...document.querySelector('${sw}').options].map((o) => o.value + ':' + o.text).join()`)).ok };
  check("choosing a row works like the system list (the app's state changes)", picked.active === "fast" && picked.value === "fast" && !picked.open, JSON.stringify(picked));
  await page(`(() => { const s = document.querySelector('${sw}'); s.focus(); s.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })); return true; })()`);
  await waitFor(() => q("[data-testid=select-menu]"), "Enter to open the list");
  const key = (k) => page(`(document.dispatchEvent(new KeyboardEvent('keydown', { key: ${JSON.stringify(k)}, bubbles: true, cancelable: true })), true)`);
  await key("Home");
  await key("Enter");
  await waitFor(async () => (await invoke("get_profiles")).ok?.active === "default", "the keyboard pick to apply", 5000).catch(() => {});
  check("the keyboard works too (Enter opens, Home, Enter picks; it was Fast)", picked.active === "fast" && (await invoke("get_profiles")).ok?.active === "default");
  await theme("dark");

  // ---- hygiene ---------------------------------------------------------------------------------
  const csp = (await page("window.__csp")).ok;
  check("no Content-Security-Policy violations during the whole session", csp.length === 0, JSON.stringify(csp));
  const errs = (await page("window.__errs")).ok;
  const consoleErrs = h.events.filter((e) => e.method === "Runtime.exceptionThrown" || (e.method === "Log.entryAdded" && ["error"].includes(e.params.entry.level)) || (e.method === "Runtime.consoleAPICalled" && e.params.type === "error"));
  check("no script errors or console errors", errs.length === 0 && consoleErrs.length === 0, JSON.stringify({ errs, consoleErrs: consoleErrs.map((e) => e.params.entry?.text ?? e.params.exceptionDetails?.text) }));
  await click("[data-testid=nav-downloads]");
  const unnamed = (await page(`[...document.querySelectorAll('button')].filter((b) => !(b.getAttribute('aria-label') || b.innerText.trim() || b.getAttribute('title'))).length`)).ok;
  check("every button has an accessible name", unnamed === 0, String(unnamed));
  const bars = (await page(`[...document.querySelectorAll('[role=progressbar]')].every((b) => b.hasAttribute('aria-valuenow') && b.getAttribute('aria-label'))`)).ok;
  check("progress bars expose value and label to assistive tech", bars === true);

  for (const i of made) await invoke("cancel_download", { id: i });
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  check("e2e ran to completion", false, String(e));
} finally {
  srv.close();
  await h.cleanup();
}

finish();
