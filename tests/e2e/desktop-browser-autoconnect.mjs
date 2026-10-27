// End-to-end: the app connects the browsers installed on this computer by itself (bridge.rs `connect_found_browsers`,
// native_messaging.rs `sibling_targets`), so an extension added to any of them reaches the app with nothing to press.
//
//  A. Start: every browser found is connected (Chrome, Firefox, the Chromium line), with the other browsers of the line
//     the user has (Chromium, Vivaldi; LibreWolf) in their own folders; a browser that is not installed is not.
//  B. Disconnect in Settings removes the line's registrations, and a restart does not connect it again; Connect does.
//  C. The helper a connected browser starts reaches the app.
//
//   node tests/e2e/desktop-browser-autoconnect.mjs [path-to-debug-velox-desktop]
//
// Linux only (the browsers are stand-ins on the PATH). Registrations go to a scratch home (debug builds only): the
// real browsers' folders are never touched, and no browser is ever started.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, finish, path, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const HOST = path.join(path.dirname(APP), exe("velox-native-host"));

if (process.platform !== "linux") {
  console.log("SKIP  Linux only (the browsers are stand-ins on the PATH)");
  process.exit(0);
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-autoconnect-"));
const bin = path.join(tmp, "bin");
const home = path.join(tmp, "home");
fs.mkdirSync(bin, { recursive: true });
for (const b of ["google-chrome", "firefox", "chromium"]) fs.writeFileSync(path.join(bin, b), "#!/bin/sh\nexit 0\n", { mode: 0o755 });
fs.writeFileSync(path.join(bin, "xdg-settings"), `#!/bin/sh\n[ "$*" = "get default-web-browser" ] && echo google-chrome.desktop\n`, { mode: 0o755 });
fs.writeFileSync(path.join(bin, "xdg-open"), "#!/bin/sh\nexit 0\n", { mode: 0o755 });
// The other browsers this user has: their profile folders exist.
for (const d of [".config/chromium", ".config/vivaldi", ".librewolf"]) fs.mkdirSync(path.join(home, d), { recursive: true });

const h = createHarness({
  port: 9431,
  appPath: APP,
  env: {
    // Only the stand-ins, the system's tools and the test driver: no real browser can be found (or started).
    PATH: [bin, ...(process.env.PATH ?? "").split(":").filter((d) => fs.existsSync(path.join(d, "WebKitWebDriver"))), "/usr/bin", "/bin"].join(":"),
    VELOX_AUTO_EXTENSION: "1",
    VELOX_NO_LAUNCH: "1",
    VELOX_NMH_SCRATCH_HOME: home,
    VELOX_STORE_SYSTEM_ROOT: path.join(tmp, "root"),
  },
});
const reg = (rel) => path.join(home, rel, "app.velox.fetch.json");
const FILES = {
  chrome: reg(".config/google-chrome/NativeMessagingHosts"),
  firefox: reg(".mozilla/native-messaging-hosts"),
  brave: reg(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
  chromium: reg(".config/chromium/NativeMessagingHosts"),
  vivaldi: reg(".config/vivaldi/NativeMessagingHosts"),
  librewolf: reg(".librewolf/native-messaging-hosts"),
  edge: reg(".config/microsoft-edge/NativeMessagingHosts"),
};
const has = (k) => fs.existsSync(FILES[k]);
const connected = async (b) => (await h.invoke("get_browser_integration")).ok.browsers.find((x) => x.browser === b)?.registered;

/** One helper run as a connected browser would start it, from the path its registration names: a ping, the answer. */
function ping(registration, origin) {
  const exePath = JSON.parse(fs.readFileSync(registration, "utf8")).path;
  return new Promise((resolve, reject) => {
    const child = spawn(exePath, [origin], { env: { ...process.env, VELOX_DATA_DIR: h.dataDir, VELOX_BRIDGE_ENDPOINT: h.endpoint }, stdio: ["pipe", "pipe", "ignore"] });
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

try {
  // ============================================================ A. every browser found is connected
  await h.launch();
  await waitFor(async () => (await h.page("document.querySelector('header')?.innerText ?? ''")).ok.includes("VeloxFetch"), "the interface");
  await waitFor(() => has("chrome") && has("firefox") && has("brave"), "the browsers found to be connected", 30000);
  check("Chrome, Firefox and the Chromium line are connected on their own", (await connected("chrome")) && (await connected("firefox")) && (await connected("brave")));
  check("the line's other browsers this user has get their own registration (Chromium, Vivaldi, LibreWolf)", has("chromium") && has("vivaldi") && has("librewolf"));
  const m = JSON.parse(fs.readFileSync(FILES.vivaldi, "utf8"));
  check("each names the app's helper and only the shipped extensions", m.path === HOST && m.allowed_origins.every((o) => /^chrome-extension:\/\/[a-p]{32}\/$/.test(o)), JSON.stringify(m));
  check("a browser that is not installed is not connected (Edge)", !has("edge") && !(await connected("edge")));
  check("nothing is made for a browser this user does not have (Opera)", !fs.existsSync(path.join(home, ".config/opera")));

  // ============================================================ C. a connected browser's helper reaches the app
  const pong = await ping(FILES.vivaldi, `chrome-extension://${m.allowed_origins[0].slice(19, 51)}/`);
  check("the helper a connected browser starts reaches the app", pong.ok === true, JSON.stringify(pong));

  // ============================================================ B. Disconnect is remembered
  const off = await h.invoke("set_browser_integration", { request: { browser: "brave", enabled: false } });
  check("Disconnect removes the line's registrations", off.ok && !has("brave") && !has("chromium") && !has("vivaldi"), JSON.stringify(off.err ?? ""));
  h.app.kill();
  await waitFor(async () => h.app.exitCode !== null || h.app.signalCode !== null, "the app to stop", 15000);
  await h.launch();
  await waitFor(async () => (await h.page("document.querySelector('header')?.innerText ?? ''")).ok.includes("VeloxFetch"), "the interface after a restart");
  await new Promise((r) => setTimeout(r, 8000)); // past the start's set-up (4 s)
  check("after a restart a disconnected browser stays disconnected", !has("brave") && !has("chromium") && !(await connected("brave")));
  check("the others stay connected", has("chrome") && has("firefox") && has("librewolf"));
  const on = await h.invoke("set_browser_integration", { request: { browser: "brave", enabled: true } });
  check("Connect brings the line back, with its other browsers", on.ok && has("brave") && has("chromium") && has("vivaldi"));
} catch (e) {
  check(`unexpected error: ${e?.stack ?? e}`, false);
} finally {
  await h.cleanup();
  fs.rmSync(tmp, { recursive: true, force: true });
}
finish();
