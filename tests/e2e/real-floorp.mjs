// By hand, Linux, needs Floorp installed: the real Floorp with the real VeloxFetch extension, against the real app.
//  A. The app connects the Firefox line by itself, in the one folder Floorp reads (~/.mozilla/native-messaging-hosts).
//  B. Floorp starts the helper from that registration and the extension reaches the app.
//  C. A file downloaded in Floorp is caught by the extension and handed to the app: the confirmation shows it, and
//     with the confirmation off the app downloads it itself, byte for byte.
//   node tests/e2e/real-floorp.mjs [app] [extension zip]
// Everything runs in a scratch home (Floorp headless, its own profile): the user's Floorp and folders are never touched.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const ZIP = path.resolve(process.argv[3] ?? "release/VeloxFetch-extension-firefox.zip");
const FLOORP = ["/usr/bin/floorp", "/usr/lib/floorp/floorp"].find((p) => fs.existsSync(p));
if (process.platform !== "linux" || !FLOORP || !fs.existsSync(ZIP)) {
  console.log("SKIP  Linux with Floorp installed, and the Firefox extension zip");
  process.exit(0);
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-floorp-"));
const home = path.join(tmp, "home");
fs.mkdirSync(home, { recursive: true });
const srv = await startFileServer(3 * 1024 * 1024);
const h = createHarness({
  port: 9433,
  appPath: APP,
  env: { VELOX_AUTO_EXTENSION: "1", VELOX_NO_LAUNCH: "1", VELOX_NMH_SCRATCH_HOME: home, VELOX_STORE_SYSTEM_ROOT: path.join(tmp, "root") },
});
const REG = path.join(home, ".mozilla/native-messaging-hosts/app.velox.fetch.json");
let floorp;

/** WebDriver BiDi, which Floorp speaks like Firefox: install the extension, open pages. */
async function bidi(port) {
  let ws;
  for (let i = 0; i < 80 && !ws; i++) {
    await sleep(500);
    try {
      const w = new WebSocket(`ws://127.0.0.1:${port}/session`);
      await new Promise((ok, no) => ((w.onopen = ok), (w.onerror = no)));
      ws = w;
    } catch {}
  }
  if (!ws) throw new Error("Floorp did not open its remote port");
  let id = 0;
  const waiting = new Map();
  ws.onmessage = (m) => {
    const r = JSON.parse(m.data);
    if (r.id && waiting.has(r.id)) waiting.get(r.id)(r), waiting.delete(r.id);
  };
  const send = (method, params) => new Promise((ok) => (waiting.set(++id, ok), ws.send(JSON.stringify({ id, method, params }))));
  await send("session.new", { capabilities: {} });
  return { send, close: () => ws.close() };
}

try {
  await h.launch();
  await waitFor(async () => (await h.page("document.querySelector('header')?.innerText ?? ''")).ok?.includes("VeloxFetch"), "UI");

  // ================================================================== A. connected in the folder Floorp reads
  await waitFor(() => fs.existsSync(REG), "the Firefox line's registration", 20000);
  const reg = JSON.parse(fs.readFileSync(REG, "utf8"));
  check("A. the app connects the Firefox line by itself, in ~/.mozilla (the folder Floorp reads)", reg.allowed_extensions?.includes("velox-fetch@velox.app"), JSON.stringify(reg));
  check("A. the registration names the helper next to the app", fs.existsSync(reg.path) && path.basename(reg.path) === exe("velox-native-host"), reg.path);

  // ================================================================== B. the real Floorp starts the helper; the extension reaches the app
  const port = 9556;
  const profile = path.join(tmp, "profile");
  fs.mkdirSync(profile);
  // Downloads go straight to a folder (no "save as" question), so the extension sees them as a user's would.
  fs.writeFileSync(path.join(profile, "user.js"), `user_pref("browser.download.useDownloadDir", true);\nuser_pref("browser.download.dir", ${JSON.stringify(path.join(tmp, "floorp-downloads"))});\nuser_pref("browser.download.folderList", 2);\nuser_pref("browser.download.always_ask_before_handling_new_types", false);\n`);
  floorp = spawn(FLOORP, ["--headless", "--no-remote", "--remote-debugging-port", String(port), "--profile", profile], {
    // The helper Floorp starts gets this environment: it finds this test's app (debug builds only).
    env: { HOME: home, PATH: "/usr/bin:/bin", MOZ_HEADLESS: "1", XDG_CONFIG_HOME: path.join(home, ".config"), VELOX_DATA_DIR: h.dataDir, VELOX_BRIDGE_ENDPOINT: h.endpoint },
    stdio: "ignore",
  });
  const b = await bidi(port);
  const inst = await b.send("webExtension.install", { extensionData: { type: "archivePath", path: ZIP } });
  check("B. the real VeloxFetch extension installs in Floorp", inst.result?.extension === "velox-fetch@velox.app", JSON.stringify(inst));
  await waitFor(async () => !!(await h.invoke("get_browser_integration")).ok?.extensionContact, "the extension's first contact", 30000);
  const contact = (await h.invoke("get_browser_integration")).ok.extensionContact;
  check("B. Floorp started the helper and the extension reached the app", !!contact, JSON.stringify(contact));

  // ================================================================== C. a download in Floorp goes to the app
  const ctx = (await b.send("browsingContext.getTree", {})).result.contexts[0].context;
  const dl = (name) => b.send("browsingContext.navigate", { context: ctx, url: `${srv.base}/files/${name}`, wait: "none" });
  await dl("from-floorp-1.bin");
  await waitFor(async () => (await h.page("document.querySelector('[data-testid=pending-host]')?.innerText ?? ''")).ok, "the confirmation", 20000);
  check("C. a download in Floorp is caught and the app asks first (the default)", (await h.page("document.body.innerText")).ok.includes("from-floorp-1.bin"));
  await h.page(`(() => { document.querySelector('[data-testid=pending-cancel], [data-testid=pending-dismiss]')?.click(); return true; })()`);

  await h.invoke("set_browser_confirm", { enabled: false });
  await sleep(500);
  await dl("from-floorp-2.bin");
  await waitFor(async () => ((await h.invoke("list_downloads", { filter: {} })).ok ?? []).some((d) => d.filename === "from-floorp-2.bin" && d.status === "completed"), "the app's download", 30000);
  const got = path.join(h.root, "from-floorp-2.bin");
  check("C. with the confirmation off, the app downloads it itself, byte for byte", fs.existsSync(got) && fs.statSync(got).size === srv.FILE.length);
  const leftover = fs.existsSync(path.join(tmp, "floorp-downloads")) ? fs.readdirSync(path.join(tmp, "floorp-downloads")).filter((n) => n.includes("from-floorp-2") && !n.endsWith(".part")) : [];
  check("C. Floorp does not download it a second time", leftover.length === 0, leftover.join(","));
  b.close();
} catch (e) {
  check("no exception", false, e.stack);
} finally {
  floorp?.kill();
  await h.cleanup();
  srv.close?.();
}
finish();
