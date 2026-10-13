// Shared machinery for the end-to-end tests: a local range-capable file server, a tiny Chrome
// DevTools Protocol client, and a launcher for the real desktop app (debug build, WebView2).
// On Linux the same `h` is driven over WebDriver instead (WebKitGTK): see linux-webdriver.mjs.

import { execFileSync, spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { createLinuxHarness } from "./linux-webdriver.mjs";

export { sleep, fs, path, crypto };

/** `target/debug/velox-desktop` -> `target/debug/velox-desktop.exe` on Windows. */
export const exe = (p) => (process.platform === "win32" ? `${p}.exe` : p);

export const sha256 = (buf) => crypto.createHash("sha256").update(buf).digest("hex");

/**
 * Tests that close the window mean "quit". Where a tray is available the app keeps running in it when its
 * window is closed (on by default since the tray came in), so that is turned off first, like a user who chose
 * "quit when the window is closed".
 */
export async function quitOnClose(h) {
  await h.invoke("set_desktop_options", { request: { closeToTray: false } }).catch(() => {});
}

/** Local file server with ranges, a per-chunk delay (throttle) and a request counter. */
export async function startFileServer(sizeBytes) {
  const FILE = crypto.randomBytes(sizeBytes);
  // `rateBps`: the whole server's speed, shared by every connection (like a real line), when set.
  // `busy`: `/busy/..` answers 502 (a server in passing trouble) while it is set.
  const state = { delayMs: 0, rateBps: 0, next: 0, requests: 0, busy: false, FILE, sha: sha256(FILE), base: "", headers: [] };
  const server = http.createServer(async (req, res) => {
    state.requests++;
    state.headers.push(req.headers); // lets a test check what the client sent
    if (req.url.startsWith("/404")) return void res.writeHead(404).end();
    if (req.url.startsWith("/busy/") && state.busy) return void res.writeHead(502).end();
    // `/auth/..` wants user:pass (HTTP Basic), like a members-only file.
    if (req.url.startsWith("/auth/") && req.headers.authorization !== `Basic ${Buffer.from("user:pass").toString("base64")}`) {
      return void res.writeHead(401, { "WWW-Authenticate": 'Basic realm="e2e"' }).end();
    }
    const total = FILE.length;
    const m = /bytes=(\d+)-(\d*)/.exec(req.headers.range ?? "");
    let [start, end, status] = [0, total - 1, 200];
    const headers = { "Accept-Ranges": "bytes", ETag: '"e2e-1"', Server: "velox-e2e/1.0", "Content-Type": "application/octet-stream" };
    if (m && req.method === "GET") {
      start = Number(m[1]);
      end = m[2] ? Math.min(Number(m[2]), total - 1) : total - 1;
      status = 206;
      headers["Content-Range"] = `bytes ${start}-${end}/${total}`;
    }
    headers["Content-Length"] = end - start + 1;
    res.writeHead(status, headers);
    if (req.method === "HEAD") return void res.end();
    for (let pos = start; pos <= end; pos += 65536) {
      if (res.destroyed) return;
      res.write(FILE.subarray(pos, Math.min(pos + 65536, end + 1)));
      if (state.delayMs) await sleep(state.delayMs);
      if (state.rateBps) {
        const now = Date.now();
        state.next = Math.max(state.next, now) + (65536 / state.rateBps) * 1000;
        await sleep(state.next - now);
      }
    }
    res.end();
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  state.base = `http://127.0.0.1:${server.address().port}`;
  state.close = () => server.close();
  return state;
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.listeners = [];
    this.closed = false;
    // The web view went away (crashed and was rebuilt): every waiting call fails now, not after 120 s.
    ws.addEventListener("close", () => {
      this.closed = true;
      for (const { reject } of this.pending.values()) reject(new Error("the web view went away (WebView2 restarted)"));
      this.pending.clear();
    });
    ws.addEventListener("message", (m) => {
      const msg = JSON.parse(m.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
      } else if (msg.method) {
        for (const l of this.listeners) l(msg);
      }
    });
  }
  send(method, params = {}) {
    if (this.closed) return Promise.reject(new Error("the web view went away (WebView2 restarted)"));
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    // A web view that went away (crashed, or replaced by a WebView2 runtime update) never answers:
    // fail the step instead of hanging the whole suite.
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`no answer from the web view to ${method} within 120 s (did WebView2 exit?)`));
      }, 120000);
      this.pending.set(id, { resolve: (v) => (clearTimeout(t), resolve(v)), reject: (e) => (clearTimeout(t), reject(e)) });
    });
  }
  on(fn) {
    this.listeners.push(fn);
  }
}

