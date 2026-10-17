// By hand, with the internet, on Linux with Brave, Chrome or Edge: the extension from the Chrome Web Store, end to
// end, with the real browser and the real store:
//
//  1. First start of VeloxFetch right after installing (default browser = this browser): it connects the browser
//     and opens nothing: the packages' pointer (postinst.sh) is in place, so the browser adds the extension by
//     itself. No store page, no folder (a stand-in `xdg-open` proves it), no extensions page. VeloxFetch waits.
//  2. The browser starts (as the user's does) and adds the published extension from the Chrome Web Store by
//     itself, switched on, in a fresh profile, with no click anywhere.
//  3. The extension reaches VeloxFetch through the browser's own native messaging: the window says "installed and
//     connected" (banner) and Settings shows it working; a link it hands over is downloaded, intact.
//
//   sudo sh -c "$(sed -n '/^VELOX_EXTENSION_ID=/,/^done$/p' apps/desktop/src-tauri/packaging/postinst.sh)"   (once)
//   node tests/e2e/real-store-extension.mjs [path-to-debug-velox-desktop]
//   VELOX_REAL_BROWSER=chrome|edge|brave        which browser (default: the first one installed)
//   VELOX_REAL_BROWSER_DIR=<folder>             a browser unpacked without installing it (dpkg-deb -x google-chrome.deb
//                                               x, then x/opt/google/chrome), instead of the installed one
//
// What it touches, only for the run: a throwaway browser profile (your own profile is never opened, and the browser
// runs on the test's hidden display); the test app's own folders. The browser is started by a stand-in launcher
// that adds the throwaway profile, so VeloxFetch's "open the store page" never reaches your running browser. The
// browser is stopped by its own process id.

import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, finish, path, sha256, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const HOST = path.join(path.dirname(APP), exe("velox-native-host"));
const ID = /CHROME_STORE_ID: Option<&str> = Some\("([a-p]{32})"\)/.exec(fs.readFileSync("crates/browser-bridge/src/caller.rs", "utf8"))[1];
// Per browser: its launcher and desktop file (what VeloxFetch looks for), its program folder, its profile folder
// under $HOME, and its system pointer folder (the one compiled into the browser; postinst.sh writes all three).
const BROWSERS = {
  brave: { launcher: "brave-browser", dir: "/opt/brave.com/brave", exe: "brave", config: ".config/BraveSoftware/Brave-Browser", system: "/usr/share/chromium/extensions" },
  chrome: { launcher: "google-chrome", dir: "/opt/google/chrome", exe: "chrome", config: ".config/google-chrome", system: "/usr/share/google-chrome/extensions" },
  edge: { launcher: "microsoft-edge", dir: "/opt/microsoft/msedge", exe: "msedge", config: ".config/microsoft-edge", system: "/usr/share/microsoft-edge/extensions" },
};
const NAME =
  process.env.VELOX_REAL_BROWSER ?? Object.keys(BROWSERS).find((k) => fs.existsSync(path.join(BROWSERS[k].dir, BROWSERS[k].exe)));
const B = BROWSERS[NAME];
const DIR = process.env.VELOX_REAL_BROWSER_DIR ? path.resolve(process.env.VELOX_REAL_BROWSER_DIR) : B?.dir;
const MB = 1024 * 1024;
const DBG = 9404;

