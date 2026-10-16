// By hand, with the internet: the dashboard's network usage (issue #45) in the real desktop app with a real https
// download (145 MB from a CDN). What the app counts matches the file and the system's own byte counters (Linux); it is
// kept when the app is closed and opened again, and when every download is deleted and the history cleared.
//
//   node tests/e2e/real-network-usage.mjs [path-to-debug-velox-desktop]

import fs from "node:fs";
import { check, createHarness, exe, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const URL = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz";
const SIZE = 145142812;
const h = createHarness({ port: 9383, appPath: APP });
const { page, invoke } = h;
const received = () =>
  process.platform !== "linux"
    ? null
    : fs.readFileSync("/proc/net/dev", "utf8").split("\n").slice(2).filter(Boolean).reduce((a, l) => {
        const [name, rest] = l.split(":");
        return name.trim() === "lo" ? a : a + Number(rest.trim().split(/\s+/)[0]);
      }, 0);
const mb = (v) => (v / 1e6).toFixed(1);

try {
  await h.launch();
  await waitFor(async () => (await page(`document.querySelector('header')?.innerText ?? ''`)).ok.includes("VeloxFetch"), "UI to render");
  const start = (await invoke("get_metrics")).ok;
  check("a new install starts at 0", start.networkTotal === 0 && start.networkToday === 0 && start.networkMonth === 0, JSON.stringify(start));
  const sys0 = received();
  const d = (await invoke("create_download", { request: { url: URL } })).ok;
  const t = Date.now();
  while (!["completed", "failed"].includes((await invoke("get_download", { id: d.id })).ok?.status) && Date.now() - t < 900000) await sleep(2000);
  const done = (await invoke("get_download", { id: d.id })).ok;
  check("the download finished whole", done.status === "completed" && fs.statSync(done.destination).size === SIZE, JSON.stringify(done.errorDetail));
  await sleep(1500);
  const m = (await invoke("get_metrics")).ok;
  const sys = sys0 === null ? null : received() - sys0;
  console.log(`app: total ${mb(m.networkTotal)} today ${mb(m.networkToday)} month ${mb(m.networkMonth)} MB; file ${mb(SIZE)} MB; system received ${sys === null ? "?" : mb(sys)} MB`);
  check("the app counted the file (within 3 %, retries can add a little)", m.networkTotal >= SIZE * 0.99 && m.networkTotal <= SIZE * 1.03, mb(m.networkTotal));
  check("today and this month say the same", m.networkToday === m.networkTotal && m.networkMonth === m.networkTotal);
  if (sys !== null) check("never more than the system received", m.networkTotal <= sys, `${mb(m.networkTotal)} vs ${mb(sys)}`);
  const tile = (await page(`document.querySelector('[data-testid=tile-network]')?.innerText ?? ''`)).ok;
  console.log(`card: ${JSON.stringify(tile)}`);
  const fmt = (n) => { let v = n, i = 0; while (v >= 1024 && i < 4) { v /= 1024; i++; } return `${i === 0 ? Math.round(v) : v.toFixed(v >= 100 ? 0 : 1)} ${["B", "KB", "MB", "GB", "TB"][i]}`; };
  check("the dashboard card shows it", tile.includes(fmt(m.networkTotal)) && /Today\n/.test(tile) && /This month\n/.test(tile) && (await page(`document.querySelector('[data-testid=network-today]').innerText`)).ok === fmt(m.networkToday) && (await page(`document.querySelector('[data-testid=network-month]').innerText`)).ok === fmt(m.networkMonth), tile);

  // Closed and opened again.
  const gone = h.exited();
  await h.closeWindow();
  check("the app closed", (await Promise.race([gone, sleep(20000).then(() => "timeout")])) === 0);
  h.app = null;
  await h.launch();
  await waitFor(async () => (await page(`document.querySelector('header')?.innerText ?? ''`)).ok.includes("VeloxFetch"), "UI to render");
  const back = (await invoke("get_metrics")).ok;
  check("opened again: the usage is kept", back.networkTotal === m.networkTotal && back.networkToday === m.networkToday && back.sessionDownloaded === 0, JSON.stringify(back));
  for (const x of (await invoke("list_downloads", { filter: {} })).ok) await invoke("delete_download", { id: x.id, deleteFile: true });
  await invoke("forget_history", { id: null });
  const empty = (await invoke("get_metrics")).ok;
  check("every download deleted and the history cleared: still kept", empty.networkTotal === m.networkTotal && empty.completed === 0, JSON.stringify(empty));
  check("the file was deleted", !fs.existsSync(done.destination));
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  finish();
}
