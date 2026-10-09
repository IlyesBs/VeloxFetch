// End-to-end test of how fast the real desktop app feels: how soon its window shows the interface after launch, and
// the frame rate of the dashboard with six running downloads (each card animates its connection matrix), still and
// while scrolling. Prints the numbers; the checks hold on a virtual display (software drawing) too.
//
//   node tests/e2e/desktop-perf.mjs [path-to-debug-velox-desktop]
//
// On Linux, VELOX_E2E_DISPLAY=:0 measures on the real screen (GPU); PERF_WAYLAND=1 there runs it on Wayland itself.

import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const MB = 1024 * 1024;

const srv = await startFileServer(300 * MB);
srv.rateBps = 6 * MB; // a 6 MB/s line: the downloads run for the whole test
const env = process.env.PERF_WAYLAND ? { WAYLAND_DISPLAY: process.env.WAYLAND_DISPLAY || "wayland-0", GDK_BACKEND: "wayland" } : {};
const h = createHarness({ port: 9373, appPath: APP, env });
const { page, invoke } = h;
const ev = async (e) => (await page(e)).ok;

/** Frame rate over `ms` (requestAnimationFrame), the 95th-percentile frame and the longest pause of the page. */
const frames = (ms, during = "") =>
  ev(`new Promise((res) => {
    const d = []; let last = performance.now(); const s = last;
    ${during}
    const f = (t) => { d.push(t - last); last = t; if (t - s < ${ms}) requestAnimationFrame(f); else {
      const so = [...d].sort((a, b) => a - b);
      res({ fps: +(d.length / (t - s) * 1000).toFixed(1), p95: +so[Math.floor(so.length * 0.95)].toFixed(1), max: +so[so.length - 1].toFixed(1) }); } };
    requestAnimationFrame(f);
  })`);
const SCROLL = `const m = document.querySelector('[data-testid=main]'); let dir = 1;
  const sc = setInterval(() => { m.scrollTop += dir * 40; if (m.scrollTop + m.clientHeight >= m.scrollHeight - 2 || m.scrollTop <= 0) dir = -dir; }, 8);
  setTimeout(() => clearInterval(sc), 2900);`;

try {
  console.log(`app: ${APP}\n`);
  const launchedAt = Date.now();
  await h.launch();
  await waitFor(() => ev("!!document.querySelector('[data-testid=throttle-hud]')"), "the interface");

  // ---- start: the page's own clock (0 = the web view started loading it) ----------------------------------------
  await sleep(800);
  const start = await ev(`(() => { const n = performance.getEntriesByType('navigation')[0]; const p = performance.getEntriesByType('paint').find((e) => e.name === 'first-contentful-paint');
    return { script: Math.round(n.domContentLoadedEventEnd), firstPaint: p ? Math.round(p.startTime) : null, ready: window.__veloxReadyAt ?? null, paintedAt: p ? Math.round(performance.timeOrigin + p.startTime) : null }; })()`);
  if (start.paintedAt) console.log(`      launch -> first paint: ${start.paintedAt - launchedAt} ms (includes starting the test driver)`);
  console.log(`      start: page ready ${start.script} ms, window asked to show at ${start.ready} ms, first paint ${start.firstPaint} ms`);
  check("the window is asked to show as soon as the interface is on the page (< 150 ms after the script ran)", start.ready !== null && start.ready - start.script < 150, JSON.stringify(start));
  check("the interface is painted within a second of the page loading", start.firstPaint !== null && start.firstPaint < 1000, JSON.stringify(start));

  // ---- frame rate ---------------------------------------------------------------------------------------------
  const idle = await frames(2000);
  console.log(`      idle dashboard: ${JSON.stringify(idle)}`);
  for (const n of ["a.iso", "b.zip", "c.tar.gz", "d.bin", "e.mkv", "f.iso"]) await invoke("create_download", { request: { url: `${srv.base}/${n}` } });
  await waitFor(async () => (await ev("document.querySelectorAll('[data-testid^=download-][data-status=downloading]').length")) >= 3, "the downloads to run (as many as the queue allows at once)", 20000);
  await sleep(2500);
  // PERF_CSS: a stylesheet laid over the page before measuring (to find out what costs, by hand).
  if (process.env.PERF_CSS) await ev(`(() => { const sh = new CSSStyleSheet(); sh.replaceSync(${JSON.stringify(process.env.PERF_CSS)}); document.adoptedStyleSheets = [sh]; return true; })()`);
  const running = await frames(3000);
  console.log(`      downloads running: ${JSON.stringify(running)}`);
  const scrolling = await frames(3000, SCROLL);
  console.log(`      downloads + scrolling: ${JSON.stringify(scrolling)}`);
  // The page reaches the screen's rate on a GPU (Linux: WebKitGTK's own 60 fps ceiling); a virtual display draws in
  // software, so the floor here is lower than what users see.
  check("running downloads keep the dashboard smooth (>= 45 fps)", running.fps >= 45, JSON.stringify(running));
  check("and scrolling over them too (>= 40 fps)", scrolling.fps >= 40, JSON.stringify(scrolling));
  if ((await ev("document.documentElement.dataset.platform")) === "linux") {
    const shadows = await ev(`[...document.querySelectorAll('[data-testid^=download-], [data-testid=bento] > *, .panel')].filter((e) => getComputedStyle(e).boxShadow !== 'none').length`);
    check("Linux: no shadow is painted on the cards and panels (the costly part there)", shadows === 0, String(shadows));
  }
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  srv.close();
  await h.cleanup();
}

finish();
