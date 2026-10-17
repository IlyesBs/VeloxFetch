// By hand, with the internet, on Linux with Brave (or Chrome / Chromium) installed: the extension from the Chrome
// Web Store, end to end, with the real browser and the real store:
//
//  1. The packages' pointer (postinst.sh) is in the browser's system folder: the browser adds the published
//     extension by itself from the Chrome Web Store, in a fresh profile, without anyone clicking anything in
//     VeloxFetch's folder or the extensions page.
//  2. First start of VeloxFetch (default browser = this browser): it connects the browser and opens the extension's
//     store page in it. It never opens a folder (a stand-in `xdg-open` proves it) or the extensions page.
//  3. The store extension talks to VeloxFetch through the browser's own native messaging (ping), and a link it
//     hands over is downloaded, intact.
//
//   sudo sh -c "$(sed -n '/^VELOX_EXTENSION_ID=/,/^done$/p' apps/desktop/src-tauri/packaging/postinst.sh)"   (once)
//   node tests/e2e/real-store-extension.mjs [path-to-debug-velox-desktop]
//
// What it touches, only for the run: a throwaway browser profile (your own profile is never opened, and the browser
// runs on the test's hidden display); the test app's own folders. The browser is started by a stand-in launcher
// that adds the throwaway profile, so VeloxFetch's "open the store page" never reaches your running browser. The
// browser is stopped by its own process id.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, finish, path, sha256, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const HOST = path.join(path.dirname(APP), exe("velox-native-host"));
const ID = /CHROME_STORE_ID: Option<&str> = Some\("([a-p]{32})"\)/.exec(fs.readFileSync("crates/browser-bridge/src/caller.rs", "utf8"))[1];
const POINTER = `/usr/share/chromium/extensions/${ID}.json`;
let REAL = ["/usr/bin/brave-browser", "/usr/bin/google-chrome", "/usr/bin/chromium"].find((p) => fs.existsSync(p));
const MB = 1024 * 1024;
const DBG = 9404;