const results = { failures: 0 };
export const check = (name, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${ok || !detail ? "" : `  -> ${detail}`}`);
  if (!ok) results.failures++;
};
export const fail = () => void results.failures++;
export function finish() {
  console.log(results.failures ? `\n${results.failures} check(s) FAILED` : "\nall checks passed");
  process.exit(results.failures ? 1 : 0);
}

export async function waitFor(fn, what, ms = 20000) {
  const t = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - t > ms) throw new Error(`timeout waiting for ${what}`);
    await sleep(50);
  }
}

/** Launches the debug desktop app in a private profile and connects DevTools to its WebView. */
export function createHarness({ port, appPath, env = {}, captureStderr = false }) {
  if (process.platform === "linux") return createLinuxHarness({ port, appPath, env, captureStderr, waitFor });
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-"));
  const h = {
    tmp,
    dataDir: path.join(tmp, "data"),
    root: path.join(tmp, "downloads"),
    profile: path.join(tmp, "webview"),
    endpoint: process.platform === "win32" ? String.raw`\\.\pipe\velox-e2e-` + path.basename(tmp) : path.join(tmp, "bridge.sock"),
    app: null,
    cdp: null,
    events: [], // CDP events collected since launch (console, log, exceptions)
    stderr: "", // the app's stderr, when captureStderr is set
  };

  h.launch = async () => {
    h.app = spawn(appPath, [], {
      env: {
        ...process.env,
        VELOX_DATA_DIR: h.dataDir,
        VELOX_DOWNLOAD_ROOT: h.root,
        WEBVIEW2_USER_DATA_FOLDER: h.profile,
        VELOX_E2E_DEBUG_PORT: String(port),
        VELOX_ORGANIZE: "0", // files stay in the download folder itself unless a test asks for sorting
        VELOX_DRY_RUN_SHELL: "1", // "Show in folder" / "Open" only print what they would do: no Explorer windows
        // A private browser link: the app is single-instance by this link, so a test app must never meet a
        // VeloxFetch the user is running (or another test's app).
        VELOX_BRIDGE_ENDPOINT: h.endpoint,
        // The interface follows the computer's language on first start: tests run in English unless they set it.
        LANG: "en_US.UTF-8",
        LC_ALL: "en_US.UTF-8",
        LANGUAGE: "en_US",
        VELOX_E2E_LANG: "en-US",
        ...env,
      },
      stdio: captureStderr ? ["ignore", "ignore", "pipe"] : "ignore",
    });
    h.app.stderr?.on("data", (d) => (h.stderr += d.toString()));
    await h.attach();
  };

  /** Connect to the app's WebView2 debug port (the app may have been started by someone else). */
  h.attach = async () => {
    const target = await waitFor(async () => {
      try {
        // A port that accepts but never answers must not hang the whole run: time each probe out.
        const list = await (await fetch(`http://127.0.0.1:${port}/json`, { signal: AbortSignal.timeout(2000) })).json();
        return list.find((t) => t.type === "page");
      } catch {
        return null;
      }
    }, "WebView2 debug target");
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
    h.cdp = new Cdp(ws);
    h.cdp.on((m) => h.events.push(m));
    await h.cdp.send("Runtime.enable");
    await h.cdp.send("Log.enable");
    await h.cdp.send("Page.enable");
  };

  /** Evaluate an async expression in the page; resolves {ok} or {err} (invoke rejections keep their shape). */
  const evaluate = async (expr) => {
    const r = await h.cdp.send("Runtime.evaluate", {
      expression: `(async () => { try { return { ok: await (${expr}) }; } catch (e) { return { err: (e && e.code) ? e : { message: String(e) } }; } })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    return r.result.value ?? { err: { message: JSON.stringify(r) } };
  };
  h.page = async (expr) => {
    try {
      return await evaluate(expr);
    } catch (e) {
      // WebView2 can crash under a heavy test load; the app rebuilds its window (webview_guard). That
      // is the app recovering, not a test failure: connect to the new page once and ask again.
      if (!h.app || h.app.exitCode !== null || !/went away|no answer/.test(String(e))) throw e;
      console.log(`NOTE  ${String(e.message ?? e)}; reconnecting to the rebuilt window`);
      await sleep(1500);
      await h.attach();
      await waitFor(async () => (await evaluate("!!window.__TAURI_INTERNALS__ && document.readyState === 'complete'").catch(() => ({}))).ok, "the rebuilt window", 30000);
      return await evaluate(expr);
    }
  };
  h.invoke = (cmd, args = {}) => h.page(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`);
  h.exited = () => new Promise((r) => h.app.once("exit", (code) => r(code)));
  /** Close the main window the way a user would (the app must pause, save and exit cleanly). */
  h.closeWindow = async () => {
    await quitOnClose(h);
    execFileSync("powershell", ["-NoProfile", "-Command", `(Get-Process -Id ${h.app.pid}).CloseMainWindow() | Out-Null`]);
  };
  h.screenshot = async (file, size = {}) => {
    try {
      return await shoot(file, size);
    } catch (e) {
      // The same recovery as `page`: the window was rebuilt under the test.
      if (!h.app || h.app.exitCode !== null || !/went away|no answer/.test(String(e))) throw e;
      console.log(`NOTE  ${String(e.message ?? e)}; reconnecting to the rebuilt window`);
      await sleep(1500);
      await h.attach();
      return await shoot(file, size);
    }
  };
  const shoot = async (file, { width, height } = {}) => {
    if (width) await h.cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
    const { data } = await h.cdp.send("Page.captureScreenshot", { format: "png" });
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, Buffer.from(data, "base64"));
    if (width) await h.cdp.send("Emulation.clearDeviceMetricsOverride");
  };
  h.cleanup = async () => {
    try {
      h.app?.kill();
    } catch {}
    await sleep(300);
    fs.rmSync(tmp, { recursive: true, force: true });
  };
  return h;
}
