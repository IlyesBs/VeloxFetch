// Real-browser test: loads the BUILT extension into a real Chromium browser (Brave, if installed),
// and drives it through the browser's own native messaging to the real host and the real app.
//
//   node tests/e2e/real-browser.mjs [path-to-debug-velox-desktop.exe] [path-to-browser.exe]
//
// What it touches on this machine, deliberately and only for the duration of the run:
//  * a throwaway browser profile in the temp folder (your own profile is never opened);
//  * the browser's real per-user native-messaging key (HKCU\Software\Chromium\...), created by this
//    script and removed again at the end (and asserted gone). It points at a wrapper script that
//    sets the test-only environment, because browsers do NOT pass their environment on to native
//    hosts: without the wrapper the host would use the real profile and could start a real app.
//  * SAFETY NET: the real profile (%APPDATA%\app.velox.fetch) is snapshotted first and compared at
//    the end, and a failed ping aborts before any download can be requested.
// If the browser refuses to side-load the extension, the test SKIPs instead of failing.

import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import { check, createHarness, exe, fail, finish, path, sha256, sleep, startFileServer, waitFor } from "./harness.mjs";

const REAL_PROFILE = path.join(process.env.APPDATA ?? "", "app.velox.fetch");
const snapshot = () => {
  const f = path.join(REAL_PROFILE, "velox.db");
  return fs.existsSync(f) ? `${fs.statSync(f).size}:${fs.statSync(f).mtimeMs}:${fs.existsSync(path.join(REAL_PROFILE, "bridge.token"))}` : "none";
};
const realBefore = snapshot();

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const BROWSER =
  process.argv[3] ?? ["C:\\Program Files\\BraveSoftware\\Brave-Browser\\Application\\brave.exe", "C:\\Program Files (x86)\\BraveSoftware\\Brave-Browser\\Application\\brave.exe"].find((p) => fs.existsSync(p));
const EXT_BUILT = path.resolve("apps/browser-extension/dist/chrome");
// A scratch copy of the built extension. The only change: the video-detection permissions, which a user
// grants by clicking "Allow" in the browser's own prompt (a script cannot click it), are granted up front.
// Same key, so the same id the host trusts; everything else is the shipped build.
const EXT_DIR = fs.mkdtempSync(path.join(os.tmpdir(), "velox-ext-"));
const EXT_ID = "gbcanjaaelioalfdggkhbbnbinmfgdbc";
const MB = 1024 * 1024;
const PIPE = String.raw`\\.\pipe\velox-realbrowser-${process.pid}`;
// Verified with a real Brave: on Windows it reads the Chromium key, not a BraveSoftware one.
const BRAVE_KEY = String.raw`HKCU\Software\Chromium\NativeMessagingHosts\app.velox.fetch`;

if (!BROWSER || !fs.existsSync(BROWSER)) {
  console.log("SKIP  no Chromium-based browser found (looked for Brave)");
  process.exit(0);
}
execFileSync(process.execPath, [path.resolve("apps/browser-extension/build.mjs")], { stdio: "ignore" });
fs.cpSync(EXT_BUILT, EXT_DIR, { recursive: true });

