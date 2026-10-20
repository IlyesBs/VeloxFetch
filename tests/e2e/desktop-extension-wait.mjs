// End-to-end: the first start after installing, with the extension from the Chrome Web Store (bridge.rs
// `auto_install_once`, `ContactLog`; ExtensionBanner, Settings › Browser). A REAL helper process plays the browser's
// part (native messaging framing, the store extension's origin); no browser is involved.
//
//  A. The installer's pointer is in place (deb / rpm): the first start connects the default browser and opens
//     nothing; Settings says the browser adds the extension by itself. Not there after the wait: a banner offers the
//     store page (Later hides it). The extension's first contact: "installed and connected", Settings "installed and
//     working" with the store badge, the wait is over, only a time and the source are kept; a restart remembers it
//     without announcing it again.
//  B. No pointer (an AppImage): the first start opens the store page in the default browser, as before.
//
//   node tests/e2e/desktop-extension-wait.mjs [path-to-debug-velox-desktop]
//
// Linux only (the default browser is chosen through a stand-in `xdg-settings`; Windows and macOS read it from the
// system). Stand-ins record what would be opened: no real browser or folder is ever started. Browser registrations go
// to a scratch home (debug builds only).

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, fail, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const HOST = path.join(path.dirname(APP), exe("velox-native-host"));
const SHOTS = path.resolve("docs/screenshots");
const ID = /CHROME_STORE_ID: Option<&str> = Some\("([a-p]{32})"\)/.exec(fs.readFileSync("crates/browser-bridge/src/generated/identity.rs", "utf8"))[1];
const STORE_ORIGIN = `chrome-extension://${ID}/`;
const WAIT_MS = 5000;

