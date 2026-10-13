// End-to-end test of "several networks at once" in the real desktop app: Settings > Network lists the networks, the
// switch turns it on, a download is spread over the networks (the test server sees requests from both) and is intact,
// Settings and the download's details say how much came through each one, a network can be left out, and the
// choice survives. The two "networks" are addresses of this computer's loopback (`VELOX_TEST_NETWORKS`, debug
// builds only); everything else is the app's own path.
// Writes docs/screenshots/multi-network-dark.png.
//
//   node tests/e2e/desktop-multinet.mjs [path-to-debug-velox-desktop]

import fs from "node:fs";
import crypto from "node:crypto";
import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const srv = await startFileServer(12 * 1024 * 1024);
// Each connection is slow, like a network's own limit: spreading over the networks is what makes it fast.
srv.delayMs = 4;
const iface = process.platform === "linux" ? "lo" : "-";
const h = createHarness({ port: 9381, appPath: APP, env: { VELOX_TEST_NETWORKS: `Wi-Fi=${iface}@127.0.0.2,Ethernet=${iface}@127.0.0.3` } });
const { page, invoke } = h;

const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const clickText = async (sel, label) => {
  await waitFor(() => ev(`[...document.querySelectorAll(${JSON.stringify(sel)})].some((b) => b.innerText.trim() === ${JSON.stringify(label)})`), `${label}`);
  return page(`(() => { [...document.querySelectorAll(${JSON.stringify(sel)})].find((b) => b.innerText.trim() === ${JSON.stringify(label)}).click(); return true; })()`);
};
const item = (name) => `[data-testid=multinet-item][data-name=${JSON.stringify(name)}]`;
const toasts = () => ev(`[...document.querySelectorAll('[role=alert],[role=status]')].map((e) => e.innerText).join(' | ')`);
const multi = async () => (await invoke("get_multi_network")).ok;
const openNetwork = async () => {
  await click("[data-testid=nav-settings]");
  await waitFor(() => has("[data-testid=settings-nav]"), "settings nav");
  await clickText("[data-testid=settings-nav] button", "Network");
  await waitFor(() => has("[data-testid=multinet-list]"), "the networks list");
  await page(`(() => { document.querySelector('#multi-network').scrollIntoView(); return true; })()`);
};

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // ============================================================ off by default, the networks are listed
  await openNetwork();
  check("off by default", !(await ev("document.querySelector('[data-testid=multinet-toggle]').checked")));
  check("both networks are listed by name, both chosen", (await has(item("Wi-Fi"))) && (await has(item("Ethernet"))) && (await ev(`[...document.querySelectorAll('[data-testid=multinet-item] input')].every((i) => i.checked)`)));
  check("the computer's usual network is marked", (await text(item("Wi-Fi")))?.includes("usual") && !(await text(item("Ethernet")))?.includes("usual"));
  check("off: no network is in use", !(await has("[data-testid=multinet-state]")));
  check("the panel says it is only for secure links to the internet", (await text("#multi-network"))?.includes("https"));
  const shown = await text("#multi-network");
  check("no network address is shown", !/127\.0\.0\.|\b\d+\.\d+\.\d+\.\d+\b/.test(shown), shown);

  // ============================================================ on: a download is spread and intact
  await click("[data-testid=multinet-toggle]");
  await waitFor(async () => (await multi()).enabled, "turned on");
  await waitFor(async () => (await toasts()).includes("Several networks at once: on"), "the toast");
  await waitFor(() => ev(`document.querySelectorAll('[data-testid=multinet-state][data-state=ready]').length === 2`), "both ready", 8000);
  check("on: both networks are ready", true);

  const d = (await invoke("create_download", { request: { url: `${srv.base}/files/spread.bin` } })).ok;
  await waitFor(async () => ["completed", "failed"].includes((await invoke("get_download", { id: d.id })).ok?.status), "download", 90000);
  const done = (await invoke("get_download", { id: d.id })).ok;
  check("the download finished", done.status === "completed", JSON.stringify(done.errorDetail));
  const file = path.join(done.destination);
  const intact = fs.existsSync(file) && crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex") === srv.sha;
  check("the file is intact", intact, file);
  const a = srv.peers["127.0.0.2"] ?? srv.peers["::ffff:127.0.0.2"] ?? 0;
  const b = srv.peers["127.0.0.3"] ?? srv.peers["::ffff:127.0.0.3"] ?? 0;
  check("the server got parts through both networks", a > 1024 * 1024 && b > 1024 * 1024, JSON.stringify(srv.peers));

  await waitFor(() => ev(`document.querySelectorAll('[data-testid=multinet-state][data-state=working]').length === 2`), "both in use", 8000);
  check("Settings: both networks in use, with what each received", (await text(item("Wi-Fi")))?.includes("received") && (await text(item("Ethernet")))?.includes("received"));
  await page(`(() => { document.documentElement.dataset.theme = 'dark'; return true; })()`);
  await sleep(300);
  await h.screenshot(path.join(SHOTS, "multi-network-dark.png"), { width: 1000, height: 720 });
  await page(`(() => { delete document.documentElement.dataset.theme; return true; })()`);
  const wide = await ev(`(() => { const p = document.querySelector('#multi-network'); return p.scrollWidth > p.clientWidth + 1; })()`);
  check("the panel fits the window", !wide);

  // The download's details: how much came through each network.
  await click("[data-testid=nav-downloads]");
  await click(`[data-testid=download-${d.id}]`);
  await waitFor(() => has("[data-testid=diag-networks]"), "the networks line in the details");
  const line = await text("[data-testid=diag-networks]");
  check("the details say how much came through each network", /Wi-Fi \d/.test(line) && /Ethernet \d/.test(line), line);
  const report = (await invoke("get_download_diagnostics", { id: d.id })).ok.report;
  check("the copied report names the networks, never their addresses", /Networks: /.test(report) && !/127\.0\.0\.[23]/.test(report));

  // ============================================================ leave one out; the choice is kept
  await openNetwork();
  await click(`${item("Ethernet")} input`);
  await waitFor(async () => (await multi()).networks.find((n) => n.name === "Ethernet")?.chosen === false, "Ethernet left out");
  await waitFor(() => has("[data-testid=multinet-one]"), "the 'choose two' hint");
  check("one network left: the panel asks for at least two", (await text("[data-testid=multinet-one]"))?.includes("at least two"));
  const before = { ...srv.peers };
  const d2 = (await invoke("create_download", { request: { url: `${srv.base}/files/one.bin` } })).ok;
  await waitFor(async () => (await invoke("get_download", { id: d2.id })).ok?.status === "completed", "second download", 90000);
  check("with one network chosen, nothing goes through the networks", (srv.peers["127.0.0.3"] ?? 0) === (before["127.0.0.3"] ?? 0) && (srv.peers["127.0.0.2"] ?? 0) === (before["127.0.0.2"] ?? 0));

  // Refused input never changes anything.
  const bad = await invoke("set_multi_network", { request: { enabled: true, skip: ["a\nb"] } });
  check("a bad network name is refused", bad.err?.code === "invalid_input", JSON.stringify(bad));
  const extra = await invoke("set_multi_network", { request: { enabled: true, skip: [], bind: "10.0.0.1" } });
  check("an unknown field is refused", !!extra.err, JSON.stringify(extra));
  check("... and nothing was changed", (await multi()).networks.find((n) => n.name === "Ethernet")?.chosen === false);

  // Back on both, then off: off means nothing is used.
  await click(`${item("Ethernet")} input`);
  await waitFor(async () => (await multi()).networks.every((n) => n.chosen), "both chosen again");
  await click("[data-testid=multinet-toggle]");
  await waitFor(async () => !(await multi()).enabled, "turned off");
  await waitFor(async () => !(await has("[data-testid=multinet-state]")), "no network in use once off", 8000);
  check("off: no network in use", true);

  const errs = await ev("window.__errs");
  check("no JavaScript errors", errs.length === 0, errs.join(" | "));
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  srv.close();
  finish();
}
