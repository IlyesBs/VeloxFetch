// End-to-end test of the redesigned shell: the frameless title bar and its window buttons, the window
// surface (Mica on Windows 11), the icon rail, the Arabic (right-to-left) and French interface, the 8x8
// connection matrix on running task cards and the Smart throttle HUD morphing the accent colour.
// Writes screenshots to docs/screenshots (redesign-*.png).
//
//   node tests/e2e/desktop-redesign.mjs [path-to-velox-desktop.exe]
//
// Build first (see docs/testing.md). Windows + WebView2 only. Never minimizes or closes the window
// through its buttons (that would end the run); only checks that they are there and named.

import os from "node:os";
import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

const srv = await startFileServer(96 * MB);
const h = createHarness({ port: 9363, appPath: APP });
const { page, invoke } = h;

const ev = async (expr) => (await page(expr)).ok;
const q = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = async (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = (sel) => page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no element ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
const accent = () => ev(`getComputedStyle(document.documentElement).getPropertyValue('--accent').trim().toLowerCase()`);
const theme = (t) => page(`(document.documentElement.setAttribute('data-theme', ${JSON.stringify(t)}), true)`);
const toTop = () => page(`(document.querySelector('[data-testid=main]').scrollTop = 0, true)`);
const toCards = () => page(`(() => { const c = document.querySelector('[data-testid^=download-]'); c && c.scrollIntoView({ block: 'center' }); return true; })()`);
const win11 = process.platform === "win32" && Number(os.release().split(".")[2] ?? 0) >= 22000;

try {
  await h.launch();
  await waitFor(() => q("[data-testid=titlebar]"), "the title bar to render");
  await page(`(() => {
    window.__csp = []; window.__errs = [];
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI));
    window.addEventListener('error', (e) => window.__errs.push(e.message));
    window.addEventListener('unhandledrejection', (e) => window.__errs.push(String(e.reason)));
    return true; })()`);

  // ---- frame ------------------------------------------------------------------------------------
  const platform = { win32: "windows", linux: "linux", darwin: "macos" }[process.platform];
  check("Rust tells the page its frame (platform + surface)", (await ev("document.documentElement.dataset.platform")) === platform);
  const surface = await ev("document.documentElement.dataset.surface");
  if (process.platform === "darwin") check("macOS: the window has the glass surface", surface === "glass", surface);
  else if (process.platform === "linux") check("Linux: an opaque window", surface === "solid", surface);
  else check(win11 ? "Windows 11: the window uses Mica (see-through page over it)" : "Windows 10: an opaque window", surface === (win11 ? "mica" : "solid"), surface);
  if (win11) {
    const bg = await ev("getComputedStyle(document.body).backgroundColor");
    check("with Mica the page background is translucent", /rgba\(.*, 0\.\d+\)/.test(bg), bg);
  }
  check("the title bar is a drag region", await q("[data-testid=titlebar][data-tauri-drag-region]"));
  const winBtns = await ev(`[...document.querySelectorAll('[data-testid=window-buttons] button')].map((b) => b.getAttribute('aria-label')).join('|')`);
  check("the UI draws named minimize / maximize / close buttons", winBtns === "Minimize|Maximize|Close", winBtns);
  const before = (await invoke("window_control", { action: "isMaximized" })).ok;
  const toggled = (await invoke("window_control", { action: "toggleMaximize" })).ok;
  const back = (await invoke("window_control", { action: "toggleMaximize" })).ok;
  if (process.platform === "linux" && !h.windowManager) console.log("SKIP  maximize / restore: no window manager on the test display (install kwin, or set VELOX_E2E_DISPLAY)");
  else check("maximize / restore go through window_control on this window", typeof before === "boolean" && toggled === !before && back === before, `${before} ${toggled} ${back}`);
  const bogus = await invoke("window_control", { action: "destroy" });
  check("window_control refuses unknown actions", !!bogus.err, JSON.stringify(bogus));
  check("the title bar names the current page", (await text("[data-testid=page-title]")) === "Downloads");

  // ---- rail -------------------------------------------------------------------------------------
  check("the sidebar holds Downloads, Active, Queue, Completed, History and Settings", (await ev(`document.querySelectorAll('[data-testid=rail] nav button').length`)) === 5 && (await q("[data-testid=rail] [data-testid=nav-settings]")));
  await click("[data-testid=nav-queue]");
  await waitFor(async () => (await text("[data-testid=page-title]")) === "Queue", "the page title to follow the rail");
  check("the active rail button is marked as the current page", (await ev(`document.querySelector('[data-testid=nav-queue]').getAttribute('aria-current')`)) === "page");
  await click("[data-testid=nav-downloads]");

  // ---- running task cards -----------------------------------------------------------------------
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 4 * MB } });
  for (const name of ["Fedora-Workstation-41.iso", "blender-4.3-windows-x64.zip", "dataset-2026.tar.gz"]) {
    await invoke("create_download", { request: { url: `${srv.base}/${name}` } });
  }
  await waitFor(() => q("[data-testid^=download-][data-status=downloading] [data-testid=thread-matrix]"), "a running card with its matrix", 15000);
  await sleep(1500);
  const matrix = await ev(`(() => {
    const card = document.querySelector('[data-testid^=download-][data-status=downloading]');
    const m = card.querySelector('.matrix');
    return { cells: m.children.length, lit: m.querySelectorAll('i.on').length, label: m.getAttribute('aria-label'),
             pulse: m.getAnimations({ subtree: true }).filter((a) => a.animationName === 'flicker').map((a) => a.playbackRate), anim: getComputedStyle(m.querySelector('i.on') ?? m).animationName };
  })()`);
  check("the matrix is 8x8 (64 cells)", matrix.cells === 64, JSON.stringify(matrix));
  check("lit cells = the download's live connections, and say so to screen readers", matrix.lit > 0 && matrix.label.startsWith(`${matrix.lit} of 64`), JSON.stringify(matrix));
  // the pace follows the throughput through the animations' playback rate (a duration change made the cells jump)
  check("lit cells pulse with a CSS animation timed by throughput", matrix.anim === "flicker" && matrix.pulse.length === matrix.lit && matrix.pulse.every((r) => r > 0 && r <= 1.6 / 0.45 + 1e-9), JSON.stringify(matrix));
  const fill = await ev(`getComputedStyle(document.querySelector('[data-testid^=download-][data-status=downloading] [role=progressbar] > div')).backgroundImage`);
  check("the running progress track is the accent gradient", fill.includes("linear-gradient"), fill);

  await theme("dark");
  await sleep(400);
  await h.screenshot(path.join(SHOTS, "redesign-dashboard-dark.png"), { width: 1536, height: 1024 });
  await toCards();
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "redesign-tasks-dark.png"), { width: 1240, height: 860 });
  await theme("light");
  await sleep(400);
  await h.screenshot(path.join(SHOTS, "redesign-tasks-light.png"), { width: 1240, height: 860 });
  await toTop();
  await h.screenshot(path.join(SHOTS, "redesign-dashboard-light.png"), { width: 1536, height: 1024 });

  // ---- Smart throttle HUD -----------------------------------------------------------------------
  await theme("dark");
  const cyan = await accent();
  check("dark theme accent is neon cyan", cyan === "#00e1f5", cyan);
  await click("[data-testid=hud-toggle]");
  await waitFor(async () => !!(await ev("document.documentElement.dataset.throttle")), "the throttle to switch on");
  const amber = await accent();
  check("turning the HUD on morphs the accent to stealth amber", amber === "#ffb347", amber);
  check("the HUD switch and gauge say it is on", (await ev(`document.querySelector('[data-testid=hud-switch]').getAttribute('aria-checked')`)) === "true");
  check("the status bar shows the Smart throttle", await q("[data-testid=status-throttle]"));
  await sleep(500);
  await h.screenshot(path.join(SHOTS, "redesign-throttle-dark.png"), { width: 1240, height: 860 });
  await click("[data-testid=hud-switch]");
  await waitFor(async () => !(await ev("document.documentElement.dataset.throttle")), "the throttle to switch off");
  check("turning it off brings the cyan back", (await accent()) === "#00e1f5");
  const setting = (await invoke("get_smart_throttle")).ok;
  check("the HUD changed the real setting (and back)", setting && setting.enabled === false);

  // ---- responsive: every window size from a big screen down to the minimum (640 x 480) ----------
  for (const [width, height] of [[1920, 1080], [1536, 1024], [1280, 800], [1024, 700], [860, 640], [640, 480]]) {
    await h.cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
    await sleep(350);
    const r = await ev(`(() => {
      const main = document.querySelector('[data-testid=main]');
      const mr = main.getBoundingClientRect();
      const inside = (sel) => [...document.querySelectorAll(sel)].every((e) => { const b = e.getBoundingClientRect(); return b.width === 0 || (b.left >= mr.left - 1 && b.right <= mr.right + 1); });
      const vis = (sel) => { const e = document.querySelector(sel); if (!e) return false; const b = e.getBoundingClientRect(); return b.width > 0 && b.height > 0 && b.left >= -1 && b.right <= innerWidth + 1; };
      return {
        pageScroll: document.documentElement.scrollWidth - innerWidth,
        mainScroll: main.scrollWidth - main.clientWidth,
        folded: document.querySelector('[data-testid=rail]').dataset.folded,
        huds: document.querySelectorAll('[data-testid=throttle-hud]').length,
        cards: inside('[data-testid^=download-]') && inside('[data-testid=bento] > div') && inside('[data-testid=list-toolbar]') && inside('[data-testid=dash-side] > section'),
        add: vis('[data-testid=add-button]') && vis('[data-testid=url-input]'),
        chrome: vis('[data-testid=window-buttons]') && vis('[data-testid=theme-toggle]') && vis('[data-testid=lang-open]') && vis('[data-testid=statusbar]'),
        nav: [...document.querySelectorAll('[data-testid=rail] nav button')].every((b) => b.getBoundingClientRect().width > 0),
      };
    })()`);
    const label = `${width}x${height}`;
    check(`${label}: nothing scrolls sideways (page and content)`, r.pageScroll <= 0 && r.mainScroll <= 0, JSON.stringify(r));
    check(`${label}: the sidebar is ${width < 900 ? "folded to icons" : "open"} and the Smart throttle is shown once`, r.folded === String(width < 900) && r.huds === 1, JSON.stringify(r));
    check(`${label}: cards, toolbar and side panels stay inside the page; link bar, title bar and status bar reachable`, r.cards && r.add && r.chrome && r.nav, JSON.stringify(r));
    await h.screenshot(path.join(SHOTS, `responsive-${label}.png`));
  }
  await h.cdp.send("Emulation.clearDeviceMetricsOverride");
  await sleep(300);

  // ---- languages --------------------------------------------------------------------------------
  await click("[data-testid=lang-fr]");
  await waitFor(async () => (await ev("document.documentElement.lang")) === "fr", "French");
  check("French: the dashboard speaks French", (await text("[data-testid=tile-speed]"))?.toLowerCase().includes("vitesse actuelle"));
  check("French: still left-to-right", (await ev("document.documentElement.dir")) === "ltr");

  await click("[data-testid=lang-ar]");
  await waitFor(async () => (await ev("document.documentElement.dir")) === "rtl", "Arabic, right-to-left");
  check("Arabic: <html lang=ar dir=rtl>", (await ev("document.documentElement.lang")) === "ar");
  check("Arabic: the Cairo font is applied", /Cairo/.test(await ev("getComputedStyle(document.body).fontFamily")));
  const sides = await ev(`(() => { const r = document.querySelector('[data-testid=rail]').getBoundingClientRect(); return { rail: r.left, width: innerWidth }; })()`);
  check("Arabic: the rail moves to the right edge (mirrored layout)", sides.rail > sides.width / 2, JSON.stringify(sides));
  check("Arabic: the window buttons move to the left", (await ev(`document.querySelector('[data-testid=window-buttons]').getBoundingClientRect().left`)) < sides.width / 2);
  check("Arabic: labels are translated", (await text("[data-testid=page-title]")) === "التنزيلات");
  const origin = await ev(`getComputedStyle(document.querySelector('[data-testid^=download-] [role=progressbar] > div')).transformOrigin`);
  check("Arabic: progress fills from the right", origin.startsWith(String(parseFloat(origin))) && parseFloat(origin) > 0, origin);
  check("Arabic: no letter-spacing on Arabic labels (it breaks the joins)", (await ev(`getComputedStyle(document.querySelector('.label')).letterSpacing`)) === "normal");
  await theme("dark");
  await sleep(500);
  await h.screenshot(path.join(SHOTS, "redesign-arabic-dark.png"), { width: 1240, height: 860 });
  await toCards();
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "redesign-arabic-tasks-dark.png"), { width: 1240, height: 860 });

  await click("[data-testid=lang-en]");
  await waitFor(async () => (await ev("document.documentElement.dir")) === "ltr", "back to English");
  check("English again: left-to-right, English labels", (await text("[data-testid=page-title]")) === "Downloads");
  check("the language choice is remembered", (await ev(`localStorage.getItem('velox.lang')`)) === "en");

  // ---- clean ------------------------------------------------------------------------------------
  check("no Content-Security-Policy violations", (await ev("window.__csp.length")) === 0, String(await ev("window.__csp.join(', ')")));
  check("no script errors", (await ev("window.__errs.length")) === 0, String(await ev("window.__errs.join(' | ')")));
  check("every button has an accessible name", (await ev(`[...document.querySelectorAll('button')].filter((b) => !(b.getAttribute('aria-label') || b.innerText.trim() || b.getAttribute('title'))).length`)) === 0);
} catch (e) {
  check("redesign e2e run", false, e.stack ?? String(e));
} finally {
  srv.close();
  await h.cleanup();
}

finish();