if (process.platform !== "linux") {
  console.log("SKIP  Linux only (the default browser comes from a stand-in xdg-settings)");
  process.exit(0);
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-extwait-"));
const bin = path.join(tmp, "bin");
const opened = path.join(tmp, "opened.log");
fs.mkdirSync(bin, { recursive: true });
// Stand-ins: Chrome (records what it is asked to open), the default browser, the folder opener.
fs.writeFileSync(path.join(bin, "google-chrome"), `#!/bin/sh\nprintf '%s\\n' "$*" >> '${opened}'\n`, { mode: 0o755 });
fs.writeFileSync(path.join(bin, "xdg-settings"), `#!/bin/sh\n[ "$*" = "get default-web-browser" ] && echo google-chrome.desktop\n`, { mode: 0o755 });
fs.writeFileSync(path.join(bin, "xdg-open"), `#!/bin/sh\nprintf 'folder %s\\n' "$*" >> '${opened}'\n`, { mode: 0o755 });
// The packages' pointer, written by postinst.sh's own lines into a scratch root.
const root = path.join(tmp, "root");
const lines = fs.readFileSync("apps/desktop/src-tauri/packaging/postinst.sh", "utf8").match(/^VELOX_EXTENSION_ID=[\s\S]*?^done$/m)[0];
await spawnSync("sh", ["-c", lines.replace(/^for dir in .*; do$/m, `for dir in '${path.join(root, "usr/share/google-chrome/extensions")}'; do`)]);

function spawnSync(cmd, args) {
  return new Promise((r) => spawn(cmd, args, { stdio: "ignore" }).once("exit", r));
}

/** One helper run as the store extension's browser would start it: a ping, the answer, done. */
function ping(h) {
  return new Promise((resolve, reject) => {
    const child = spawn(HOST, [STORE_ORIGIN], { env: { ...process.env, VELOX_DATA_DIR: h.dataDir, VELOX_BRIDGE_ENDPOINT: h.endpoint }, stdio: ["pipe", "pipe", "ignore"] });
    let buf = Buffer.alloc(0);
    const t = setTimeout(() => (child.kill(), reject(new Error("no answer from the helper"))), 10000);
    child.stdout.on("data", (d) => {
      buf = Buffer.concat([buf, d]);
      if (buf.length >= 4 && buf.length >= 4 + buf.readUInt32LE(0)) {
        clearTimeout(t);
        child.stdin.end();
        resolve(JSON.parse(buf.subarray(4, 4 + buf.readUInt32LE(0)).toString()));
      }
    });
    const body = Buffer.from(JSON.stringify({ command: "ping" }));
    const head = Buffer.alloc(4);
    head.writeUInt32LE(body.length);
    child.stdin.write(Buffer.concat([head, body]));
  });
}

const app = (port, systemRoot) =>
  createHarness({
    port,
    appPath: APP,
    env: {
      PATH: `${bin}:${process.env.PATH}`,
      VELOX_AUTO_EXTENSION: "1",
      VELOX_STORE_SYSTEM_ROOT: systemRoot,
      VELOX_EXTENSION_WAIT_MS: String(WAIT_MS),
      VELOX_NMH_SCRATCH_HOME: path.join(tmp, `home-${port}`),
    },
  });
const state = (h, sel) => h.page(`document.querySelector(${JSON.stringify(sel)})?.dataset.state ?? null`).then((r) => r.ok);
const text = (h, sel) => h.page(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`).then((r) => r.ok);
const manifest = (port) => path.join(tmp, `home-${port}`, ".config/google-chrome/NativeMessagingHosts/app.velox.fetch.json");

const a = app(9405, root);
let b = null;
try {
  // ============================================================ A. the installer's pointer is in place
  await a.launch();
  await waitFor(async () => (await text(a, "header"))?.includes("VeloxFetch"), "the interface");
  await waitFor(() => fs.existsSync(manifest(9405)), "the first start to connect the default browser", 20000);
  const waiting = await waitFor(async () => {
    const s = (await a.invoke("get_browser_integration")).ok;
    return s?.extensionAwaitedSince ? s : null;
  }, "the app to wait for the extension", 10000);
  check("first start: the default browser is connected and the app waits for it to add the extension", waiting.extensionContact === null && waiting.extensionWaitMs === WAIT_MS);
  check("nothing was opened: no store page, no folder", !fs.existsSync(opened), fs.existsSync(opened) ? fs.readFileSync(opened, "utf8") : "");
  check("no banner while the browser has time to add it", (await state(a, "[data-testid=extension-banner]")) === null);
  await a.page(`document.querySelector("[data-testid=nav-settings]").click()`);
  await waitFor(async () => (await state(a, "[data-testid=extension-status]")) === "waiting", "Settings to say the browser is adding it");
  check("Settings: the browser adds the extension by itself, nothing to do", /by itself/.test(await text(a, "[data-testid=extension-status]")));

  await waitFor(async () => (await state(a, "[data-testid=extension-banner]")) === "waiting", "the banner once the wait is over", WAIT_MS + 10000);
  const banner = await text(a, "[data-testid=extension-banner]");
  check("not there after the wait: a banner says so and offers the store page", /not reached VeloxFetch yet/.test(banner) && /Install from the Chrome Web Store/.test(banner), banner);
  await a.screenshot(path.join(SHOTS, "extension-waiting.png"), { width: 1200, height: 700 });
  await a.page(`document.querySelector("[data-testid=extension-banner-later]").click()`);
  await waitFor(async () => (await state(a, "[data-testid=extension-banner]")) === null, "Later to hide the banner");
  check("Later hides it", true);

  const pong = await ping(a);
  check("the store extension's helper reaches the app", pong.ok === true, JSON.stringify(pong));
  await waitFor(async () => (await state(a, "[data-testid=extension-banner]")) === "arrived", "the window to announce the extension");
  check("the window says: browser extension installed and connected", /Browser extension installed/.test(await text(a, "[data-testid=extension-banner]")));
  await waitFor(async () => (await state(a, "[data-testid=extension-status]")) === "ok", "Settings to show it working");
  const status = await text(a, "[data-testid=extension-status]");
  check("Settings: installed and working, from the Chrome Web Store", /installed and working/.test(status) && /Chrome Web Store/.test(status), status);
  await a.page(`document.querySelector("[data-testid=extension-status]").scrollIntoView({ block: "center" })`);
  await sleep(300);
  await a.screenshot(path.join(SHOTS, "settings-extension-working.png"), { width: 1200, height: 800 });
  const after = (await a.invoke("get_browser_integration")).ok;
  check("the wait is over and the contact is the store extension's", after.extensionAwaitedSince === null && after.extensionContact?.fromStore === true);
  const saved = JSON.parse(fs.readFileSync(path.join(a.dataDir, "browser-extension", "contact.json"), "utf8"));
  check("only a time and the source are kept", JSON.stringify(Object.keys(saved).sort()) === '["at","fromStore"]' && saved.fromStore === true, JSON.stringify(saved));
  check("no wait marker is left", !fs.existsSync(path.join(a.dataDir, "browser-extension", ".awaiting")));
  check("and still nothing was opened", !fs.existsSync(opened));

  // A restart remembers it, without announcing it again.
  const dataDir = a.dataDir;
  a.app.kill();
  await waitFor(async () => a.app.exitCode !== null || a.app.signalCode !== null, "the app to stop", 15000);
  await a.launch();
  await waitFor(async () => (await text(a, "header"))?.includes("VeloxFetch"), "the interface after the restart");
  const again = (await a.invoke("get_browser_integration")).ok;
  check("after a restart: still installed and working", again.extensionContact?.fromStore === true && again.extensionAwaitedSince === null && a.dataDir === dataDir);
  await ping(a);
  await sleep(1500);
  check("and not announced again", (await state(a, "[data-testid=extension-banner]")) === null);

  // ============================================================ B. no pointer (an AppImage)
  b = app(9406, path.join(tmp, "empty-root"));
  await b.launch();
  await waitFor(async () => (await text(b, "header"))?.includes("VeloxFetch"), "the second app's interface");
  await waitFor(() => fs.existsSync(opened), "the first start to open the store page", 20000);
  const what = fs.readFileSync(opened, "utf8").trim();
  check("no pointer: the first start opens the extension's store page in the default browser", what === `https://chromewebstore.google.com/detail/${ID}`, what);
  check("and does not wait for the browser", (await b.invoke("get_browser_integration")).ok.extensionAwaitedSince === null);
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  await a.cleanup();
  if (b) await b.cleanup();
  fs.rmSync(tmp, { recursive: true, force: true });
}
finish();
