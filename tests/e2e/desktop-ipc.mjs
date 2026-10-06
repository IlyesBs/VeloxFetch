// End-to-end test of the real desktop app: launches the Tauri binary, drives its WebView over the
// Chrome DevTools Protocol, and checks the full path UI -> IPC -> capability check -> manager ->
// engine -> disk, plus the security boundaries (unknown commands, hostile input, CSP).
//
//   node tests/e2e/desktop-ipc.mjs [path-to-velox-desktop.exe] [path-to-velox.exe]
//
// Needs a *debug* build of the app made with the bundled frontend:
//   npm --prefix apps/desktop run build
//   cargo build -p velox-desktop -p velox-cli --features velox-desktop/custom-protocol
// (VELOX_DATA_DIR / VELOX_DOWNLOAD_ROOT / VELOX_E2E_DEBUG_PORT are honoured in debug builds only.)
// Needs the WebView2 runtime (Windows). Uses a private WebView2 profile and a local test server.

import { execFileSync } from "node:child_process";
import { check, createHarness, exe, fail, finish, fs, path, sha256 as sha, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const CLI = path.resolve(process.argv[3] ?? exe("target/debug/velox"));

const srv = await startFileServer(12 * 1024 * 1024);
const h = createHarness({ port: 9333, appPath: APP });
const { page, invoke } = h;
const base = srv.base;
const FILE = srv.FILE;
const FILE_SHA = srv.sha;
const root = h.root;
const dataDir = h.dataDir;
const tmp = h.tmp;
const crypto = { createHash: () => ({ update: (b) => ({ digest: () => sha(b) }) }) };

async function launch() {
  await h.launch();
  await waitFor(async () => (await page("document.querySelector('header')?.textContent")).ok?.includes("VeloxFetch"), "UI to render");
}

try {
  console.log(`app: ${APP}\nserver: ${base}\n`);
  await launch();

  // ---- UI renders and the IPC round trip works ---------------------------------------------
  check("UI rendered by the bundled frontend", (await page("document.title")).ok === "VeloxFetch");
  const empty = await invoke("list_downloads", { filter: {} });
  check("list_downloads works over IPC (empty at first)", Array.isArray(empty.ok) && empty.ok.length === 0, JSON.stringify(empty));

  // ---- a full download through the real stack ---------------------------------------------
  const created = await invoke("create_download", { request: { url: `${base}/big.bin` } });
  check("create_download returns a DTO", created.ok?.id > 0 && created.ok.filename === "big.bin", JSON.stringify(created));
  const id = created.ok?.id;
  const done = await waitFor(async () => {
    const d = (await invoke("get_download", { id })).ok;
    return d?.status === "completed" ? d : null;
  }, "download to complete");
  check("download completed with the right size", done.downloaded === FILE.length && done.totalSize === FILE.length);
  const onDisk = path.join(root, "big.bin");
  check(
    "file on disk is byte-exact (sha256)",
    fs.existsSync(onDisk) && crypto.createHash("sha256").update(fs.readFileSync(onDisk)).digest("hex") === FILE_SHA,
  );
  const shown = await waitFor(
    async () => (await page("document.querySelector('[data-testid^=download-]')?.textContent")).ok?.includes("big.bin"),
    "UI to show the download",
  );
  check("UI lists the download (events / list reached the page)", !!shown);

  // ---- pause / resume / cancel / retry / delete over IPC ---------------------------------
  srv.delayMs = 12;
  const slow = (await invoke("create_download", { request: { url: `${base}/slow.bin` } })).ok;
  await waitFor(async () => ((await invoke("get_download", { id: slow.id })).ok?.downloaded ?? 0) > 1_500_000, "bytes flowing");
  const uiProgress = await waitFor(
    async () => {
      const v = (await page("Number(document.querySelector('[data-testid^=download-][data-status=downloading] [role=progressbar]')?.getAttribute('aria-valuenow'))")).ok;
      return v > 0 ? v : null;
    },
    "UI progress bar to move",
  ).catch(() => 0);
  check("UI progress bar receives live progress updates", uiProgress > 0, String(uiProgress));
  const paused = (await invoke("pause_download", { id: slow.id })).ok;
  check("pause_download pauses and keeps the partial file", paused?.status === "paused" && fs.existsSync(path.join(root, "slow.bin.part")));
  srv.delayMs = 0;
  await invoke("resume_download", { id: slow.id });
  const slowDone = await waitFor(async () => {
    const d = (await invoke("get_download", { id: slow.id })).ok;
    return d?.status === "completed" ? d : null;
  }, "resumed download to complete");
  check(
    "resumed download is byte-exact",
    crypto.createHash("sha256").update(fs.readFileSync(path.join(root, "slow.bin"))).digest("hex") === FILE_SHA && slowDone.downloaded === FILE.length,
  );

  srv.delayMs = 12;
  const c = (await invoke("create_download", { request: { url: `${base}/cancel.bin` } })).ok;
  await waitFor(async () => ((await invoke("get_download", { id: c.id })).ok?.downloaded ?? 0) > 500_000, "bytes flowing");
  const cancelled = (await invoke("cancel_download", { id: c.id })).ok;
  check("cancel_download discards the partial file", cancelled?.status === "cancelled" && !fs.existsSync(path.join(root, "cancel.bin.part")));
  const resumeCancelled = await invoke("resume_download", { id: c.id });
  check("a cancelled download cannot be resumed", resumeCancelled.err?.code === "invalid_state", JSON.stringify(resumeCancelled));
  srv.delayMs = 0;

  const bad = (await invoke("create_download", { request: { url: `${base}/404/x.bin` } })).ok;
  const failed = await waitFor(async () => {
    const d = (await invoke("get_download", { id: bad.id })).ok;
    return d?.status === "failed" ? d : null;
  }, "404 download to fail");
  check("failure is recorded with a plain message and the technical one", /not on the server/.test(failed.error ?? "") && /404/.test(failed.errorDetail ?? ""), JSON.stringify([failed.error, failed.errorDetail]));
  const rm = await invoke("delete_download", { id: bad.id, deleteFile: false });
  check("delete_download removes the record", rm.ok === null && (await invoke("get_download", { id: bad.id })).err?.code === "not_found");

  const idle = (await invoke("create_download", { request: { url: `${base}/idle.bin`, start: false } })).ok;
  const prio = await invoke("set_priority", { id: idle.id, priority: "high" });
  check("set_priority persists", prio.ok?.priority === "high" && (await invoke("get_download", { id: idle.id })).ok?.priority === "high", JSON.stringify(prio));
  const prioDone = await invoke("set_priority", { id: slow.id, priority: "low" });
  check("set_priority refuses a completed download", prioDone.err?.code === "invalid_state", JSON.stringify(prioDone));
  await invoke("delete_download", { id: idle.id, deleteFile: false });
  const lim = await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 50 * 1024 * 1024 } });
  check("set_speed_limit accepted", lim.ok === null, JSON.stringify(lim));

  // ---- security: hostile input is rejected at the boundary --------------------------------
  const before = srv.requests;
  for (const [name, req] of [
    ["file:// URL", { url: "file:///C:/Windows/System32/drivers/etc/hosts" }],
    ["javascript: URL", { url: "javascript:alert(1)" }],
    ["path traversal in subdir", { url: `${base}/x.bin`, subdir: "../../evil" }],
    ["absolute subdir", { url: `${base}/x.bin`, subdir: "C:\\Windows\\Temp" }],
    ["path in file name", { url: `${base}/x.bin`, filename: "..\\..\\evil.exe" }],
    ["unknown field (smuggled 'execute')", { url: `${base}/x.bin`, execute: "calc.exe" }],
    ["bad priority enum", { url: `${base}/x.bin`, priority: "urgent" }],
  ]) {
    const r = await invoke("create_download", { request: req });
    check(`rejects ${name}`, !!r.err, JSON.stringify(r));
  }
  check("rejected requests never reached the network", srv.requests === before);
  check("nothing was written outside the download root", !fs.existsSync(path.join(tmp, "evil")) && !fs.existsSync(path.join(root, "..", "evil.exe")));
  const all = (await invoke("list_downloads", { filter: {} })).ok;
  check("rejected requests were not stored", all.every((d) => d.url.startsWith(base)) && all.length === 3);

  // ---- security: no escape hatches (capabilities / least privilege) ------------------------
  for (const cmd of [
    "run_shell",
    "execute",
    "read_file",
    "plugin:fs|read_file",
    "plugin:fs|write_file",
    "plugin:shell|execute",
    "plugin:opener|open_path",
    "plugin:http|fetch",
    "plugin:webview|create_webview_window",
    "plugin:window|close",
    "plugin:window|minimize",
    "plugin:window|toggle_maximize",
    "plugin:window|set_theme",
    "plugin:window|create",
  ]) {
    const r = await invoke(cmd, { path: "C:\\Windows\\win.ini", cmd: "calc.exe" });
    check(`IPC command \`${cmd}\` is unavailable`, !!r.err, JSON.stringify(r));
  }

  // ---- security: CSP blocks script injection ----------------------------------------------
  // NOTE: code evaluated through DevTools is exempt from CSP, so eval()/new Function() cannot be
  // probed from here (the CSP directives themselves are covered by a unit test on tauri.conf.json).
  // Instead exercise the policy the way an attacker's injected markup would: real DOM, real handlers.
  const handler = await page(
    `new Promise((r) => {
       window.__ran = false;
       // An image the policy allows (same origin) that fails to load: its error event fires, and the injected
       // inline handler must be refused. (An image from a refused origin would be stopped by img-src first.)
       document.addEventListener('securitypolicyviolation', (e) => e.violatedDirective.startsWith('script-src') && r({ violated: e.violatedDirective, ran: window.__ran }));
       document.body.insertAdjacentHTML('beforeend', '<img src="/velox-e2e-missing.png" onerror="window.__ran = true">');
       setTimeout(() => r({ violated: null, ran: window.__ran }), 1500);
     })`,
  );
  check("CSP blocks injected inline event handlers (and reports the violation)", handler.ok?.violated?.startsWith("script-src") && handler.ok.ran === false, JSON.stringify(handler));
  const inline = await page(
    `new Promise((r) => { window.__pwned = false; const s = document.createElement('script'); s.textContent = 'window.__pwned = true'; document.body.appendChild(s); setTimeout(() => r(window.__pwned), 200); })`,
  );
  check("CSP blocks inline <script>", inline.ok === false, JSON.stringify(inline));
  const remote = await page(
    `new Promise((r) => { const s = document.createElement('script'); s.src = '${base}/big.bin'; s.onload = () => r('loaded'); s.onerror = () => r('blocked'); document.body.appendChild(s); setTimeout(() => r('timeout'), 1500); })`,
  );
  check("CSP blocks scripts from other origins", remote.ok !== "loaded", JSON.stringify(remote));
  const fetchRes = await page(`fetch('${base}/big.bin', { method: 'HEAD' }).then(() => 'reached', () => 'blocked')`);
  check("CSP blocks the page from making its own network requests (connect-src)", fetchRes.ok === "blocked", JSON.stringify(fetchRes));
  const proto = await page("(() => { try { Object.prototype.polluted = 1; return Object.prototype.polluted === 1 ? 'writable' : 'ignored'; } catch (e) { return 'frozen'; } })()");
  check("prototypes are frozen (freezePrototype)", proto.ok !== "writable", JSON.stringify(proto));
  check("no global __TAURI__ object is exposed", (await page("typeof window.__TAURI__")).ok === "undefined");

  // ---- graceful shutdown: running downloads are paused and saved --------------------------
  srv.delayMs = 15;
  const last = (await invoke("create_download", { request: { url: `${base}/shutdown.bin` } })).ok;
  await waitFor(async () => ((await invoke("get_download", { id: last.id })).ok?.downloaded ?? 0) > 1_000_000, "bytes flowing");
  const gone = h.exited();
  // Close the window the way a user would; the app must pause + persist before exiting.
  await h.closeWindow();
  const code = await Promise.race([gone, sleep(20000).then(() => "timeout")]);
  check("app exits cleanly when the window is closed", code === 0, String(code));
  const listing = execFileSync(CLI, ["list", "--db", path.join(dataDir, "velox.db")], { encoding: "utf8" });
  check("the running download was paused and saved on exit", new RegExp(`#${last.id}\\s+paused`).test(listing), listing);
  check("its partial file was kept for resuming", fs.existsSync(path.join(root, "shutdown.bin.part")));
  h.app = null;

  // ---- restart: state survives and the download can be resumed -----------------------------
  srv.delayMs = 0;
  await launch();
  const back = (await invoke("get_download", { id: last.id })).ok;
  check("after restart the paused download is still there", back?.status === "paused" && back.downloaded > 0, JSON.stringify(back));
  await invoke("resume_download", { id: last.id });
  await waitFor(async () => (await invoke("get_download", { id: last.id })).ok?.status === "completed", "resume after restart");
  check(
    "resume after restart produces a byte-exact file",
    crypto.createHash("sha256").update(fs.readFileSync(path.join(root, "shutdown.bin"))).digest("hex") === FILE_SHA,
  );
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  srv.close();
  await h.cleanup();
}

finish();
