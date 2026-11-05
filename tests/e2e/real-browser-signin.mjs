// Real-browser test (Linux): a file only the signed-in browser can get — SharePoint / OneDrive / Office Online
// "Download as CSV", a members-only site — still downloads. The site gives the file to the browser (it holds the
// session cookie) and a sign-in page to anyone else. VeloxFetch checks it can fetch the file before taking the
// download over; it cannot, so it declines, and the extension gives the download back to the browser, which
// saves the real file with its own session. Nothing is added to VeloxFetch, nothing fails.
//
//   node tests/e2e/real-browser-signin.mjs [path-to-velox-desktop] [path-to-brave]
//
// Touches nothing of the user's: a throwaway browser profile and HOME in the temp folder (where the browser looks
// for native-host manifests), the test app's own data folder, and a temp download folder. Windows is covered by
// real-browser.mjs (registry-based registration).

import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import { check, createHarness, exe, finish, path, sleep, waitFor } from "./harness.mjs";

if (process.platform !== "linux") {
  console.log("SKIP  Linux only (real-browser.mjs covers Windows)");
  process.exit(0);
}
const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const BROWSER = process.argv[3] ?? ["/usr/bin/brave-browser", "/usr/bin/brave", "/opt/brave.com/brave/brave"].find((p) => fs.existsSync(p));
if (!BROWSER) {
  console.log("SKIP  no Brave found");
  process.exit(0);
}
const EXT_ID = "gbcanjaaelioalfdggkhbbnbinmfgdbc";
const PIPE = path.join(os.tmpdir(), `velox-signin-${process.pid}.sock`);
execFileSync(process.execPath, [path.resolve("apps/browser-extension/build.mjs")], { stdio: "ignore" });
const EXT_DIR = fs.mkdtempSync(path.join(os.tmpdir(), "velox-ext-"));
fs.cpSync(path.resolve("apps/browser-extension/dist/chrome"), EXT_DIR, { recursive: true });

