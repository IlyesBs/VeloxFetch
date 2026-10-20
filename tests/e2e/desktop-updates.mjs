// End-to-end test of Phase 19 (updates) in the real desktop app, against a local release server and a throwaway
// signing key (never the real one): Settings > Updates and the status bar; a manifest with a bad signature is
// refused; a signed newer version is found, downloaded, verified and handed to the installer (a dry run: nothing
// is installed); the automatic check setting is kept; update requests carry no cookie or credential.
// Writes screenshots to docs/screenshots.
//
//   node tests/e2e/desktop-updates.mjs [path-to-debug-velox-desktop]

import http from "node:http";
import os from "node:os";
import { check, createHarness, crypto, exe, finish, fs, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const VERSION = "99.0.0";
const KIND = { win32: ["nsis", "windows", "VeloxFetch_99.0.0_x64-setup.exe"], darwin: ["dmg", "macos", "VeloxFetch_99.0.0_aarch64.dmg"] }[process.platform] ?? ["deb", "linux", "VeloxFetch_99.0.0_amd64.deb"];
const ARCH = process.arch === "arm64" ? "aarch64" : "x86_64";

// The package: served with ranges by the shared file server.
const pkgSrv = await startFileServer(700 * 1024);
// A throwaway key, only for this run.
const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
const spki = publicKey.export({ format: "der", type: "spki" });
const pub = spki.subarray(spki.length - 32);
const keyId = crypto.createHash("sha256").update(pub).digest().subarray(0, 8).toString("hex");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-updates-"));
const keysFile = path.join(tmp, "keys.txt");
fs.writeFileSync(keysFile, `velox-update-key-v1 ${pub.toString("hex")}\n`);
const manifest = JSON.stringify({
  schema: 1,
  product: "VeloxFetch",
  version: VERSION,
  released: Math.floor(Date.now() / 1000),
  packages: [{ target: `${KIND[1]}-${ARCH}-${KIND[0]}`, name: KIND[2], url: `${pkgSrv.base}/rel/v${VERSION}/${KIND[2]}`, size: pkgSrv.FILE.length, sha256: pkgSrv.sha }],
});
const sign = (bytes, key = privateKey) => `velox-update-sig-v1 ${keyId} ${crypto.sign(null, Buffer.concat([Buffer.from("VeloxFetch update manifest v1\n"), Buffer.from(bytes)]), key).toString("hex")}\n`;
const feed = { manifest, sig: sign(manifest, crypto.generateKeyPairSync("ed25519").privateKey), heads: [] };
const feedSrv = http.createServer((req, res) => {
  feed.heads.push(req.headers);
  if (req.url === "/feed/latest.json") return void res.writeHead(200, { "Content-Type": "application/json" }).end(feed.manifest);
  if (req.url === "/feed/latest.json.sig") return void res.writeHead(200, { "Content-Type": "text/plain" }).end(feed.sig);
  res.writeHead(404).end();
});
await new Promise((r) => feedSrv.listen(0, "127.0.0.1", r));
const feedBase = `http://127.0.0.1:${feedSrv.address().port}`;

const h = createHarness({
  port: 9372,
  appPath: APP,
  env: {
    VELOX_UPDATE_FEED: `${feedBase}/feed/latest.json`,
    VELOX_UPDATE_PREFIX: `${pkgSrv.base}/rel/`,
    VELOX_UPDATE_KEYS: keysFile,
    VELOX_UPDATE_KIND: KIND[0],
    VELOX_DRY_RUN_SHELL: "1",
  },
});
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const attr = async (sel, a) => (await page(`document.querySelector(${JSON.stringify(sel)})?.getAttribute(${JSON.stringify(a)}) ?? null`)).ok;
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const phase = () => attr("[data-testid=update-status]", "data-phase");

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // The automatic check runs shortly after start: the foreign signature is refused.
  await click("[data-testid=nav-settings]");
  await click("[data-testid=settings-nav] button:nth-last-child(2)");
  await waitFor(async () => (await phase()) === "failed", "the first automatic check", 20000);
  const refused = await text("[data-testid=update-status]");
  check("a manifest signed by another key is refused, in words", /signature is not valid/.test(refused), refused);
  check("nothing is offered to install then", !(await has("[data-testid=update-now]")));
  check("the status bar says nothing about an update then", !(await has("[data-testid=status-update]")));

  // The real signature: found.
  feed.sig = sign(feed.manifest);
  await click("[data-testid=update-check]");
  await waitFor(async () => (await phase()) === "available", "the update to be found");
  check("a signed newer version is offered", (await text("[data-testid=update-status]")).includes(`Version ${VERSION} is available`), await text("[data-testid=update-status]"));
  await waitFor(() => has("[data-testid=status-update]"), "the status bar");
  check("the status bar tells it", (await text("[data-testid=status-update]")).includes(`Update ${VERSION} available`), await text("[data-testid=status-update]"));
  await h.screenshot(path.join(SHOTS, "settings-updates.png"), { width: 1100, height: 760 });

  // One button, one click: it downloads the verified package and then hands it to the installer. There is no
  // separate "download" step and no "install" step, and no browser or file download by hand.
  check(
    "the button is a single Update now, with no separate download or install step",
    (await text("[data-testid=update-now]")).includes("Update now") && !(await has("[data-testid=update-download]")) && !(await has("[data-testid=update-install]")),
    await text("[data-testid=update-now]"),
  );
  await click("[data-testid=update-now]");
  await waitFor(() => has("[data-testid=update-opened]"), "download, verify and the dry-run install", 40000);
  const saved = path.join(h.dataDir, "updates", KIND[2]);
  check("the package was downloaded and kept in the app's own folder, byte for byte", fs.existsSync(saved) && crypto.createHash("sha256").update(fs.readFileSync(saved)).digest("hex") === pkgSrv.sha, saved);
  check("the status bar reflects the ready update", (await text("[data-testid=status-update]")).includes(`Update ${VERSION}`), await text("[data-testid=status-update]"));
  check("installing is handed over by the one click (a dry run in tests: nothing installed)", true);

  // The status bar opens Settings > Updates.
  await click("[data-testid=nav-downloads]");
  await click("[data-testid=status-update]");
  await waitFor(() => has("[data-testid=update-status]"), "settings from the status bar");
  check("the status bar's update item opens Settings > Updates", true);

  // The automatic check setting is kept.
  await click("[data-testid=update-auto]");
  await waitFor(async () => (await invoke("get_update_status")).ok?.autoCheck === false, "auto check off");
  check("automatic checks can be turned off", (await invoke("get_update_status")).ok?.autoCheck === false);
  await waitFor(async () => (await ev("document.querySelector('[data-testid=update-auto]').checked")) === false, "the box to follow");
  await click("[data-testid=update-auto]");
  await waitFor(async () => (await invoke("get_update_status")).ok?.autoCheck === true, "auto check on");

  // What the update requests carried.
  const sent = [...feed.heads, ...pkgSrv.headers];
  check("update requests carry no cookie or credential", sent.every((hd) => !hd.cookie && !hd.authorization), JSON.stringify(sent.map((hd) => Object.keys(hd))));
  check("update requests name only the product and its version", feed.heads.every((hd) => /^VeloxFetch\/\d+\.\d+\.\d+$/.test(hd["user-agent"] ?? "")), feed.heads.map((hd) => hd["user-agent"]).join(","));

  // The UI cannot point the updater anywhere: the commands take no address or path.
  const odd = await invoke("download_update", { url: "https://evil.example/x.exe", path: "/tmp/x" });
  check("extra arguments from the web view are ignored", !odd.err || odd.err.code !== "forbidden", JSON.stringify(odd));
  check("the downloaded file is still the verified one", crypto.createHash("sha256").update(fs.readFileSync(saved)).digest("hex") === pkgSrv.sha);

  const errs = await ev("window.__errs");
  check("no JavaScript errors", errs.length === 0, errs.join(" | "));
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  pkgSrv.close();
  feedSrv.close();
  fs.rmSync(tmp, { recursive: true, force: true });
  finish();
}
