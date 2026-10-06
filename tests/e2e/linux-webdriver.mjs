// The Linux side of the end-to-end harness. On Windows the tests talk to WebView2 over the Chrome
// DevTools Protocol; on Linux the web view is WebKitGTK, which speaks WebDriver instead
// (`WebKitWebDriver`, the driver `tauri-driver` uses). This file gives the tests the same `h` object:
// `page` / `invoke` / `screenshot`, the collected console events, the app process, and a small `cdp`
// stand-in for the few DevTools calls the suites make directly.
//
// Isolation, as on Windows: the app runs on its own virtual display (Xvfb, unless VELOX_E2E_DISPLAY
// names one), with private data, download, config and web view folders, so it never meets the user's
// desktop session, their VeloxFetch, or their profile.
//
// Needs: `WebKitWebDriver` (package webkit2gtk-driver) and `Xvfb` (package xvfb) on PATH.

import { spawn, spawnSync } from "node:child_process";
import { EventEmitter } from "node:events";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const WINDOW = { width: 1360, height: 860 }; // tauri.conf.json

// ---------------------------------------------------------------------------- the virtual display

let display = null;
/** Is a window manager running on the test display? (Maximize / restore need one.) */
export let windowManager = !!process.env.VELOX_E2E_DISPLAY;

/**
 * A window manager for the virtual display, when one is installed (KWin): in its own D-Bus session and
 * with its own config folders, so it never talks to, or changes the settings of, the user's desktop.
 */
async function startWindowManager(DISPLAY) {
  if (process.env.VELOX_E2E_WM === "0" || !have("kwin_x11") || !have("dbus-run-session")) return false;
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-wm-"));
  for (const d of ["config", "data", "cache", "run"]) fs.mkdirSync(path.join(dir, d), { mode: 0o700 });
  const env = {
    PATH: process.env.PATH,
    HOME: process.env.HOME,
    DISPLAY,
    QT_QPA_PLATFORM: "xcb",
    KWIN_COMPOSE: "N",
    XDG_SESSION_TYPE: "x11",
    XDG_CONFIG_HOME: path.join(dir, "config"),
    XDG_DATA_HOME: path.join(dir, "data"),
    XDG_CACHE_HOME: path.join(dir, "cache"),
    XDG_RUNTIME_DIR: path.join(dir, "run"),
  };
  const wm = spawn("dbus-run-session", ["--", "kwin_x11"], { env, stdio: "ignore", detached: true });
  wm.unref();
  process.on("exit", () => {
    try {
      process.kill(-wm.pid, "SIGTERM"); // the whole group: dbus-daemon and kwin
    } catch {}
    try {
      fs.rmSync(dir, { recursive: true, force: true });
    } catch {} // KWin may still hold a socket in it for a moment; it is only a temp folder
  });
  await sleep(2500); // KWin takes the display over in about a second
  return true;
}

/** One Xvfb for the whole test run (started on first use, stopped when node exits). */
async function virtualDisplay() {
  if (process.env.VELOX_E2E_DISPLAY) return process.env.VELOX_E2E_DISPLAY;
  if (display) return display;
  let n = 90 + (process.pid % 100);
  while (fs.existsSync(`/tmp/.X11-unix/X${n}`) || fs.existsSync(`/tmp/.X${n}-lock`)) n++;
  const xvfb = spawn("Xvfb", [`:${n}`, "-screen", "0", "1920x1280x24", "-nolisten", "tcp"], { stdio: "ignore" });
  xvfb.on("error", (e) => {
    console.error(`cannot start Xvfb (${e.message}): install xvfb, or set VELOX_E2E_DISPLAY`);
    process.exit(1);
  });
  xvfb.unref(); // never what keeps node running
  process.on("exit", () => xvfb.kill());
  const t = Date.now();
  while (!fs.existsSync(`/tmp/.X11-unix/X${n}`)) {
    if (Date.now() - t > 10000) throw new Error("Xvfb did not start");
    await sleep(50);
  }
  display = `:${n}`;
  windowManager = await startWindowManager(display);
  return display;
}

// ---------------------------------------------------------------------------- WebDriver

async function portOpen(port) {
  return new Promise((r) => {
    const c = net.connect(port, "127.0.0.1", () => (c.end(), r(true)));
    c.on("error", () => r(false));
  });
}