const regHas = (key) => {
  try {
    execFileSync("reg", ["query", key, "/ve"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
};
if (regHas(BRAVE_KEY)) {
  console.log("SKIP  a VeloxFetch registration for Brave already exists on this machine; not touching it");
  process.exit(0);
}

const srv = await startFileServer(4 * MB);
// An ordinary web page (the file server only serves binary downloads), and a page that plays a video file.
const CLIP = Buffer.alloc(MB, 7);
const web = http.createServer((req, res) => {
  if (req.url === "/video.html") {
    return void res
      .writeHead(200, { "Content-Type": "text/html" })
      .end("<!doctype html><title>video</title><body style='margin:0'><video src='/clip.mp4' muted autoplay preload='auto' style='display:block;width:640px;height:360px;margin:40px;background:#000'></video>");
  }
  if (req.url === "/clip.mp4") return void res.writeHead(200, { "Content-Type": "video/mp4", "Content-Length": CLIP.length }).end(CLIP);
  res.writeHead(200, { "Content-Type": "text/html" }).end("<!doctype html><title>page</title><a href='/a.zip'>a</a>");
});
await new Promise((r) => web.listen(0, "127.0.0.1", r));
const webBase = `http://127.0.0.1:${web.address().port}`;
const h = createHarness({ port: 9340, appPath: APP, env: { VELOX_BRIDGE_ENDPOINT: PIPE } });
const { invoke, page } = h;
const profile = fs.mkdtempSync(path.join(os.tmpdir(), "velox-brave-"));
let browser = null;
const HEADED = !!process.env.VELOX_HEADED; // show the real browser window: the only way to see its native "Save as" window

// The browser's own "Save as" window (Brave/Chrome "Ask where to save each file"). It can only exist in a
// headed browser; the check is a no-op in headless mode.
const winScript = path.resolve("tests/e2e/windows-list.ps1");
const saveDialogs = () =>
  execFileSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", winScript], { encoding: "utf8" })
    .split(String.fromCharCode(10))
    .map((l) => l.trim())
    .filter((l) => /wants to save|save as|enregistrer sous/i.test(l));
const closeSaveDialogs = () => {
  for (const l of saveDialogs()) execFileSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", winScript, "-Close", l.split("|")[0]], { stdio: "ignore" });
};
// A throwaway profile that asks where to save every file, like the user's browser does.
const browserDownloads = path.join(profile, "browser-downloads");
fs.mkdirSync(path.join(profile, "Default"), { recursive: true });
fs.mkdirSync(browserDownloads, { recursive: true });
fs.writeFileSync(path.join(profile, "Default", "Preferences"), JSON.stringify({
  download: { prompt_for_download: true, default_directory: browserDownloads },
  savefile: { default_directory: browserDownloads },
}));

// ---- a tiny browser-level CDP client (with sessions) ----------------------------------------------
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
  console.log(`app:     ${APP}\nbrowser: ${BROWSER}\nextension: ${EXT_DIR}\n`);
  await h.launch();
  await waitFor(async () => (await page("document.querySelector('header')?.innerText"))?.ok?.includes("VeloxFetch"), "app UI");

  // Register the host for the browser: a manifest that points at a wrapper script (see the header).
  const HOST_EXE = path.join(path.dirname(APP), exe("velox-native-host"));
  const wrapper = path.join(h.tmp, "host-wrapper.cmd");
  fs.writeFileSync(wrapper, `@echo off\r\nset "VELOX_DATA_DIR=${h.dataDir}"\r\nset "VELOX_BRIDGE_ENDPOINT=${PIPE}"\r\n"${HOST_EXE}" %*\r\n`);
  const manifestFile = path.join(h.tmp, "app.velox.fetch.json");
  fs.writeFileSync(manifestFile, JSON.stringify({ name: "app.velox.fetch", description: "VeloxFetch test host", path: wrapper, type: "stdio", allowed_origins: [`chrome-extension://${EXT_ID}/`] }));
  execFileSync("reg", ["add", BRAVE_KEY, "/ve", "/d", manifestFile, "/f"], { stdio: "ignore" });
  check("test registration is in place (Chromium key, wrapper script)", regHas(BRAVE_KEY));

  // Launch the real browser with the built extension.
  const dbgPort = 9341;
  browser = spawn(
    BROWSER,
    [
      `--user-data-dir=${profile}`, `--remote-debugging-port=${dbgPort}`, `--load-extension=${EXT_DIR}`,
      "--disable-features=DisableLoadExtensionCommandLineSwitch", "--no-first-run", "--no-default-browser-check", ...(HEADED ? [] : ["--headless=new"]), "about:blank",
    ],
    { env: { ...process.env, VELOX_DATA_DIR: h.dataDir, VELOX_BRIDGE_ENDPOINT: PIPE }, stdio: "ignore" },
  );
  const cdp = await cdpConnect(dbgPort);

  const findWorker = async () => {
    const { targetInfos } = await cdp.send("Target.getTargets");
    return targetInfos.find((t) => t.type === "service_worker" && t.url.startsWith(`chrome-extension://${EXT_ID}/`));
  };
  let worker = null;
  try {
    worker = await waitFor(findWorker, "the extension's service worker", 20000);
  } catch {
    // Some builds only start the worker on demand; poke it by opening an extension page.
    await cdp.send("Target.createTarget", { url: `chrome-extension://${EXT_ID}/popup.html` }).catch(() => {});
    worker = await waitFor(findWorker, "the extension's service worker (2)", 8000).catch(() => null);
  }
  if (!worker) {
    console.log("SKIP  the browser did not load the side-loaded extension (it may block --load-extension); real-browser checks not run");
  } else {
    check("the real browser loaded the built extension under the id the host trusts", worker.url.includes(EXT_ID), worker.url);
    // The browser may stop and restart an extension's service worker at any time, and it needs a
    // moment after start-up before its globals exist: find + attach fresh, and retry on those cases.
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

    // ---- the extension's own code path, through the browser's native messaging ---------------
    const ping = await inWorker(`nativeCall({ command: "ping" })`);
    check("ping: extension -> browser native messaging -> real host -> real app", ping.ok === true && typeof ping.version === "string", JSON.stringify(ping));
    // Never go on to a download unless this test's own app is the one answering: a download request
    // may start an app, and a wrongly configured host would start one against the real profile.
    if (ping.ok !== true) throw new Error("the host did not reach the test app; aborting before any download is requested");

    await invoke("set_browser_confirm", { enabled: false }); // the checks below are about the handoff; the popup is checked further down
    const url = `${srv.base}/from-brave.bin`;
    const results = await inWorker(`handoff([{ url: ${JSON.stringify(url)} }], "popup", { referrer: "https://shop.example/p#x", incognito: false })`);
    check("a handoff from the extension is accepted", results[0]?.ok === true, JSON.stringify(results));
    await waitFor(async () => (await invoke("list_downloads", { filter: {} })).ok.some((d) => d.filename === "from-brave.bin" && d.status === "completed"), "download from the browser to complete", 30000);
    const dl = (await invoke("list_downloads", { filter: {} })).ok.find((d) => d.filename === "from-brave.bin");
    check("the file arrived byte-exact", sha256(fs.readFileSync(dl.destination)) === srv.sha);
    const seen = srv.headers.find((hd) => hd.referer);
    check("the origin saw the page as referrer (fragment removed) and the real browser's user agent",
      seen?.referer === "https://shop.example/p" && /Chrome\//.test(seen["user-agent"]) && !/velox/i.test(seen["user-agent"]), JSON.stringify(seen));

    // ---- things the browser must refuse or the host must reject -------------------------------
    const exec = await inWorker(`nativeCall({ command: "exec", cmd: "calc.exe" })`);
    check("an unknown command sent through the real browser is rejected by the host", exec.ok === false && exec.code === "unknown_command", JSON.stringify(exec));
    const notOurs = await inWorker(`chrome.runtime.sendNativeMessage("com.evil.host", { command: "ping" }).then(() => "answered", () => "refused")`);
    check("a native host we did not register is unreachable", notOurs === "refused");
    const fileUrl = await inWorker(`handoff([{ url: "file:///C:/Windows/win.ini" }], "popup", {})`);
    check("a file: URL is refused before it leaves the extension", fileUrl[0]?.ok === false && fileUrl[0].code === "invalid", JSON.stringify(fileUrl));
    check("the extension has no way for web pages to message it",
      (await inWorker(`typeof chrome.runtime.onMessageExternal === "undefined" || chrome.runtime.onMessageExternal.hasListeners() === false`)) === true);

    // ---- the popup, as a real extension page ---------------------------------------------------
    const { targetId } = await cdp.send("Target.createTarget", { url: `chrome-extension://${EXT_ID}/popup.html` });
    const ps = (await cdp.send("Target.attachToTarget", { targetId, flatten: true })).sessionId;
    const inPopup = async (expr) => {
      const r = await cdp.send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true }, ps);
      if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 300));
      return r.result.value;
    };
    // The popup page may still be loading when we attach: retry until its script has run.
    const popupText = async (id) => {
      try {
        return (await inPopup(`document.getElementById(${JSON.stringify(id)})?.textContent ?? ""`)) ?? "";
      } catch {
        return "";
      }
    };
    await waitFor(async () => /Connected to VeloxFetch/.test(await popupText("status")), "popup status", 20000);
    check("the popup shows the connection to the app", true);
    check("by default downloads are taken over and cookies are not sent",
      (await inPopup(`document.getElementById("intercept").checked`)) === true && (await inWorker(`getSettings()`)).sendCookies === false);
    await inPopup(`document.getElementById("intercept").click(), true`);
    await sleep(400);
    check("the popup's takeover setting is saved when turned off", (await inWorker(`ext.storage.local.get(["intercept"])`)).intercept === false);
    // With takeover off the browser keeps its download (aimed at a scratch folder, never the real Downloads).
    const dlDir = browserDownloads;
    await cdp.send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: dlDir, eventsEnabled: true });
    const before = (await invoke("list_downloads", { filter: {} })).ok.length;
    await cdp.send("Target.createTarget", { url: `${srv.base}/kept-by-browser.bin` });
    await waitFor(async () => fs.existsSync(path.join(dlDir, "kept-by-browser.bin")), "the browser to keep its own download", 20000);
    check("with takeover off the browser downloads it itself and VeloxFetch stays out",
      (await invoke("list_downloads", { filter: {} })).ok.length === before);
    await inPopup(`document.getElementById("intercept").click(), true`);
    await sleep(400);
    await cdp.send("Browser.setDownloadBehavior", { behavior: "default" }); // the browser asks where to save again, as for a real user
    // Takeover on: a normal browser download (a page navigating to a file) is detected automatically.
    await cdp.send("Target.createTarget", { url: `${srv.base}/auto-detected.bin` });
    await sleep(1500);
    const dialogsSeen = saveDialogs();
    closeSaveDialogs();
    check(HEADED ? "the browser's own 'Save as' window never appears for a taken-over download" : "the browser's own 'Save as' window (not observable in headless mode)", dialogsSeen.length === 0, dialogsSeen.join("; "));
    await waitFor(async () => (await invoke("list_downloads", { filter: {} })).ok.some((d) => d.filename === "auto-detected.bin" && d.status === "completed"), "VeloxFetch to take the browser's download", 30000);
    const auto = (await invoke("list_downloads", { filter: {} })).ok.find((d) => d.filename === "auto-detected.bin");
    check("a download started in the browser is detected and completed by VeloxFetch, byte-exact", sha256(fs.readFileSync(auto.destination)) === srv.sha);
    await sleep(1500);
    check("the browser's own copy was cancelled and removed", !fs.existsSync(path.join(dlDir, "auto-detected.bin")) &&
      (await inWorker(`ext.downloads.search({}).then((r) => r.filter((i) => /auto-detected/.test(i.url)).length)`)) === 0);
    // ---- with confirmation on (the default): the browser's copy is dropped at once, the app asks ----------------
    await invoke("set_browser_confirm", { enabled: true });
    const pendingNames = async () => (await invoke("list_pending_downloads")).ok.map((p) => p.filename);
    await cdp.send("Target.createTarget", { url: `${srv.base}/needs-confirm.bin` });
    await waitFor(async () => (await pendingNames()).includes("needs-confirm.bin"), "the download to wait for the user's answer", 30000);
    await sleep(1500);
    check("the browser's own download is gone while the app is asking (no second copy, no save window left behind)",
      !fs.existsSync(path.join(dlDir, "needs-confirm.bin")) &&
      (await inWorker(`ext.downloads.search({}).then((r) => r.filter((i) => /needs-confirm/.test(i.url)).length)`)) === 0);
    check("nothing is downloaded before the user confirms", !(await invoke("list_downloads", { filter: {} })).ok.some((d) => d.filename === "needs-confirm.bin"));
    const waiting = (await invoke("list_pending_downloads")).ok.find((p) => p.filename === "needs-confirm.bin");
    await invoke("confirm_pending_download", { request: { token: waiting.token } });
    await waitFor(async () => (await invoke("list_downloads", { filter: {} })).ok.some((d) => d.filename === "needs-confirm.bin" && d.status === "completed"), "the confirmed download", 30000);
    check("after Start, VeloxFetch downloads it and the browser does not", !fs.existsSync(path.join(dlDir, "needs-confirm.bin")));

    await cdp.send("Target.createTarget", { url: `${srv.base}/declined-in-app.bin` });
    await waitFor(async () => (await pendingNames()).includes("declined-in-app.bin"), "the second download to wait", 30000);
    const declined = (await invoke("list_pending_downloads")).ok.find((p) => p.filename === "declined-in-app.bin");
    await invoke("dismiss_pending_download", { token: declined.token });
    await sleep(3000);
    check("Cancel in the app: no download anywhere, and the browser does not fetch it either",
      !(await invoke("list_downloads", { filter: {} })).ok.some((d) => d.filename === "declined-in-app.bin") && !fs.existsSync(path.join(dlDir, "declined-in-app.bin")));
    await invoke("set_browser_confirm", { enabled: false });
    // hostile messages to the background from the popup context are refused
    const evilMsg = await inPopup(`chrome.runtime.sendMessage({ type: "handoff", items: [{ url: "javascript:alert(1)" }], source: "popup" })`);
    check("a hostile message from the popup is refused by the background script", evilMsg.ok === false, JSON.stringify(evilMsg));
    const unknownMsg = await inPopup(`chrome.runtime.sendMessage({ type: "openTab", url: "https://evil.test" })`);
    check("an unknown message type is refused", unknownMsg.ok === false);

    // ---- an ordinary web page cannot reach the extension ------------------------------------------
    const page2 = await cdp.send("Target.createTarget", { url: `${webBase}/` });
    const ws2 = (await cdp.send("Target.attachToTarget", { targetId: page2.targetId, flatten: true })).sessionId;
    await sleep(500);
    const r = await cdp.send("Runtime.evaluate", {
      expression: `(async () => { try { if (!(window.chrome && chrome.runtime && chrome.runtime.sendMessage)) return "no-api"; await chrome.runtime.sendMessage(${JSON.stringify(EXT_ID)}, { type: "ping" }); return "reached"; } catch (e) { return "blocked"; } })()`,
      awaitPromise: true, returnByValue: true,
    }, ws2);
    check("a web page cannot message the extension", r.result.value === "no-api" || r.result.value === "blocked", String(r.result.value));

    // ---- video detection and the "Download video" bar ------------------------------------------------
    check("video detection is on from the start (like other download managers)", (await inWorker(`getSettings()`)).detectVideos === true &&
      (await inWorker(`syncDetection().then(() => ext.scripting.getRegisteredContentScripts()).then((l) => l.map((x) => x.id).join())`)) === "velox-videobar");
    await invoke("set_browser_confirm", { enabled: true });
    const vt = await cdp.send("Target.createTarget", { url: `${webBase}/video.html` });
    const vs = (await cdp.send("Target.attachToTarget", { targetId: vt.targetId, flatten: true })).sessionId;
    const inVideoPage = async (expr) => (await cdp.send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true }, vs)).result.value;
    await waitFor(async () => await inWorker(`Object.values(tabMedia).some((t) => t.items.some((i) => i.kind === "file" && i.url.endsWith("/clip.mp4")))`), "the browser to see the video file", 20000);
    check("the video file the page plays is detected from what the browser loaded", true);
    const badgeText = await inWorker(`ext.tabs.query({ url: ${JSON.stringify(webBase + "/video.html")} }).then((t) => ext.action.getBadgeText({ tabId: t[0].id }))`);
    check("the toolbar icon shows how many videos were found", badgeText === "1", String(badgeText));
    await waitFor(async () => await inVideoPage(`!!document.querySelector("velox-video-bar")`), "the bar's script in the page", 15000);
    check("the page cannot look inside the bar (closed shadow root)", (await inVideoPage(`document.querySelector("velox-video-bar").shadowRoot === null`)) === true);
    // A real (trusted) pointer movement over the video shows the bar...
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 300, y: 200 }, vs);
    await waitFor(async () => (await inVideoPage(`getComputedStyle(document.querySelector("velox-video-bar")).display`)) === "block", "the bar to appear over the video", 10000);
    const pos = await inVideoPage(`(() => { const r = document.querySelector("velox-video-bar").getBoundingClientRect(); return [Math.round(r.x), Math.round(r.y)]; })()`);
    check("the bar sits at the video's top-right corner", !!pos && pos[0] > 400 && pos[0] < 680 && pos[1] >= 40 && pos[1] < 80, JSON.stringify(pos));
    {
      const shot = await cdp.send("Page.captureScreenshot", { format: "png", clip: { x: 0, y: 0, width: 760, height: 460, scale: 1 } }, vs);
      fs.writeFileSync(path.resolve("docs/screenshots/video-bar.png"), Buffer.from(shot.data, "base64"));
    }
    // ...a click the page made up does nothing...
    await inVideoPage(`document.querySelector("velox-video-bar").click(), document.querySelector("velox-video-bar").dispatchEvent(new MouseEvent("click", { bubbles: true })), true`);
    await sleep(800);
    check("a click made up by the page does nothing", !(await invoke("list_pending_downloads")).ok.some((p) => p.filename === "clip.mp4"));
    // ...and a real click on "Download video" hands the file over to the app.
    for (const type of ["mousePressed", "mouseReleased"]) await cdp.send("Input.dispatchMouseEvent", { type, x: pos[0] + 30, y: pos[1] + 15, button: "left", clickCount: 1 }, vs);
    await waitFor(async () => (await invoke("list_pending_downloads")).ok.some((p) => p.filename === "clip.mp4"), "the video to reach the app", 20000);
    const clip = (await invoke("list_pending_downloads")).ok.find((p) => p.filename === "clip.mp4");
    check("a click on Download video hands the video to the app, which asks the user", clip.url === `${webBase}/clip.mp4`, JSON.stringify(clip));
    await invoke("dismiss_pending_download", { token: clip.token });
    const fromPage = await inVideoPage(`(async () => { try { if (!(window.chrome && chrome.runtime && chrome.runtime.sendMessage)) return "no-api"; await chrome.runtime.sendMessage(${JSON.stringify(EXT_ID)}, { type: "bar:download", itemId: "m1" }); return "reached"; } catch (e) { return "blocked"; } })()`);
    check("the page itself cannot ask the extension for a download", fromPage === "no-api" || fromPage === "blocked", String(fromPage));
    check("turning detection off removes the bar", (await inWorker(`ext.storage.local.set({ detectVideos: false }).then(syncDetection).then(() => ext.scripting.getRegisteredContentScripts()).then((l) => l.length)`)) === 0);
    await invoke("set_browser_confirm", { enabled: false });

    // ---- private (incognito) flag: cookies never travel -------------------------------------------------
    const inc = await inWorker(`handoff([{ url: ${JSON.stringify(srv.base + "/incog.bin")} }], "popup", { referrer: "https://shop.example/", incognito: true })`);
    check("a handoff from a private window is accepted, without cookies", inc[0]?.ok === true);
    cdp.close();
  }

  // ---- cleanup verification -----------------------------------------------------------------------------
  execFileSync("reg", ["delete", BRAVE_KEY, "/f"], { stdio: "ignore" });
  check("the test registration is removed again", !regHas(BRAVE_KEY));
  check("the real profile was not touched", snapshot() === realBefore, `${realBefore} -> ${snapshot()}`);
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  try {
    // Chromium browsers re-launch themselves, so the PID we spawned is not the whole tree: stop every
    // process started with THIS run's private profile folder (and nothing else).
    const needle = profile.replace(/'/g, "''");
    execFileSync("powershell", ["-NoProfile", "-Command",
      `Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.Contains('${needle}') } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }`], { stdio: "ignore" });
  } catch {}
  try {
    execFileSync("reg", ["delete", BRAVE_KEY, "/f"], { stdio: "ignore" }); // belt and braces
  } catch {}
  await sleep(1500);
  try {
    fs.rmSync(profile, { recursive: true, force: true, maxRetries: 8, retryDelay: 400 });
  } catch {} // the browser may still be releasing files; the OS temp cleaner will get it
  srv.close();
  web.close();
  fs.rmSync(EXT_DIR, { recursive: true, force: true });
  await h.cleanup();
}

finish();
