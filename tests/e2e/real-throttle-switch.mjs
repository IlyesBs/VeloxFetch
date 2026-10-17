// By hand, with the internet: turning Smart throttle on, off and switching its levels must be smooth. A real download
// from a CDN runs while the level changes every few seconds (Fast → Smart → Manual Aggressive / Balanced /
// Performance → Fast → Smart → Fast, through the window's own buttons where they exist); the download's progress is
// read every 200 ms and the longest stretch without a single new byte is reported for each step. It must never stop
// (no stretch over 1.5 s), the speed the window shows must never fall to 0 either, and each level must give the speed it
// promises. The levels are chosen with the sidebar's own buttons (Fast / Smart / Manual and Manual's modes).
//
//   node tests/e2e/real-throttle-switch.mjs [path-to-debug-velox-desktop]
//   VELOX_REAL_URL=https://fsn1-speed.hetzner.com/1GB.bin (a server that allows ~2 connections), VELOX_STEP_MS=8000

import { check, createHarness, exe, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const URL = process.env.VELOX_REAL_URL ?? "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz";
const STEP_MS = Number(process.env.VELOX_STEP_MS ?? 8000);
const MB = 1024 * 1024;
const h = createHarness({ port: 9402, appPath: APP });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const dl = async (id) => (await invoke("get_download", { id })).ok;
const kbs = (v) => `${(v / 1024).toFixed(0)} KB/s`;

const click = (sel) => page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
/** The speed the status bar shows, in bytes/s ("1.2 MB/s"). */
const shown = async () => {
  const t = (await ev(`document.querySelector('[data-testid=status-speed]')?.innerText ?? ''`)).trim();
  const m = /([\d.]+)\s*(B|KB|MB|GB)\/s/.exec(t);
  return m ? Number(m[1]) * { B: 1, KB: 1024, MB: MB, GB: 1024 * MB }[m[2]] : 0;
};

// Read progress every 200 ms for `ms`: the longest stretch with no new byte, the longest stretch the window showed
// 0 B/s, and the average speed.
async function watch(id, ms) {
  const t0 = Date.now();
  let last = (await dl(id)).downloaded;
  let lastAt = Date.now();
  let zeroFrom = null;
  let longest = 0;
  let shownZero = 0;
  const first = last;
  while (Date.now() - t0 < ms) {
    await sleep(200);
    const d = await dl(id);
    if (d.downloaded > last) {
      last = d.downloaded;
      lastAt = Date.now();
    } else {
      longest = Math.max(longest, Date.now() - lastAt);
    }
    if ((await shown()) === 0) {
      zeroFrom ??= Date.now();
      shownZero = Math.max(shownZero, Date.now() - zeroFrom);
    } else zeroFrom = null;
    if (d.status !== "downloading") return { longest: Infinity, shownZero, speed: 0, status: d.status };
  }
  return { longest, shownZero, speed: ((last - first) * 1000) / (Date.now() - t0), status: "downloading" };
}

// Each step: its name and the sidebar buttons to press, in order.
const STEPS = [
  ["Smart (automatic)", ["hud-level-smart"]],
  ["Manual · Aggressive", ["hud-level-manual", "hud-mode-10"]],
  ["Manual · Balanced", ["hud-mode-25"]],
  ["Manual · Performance", ["hud-mode-50"]],
  ["Manual · Aggressive (straight from Performance)", ["hud-mode-10"]],
  ["Fast", ["hud-level-fast"]],
  ["Smart (automatic)", ["hud-level-smart"]],
  ["Manual · Balanced", ["hud-level-manual", "hud-mode-25"]],
  ["Fast", ["hud-level-fast"]],
  ["Smart switch off and on", ["hud-switch", "hud-switch"]],
  ["Fast", ["hud-level-fast"]],
];

try {
  await h.launch();
  await waitFor(() => has("[data-testid=statusbar]"), "the interface");
  await invoke("set_smart_throttle", { request: { enabled: false, sharePercent: 25 } });
  if (process.env.VELOX_DEBUG) await invoke("set_detailed_logging", { enabled: true });
  const d = (await invoke("create_download", { request: { url: URL } })).ok;
  await waitFor(async () => (await dl(d.id)).downloaded > 4 * MB, "the download to get going", 60000);
  const free = await watch(d.id, 10000);
  console.log(`Fast (start): ${kbs(free.speed)}, longest stop ${free.longest} ms`);
  check("Fast: the download runs", free.speed > 300 * 1024 && free.longest < 1500, `${kbs(free.speed)}, ${free.longest} ms`);
  for (const [name, buttons] of STEPS) {
    for (const b of buttons) {
      await waitFor(() => has(`[data-testid=${b}]:not([disabled])`), b, 5000);
      await click(`[data-testid=${b}]`);
      await sleep(150);
    }
    const s = (await invoke("get_smart_throttle")).ok;
    const startedAt = new Date().toISOString().slice(11, 23);
    const w = await watch(d.id, STEP_MS);
    w.from = startedAt;
    console.log(`[${new Date().toISOString().slice(11, 23)}] ${name}: ${s.reason}, cap ${s.limitBps === null ? "none" : kbs(s.limitBps)} → ${kbs(w.speed)}, longest stop ${w.longest} ms, shown 0 B/s for ${w.shownZero} ms (step from ${w.from})`);
    check(`${name}: never stops (no stretch over 1.5 s without bytes)`, w.longest < 1500 && w.status === "downloading", `${w.longest} ms, ${w.status}`);
    check(`${name}: the window never shows 0 B/s for long`, w.shownZero < 1500, `${w.shownZero} ms`);
    if (s.limitBps) check(`${name}: held near its cap`, w.speed < s.limitBps * 1.4, `${kbs(w.speed)} vs ${kbs(s.limitBps)}`);
  }
  if (process.env.VELOX_DEBUG) {
    const diag = (await invoke("get_download_diagnostics", { id: d.id })).ok;
    console.log("TIMELINE", JSON.stringify(diag?.timeline ?? diag, null, 0).slice(0, 20000));
    const logs = (await invoke("get_logs", { limit: 5000, problemsOnly: false })).ok;
    console.log("LOGS\n" + (logs?.lines ?? []).map((l) => `${l.at} ${l.level} ${l.target}: ${l.text}`).join("\n"));
  }
  await invoke("cancel_download", { id: d.id });
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  finish();
}
