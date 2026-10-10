// End-to-end test of Smart throttle and Live preview in the real desktop app (WebView2, WebKitGTK):
//  * preview: a real, playable audio file (WAV) served slowly; the details' Preview card plays it through
//    the app's own `velox-preview` protocol (Range requests, 206) while it is still downloading;
//    a page script cannot read the protocol (CSP);
//  * smart throttle: the app's own process stands in for "a game" (so the real process scan runs), the
//    status line follows it, a download is held back, and turning it off restores full speed at once.
//
//   node tests/e2e/desktop-preview.mjs [path-to-debug-velox-desktop.exe]

import http from "node:http";
import os from "node:os";
import { spawnSync } from "node:child_process";
import { check, createHarness, exe, fail, finish, fs, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

// ---- a real WAV: 60 s of a 440 Hz tone, 44.1 kHz, 16-bit mono (~5 MB) -------------------------------
function wav(seconds) {
  const rate = 44100;
  const n = rate * seconds;
  const b = Buffer.alloc(44 + n * 2);
  b.write("RIFF", 0);
  b.writeUInt32LE(36 + n * 2, 4);
  b.write("WAVEfmt ", 8);
  b.writeUInt32LE(16, 16);
  b.writeUInt16LE(1, 20);
  b.writeUInt16LE(1, 22);
  b.writeUInt32LE(rate, 24);
  b.writeUInt32LE(rate * 2, 28);
  b.writeUInt16LE(2, 32);
  b.writeUInt16LE(16, 34);
  b.write("data", 36);
  b.writeUInt32LE(n * 2, 40);
  for (let i = 0; i < n; i++) b.writeInt16LE(Math.round(Math.sin((2 * Math.PI * 440 * i) / rate) * 8000), 44 + i * 2);
  return b;
}
const SONG = wav(60);

/** Linux: can this computer's sound server take a stream at all? (A stuck PulseAudio layer makes every player wait
 *  forever, browsers included; then playback cannot advance in any app, and that check is skipped, said out loud.) */
function soundWorks() {
  if (process.platform !== "linux") return true;
  const f = path.join(os.tmpdir(), `velox-e2e-silence-${process.pid}.wav`);
  fs.writeFileSync(f, wav(0.2).fill(0, 44));
  const r = spawnSync("paplay", [f], { timeout: 5000 });
  fs.rmSync(f, { force: true });
  return r.error?.code === "ENOENT" || (!r.error && r.status === 0); // stopped by the timeout: it hung
}
const BIG = Buffer.alloc(24 * MB, 3);
let chunkDelay = 40; // ms per 64 KB per connection: slow enough to watch while it downloads
const site = http.createServer(async (req, res) => {
  const body = req.url.startsWith("/song.wav") ? SONG : BIG;
  const type = req.url.startsWith("/song.wav") ? "audio/wav" : "application/octet-stream";
  const m = /bytes=(\d+)-(\d*)/.exec(req.headers.range ?? "");
  const start = m ? Number(m[1]) : 0;
  const end = m && m[2] ? Number(m[2]) : body.length - 1;
  const head = { "Content-Type": type, "Accept-Ranges": "bytes", "Content-Length": end - start + 1, ETag: '"e2e"' };
  if (m) res.writeHead(206, { ...head, "Content-Range": `bytes ${start}-${end}/${body.length}` });
  else res.writeHead(200, head);
  if (req.method === "HEAD") return void res.end();
  for (let p = start; p <= end; p += 65536) {
    if (res.destroyed) return;
    res.write(body.subarray(p, Math.min(p + 65536, end + 1)));
    if (chunkDelay) await sleep(chunkDelay);
  }
  res.end();
});
await new Promise((r) => site.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${site.address().port}`;

const h = createHarness({ port: 9355, appPath: APP });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const text = (sel) => ev(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`);
const click = async (sel) => {
  await waitFor(() => has(sel), `the element ${sel}`, 10000);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};
const dl = async (id) => (await invoke("get_download", { id })).ok;

try {
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => { window.__errs = []; window.__csp = []; window.addEventListener('error', (e) => window.__errs.push(e.message));
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI)); return true; })()`);

  // ======================================================================== live preview
  // About 200 kB/s overall, so the 5 MB song takes ~26 s: long enough to play it while it downloads.
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: 200 * 1024 } });
  const song = (await invoke("create_download", { request: { url: `${base}/song.wav` } })).ok;
  await waitFor(async () => (await dl(song.id))?.status === "downloading" && (await dl(song.id)).totalSize > 0, "the song to start");
  await click("[data-testid=nav-downloads]");
  await click(`[data-testid=download-${song.id}]`);
  await click("[data-testid=preview-open]");
  check("the details of an audio / video download offer a preview", true);
  await waitFor(() => has("[data-testid=preview-video]"), "the player");
  const src = await ev(`document.querySelector('[data-testid=preview-video]').src`);
  // Tauri's address for a custom protocol on Windows: http://<name>.localhost/. Linux's and macOS's web views cannot
  // stream a growing file from a custom protocol: the app's private address there (this computer only, a 64-hex secret).
  const previewUrl = process.platform === "win32" ? `http://velox-preview.localhost/${song.id}` : null;
  const ok = previewUrl ? src === previewUrl : new RegExp(`^http://127\\.0\\.0\\.1:\\d+/[0-9a-f]{64}/${song.id}$`).test(src);
  check("the player reads from the app's own address, by id", ok, src);
  if (!previewUrl) {
    const port = new URL(src).port;
    const wrong = await new Promise((res) =>
      http.get({ host: "127.0.0.1", port, path: `/${"0".repeat(64)}/${song.id}` }, (r) => (r.resume(), res(r.statusCode))).on("error", () => res(0)),
    );
    check("Linux / macOS: without the secret the private address gives nothing", wrong === 404, String(wrong));
    const rebound = await new Promise((res) =>
      http.get({ host: "127.0.0.1", port, path: new URL(src).pathname, headers: { Host: `evil.example:${port}` } }, (r) => (r.resume(), res(r.statusCode))).on("error", () => res(0)),
    );
    check("Linux / macOS: another host name for it is refused (DNS rebinding)", rebound === 403, String(rebound));
  }
  await waitFor(async () => (await ev(`document.querySelector('[data-testid=preview-video]').readyState`)) >= 1, "the player to read the file's header", 20000);
  const duration = await ev(`document.querySelector('[data-testid=preview-video]').duration`);
  check("the player knows the whole length from the first bytes (60 s)", Math.abs(duration - 60) < 1, String(duration));
  const before = await dl(song.id);
  check("...while the file is still downloading", before.status === "downloading" && before.downloaded < before.totalSize, `${before.downloaded}/${before.totalSize}`);
  await page(`(() => { const v = document.querySelector('[data-testid=preview-video]'); v.volume = 0; v.scrollIntoView({ block: "center" }); return v.play().then(() => true, (e) => String(e)); })()`);
  // The bytes keep coming as the download writes them (one long answer, not just what was there at first).
  const buffered = () => ev(`(() => { const b = document.querySelector('[data-testid=preview-video]').buffered; return b.length ? b.end(b.length - 1) : 0; })()`);
  const b1 = await buffered();
  await waitFor(async () => (await buffered()) > b1 + 2, "the player to receive more as the download goes on", 20000);
  check("the player keeps receiving bytes while the file downloads", true);
  if (soundWorks()) {
    await waitFor(async () => (await ev(`document.querySelector('[data-testid=preview-video]').currentTime`)) > 1.5, "playback to advance", 20000);
    const during = await dl(song.id);
    check("it plays while downloading", during.status === "downloading" && during.downloaded < during.totalSize, `${during.status} ${during.downloaded}/${during.totalSize}`);
  } else {
    console.log("SKIP  it plays while downloading: this computer's sound server takes no stream (paplay hangs too)");
  }
  const err = await ev(`document.querySelector('[data-testid=preview-video]').error?.code ?? 0`);
  check("the player reports no error (no corrupt or missing bytes)", err === 0, String(err));
  await h.page("document.documentElement.setAttribute('data-theme','dark')");
  await page(`(() => { document.querySelector('[data-testid=preview-card]').scrollIntoView({ block: "center" }); return true; })()`);
  await sleep(400);
  await h.screenshot(path.join(SHOTS, "preview-dark.png"), { width: 1000, height: 820 });
  await h.page("document.documentElement.removeAttribute('data-theme')");

  const sneaky = await ev(`fetch("http://velox-preview.localhost/${song.id}").then(() => "read", () => "blocked")`);
  check("a page script cannot read the preview protocol (CSP connect-src)", sneaky === "blocked", sneaky);
  if (process.platform !== "win32") {
    const src2 = await ev(`document.querySelector('[data-testid=preview-video]').src`);
    const sneaky2 = await ev(`fetch(${JSON.stringify("SRC")}.replace("SRC", document.querySelector('[data-testid=preview-video]').src)).then(() => "read", () => "blocked")`);
    check("nor the private address (Linux / macOS)", sneaky2 === "blocked", `${sneaky2} ${src2}`);
  }
  check("a seek far ahead does not break the player", await ev(`(async () => { const v = document.querySelector('[data-testid=preview-video]'); v.currentTime = 50; await new Promise((r) => setTimeout(r, 1500)); return !v.error; })()`));
  await invoke("set_speed_limit", { request: { target: { kind: "global" }, bytesPerSec: null } });
  chunkDelay = 0;
  await waitFor(async () => (await dl(song.id)).status === "completed", "the song to finish", 60000);
  check("the preview did not disturb the download (byte-exact)", Buffer.compare(fs.readFileSync((await dl(song.id)).destination), SONG) === 0);
  await click("[data-testid=preview-close]");
  const notVideo = await invoke("get_preview_info", { id: (await invoke("create_download", { request: { url: `${base}/big.bin`, start: false } })).ok.id });
  check("a file that is not audio or video has no preview", !!notVideo.err, JSON.stringify(notVideo).slice(0, 120));

  // ======================================================================== smart throttle
  await click("[data-testid=nav-settings]");
  await page(`(() => { [...document.querySelectorAll("[data-testid=settings-nav] button")].find((b) => b.innerText.trim() === "Speed").click(); return true; })()`);
  await waitFor(() => has("[data-testid=smart-enabled]"), "Settings > Speed > Smart throttle");
  check("smart throttle is off by default", (await ev(`document.querySelector('[data-testid=smart-enabled]').checked`)) === false);
  // The app itself is a running program: listing it makes the real scan find something.
  await page(`(() => { const el = document.querySelector('[data-testid=smart-apps]');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(el, ["velox-desktop", "League of Legends", "Zoom"].join(String.fromCharCode(10)));
    el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
  await click("[data-testid=smart-save]");
  await waitFor(async () => JSON.stringify((await invoke("get_smart_throttle")).ok.apps) === '["velox-desktop","League of Legends","Zoom"]', "the list to be saved from the UI");
  await page(`(() => { const el = document.querySelector('[data-testid=smart-mode]');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(el, "10");
    el.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await waitFor(async () => (await invoke("get_smart_throttle")).ok.sharePercent === 10, "the share to be saved");
  check("the program list and the share are saved from the UI", true);
  await click("[data-testid=smart-enabled]");
  await waitFor(async () => /velox-desktop is running/.test((await text("[data-testid=smart-status]")) ?? ""), "the scan to find the program", 15000);
  const st = (await invoke("get_smart_throttle")).ok;
  check("the real process scan finds the listed program and sets a limit", st.activeApp === "velox-desktop" && st.limitBps > 0 && st.scanOk, JSON.stringify(st));
  await h.screenshot(path.join(SHOTS, "smart-throttle.png"), { width: 1000, height: 900 });
  chunkDelay = 0;
  const big = (await invoke("create_download", { request: { url: `${base}/big.bin?x=1` } })).ok;
  await sleep(2500);
  const slow = (await dl(big.id)).downloaded;
  check("downloads are held back while it runs", slow < 12 * MB && slow > 0, `${slow} bytes in 2.5 s with a ${st.limitBps} B/s limit`);
  await click("[data-testid=smart-enabled]");
  await waitFor(async () => /Off/.test((await text("[data-testid=smart-status]")) ?? ""), "the status to say off");
  const t = Date.now();
  await waitFor(async () => (await dl(big.id)).status === "completed", "the rest at full speed", 20000);
  check("turning it off gives full speed back at once", Date.now() - t < 6000, `${Date.now() - t} ms`);
  check("and nothing is limited any more", (await invoke("get_smart_throttle")).ok.limitBps === null);

  check("no CSP violations other than the blocked script read", ((await ev("window.__csp")) ?? []).every((v) => v.startsWith("connect-src")), JSON.stringify(await ev("window.__csp")));
  check("no page errors", ((await ev("window.__errs")) ?? []).length === 0, JSON.stringify(await ev("window.__errs")));
} catch (e) {
  console.error(e);
  fail();
} finally {
  await h.cleanup();
  site.close();
}
finish();