if (process.platform !== "linux" || !REAL) {
  console.log("SKIP  Linux with Brave, Chrome or Chromium only");
  process.exit(0);
}
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-store-"));
// Without the system pointer (no root to run the packages' script): a private copy of the installed Brave, whose
// `extensions` folder beside the program Chromium reads the same way (DIR_EXTERNAL_EXTENSIONS), gets the pointer
// written by the packages' own lines (postinst.sh, only the folder changed).
if (!fs.existsSync(POINTER)) {
  const BRAVE_DIR = "/opt/brave.com/brave";
  if (!fs.existsSync(path.join(BRAVE_DIR, "brave"))) {
    console.log(`SKIP  ${POINTER} is missing and there is no Brave to copy: run the postinst part shown in this file's header (sudo)`);
    process.exit(0);
  }
  const copy = path.join(tmp, "brave");
  execFileSync("cp", ["-a", "--reflink=auto", BRAVE_DIR, copy]);
  const lines = fs.readFileSync("apps/desktop/src-tauri/packaging/postinst.sh", "utf8").match(/^VELOX_EXTENSION_ID=[\s\S]*?^done$/m)[0];
  const script = lines.replace(/^for dir in .*; do$/m, `for dir in '${path.join(copy, "extensions")}'; do`);
  execFileSync("sh", ["-c", script]);
  const written = path.join(copy, "extensions", `${ID}.json`);
  console.log(`no system pointer: a private copy of Brave, pointer written by postinst.sh's lines: ${fs.readFileSync(written, "utf8").trim()}`);
  REAL = path.join(copy, "brave");
}
const home = path.join(tmp, "home");
const profile = path.join(home, ".config/BraveSoftware/Brave-Browser");
const bin = path.join(tmp, "bin");
const opened = path.join(tmp, "opened.log");
const folderOpened = path.join(tmp, "folder-opened.log");
fs.mkdirSync(bin, { recursive: true });
fs.mkdirSync(profile, { recursive: true });
const srv = await startFileServer(8 * MB);
const h = createHarness({
  port: 9403,
  appPath: APP,
  env: {
    PATH: `${bin}:${process.env.PATH}`,
    VELOX_NMH_SCRATCH_HOME: home,
    VELOX_AUTO_EXTENSION: "1",
  },
});
const { page, invoke } = h;
const text = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`)).ok;
let browserPid = null;

try {
  // The helper the browser starts gets the test app's folders (a browser does not have to pass its environment on).
  const hostWrapper = path.join(bin, "velox-host-wrapper");
  fs.writeFileSync(hostWrapper, `#!/bin/sh\nexport VELOX_DATA_DIR='${h.dataDir}' VELOX_BRIDGE_ENDPOINT='${h.endpoint}'\nexec '${HOST}' "$@"\n`, { mode: 0o755 });
  const manifest = path.join(profile, "NativeMessagingHosts", "app.velox.fetch.json");
  // Stand-ins: the browser launcher adds the throwaway profile and points the manifest VeloxFetch wrote at the
  // wrapper; xdg-open only records that something tried to open a folder.
  fs.writeFileSync(
    path.join(bin, "brave-browser"),
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
  // The test's desktop says this browser is the default one (the app's settings folder is the test's own).
  fs.mkdirSync(path.join(h.profile, "config"), { recursive: true });
  fs.writeFileSync(path.join(h.profile, "config", "mimeapps.list"), "[Default Applications]\nx-scheme-handler/http=brave-browser.desktop\nx-scheme-handler/https=brave-browser.desktop\ntext/html=brave-browser.desktop\n");

  // ============================================================ 2. first start
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "the interface");
  await waitFor(() => fs.existsSync(opened), "VeloxFetch to open the browser by itself on first start", 30000);
  const args = fs.readFileSync(opened, "utf8").trim();
  console.log("the browser was opened with:", args);
  check("first start opened the default browser on the extension's Chrome Web Store page", args === `https://chromewebstore.google.com/detail/${ID}`, args);
  check("no folder was opened", !fs.existsSync(folderOpened), fs.existsSync(folderOpened) ? fs.readFileSync(folderOpened, "utf8") : "");
  check("not the extensions page", !/extensions/.test(args));
  check("no extension folder was prepared", !fs.existsSync(path.join(h.dataDir, "browser-extension", "chromium")));
  const m = JSON.parse(fs.readFileSync(manifest, "utf8"));
  check("the browser is connected: it may start the helper for the store extension", m.allowed_origins.includes(`chrome-extension://${ID}/`), JSON.stringify(m.allowed_origins));
  browserPid = Number(fs.readFileSync(path.join(tmp, "browser.pid"), "utf8"));

  // The store page, as the browser shows it.
  const cdp = async () => {
    const list = await (await fetch(`http://127.0.0.1:${DBG}/json/list`)).json();
    return list;
  };
  const tabs = await waitFor(async () => {
    try {
      const l = await cdp();
      return l.some((t) => t.type === "page" && t.url.includes(ID) && /VeloxFetch/i.test(t.title)) ? l : null;
    } catch {
      return null;
    }
  }, "the store page to load", 60000).catch(async (e) => {
    console.log("tabs:", JSON.stringify(await cdp().catch((x) => String(x))).slice(0, 2000));
    console.log("browser log:", fs.existsSync(path.join(tmp, "browser.log")) ? fs.readFileSync(path.join(tmp, "browser.log"), "utf8").slice(-3000) : "none");
    throw e;
  });
  const store = tabs.find((t) => t.type === "page" && t.url.includes(ID));
  check("the Chrome Web Store page of VeloxFetch is open in the browser", /VeloxFetch/i.test(store.title), store.title);
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
  await sleep(2500);
  await shot(store, path.resolve("docs/screenshots/store-extension-page.png"));

  // ============================================================ 1. the browser adds it by itself (the pointer)
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