function driver(port) {
  const call = async (method, p, body) => {
    const res = await fetch(`http://127.0.0.1:${port}${p}`, {
      method,
      headers: { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(150000),
    });
    const json = await res.json();
    if (json?.value?.error) throw new Error(`WebDriver ${method} ${p}: ${json.value.error}: ${json.value.message}`);
    return json.value;
  };
  return call;
}

// Collects what the DevTools events would have told the Windows tests (exceptions, console errors, CSP
// refusals), in the same shape. Installed again whenever the page was reloaded.
const COLLECTOR = `if (!window.__veloxE2e) {
  const q = (window.__veloxE2e = []);
  window.addEventListener("error", (e) => q.push({ method: "Runtime.exceptionThrown", params: { exceptionDetails: { text: String(e.message) } } }));
  window.addEventListener("unhandledrejection", (e) => q.push({ method: "Runtime.exceptionThrown", params: { exceptionDetails: { text: String(e.reason) } } }));
  document.addEventListener("securitypolicyviolation", (e) => q.push({ method: "Log.entryAdded", params: { entry: { level: "error", text: "Refused to load " + e.blockedURI + " because it violates the Content Security Policy directive " + e.violatedDirective } } }));
  const err = console.error.bind(console);
  console.error = (...a) => { q.push({ method: "Runtime.consoleAPICalled", params: { type: "error", args: a.map((x) => ({ value: String(x) })) } }); err(...a); };
}`;

// ---------------------------------------------------------------------------- the app process

/**
 * WebKitWebDriver starts the app itself, so the app is its child, not ours. A tiny launcher script sits
 * in between and writes the app's exit code to a file: `h.app` then behaves like a child process
 * (pid, exitCode, kill, "exit" with the real code).
 */
function launcherScript(tmp, appPath) {
  const script = path.join(tmp, "launch-app.sh");
  const codeFile = path.join(tmp, "app-exit-code");
  const pidFile = path.join(tmp, "app-pid");
  fs.writeFileSync(
    script,
    `#!/bin/sh\nrm -f '${codeFile}'\n'${appPath}' "$@" &\nchild=$!\necho $child > '${pidFile}'\ntrap 'kill -TERM $child 2>/dev/null' TERM INT\nwait $child\ncode=$?\necho $code > '${codeFile}'\nexit $code\n`,
    { mode: 0o755 },
  );
  return { script, codeFile, pidFile };
}

function appHandle(files) {
  const app = new EventEmitter();
  app.pid = null;
  app.exitCode = null;
  app.stderr = null;
  const t0 = Date.now();
  const poll = setInterval(() => {
    if (app.pid === null && fs.existsSync(files.pidFile)) app.pid = Number(fs.readFileSync(files.pidFile, "utf8"));
    if (fs.existsSync(files.codeFile)) {
      const code = Number(fs.readFileSync(files.codeFile, "utf8").trim());
      if (Number.isFinite(code)) {
        clearInterval(poll);
        app.exitCode = code;
        app.emit("exit", code);
      }
    } else if (app.pid === null && Date.now() - t0 > 30000) {
      clearInterval(poll);
    }
  }, 50);
  poll.unref?.();
  app.kill = (signal = "SIGTERM") => {
    try {
      if (app.pid) process.kill(app.pid, signal);
      return true;
    } catch {
      return false;
    }
  };
  return app;
}

/** Close the app's window the way a user would: the window manager's close button (WM_DELETE_WINDOW). */
export async function closeWindowLikeAUser(h) {
  const helper = path.join(path.dirname(new URL(import.meta.url).pathname), "x11-close-window.py");
  const r = spawnSync("python3", [helper, String(h.app.pid)], { env: { ...process.env, DISPLAY: h.display }, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`could not close the app's window (x11-close-window.py exit ${r.status}) ${r.stderr ?? ""}`);
}

// ---------------------------------------------------------------------------- the harness

export function createLinuxHarness({ port, appPath, env = {}, captureStderr = false, waitFor }) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-"));
  const h = {
    tmp,
    dataDir: path.join(tmp, "data"),
    root: path.join(tmp, "downloads"),
    profile: path.join(tmp, "webview"),
    endpoint: path.join(tmp, "bridge.sock"),
    app: null,
    cdp: null,
    events: [],
    stderr: "",
    session: null,
    driverProc: null,
  };
  h.webdriver = driver(port);
  const wd = h.webdriver;

  h.launch = async () => {
    const DISPLAY = await virtualDisplay();
    h.display = DISPLAY;
    h.windowManager = windowManager;
    const files = launcherScript(tmp, appPath);
    const appEnv = {
      ...process.env,
      DISPLAY,
      WAYLAND_DISPLAY: "",
      GDK_BACKEND: "x11",
      // The web view's own data (local storage...) and the app's config go to the test's folder, never the user's.
      XDG_DATA_HOME: path.join(h.profile, "data"),
      XDG_CONFIG_HOME: path.join(h.profile, "config"),
      XDG_CACHE_HOME: path.join(h.profile, "cache"),
      TAURI_WEBVIEW_AUTOMATION: "true",
      VELOX_DATA_DIR: h.dataDir,
      VELOX_DOWNLOAD_ROOT: h.root,
      VELOX_ORGANIZE: "0",
      VELOX_DRY_RUN_SHELL: "1",
      VELOX_BRIDGE_ENDPOINT: h.endpoint,
      ...env,
    };
    delete appEnv.VELOX_E2E_DEBUG_PORT;
    // One session per driver, and a session outlives the app it drove: a fresh driver for every launch.
    if (h.driverProc && h.driverProc.exitCode === null) {
      if (h.session) await wd("DELETE", `/session/${h.session}`).catch(() => {});
      h.driverProc.kill();
      await waitFor(async () => !(await portOpen(port)), "the old WebKitWebDriver to stop", 10000);
    }
    h.session = null;
    {
      h.driverProc = spawn("WebKitWebDriver", [`--port=${port}`], { env: appEnv, stdio: ["ignore", "ignore", captureStderr ? "pipe" : "ignore"] });
      h.driverProc.on("error", (e) => {
        console.error(`cannot start WebKitWebDriver (${e.message}): install webkit2gtk-driver`);
        process.exit(1);
      });
      // The app's stderr comes out through the driver, which started it.
      h.driverProc.stderr?.on("data", (d) => (h.stderr += d.toString()));
      await waitFor(() => portOpen(port), "WebKitWebDriver", 10000);
    }
    h.app = appHandle(files);
    const s = await wd("POST", "/session", { capabilities: { alwaysMatch: { "webkitgtk:browserOptions": { binary: files.script, args: [] } } } });
    h.session = s.sessionId;
    await wd("POST", `/session/${h.session}/timeouts`, { script: 140000 });
    await waitFor(async () => h.app.pid !== null, "the app process", 10000);
    await h.attach();
  };

  /** The session is made when the app starts; on Linux there is no attaching to an app started elsewhere. */
  h.attach = async () => {
    if (!h.session || !h.app || h.app.exitCode !== null) {
      throw new Error("Linux: WebKitWebDriver can only drive an app it started itself (use h.launch)");
    }
    await waitFor(async () => (await evaluate("!!window.__TAURI_INTERNALS__ && document.readyState === 'complete'").catch(() => ({}))).ok, "the app page", 30000);
    h.cdp = cdp;
  };

  const evaluate = async (expr) => {
    const script = `const done = arguments[arguments.length - 1];
${COLLECTOR}
(async () => { try { return { ok: await (${expr}) }; } catch (e) { return { err: (e && e.code) ? e : { message: String(e) } }; } })()
  // WebDriver would turn \`undefined\` into null: mark it, so the result is DevTools' \`{ ok: undefined }\` exactly.
  .then((v) => done({ v: v && "ok" in v && v.ok === undefined ? { undef: true } : v, ev: window.__veloxE2e.splice(0) }),
        (e) => done({ v: { err: { message: String(e) } }, ev: [] }));`;
    const r = await wd("POST", `/session/${h.session}/execute/async`, { script, args: [] });
    for (const e of r?.ev ?? []) h.events.push(e);
    // DevTools' returnByValue gives \`{ ok: undefined }\` for an expression without a value: the same here.
    return !r?.v || r.v.undef ? { ok: undefined } : r.v;
  };
  h.page = (expr) => evaluate(expr);
  h.invoke = (cmd, args = {}) => h.page(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`);
  h.exited = () => new Promise((r) => (h.app.exitCode !== null ? r(h.app.exitCode) : h.app.once("exit", (code) => r(code))));
  h.closeWindow = () => closeWindowLikeAUser(h);

  const setSize = (width, height) => wd("POST", `/session/${h.session}/window/rect`, { width, height }).catch(() => {});
  h.screenshot = async (file, { width, height } = {}) => {
    if (width) {
      await setSize(width, height);
      await sleep(250); // let the layout settle at the new size
    }
    const data = await wd("GET", `/session/${h.session}/screenshot`);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, Buffer.from(data, "base64"));
    if (width) await setSize(WINDOW.width, WINDOW.height);
  };

  // The DevTools calls the suites make themselves, done the WebDriver way.
  const cdp = {
    send: async (method, params = {}) => {
      switch (method) {
        case "Runtime.enable":
        case "Log.enable":
        case "Page.enable":
          return {};
        case "Runtime.evaluate": {
          const v = await evaluate(params.expression);
          if (v.err) return { result: { value: undefined }, exceptionDetails: { text: v.err.message } };
          return { result: { value: v.ok } };
        }
        case "Emulation.setDeviceMetricsOverride":
          await setSize(params.width, params.height);
          await sleep(250);
          return {};
        case "Emulation.clearDeviceMetricsOverride":
          await setSize(WINDOW.width, WINDOW.height);
          return {};
        case "Page.captureScreenshot":
          return { data: await wd("GET", `/session/${h.session}/screenshot`) };
        case "Browser.close":
          await wd("DELETE", `/session/${h.session}`).catch(() => {});
          h.session = null;
          return {};
        default:
          throw new Error(`${method} has no WebDriver equivalent in the Linux harness`);
      }
    },
    on: () => {},
  };

  h.cleanup = async () => {
    if (h.session) await wd("DELETE", `/session/${h.session}`).catch(() => {});
    h.app?.kill();
    h.driverProc?.kill();
    await sleep(300);
    fs.rmSync(tmp, { recursive: true, force: true });
  };
  return h;
}

/** Is a program on PATH? (For a clear message instead of a stack trace.) */
export function have(program) {
  return spawnSync("sh", ["-c", `command -v ${program}`], { stdio: "ignore" }).status === 0;
}
