// End-to-end test of Phase 7: a REAL velox-native-host.exe process, speaking browser native-messaging
// framing on stdin/stdout, talking to the REAL desktop app over its local authenticated pipe.
// (No browser is involved: this is exactly what a browser does, minus the browser.)
//
//   node tests/e2e/desktop-browser.mjs [path-to-debug-velox-desktop.exe]
//
// Safety: browser registration is redirected to a scratch registry key (debug builds only), so the
// real Chrome / Edge / Firefox keys are never touched; the test also asserts that.

import { execFileSync, spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, fail, finish, path, sha256, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const HOST = path.join(path.dirname(APP), exe("velox-native-host"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;
const CHROME_ORIGIN = "chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/";
const EVIL_ORIGIN = "chrome-extension://abcdefghijklmnopabcdefghijklmnop/";
const PIPE = String.raw`\\.\pipe\velox-e2e-${process.pid}`;
const SCRATCH = String.raw`Software\VeloxFetchE2E-${process.pid}`;
const REAL_KEYS = [
  String.raw`HKCU\Software\Google\Chrome\NativeMessagingHosts\app.velox.fetch`,
  String.raw`HKCU\Software\Microsoft\Edge\NativeMessagingHosts\app.velox.fetch`,
  String.raw`HKCU\Software\Chromium\NativeMessagingHosts\app.velox.fetch`,
  String.raw`HKCU\Software\Mozilla\NativeMessagingHosts\app.velox.fetch`,
];
const startedAt = Date.now();
// The extension the "Install extension" button copies: a scratch copy of the built dist (chromium + firefox).
const EXT_SRC = fs.mkdtempSync(path.join(os.tmpdir(), "velox-extsrc-"));
fs.cpSync(path.resolve("apps/browser-extension/dist/chrome"), path.join(EXT_SRC, "chromium"), { recursive: true });
fs.cpSync(path.resolve("apps/browser-extension/dist/firefox"), path.join(EXT_SRC, "firefox"), { recursive: true });

if (!fs.existsSync(HOST)) {
  console.error(`missing ${HOST}: build it first (cargo build -p velox-native-host)`);
  process.exit(1);
}

const srv = await startFileServer(6 * MB);
const h = createHarness({
  port: 9338,
  appPath: APP,
  env: { VELOX_BRIDGE_ENDPOINT: PIPE, VELOX_NMH_SCRATCH_KEY: SCRATCH, VELOX_EXTENSION_SRC: EXT_SRC, VELOX_NO_LAUNCH: "1" },
});
const { page, invoke } = h;

// ---- driving the host like a browser does ---------------------------------------------------------
const frame = (v) => {
  const body = Buffer.isBuffer(v) ? v : Buffer.from(JSON.stringify(v));
  const head = Buffer.alloc(4);
  head.writeUInt32LE(body.length);
  return Buffer.concat([head, body]);
};

function startHost(args = [CHROME_ORIGIN], env = {}) {
  const child = spawn(HOST, args, {
    env: { ...process.env, VELOX_DATA_DIR: h.dataDir, VELOX_BRIDGE_ENDPOINT: PIPE, ...env },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const host = { child, buf: Buffer.alloc(0), stderr: "", waiters: [], code: undefined, stdoutBytes: 0 };
  host.exited = new Promise((r) => child.once("exit", (c) => ((host.code = c), r(c))));
  child.stderr.on("data", (d) => (host.stderr += d.toString()));
  child.stdout.on("data", (d) => {
    host.stdoutBytes += d.length;
    host.buf = Buffer.concat([host.buf, d]);
    for (;;) {
      if (host.buf.length < 4) return;
      const len = host.buf.readUInt32LE(0);
      if (host.buf.length < 4 + len) return;
      const body = host.buf.subarray(4, 4 + len);
      host.buf = host.buf.subarray(4 + len);
      host.waiters.shift()?.(JSON.parse(body.toString()));
    }
  });
  host.send = (v) => child.stdin.write(frame(v));
  host.ask = (v, ms = 15000) =>
    new Promise((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("host did not answer: " + JSON.stringify(v).slice(0, 80))), ms);
      host.waiters.push((r) => (clearTimeout(t), resolve(r)));
      host.send(v);
    });
  host.close = () => child.stdin.end();
  return host;
}

const text = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`)).ok;
const click = (sel) => page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
const list = async () => (await invoke("list_downloads", { filter: {} })).ok;
const scratchKey = (browser) => "HKCU\\" + SCRATCH + "\\" + browser + "\\app.velox.fetch";
const storedHeaders = (id) => {
  const db = new DatabaseSync(path.join(h.dataDir, "velox.db"), { readOnly: true });
  try {
    return db.prepare("SELECT request_headers FROM downloads WHERE id = ?").get(id)?.request_headers ?? null;
  } finally {
    db.close();
  }
};
const regQuery = (key) => {
  try {
    return execFileSync("reg", ["query", key, "/ve"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
  } catch {
    return null;
  }
};

// (Re)install the CSP / error watchers; needed again after the page is replaced by an app restart.
const watchPage = () =>
  page(`(() => { window.__csp = []; window.__errs = [];
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective));
    window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

const dl = (name, extra = {}) => ({ command: "download", url: `${srv.base}/${name}`, source: "contextMenu", ...extra });

try {
  console.log(`app:  ${APP}\nhost: ${HOST}\npipe: ${PIPE}\n`);
  // The machine may legitimately have real registrations (the user connected a browser): the test only
  // proves it leaves them exactly as they were.
  const realBefore = REAL_KEYS.map((k) => regQuery(k));

  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await watchPage();
  check("confirmation before browser downloads is ON by default", (await invoke("get_browser_integration")).ok.confirmDownloads === true);
  check("the app reports that cookies are sealed on disk (secure storage)", (await invoke("get_browser_integration")).ok.secretsProtected === true);
  await invoke("set_browser_confirm", { enabled: false }); // the sections below test the handoff itself

  // ---- 1. ping + a real handoff --------------------------------------------------------------------
  const host = startHost();
  const pong = await host.ask({ command: "ping" });
  check("ping through the real host reaches the app", pong.ok === true && pong.result?.app === "VeloxFetch" && typeof pong.result.version === "string", JSON.stringify(pong));

  const before = (await list()).length;
  const r1 = await host.ask(dl("report.bin", {
    filename: "report.bin",
    referrer: "https://shop.example/downloads#top",
    cookies: "session=s3cr3t-e2e; theme=dark",
    userAgent: "Mozilla/5.0 E2EBrowser/1",
  }));
  check("a download handed over by the browser is accepted", r1.ok === true && typeof r1.result?.id === "number", JSON.stringify(r1));
  const id = r1.result.id;
  await waitFor(async () => (await invoke("get_download", { id })).ok?.status === "completed", "handed-over download to complete", 30000);
  const done = (await invoke("get_download", { id })).ok;
  check("the file arrived byte-exact", sha256(fs.readFileSync(done.destination)) === srv.sha);
  const mine = srv.headers.filter((hd) => hd.cookie);
  check("the server saw the browser's cookie, referrer and user agent on its requests",
    mine.length >= 2 && mine.every((hd) => hd.cookie === "session=s3cr3t-e2e; theme=dark" && hd.referer === "https://shop.example/downloads" && hd["user-agent"] === "Mozilla/5.0 E2EBrowser/1"),
    JSON.stringify(mine[0]));
  check("the cookie was wiped from the database record when the download completed", storedHeaders(id) === null, String(storedHeaders(id)));
  await waitFor(async () => (await text("main"))?.includes("report.bin"), "the download to show in the UI");
  check("it shows up in the UI", true);
  check("the list grew by exactly one", (await list()).length === before + 1);

  // ---- 2. plain URL without any hints ----------------------------------------------------------------
  const r2 = await host.ask(dl("plain.bin"));
  check("a bare URL works too (no filename, no headers)", r2.ok === true, JSON.stringify(r2));
  await waitFor(async () => (await invoke("get_download", { id: r2.result.id })).ok?.status === "completed", "plain download", 30000);
  const plainHeaders = srv.headers.filter((hd) => !hd.cookie).slice(-1)[0];
  check("and then sends no browser headers", plainHeaders && !plainHeaders.referer);

  // ---- 3. hostile input through the real host -----------------------------------------------------------
  const count = (await list()).length;
  const bad = [
    [{ command: "exec", cmd: "calc.exe" }, "unknown_command"],
    [{ command: "readFile", path: "C:\\Windows\\win.ini" }, "unknown_command"],
    [{ command: "ping", x: 1 }, "bad_fields"],
    [dl("x.bin", { url: "file:///C:/Windows/win.ini" }), "invalid"],
    [dl("x.bin", { url: "javascript:alert(1)" }), "invalid"],
    [dl("x.bin", { url: "https://user:pw@example.com/x" }), "invalid"],
    [dl("x.bin", { destination: "C:\\Windows\\System32" }), "bad_fields"],
    [dl("x.bin", { subdir: "..\\..\\evil" }), "bad_fields"],
    [dl("x.bin", { filename: "..\\..\\evil.exe" }), "invalid"],
    [dl("x.bin", { cookies: "a=b\r\nX-Evil: 1" }), "invalid"],
    [dl("x.bin", { userAgent: "ua\nX-Evil: 1" }), "invalid"],
    [dl("x.bin", { referrer: "javascript:1" }), "invalid"],
    [dl("x.bin", { incognito: true, cookies: "sid=1" }), "invalid"],
    [["download"], "bad_shape"],
  ];
  for (const [msg, code] of bad) {
    const r = await host.ask(msg);
    check(`rejected: ${JSON.stringify(msg).slice(0, 70)}`, r.ok === false && r.error?.code === code, JSON.stringify(r));
  }
  host.send(Buffer.from("{not json"));
  const rj = await new Promise((res) => host.waiters.push(res));
  check("rejected: malformed JSON", rj.error?.code === "bad_json");
  check("none of it created a download", (await list()).length === count);
  check("the host never wrote anything but frames to stdout", host.buf.length === 0);
  check("still healthy afterwards", (await host.ask({ command: "ping" })).ok === true);

  // an oversized message: the host answers and ends the session without reading the body
  const big = startHost();
  big.child.stdin.write(Buffer.from([0x00, 0x00, 0x00, 0x40])); // header: 1 GiB
  const rbig = await new Promise((res) => big.waiters.push(res));
  check("an oversized message is refused with `too_large`", rbig.error?.code === "too_large", JSON.stringify(rbig));
  check("and the host ends the session (exit 0)", (await Promise.race([big.exited, sleep(5000).then(() => "timeout")])) === 0);

  // ---- 4. who may talk to the host ---------------------------------------------------------------------------
  const evil = startHost([EVIL_ORIGIN]);
  check("a well-formed but unknown extension is refused (exit 2)", (await Promise.race([evil.exited, sleep(5000).then(() => "timeout")])) === 2 && evil.stdoutBytes === 0, `stderr: ${evil.stderr}`);
  const byHand = startHost([]);
  check("started by hand (no browser arguments): refused", (await Promise.race([byHand.exited, sleep(5000).then(() => "timeout")])) === 2 && /browser/.test(byHand.stderr), byHand.stderr);
  const weird = startHost(["--parent-window=123"]);
  check("only a --parent-window argument: refused", (await Promise.race([weird.exited, sleep(5000).then(() => "timeout")])) === 2);
  const ff = startHost([path.join(os.tmpdir(), "velox-firefox.json"), "velox-fetch@velox.app"]);
  check("Firefox-style arguments with our add-on id are accepted", (await ff.ask({ command: "ping" })).ok === true);
  ff.close();
  const ffEvil = startHost([path.join(os.tmpdir(), "x.json"), "evil@evil.test"]);
  check("Firefox-style arguments with another add-on id are refused", (await Promise.race([ffEvil.exited, sleep(5000).then(() => "timeout")])) === 2);

  // a host that does not know the token cannot get in, and the app keeps serving
  const other = fs.mkdtempSync(path.join(os.tmpdir(), "velox-e2e-tok-"));
  fs.writeFileSync(path.join(other, "bridge.token"), "ab".repeat(32));
  const wrong = startHost([CHROME_ORIGIN], { VELOX_DATA_DIR: other });
  const rw = await wrong.ask({ command: "ping" });
  check("a host with the wrong token is refused by the app", rw.ok === false && rw.error?.code === "auth_failed", JSON.stringify(rw));
  wrong.close();
  fs.rmSync(other, { recursive: true, force: true });

  // hostile raw client on the pipe (not our host): garbage and a huge length must not hurt the app
  const net = await import("node:net");
  await new Promise((resolve) => {
    const s = net.connect(PIPE, () => {
      s.write(Buffer.from([0xff, 0xff, 0xff, 0x7f, 1, 2, 3]));
      setTimeout(() => (s.destroy(), resolve()), 300);
    });
    s.on("error", resolve);
  });
  check("garbage sent straight to the pipe does not disturb the app", (await host.ask({ command: "ping" })).ok === true);

  // ---- 5. rate limiting at the app -------------------------------------------------------------------------------
  const flood = startHost();
  let limited = 0;
  for (let i = 0; i < 40; i++) if ((await flood.ask({ command: "ping" })).error?.code === "rate_limited") limited++;
  check("a flood of requests on one connection is rate limited", limited >= 5, `limited: ${limited}`);
  flood.close();

  // ---- 6. the app is started on demand ------------------------------------------------------------------------------
  host.close();
  const closing = h.exited();
  await h.closeWindow();
  check("the app closes cleanly", (await Promise.race([closing, sleep(20000).then(() => "timeout")])) === 0);
  h.app = null;
  await sleep(500);

  const cold = startHost([CHROME_ORIGIN], { VELOX_E2E_DEBUG_PORT: "9338", VELOX_DOWNLOAD_ROOT: h.root, WEBVIEW2_USER_DATA_FOLDER: h.profile, VELOX_NMH_SCRATCH_KEY: SCRATCH, VELOX_EXTENSION_SRC: EXT_SRC, VELOX_NO_LAUNCH: "1" });
  const cp = await cold.ask({ command: "ping" });
  check("a ping never starts the app", cp.ok === false && cp.error?.code === "app_not_running", JSON.stringify(cp));
  const launched = await cold.ask(dl("cold.bin"), 60000);
  check("a download request starts the app and is then accepted", launched.ok === true, JSON.stringify(launched));
  await h.attach();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI after the cold start");
  await watchPage();
  await waitFor(async () => (await invoke("get_download", { id: launched.result.id })).ok?.status === "completed", "cold-start download", 30000);
  check("and it completes", true);
  cold.close();

  // ---- 7. registering the host with browsers (from the UI; scratch registry only) ---------------------------------------
  await click("[data-testid=nav-settings]");
  await waitFor(async () => (await text("[data-testid=browser-list]")) !== null, "browser panel");
  check("browser panel lists Chrome, Edge, Brave and Firefox, none connected yet",
    (await Promise.all(["chrome", "edge", "brave", "firefox"].map((b) => text(`[data-testid=browser-${b}-status]`)))).every((t) => t === "Not connected"));
  await click("[data-testid=browser-chrome-toggle]");
  await waitFor(async () => (await text("[data-testid=browser-chrome-status]")) === "Connected", "Chrome to show as connected");
  const key = regQuery(scratchKey("chrome"));
  check("Connect wrote the registration (scratch key)", !!key && key.includes("app.velox.fetch.chrome.json"), String(key));
  const manifestPath = /REG_SZ\s+(.+)\s*$/m.exec(key ?? "")?.[1]?.trim();
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  check("the manifest allows exactly our extension and points at the host next to the app",
    JSON.stringify(manifest.allowed_origins) === JSON.stringify([CHROME_ORIGIN]) && manifest.type === "stdio" && manifest.name === "app.velox.fetch" && path.resolve(manifest.path).toLowerCase() === HOST.toLowerCase(), JSON.stringify(manifest));
  await h.page("document.documentElement.setAttribute('data-theme','dark')");
  await sleep(350);
  await h.screenshot(path.join(SHOTS, "settings-browser-dark.png"), { width: 1100, height: 1500 });
  await click("[data-testid=browser-chrome-toggle]");
  await waitFor(async () => (await text("[data-testid=browser-chrome-status]")) === "Not connected", "Chrome to disconnect");
  check("Disconnect removed the registration and the manifest", regQuery(scratchKey("chrome")) === null && !fs.existsSync(manifestPath));
  const badBrowser = await invoke("set_browser_integration", { request: { browser: "netscape", enabled: true } });
  check("an unknown browser name is rejected at the IPC boundary", !!badBrowser.err);
  const extraField = await invoke("set_browser_integration", { request: { browser: "chrome", enabled: true, path: "C:\\evil.exe" } });
  check("an extra field (a path) is rejected at the IPC boundary", !!extraField.err);
  check("the rejected requests registered nothing", regQuery(scratchKey("chrome")) === null);

  // ---- 9. the confirmation popup -----------------------------------------------------------------------------------
  await click("[data-testid=browser-confirm-toggle]");
  await waitFor(async () => (await page("document.querySelector('[data-testid=browser-confirm-toggle]').checked")).ok === true, "confirmation switched on");
  check("the Settings switch turns the confirmation on", (await invoke("get_browser_integration")).ok.confirmDownloads === true);
  const dlg = (sel) => text(`[data-testid=${sel}]`);
  const setName = (v) => page(`(() => { const el = document.querySelector('[data-testid=pending-name]'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, ${JSON.stringify(v)}); el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
  const asker = startHost();
  const sizeBefore = (await list()).length;

  const first = await asker.ask(dl("dialog.bin"));
  check("with confirmation on, the app answers 'received' (id 0) and starts nothing", first.ok === true && first.result?.id === 0, JSON.stringify(first));
  await waitFor(async () => (await dlg("pending-host")) !== null, "the confirmation popup");
  await waitFor(async () => /MB/.test((await dlg("pending-size")) ?? ""), "size to be filled in by the server");
  check("the popup shows the file name, the server, the size, the type and the folder",
    (await page("document.querySelector('[data-testid=pending-name]').value")).ok === "dialog.bin" && (await dlg("pending-host")) === "127.0.0.1" &&
    /MB/.test((await dlg("pending-size")) ?? "") && /octet-stream/.test((await dlg("pending-type")) ?? "") && (await dlg("pending-dir"))?.includes("downloads"));
  check("nothing has been downloaded before the user answers", (await list()).length === sizeBefore);
  await h.screenshot(path.join(SHOTS, "confirm-download-dark.png"), { width: 1000, height: 760 });
  await click("[data-testid=pending-start]");
  await waitFor(async () => (await list()).some((d) => d.filename === "dialog.bin" && d.status === "completed"), "Start to download the file", 30000);
  check("Start downloads it", true);
  check("the popup closes after the answer", (await dlg("pending-dialog")) === null);

  const second = await asker.ask(dl("declined.bin"));
  await waitFor(async () => (await dlg("pending-host")) !== null, "popup for the second file");
  await click("[data-testid=pending-cancel]");
  await waitFor(async () => (await dlg("pending-dialog")) === null, "popup to close on Cancel");
  await sleep(600);
  check("Cancel downloads nothing", second.result?.id === 0 && !(await list()).some((d) => d.filename === "declined.bin"));

  await asker.ask(dl("orig.bin"));
  await waitFor(async () => (await dlg("pending-host")) !== null, "popup for the renamed file");
  await setName("renamed.bin");
  await click("[data-testid=pending-start]");
  await waitFor(async () => (await list()).some((d) => d.filename === "renamed.bin" && d.status === "completed"), "the renamed download", 30000);
  check("the user can rename the file in the popup", !(await list()).some((d) => d.filename === "orig.bin"));

  await asker.ask(dl("later.bin"));
  await waitFor(async () => (await dlg("pending-host")) !== null, "popup for Download later");
  await click("[data-testid=pending-later]");
  await waitFor(async () => (await list()).some((d) => d.filename === "later.bin"), "the file to be added");
  await sleep(600);
  check("Download later adds it to the list without starting it", (await list()).find((d) => d.filename === "later.bin")?.status !== "downloading" && (await list()).find((d) => d.filename === "later.bin")?.status !== "completed");

  await asker.ask(dl("q1.bin"));
  await asker.ask(dl("q2.bin"));
  await waitFor(async () => /2 downloads are waiting/.test((await dlg("pending-count")) ?? ""), "the waiting counter");
  check("several waiting downloads are answered one after the other", true);
  await click("[data-testid=pending-cancel]");
  await waitFor(async () => /Do you want/.test((await dlg("pending-count")) ?? ""), "the second one to show");
  await click("[data-testid=pending-cancel]");
  await waitFor(async () => (await dlg("pending-dialog")) === null, "queue of popups to empty");
  asker.close();
  const badId = await invoke("confirm_pending_download", { request: { token: 999999 } });
  check("answering a request that does not exist is refused", !!badId.err);
  const evilAnswer = await invoke("confirm_pending_download", { request: { token: 1, filename: "x", start: true, path: "C:\\evil" } });
  check("an extra field in the answer is refused at the IPC boundary", !!evilAnswer.err);
  await click("[data-testid=browser-confirm-toggle]");
  await waitFor(async () => (await page("document.querySelector('[data-testid=browser-confirm-toggle]').checked")).ok === false, "confirmation switched off");

  // ---- 8. "Install extension" button -------------------------------------------------------------------------------
  check("each browser row offers Install extension (nothing is connected yet)", (await text("[data-testid=browser-brave-toggle]")) === "Install extension" && (await page("document.querySelectorAll('[data-testid=extension-steps]').length")).ok === 0);
  await click("[data-testid=browser-brave-toggle]");
  await waitFor(async () => (await text("[data-testid=extension-steps]")) !== null, "installation steps to appear");
  const folder = (await text("[data-testid=extension-folder]"))?.trim() ?? "";
  const inData = path.resolve(folder).toLowerCase().startsWith(path.join(h.dataDir, "browser-extension").toLowerCase());
  check("the button copied the extension into the app's own data folder", inData && fs.existsSync(path.join(folder, "manifest.json")), folder);
  const m1 = JSON.parse(fs.readFileSync(path.join(folder, "manifest.json"), "utf8"));
  check("the copy is complete (background script, popup, icons) and pins the trusted extension id",
    ["background.js", "popup.html", "popup.js", "lib/validate.js", "icons/128.png"].every((f) => fs.existsSync(path.join(folder, f))) && !!m1.key);
  check("the steps explain what to do in the browser", /Developer mode|Temporary Add-on/.test((await text("[data-testid=extension-steps]")) ?? ""));
  const statuses = await Promise.all(["chrome", "edge", "brave", "firefox"].map((b) => text(`[data-testid=browser-${b}-status]`)));
  check("only the browser whose button was pressed got connected (Brave, not the default one)", statuses.join() === "Not connected,Not connected,Connected,Not connected", statuses.join());
  check("a connected row offers Disconnect and Show steps", (await text("[data-testid=browser-brave-toggle]")) === "Disconnect" && (await page("!!document.querySelector('[data-testid=browser-brave-steps]')")).ok === true);
  const ffInstall = await invoke("install_browser_extension", { request: { browser: "firefox" } });
  check("Firefox gets its own build (temporary add-on manifest)", !!ffInstall.ok && ffInstall.ok.folder.endsWith("firefox") && !!JSON.parse(fs.readFileSync(path.join(ffInstall.ok.folder, "manifest.json"), "utf8")).browser_specific_settings, JSON.stringify(ffInstall));
  check("no program was started by the test run", ffInstall.ok?.openedBrowser === false && ffInstall.ok?.openedFolder === false);
  const badExt = await invoke("install_browser_extension", { request: { browser: "netscape" } });
  check("an unknown browser is rejected", !!badExt.err);
  const pathExt = await invoke("install_browser_extension", { request: { browser: "chrome", folder: "C:\evil" } });
  check("an extra field (a path or URL) is rejected", !!pathExt.err);

  REAL_KEYS.forEach((k, i) => check(`after: real browser key untouched (${k.split("\\")[2]})`, regQuery(k) === realBefore[i]));
  const csp = (await page("window.__csp")).ok, errs = (await page("window.__errs")).ok;
  check("no CSP violations or script errors", csp.length === 0 && errs.length === 0, JSON.stringify({ csp, errs }));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  try { fs.rmSync(EXT_SRC, { recursive: true, force: true }); } catch {}
  // scratch registry + any app the host started (only processes newer than this test)
  try { execFileSync("reg", ["delete", `HKCU\\${SCRATCH}`, "/f"], { stdio: "ignore" }); } catch {}
  try {
    execFileSync("powershell", ["-NoProfile", "-Command",
      `Get-Process velox-desktop -ErrorAction SilentlyContinue | Where-Object { $_.StartTime -gt [DateTime]::Now.AddMilliseconds(-${Date.now() - startedAt + 2000}) } | Stop-Process -Force`], { stdio: "ignore" });
  } catch {}
  srv.close();
  await h.cleanup();
}

finish();