if (process.platform !== "linux" || !B || !fs.existsSync(path.join(DIR, B.exe))) {
  console.log("SKIP  Linux with Brave, Chrome or Edge only (VELOX_REAL_BROWSER, VELOX_REAL_BROWSER_DIR)");
  process.exit(0);
}
console.log(`browser: ${NAME}, ${execFileSync(path.join(DIR, B.exe), ["--version"], { encoding: "utf8" }).trim()}`);
const POINTER = path.join(B.system, `${ID}.json`);
let REAL = path.join(DIR, B.exe);
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-store-"));
const postinst = (dir) => {
  const lines = fs.readFileSync("apps/desktop/src-tauri/packaging/postinst.sh", "utf8").match(/^VELOX_EXTENSION_ID=[\s\S]*?^done$/m)[0];
  execFileSync("sh", ["-c", lines.replace(/^for dir in .*; do$/m, `for dir in '${dir}'; do`)]);
  return fs.readFileSync(path.join(dir, `${ID}.json`), "utf8").trim();
};
// Where VeloxFetch looks for the packages' pointer: the real system folders when the packages wrote it (VeloxFetch
// installed here), else a scratch root that gets it from the packages' own lines.
let systemRoot = "/";
// Without the system pointer (no root to run the packages' script): a private copy of the browser, whose
// `extensions` folder beside the program Chromium reads the same way (DIR_EXTERNAL_EXTENSIONS), gets the pointer
// written by the packages' own lines (postinst.sh, only the folder changed).
if (!fs.existsSync(POINTER)) {
  const copy = path.join(tmp, "browser");
  execFileSync("cp", ["-a", "--reflink=auto", DIR, copy]);
  console.log(`no system pointer: a private copy of the browser, pointer written by postinst.sh's lines: ${postinst(path.join(copy, "extensions"))}`);
  REAL = path.join(copy, B.exe);
  systemRoot = path.join(tmp, "root");
  postinst(path.join(systemRoot, B.system));
} else {
  console.log(`the packages' pointer is installed: ${POINTER}`);
}
const home = path.join(tmp, "home");
// The browser's helpers are looked for under $HOME's default profile folder; its data goes to a folder of its own
// (Chrome allows remote debugging only with a data folder other than the default one).
const nmDir = path.join(home, B.config);
const profile = path.join(tmp, "browser-data");
const bin = path.join(tmp, "bin");
const opened = path.join(tmp, "opened.log");
const folderOpened = path.join(tmp, "folder-opened.log");
fs.mkdirSync(bin, { recursive: true });
fs.mkdirSync(profile, { recursive: true });
fs.mkdirSync(path.join(nmDir, "NativeMessagingHosts"), { recursive: true });
// Some browsers look for helpers in the data folder given on the command line instead: the same folder there.
fs.symlinkSync(path.join(nmDir, "NativeMessagingHosts"), path.join(profile, "NativeMessagingHosts"));
const srv = await startFileServer(8 * MB);
const h = createHarness({
  port: 9403,
  appPath: APP,
  env: {
    PATH: `${bin}:${process.env.PATH}`,
    VELOX_NMH_SCRATCH_HOME: home,
    VELOX_AUTO_EXTENSION: "1",
    VELOX_STORE_SYSTEM_ROOT: systemRoot,
  },
});
const { page, invoke } = h;
const text = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`)).ok;
let browserPid = null;

try {
  // The helper the browser starts gets the test app's folders (a browser does not have to pass its environment on).
  const hostWrapper = path.join(bin, "velox-host-wrapper");
  fs.writeFileSync(hostWrapper, `#!/bin/sh\necho "$(date +%T) $*" >> '${tmp}/host.log'\nexport VELOX_DATA_DIR='${h.dataDir}' VELOX_BRIDGE_ENDPOINT='${h.endpoint}'\nexec '${HOST}' "$@"\n`, { mode: 0o755 });
  const manifest = path.join(nmDir, "NativeMessagingHosts", "app.velox.fetch.json");
  // Stand-ins: the browser launcher adds the throwaway profile and points the manifest VeloxFetch wrote at the
  // wrapper; xdg-open only records that something tried to open a folder.
  fs.writeFileSync(
    path.join(bin, B.launcher),
    `#!/bin/sh
printf '%s\\n' "$*" >> '${opened}'
[ -f '${manifest}' ] && sed -i 's|"path": ".*"|"path": "${hostWrapper}"|' '${manifest}'
echo $$ > '${tmp}/browser.pid'
# Linux browsers look for helpers under the default profile folder of $HOME, not under --user-data-dir.
export HOME='${home}' XDG_CONFIG_HOME='${home}/.config'
exec '${REAL}' --user-data-dir='${profile}' --remote-debugging-port=${DBG} --ozone-platform=x11 --no-first-run --no-default-browser-check --password-store=basic "$@" 2>> '${tmp}/browser.log'
`,
    { mode: 0o755 },
  );
  fs.writeFileSync(path.join(bin, "xdg-open"), `#!/bin/sh\nprintf '%s\\n' "$*" >> '${folderOpened}'\n`, { mode: 0o755 });
  // The test's desktop says this browser is the default one: the app's settings folder is the test's own, and a
  // stand-in `xdg-settings` answers for desktops (KDE) that keep the default browser outside mimeapps.list.
  fs.writeFileSync(path.join(bin, "xdg-settings"), `#!/bin/sh\n[ "$*" = "get default-web-browser" ] && echo ${B.launcher}.desktop\n`, { mode: 0o755 });
  fs.mkdirSync(path.join(h.profile, "config"), { recursive: true });
  fs.writeFileSync(path.join(h.profile, "config", "mimeapps.list"), `[Default Applications]\nx-scheme-handler/http=${B.launcher}.desktop\nx-scheme-handler/https=${B.launcher}.desktop\ntext/html=${B.launcher}.desktop\n`);

  // ============================================================ 1. first start
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "the interface");
  await waitFor(() => fs.existsSync(manifest), "VeloxFetch to connect the default browser by itself on first start", 30000);
  const first = await waitFor(async () => {
    const s = (await invoke("get_browser_integration")).ok;
    return s?.extensionAwaitedSince ? s : null;
  }, "VeloxFetch to wait for the browser to add the extension", 10000);
  await sleep(6000); // whatever the first start would open, it has opened by now
  check("first start connected the browser and waits for it to add the extension by itself", first.extensionAwaitedSince > 0 && first.extensionContact === null);
  check("no store page or any page was opened: nothing for the user to do", !fs.existsSync(opened), fs.existsSync(opened) ? fs.readFileSync(opened, "utf8") : "");
  check("no folder was opened", !fs.existsSync(folderOpened), fs.existsSync(folderOpened) ? fs.readFileSync(folderOpened, "utf8") : "");
  check("no extension folder was prepared", !fs.existsSync(path.join(h.dataDir, "browser-extension", "chromium")));
  const m = JSON.parse(fs.readFileSync(manifest, "utf8"));
  check("the browser is connected: it may start the helper for the store extension", m.allowed_origins.includes(`chrome-extension://${ID}/`), JSON.stringify(m.allowed_origins));
  await page(`document.querySelector("[data-testid=nav-settings]").click()`);
  await waitFor(async () => (await page(`document.querySelector('[data-testid=extension-status]')?.dataset.state`)).ok === "waiting", "Settings to say the browser is adding the extension");
  check("Settings says the browser adds the extension by itself", /by itself/.test(await text("[data-testid=extension-status]")));

  // ============================================================ 2. the browser starts, as the user's does
  spawn(path.join(bin, B.launcher), ["about:blank"], { stdio: "ignore", detached: true }).unref();
  await waitFor(() => fs.existsSync(path.join(tmp, "browser.pid")), "the browser to start");
  browserPid = Number(fs.readFileSync(path.join(tmp, "browser.pid"), "utf8"));
  const cdp = async () => {
    const list = await (await fetch(`http://127.0.0.1:${DBG}/json/list`)).json();
    return list;
  };
  const shot = async (target, file) => {
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
    const res = await new Promise((r) => {
      ws.onmessage = (e) => {
        const msg = JSON.parse(e.data);
        if (msg.id === 1) r(msg.result);
      };
      ws.send(JSON.stringify({ id: 1, method: "Page.captureScreenshot", params: { format: "png" } }));
    });
    ws.close();
    fs.writeFileSync(file, Buffer.from(res.data, "base64"));
  };

  // ============================================================ 2. the browser adds it by itself (the pointer)
  const worker = await waitFor(async () => {
    try {
      return (await cdp()).find((t) => t.type === "service_worker" && t.url.startsWith(`chrome-extension://${ID}/`)) ?? null;
    } catch {
      return null;
    }
  }, "the browser to add the store extension by itself", 180000).catch(() => null);
  // The browser saves its record a few seconds later. Location 6: added from an external pointer, downloaded.
  const prefs = await waitFor(async () => {
    for (const f of ["Secure Preferences", "Preferences"]) {
      try {
        const s = JSON.parse(fs.readFileSync(path.join(profile, "Default", f), "utf8"))?.extensions?.settings?.[ID];
        if (s?.manifest) return s;
      } catch {}
    }
    return null;
  }, "the browser to save its record of the extension", 30000).catch(() => null);
  console.log("the browser's record of the extension:", JSON.stringify(prefs && { location: prefs.location, from_webstore: prefs.from_webstore, disable_reasons: prefs.disable_reasons, name: prefs.manifest?.name, version: prefs.manifest?.version }));
  check("the browser added the extension from the Chrome Web Store by itself (the packages' pointer)", prefs?.from_webstore === true && prefs.location === 6, JSON.stringify(prefs?.manifest?.name));
  check("enabled at once, nothing for the user to do", (prefs?.disable_reasons ?? [1]).length === 0 && !(prefs?.state === 0), JSON.stringify(prefs?.disable_reasons));
  check("and it runs (enabled)", !!worker, "not running: the browser keeps it off until the user enables it");

  // Nothing done by anyone yet: has the extension reached VeloxFetch by itself (its start-up ping)?
  const auto = await waitFor(async () => (await invoke("get_browser_integration")).ok?.extensionContact ?? null, "the extension's own first contact", 60000).catch(() => null);
  console.log("the extension's own first contact:", JSON.stringify(auto));
  if (process.env.VELOX_DEBUG && worker) {
    const fresh = (await cdp()).find((t) => t.type === "service_worker" && t.url.startsWith(`chrome-extension://${ID}/`));
    console.log("worker now:", JSON.stringify(fresh?.url ?? null));
    const ws = new WebSocket((fresh ?? worker).webSocketDebuggerUrl);
    await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
    const out = await new Promise((r) => {
      ws.onmessage = (e) => {
        const msg = JSON.parse(e.data);
        if (msg.id === 1) r(JSON.stringify(msg.result).slice(0, 500));
      };
      ws.send(JSON.stringify({ id: 1, method: "Runtime.evaluate", params: { expression: `new Promise((r) => chrome.runtime.sendNativeMessage("app.velox.fetch", { command: "ping" }, (x) => r(JSON.stringify(x ?? chrome.runtime.lastError?.message))))`, awaitPromise: true, returnByValue: true } }));
    });
    ws.close();
    console.log("direct ping:", out, "\nbrowser log:", fs.readFileSync(path.join(tmp, "browser.log"), "utf8").split("\n").filter((l) => /native|messag|host/i.test(l)).slice(-10).join("\n"));
  }
  // ============================================================ 3. VeloxFetch sees it arrive
  const arrived = await waitFor(
    async () => (await page(`document.querySelector('[data-testid=extension-banner]')?.dataset.state`)).ok === "arrived",
    "the window to say the extension is installed and connected",
    60000,
  ).catch(() => false);
  check("the window says: browser extension installed and connected", !!arrived && /installed/.test(await text("[data-testid=extension-banner]")));
  const after = (await invoke("get_browser_integration")).ok;
  check("the contact is from the store extension, and the wait is over", after.extensionContact?.fromStore === true && after.extensionAwaitedSince === null, JSON.stringify(after.extensionContact));
  await waitFor(async () => (await page(`document.querySelector('[data-testid=extension-status]')?.dataset.state`)).ok === "ok", "Settings to show the extension working");
  check("Settings shows: extension installed and working", /installed and working/.test(await text("[data-testid=extension-status]")));
  if (arrived) await h.screenshot(path.resolve("docs/screenshots/extension-arrived.png"));

  if (worker) {
    // ============================================================ 3. the store extension and VeloxFetch
    const ws = new WebSocket(worker.webSocketDebuggerUrl);
    await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
    let n = 0;
    const evalIn = (expr) =>
      new Promise((resolve, reject) => {
        const id = ++n;
        const t = setTimeout(() => reject(new Error("no answer")), 20000);
        ws.addEventListener("message", function on(e) {
          const msg = JSON.parse(e.data);
          if (msg.id !== id) return;
          ws.removeEventListener("message", on);
          clearTimeout(t);
          msg.result?.exceptionDetails ? reject(new Error(JSON.stringify(msg.result.exceptionDetails).slice(0, 300))) : resolve(msg.result?.result?.value);
        });
        ws.send(JSON.stringify({ id, method: "Runtime.evaluate", params: { expression: expr, awaitPromise: true, returnByValue: true } }));
      });
    if (process.env.VELOX_DEBUG) {
      console.log("direct:", await evalIn(`new Promise((r) => chrome.runtime.sendNativeMessage("app.velox.fetch", { command: "ping" }, (x) => r(JSON.stringify(x ?? chrome.runtime.lastError?.message))))`).catch((e) => e.message));
    }
    const ping = await waitFor(async () => {
      const r = await evalIn(`nativeCall({ command: "ping" })`).catch(() => null);
      return r?.ok ? r : null;
    }, "the store extension to reach VeloxFetch", 30000);
    check("ping: store extension -> the browser's native messaging -> helper -> this test's VeloxFetch", ping.ok === true && typeof ping.version === "string", JSON.stringify(ping));
    await invoke("set_browser_confirm", { enabled: false });
    const url = `${srv.base}/store-handoff.bin`;
    const r = await evalIn(`nativeCall({ command: "download", url: ${JSON.stringify(url)}, source: "contextMenu" })`);
    check("a link the store extension hands over is accepted", r?.ok === true, JSON.stringify(r));
    const d = await waitFor(async () => (await invoke("list_downloads", { filter: {} })).ok.find((x) => x.filename === "store-handoff.bin" && x.status === "completed"), "the handed-over download", 60000);
    check("and downloaded intact", sha256(fs.readFileSync(d.destination)) === srv.sha);
    ws.close();
  }

  // The extensions page, as the browser shows it: VeloxFetch from the Chrome Web Store.
  const ext = await (await fetch(`http://127.0.0.1:${DBG}/json/new?chrome://extensions/?id=${ID}`, { method: "PUT" })).json();
  await sleep(2500);
  await shot(ext, path.resolve("docs/screenshots/store-extension-installed.png"));
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  if (browserPid) {
    try {
      process.kill(browserPid, "SIGTERM");
    } catch {}
    await waitFor(async () => {
      try {
        process.kill(browserPid, 0);
        return false;
      } catch {
        return true;
      }
    }, "the test browser to stop", 15000).catch(() => {
      try {
        process.kill(browserPid, "SIGKILL");
      } catch {}
    });
  }
  srv.close();
  await h.cleanup();
  if (process.env.VELOX_KEEP) console.log("kept:", tmp);
  else fs.rmSync(tmp, { recursive: true, force: true });
  finish();
}
