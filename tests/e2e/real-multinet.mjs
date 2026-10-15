// By hand, on a computer with two or more connected networks and the internet: "several networks at once" in the real
// desktop app with the computer's own networks (no stand-ins). Settings > Network lists them by name, the switch turns
// it on, a real https download (145 MB from a CDN) goes through every network (the system's own byte counters say so
// on Linux) and arrives whole, and the details say how much came through each. Then it is turned off again.
//
//   node tests/e2e/real-multinet.mjs [path-to-debug-velox-desktop]

import fs from "node:fs";
import { check, createHarness, exe, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const URL = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz";
const SIZE = 145142812;
const h = createHarness({ port: 9382, appPath: APP });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const counters = () =>
  process.platform !== "linux"
    ? {}
    : Object.fromEntries(
        fs.readFileSync("/proc/net/dev", "utf8").split("\n").slice(2).filter(Boolean).map((l) => {
          const [name, rest] = l.split(":");
          return [name.trim(), Number(rest.trim().split(/\s+/)[0])];
        }),
      );

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await click("[data-testid=nav-settings]");
  await waitFor(() => has("[data-testid=multinet-list]"), "the networks list", 15000);
  const dto = (await invoke("get_multi_network")).ok;
  const names = dto.networks.map((n) => n.name);
  console.log(`networks: ${JSON.stringify(dto.networks.map((n) => [n.name, n.main]))}`);
  check("two or more networks are found", names.length >= 2, JSON.stringify(names));
  check("exactly one is the usual one", dto.networks.filter((n) => n.main).length === 1);
  check("each is listed", (await ev(`document.querySelectorAll('[data-testid=multinet-item]').length`)) === names.length);
  const shown = await ev(`[...document.querySelectorAll('[data-testid=multinet-name]')].map((e) => e.innerText.trim())`);
  console.log(`shown as: ${JSON.stringify(shown)} (kinds ${JSON.stringify(dto.networks.map((n) => n.kind))})`);
  check("networks of a known kind are shown by what they are, never by the system's identifier", dto.networks.every((n, i) => n.kind === "other" || !shown[i].includes(n.name)), JSON.stringify(shown));
  const words = { wifi: "Wi-Fi", ethernet: "Ethernet", usbEthernet: "USB Ethernet", phone: "Phone (USB)", mobile: "Mobile network", bluetooth: "Bluetooth" };
  check("each name says what it is", dto.networks.every((n, i) => n.kind === "other" || shown[i].startsWith(words[n.kind])), JSON.stringify(shown));
  await page(`(() => { document.querySelector('#multi-network').scrollIntoView(); return true; })()`);
  // Smart throttle on first: turning several networks on holds it off (every network at full speed).
  await invoke("set_smart_throttle", { request: { enabled: true } });
  await click("[data-testid=multinet-toggle]");
  await waitFor(async () => (await invoke("get_multi_network")).ok.enabled, "turned on");
  const smart = (await invoke("get_smart_throttle")).ok;
  check("Smart throttle is held off while several networks are on (the user's switch kept)", smart.heldByNetworks && smart.enabled && smart.reason === "off", JSON.stringify(smart));
  check("Smart throttle cannot be turned on meanwhile", !!(await invoke("set_smart_throttle", { request: { enabled: true } })).err);
  await waitFor(() => ev(`document.querySelector('[data-testid=hud-switch]')?.disabled === true`), "the sidebar's switch greyed out", 2000).catch(() => null);
  check("the sidebar's Smart throttle switch is greyed out at once", await ev(`document.querySelector('[data-testid=hud-switch]')?.disabled === true`));

  const before = counters();
  const t = Date.now();
  const d = (await invoke("create_download", { request: { url: URL } })).ok;
  // The dashboard while it runs (issue #44): the indicator, and each network's live speed against the system's own
  // counters over the same seconds.
  await click("[data-testid=nav-downloads]");
  await waitFor(() => has("[data-testid=panel-networks]"), "the networks card", 10000);
  const badge = (await text("[data-testid=multinet-indicator]"))?.trim();
  check("the indicator says how many networks are used", badge === `${names.length} networks`, badge);
  check("the status bar says it too", (await text("[data-testid=status-multinet]"))?.trim() === `${names.length} networks`);
  const samples = [];
  let prev = { at: Date.now(), c: counters() };
  while (!["completed", "failed"].includes((await invoke("get_download", { id: d.id })).ok?.status) && Date.now() - t < 600000) {
    await sleep(3000);
    const m = (await invoke("get_metrics")).ok;
    const now = { at: Date.now(), c: counters() };
    const secs = (now.at - prev.at) / 1000;
    const sys = Object.fromEntries(names.map((n) => [n, ((now.c[n] ?? 0) - (prev.c[n] ?? 0)) / secs]));
    prev = now;
    if (m.networks) samples.push({ app: Object.fromEntries(m.networks.networks.map((x) => [x.name, x.speedBps])), total: m.networks.totalBps, sum: m.networks.networks.reduce((a, x) => a + x.speedBps, m.networks.otherBps), speed: m.speedBps, sys });
  }
  const mb = (v) => (v / 1e6).toFixed(2);
  for (const x of samples) console.log(`  app ${names.map((n) => `${n} ${mb(x.app[n] ?? 0)}`).join(", ")} total ${mb(x.total)} (downloads ${mb(x.speed)}) | system ${names.map((n) => `${n} ${mb(x.sys[n])}`).join(", ")} MB/s`);
  const mid = samples.slice(1, -1); // the first and last windows straddle the start and the end
  check("each network showed its own speed while downloading", names.every((n) => mid.some((x) => (x.app[n] ?? 0) > 100e3)), JSON.stringify(mid.at(0)));
  check("the total is always the sum of the networks", samples.every((x) => x.total === x.sum));
  const near = (a, b) => Math.abs(a - b) <= 0.35 * Math.max(a, b, 200e3);
  const agree = mid.filter((x) => names.every((n) => near(x.app[n] ?? 0, x.sys[n])));
  check("each network's speed matches the system's counters for that network (most readings, within 35 %)", agree.length >= Math.ceil(mid.length * 0.7), `${agree.length}/${mid.length}`);
  const secs = (Date.now() - t) / 1000;
  const done = (await invoke("get_download", { id: d.id })).ok;
  check("the download finished", done.status === "completed", JSON.stringify(done.errorDetail));
  check("the whole file is there", fs.existsSync(done.destination) && fs.statSync(done.destination).size === SIZE, done.destination);
  const after = counters();
  const used = Object.fromEntries(names.map((n) => [n, (after[n] ?? 0) - (before[n] ?? 0)]));
  const diag = (await invoke("get_download_diagnostics", { id: d.id })).ok;
  console.log(`${secs.toFixed(1)} s, ${(SIZE / secs / 1e6).toFixed(2)} MB/s; app per network: ${JSON.stringify(diag.networks)}; system: ${JSON.stringify(used)}`);
  check("every network carried part of the file (app)", names.every((n) => diag.networks.find((x) => x.name === n)?.bytes > 1e6), JSON.stringify(diag.networks));
  if (process.platform === "linux") check("every network carried part of the file (system counters)", names.every((n) => used[n] > 1e6), JSON.stringify(used));
  await click("[data-testid=nav-settings]");
  await waitFor(() => has("[data-testid=multinet-list]"), "the networks list", 15000);
  await waitFor(() => ev(`document.querySelectorAll('[data-testid=multinet-state][data-state=working]').length >= 2`), "networks shown in use", 10000);
  check("Settings shows the networks in use", true);

  await click("[data-testid=multinet-toggle]");
  await waitFor(async () => !(await invoke("get_multi_network")).ok.enabled, "turned off");
  check("turned off again", true);
  const back = (await invoke("get_smart_throttle")).ok;
  check("Smart throttle is back as the user had it", !back.heldByNetworks && back.enabled, JSON.stringify(back));
  await invoke("set_smart_throttle", { request: { enabled: false } });
  fs.rmSync(done.destination, { force: true });
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  finish();
}