// ---- the site: the CSV for the signed-in browser, a sign-in page for anyone else -------------------------------
const CSV = "id,name,amount\n1,alpha,10\n2,beta,20\n";
const NAME = "Template test case(DATA).csv";
const EXPORT = "/x/_layouts/XlFileHandler.aspx?sheetName=DATA&downloadAsCsvEnabled=1";
const seen = []; // every request for the export: { cookie, ua }
const site = http.createServer((req, res) => {
  if (req.url === "/login") {
    return void res
      .writeHead(200, { "Content-Type": "text/html", "Set-Cookie": "session=signed-in; Path=/; HttpOnly" })
      .end(`<!doctype html><title>Signed in</title><a id="dl" href="${EXPORT}">Download as CSV</a>`);
  }
  if (req.url === EXPORT) {
    seen.push({ method: req.method, cookie: req.headers.cookie ?? null });
    if (/session=signed-in/.test(req.headers.cookie ?? "")) {
      res.writeHead(200, { "Content-Type": "text/csv", "Content-Disposition": `attachment; filename="${NAME}"`, "Content-Length": Buffer.byteLength(CSV) });
      return void res.end(req.method === "HEAD" ? undefined : CSV);
    }
    const page = "<!doctype html><title>Sign in</title><p>Sign in to your account</p>";
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8", "Content-Length": Buffer.byteLength(page) });
    return void res.end(req.method === "HEAD" ? undefined : page);
  }
  if (req.url === "/normal.bin") {
    const body = Buffer.alloc(256 * 1024, 3);
    res.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": body.length, "Accept-Ranges": "bytes" });
    return void res.end(req.method === "HEAD" ? undefined : body);
  }
  res.writeHead(404).end();
});
await new Promise((r) => site.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${site.address().port}`;

const h = createHarness({ port: 9343, appPath: APP, env: { VELOX_BRIDGE_ENDPOINT: PIPE } });
const { invoke, page } = h;
const profile = fs.mkdtempSync(path.join(os.tmpdir(), "velox-brave-"));
const fakeHome = path.join(profile, "home");
const browserDownloads = path.join(profile, "browser-downloads");
fs.mkdirSync(path.join(profile, "Default"), { recursive: true });
fs.mkdirSync(browserDownloads, { recursive: true });
fs.writeFileSync(path.join(profile, "Default", "Preferences"), JSON.stringify({ download: { prompt_for_download: false, default_directory: browserDownloads } }));
let browser = null;

async function cdpConnect(port) {
  const info = await waitFor(async () => {
    try {
      return await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    } catch {
      return null;
    }
  }, "browser debug port", 30000);
  const ws = new WebSocket(info.webSocketDebuggerUrl);
  await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
  let id = 0;
  const pending = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      const p = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result);
    }
  };
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve, reject) => {
      const i = ++id;
      pending.set(i, { resolve, reject });
      ws.send(JSON.stringify({ id: i, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  return { send, close: () => ws.close() };
}

try {
  await h.launch();
  await waitFor(async () => (await page("document.querySelector('header')?.innerText"))?.ok?.includes("VeloxFetch"), "app UI");

  // The browser finds the host through a manifest inside its own (throwaway) profile; a wrapper gives the host the
  // test app's data folder and link (browsers do not pass their environment on to native hosts).
  const HOST_EXE = path.join(path.dirname(APP), exe("velox-native-host"));
  const wrapper = path.join(profile, "host-wrapper.sh");
  fs.writeFileSync(wrapper, `#!/bin/sh\nexport VELOX_DATA_DIR='${h.dataDir}'\nexport VELOX_BRIDGE_ENDPOINT='${PIPE}'\nexec '${HOST_EXE}' "$@"\n`, { mode: 0o755 });
  // The browser runs with a throwaway HOME / XDG_CONFIG_HOME, so its per-user native-messaging folder is inside the
  // temp folder too: the user's real registrations are never read or written.
  const nmDir = path.join(fakeHome, ".config", "BraveSoftware", "Brave-Browser", "NativeMessagingHosts");
  fs.mkdirSync(nmDir, { recursive: true });
  fs.writeFileSync(
    path.join(nmDir, "app.velox.fetch.json"),
    JSON.stringify({ name: "app.velox.fetch", description: "VeloxFetch test host", path: wrapper, type: "stdio", allowed_origins: [`chrome-extension://${EXT_ID}/`] }),
  );

  const dbgPort = 9344;
  browser = spawn(
    BROWSER,
    [
      `--user-data-dir=${profile}`, `--remote-debugging-port=${dbgPort}`, `--load-extension=${EXT_DIR}`,
      "--disable-features=DisableLoadExtensionCommandLineSwitch", "--no-first-run", "--no-default-browser-check", "--headless=new", "about:blank",
    ],
    // Its own process group: the launcher is a shell script, so the browser itself is a child that must be stopped too.
    { stdio: "ignore", detached: true, env: { ...process.env, HOME: fakeHome, XDG_CONFIG_HOME: path.join(fakeHome, ".config") } },
  );
  const cdp = await cdpConnect(dbgPort);
  const findWorker = async () =>
    (await cdp.send("Target.getTargets")).targetInfos.find((t) => t.type === "service_worker" && t.url.startsWith(`chrome-extension://${EXT_ID}/`));
  let worker = await waitFor(findWorker, "the extension's service worker", 20000).catch(() => null);
  if (!worker) {
    await cdp.send("Target.createTarget", { url: `chrome-extension://${EXT_ID}/popup.html` }).catch(() => {});
    worker = await waitFor(findWorker, "the extension's service worker (2)", 8000).catch(() => null);
  }
  if (!worker) throw new Error("the browser did not load the extension");
  const inWorker = async (expr) => {
    let last;
    for (let attempt = 0; attempt < 12; attempt++) {
      try {
        const w = await findWorker();
        if (!w) throw new Error("service worker not running");
        const { sessionId } = await cdp.send("Target.attachToTarget", { targetId: w.targetId, flatten: true });
        const r = await cdp.send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true }, sessionId);
        if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 300));
        return r.result.value;
      } catch (e) {
        last = e;
        if (!/not defined|not running|Cannot find|No target|detached|closed/i.test(String(e.message))) throw e;
        await sleep(500);
      }
    }
    throw last;
  };
  const ping = await inWorker(`nativeCall({ command: "ping" })`);
  check("ping: real browser -> native messaging -> real host -> this test's app", ping.ok === true, JSON.stringify(ping));
  if (ping.ok !== true) throw new Error("the host did not reach the test app; aborting before any download");
  check("defaults: downloads are taken over, cookies are not sent", (await inWorker(`getSettings()`)).intercept === true && (await inWorker(`getSettings()`)).sendCookies === false);

  // ---- sign in, then "Download as CSV" in the browser ------------------------------------------------------------
  const { targetId } = await cdp.send("Target.createTarget", { url: `${base}/login` });
  const ps = (await cdp.send("Target.attachToTarget", { targetId, flatten: true })).sessionId;
  const inPage = async (expr) => (await cdp.send("Runtime.evaluate", { expression: expr, returnByValue: true }, ps)).result.value;
  await waitFor(async () => (await inPage("document.readyState")) === "complete" && (await inPage("!!document.getElementById('dl')")), "the signed-in page");
  await inPage("document.getElementById('dl').click(), true");

  const saved = path.join(browserDownloads, NAME);
  await waitFor(async () => fs.existsSync(saved) && !fs.readdirSync(browserDownloads).some((f) => f.endsWith(".crdownload")), "the browser to save the CSV", 30000);
  check("the browser saved the real file with its own session (not a sign-in page)", fs.readFileSync(saved, "utf8") === CSV, fs.readFileSync(saved, "utf8").slice(0, 80));
  const withoutSession = seen.filter((s) => !s.cookie).length;
  const withSession = seen.filter((s) => s.cookie).length;
  check("VeloxFetch checked the link first (no session: it got the sign-in page) and the browser fetched it signed in", withoutSession >= 1 && withSession >= 2, JSON.stringify(seen));
  const list = (await invoke("list_downloads", { filter: {} })).ok;
  const waiting = (await invoke("list_pending_downloads")).ok;
  check("nothing was added to VeloxFetch: no failed download, nothing waiting for confirmation",
    !list.some((d) => d.filename === NAME || d.url.includes("XlFileHandler")) && !waiting.some((p) => p.filename === NAME), JSON.stringify({ list: list.length, waiting: waiting.length }));

  // ---- a file VeloxFetch can fetch is still taken over -----------------------------------------------------------
  await cdp.send("Target.createTarget", { url: `${base}/normal.bin` });
  await waitFor(async () => (await invoke("list_pending_downloads")).ok.some((p) => p.filename === "normal.bin"), "VeloxFetch to take the normal download", 30000);
  await sleep(1000);
  check("a normal browser download is still taken over by VeloxFetch (the browser's copy dropped)", !fs.existsSync(path.join(browserDownloads, "normal.bin")));
} catch (e) {
  check(`error: ${e.message}`, false);
} finally {
  try {
    if (browser) process.kill(-browser.pid, "SIGTERM"); // the launcher script, the browser and all its processes
  } catch {}
  await h.cleanup?.();
  site.close();
  await sleep(500);
  fs.rmSync(profile, { recursive: true, force: true });
  fs.rmSync(EXT_DIR, { recursive: true, force: true });
  finish();
}
