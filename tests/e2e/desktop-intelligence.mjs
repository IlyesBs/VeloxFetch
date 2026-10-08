// End-to-end test of Download Intelligence in the real desktop app: "Check link" in the Add dialog (rows, warnings,
// "Why?"), a sign-in and a missing file, nothing downloaded or added by a check, the panel in the confirmation
// dialog of a link handed over for review, and hostile IPC.
//
//   node tests/e2e/desktop-intelligence.mjs [path-to-debug-velox-desktop]

import { check, createHarness, exe, fail, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const MB = 1024 * 1024;

const srv = await startFileServer(8 * MB);
const h = createHarness({ port: 9369, appPath: APP });
const { page, invoke } = h;

const q = (sel) => page(`!!document.querySelector(${JSON.stringify(sel)})`).then((r) => r.ok);
const text = async (sel) => (await page(`document.querySelector(${JSON.stringify(sel)})?.innerText ?? null`)).ok;
const attr = async (sel, a) => (await page(`document.querySelector(${JSON.stringify(sel)})?.getAttribute(${JSON.stringify(a)}) ?? null`)).ok;
const click = (sel) =>
  page(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no element ' + ${JSON.stringify(sel)}); e.click(); return true; })()`);
const fill = (sel, v) =>
  page(`(() => {
    const el = document.querySelector(${JSON.stringify(sel)}); if (!el) throw new Error('no input ' + ${JSON.stringify(sel)});
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, ${JSON.stringify(v)});
    el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
const theme = (t) => page(`(document.documentElement.setAttribute('data-theme', ${JSON.stringify(t)}), true)`);
const URL_INPUT = "dialog[aria-labelledby=add-title] input.input";
const PANEL = "dialog[aria-labelledby=add-title] [data-testid=intel]";
const checkLink = async (url) => {
  await fill(URL_INPUT, url);
  await click("[data-testid=add-check]");
  await waitFor(() => q(PANEL), `the analysis of ${url}`);
};

try {
  console.log(`app: ${APP}\nserver: ${srv.base}\n`);
  await h.launch();
  await waitFor(async () => (await text("header"))?.includes("VeloxFetch"), "UI to render");
  await page(`(() => {
    window.__csp = []; window.__errs = [];
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI));
    window.addEventListener('error', (e) => window.__errs.push(e.message));
    window.addEventListener('unhandledrejection', (e) => window.__errs.push(String(e.reason)));
    return true;
  })()`);

  // ---- the Add dialog: Check link ----------------------------------------------------------------------------
  await page(`(window.dispatchEvent(new Event('velox:open-add')), true)`);
  await waitFor(() => q("[data-testid=add-check]"), "the Check link button");
  check("Check link waits for a link", (await page(`document.querySelector('[data-testid=add-check]').disabled`)).ok === true);
  const before = srv.requests;
  await checkLink(`${srv.base}/pkg/app.zip`);
  check("file name", (await text("[data-testid=intel-file]")) === "app.zip");
  check("size from the server", (await text("[data-testid=intel-size]"))?.includes("8.0 MB"), await text("[data-testid=intel-size]"));
  check("resume: supported (ranges proved, version tag given)", (await text("[data-testid=intel-resume]")) === "Supported");
  check("strategy: parallel, adjusted while it runs", (await text("[data-testid=intel-strategy]"))?.startsWith("Parallel"), await text("[data-testid=intel-strategy]"));
  check("storage checked against the size", (await text("[data-testid=intel-storage]"))?.startsWith("OK"));
  check("plain http is flagged", (await text("[data-testid=intel-security]"))?.includes("HTTP") && (await q("[data-testid=intel-finding-insecure]")));
  check("overall: read the warnings", (await attr(PANEL, "data-health")) === "caution");
  check("the check made two quick requests at most (never the file)", srv.requests - before <= 2, String(srv.requests - before));
  check("nothing was added to the list", (await invoke("list_downloads", { filter: {} })).ok.length === 0);
  await click("[data-testid=intel-why] summary");
  await sleep(150);
  check("Why? lists what the server did", (await text("[data-testid=intel-why]"))?.includes("one byte"), await text("[data-testid=intel-why]"));
  await theme("dark");
  await sleep(250);
  await h.screenshot(path.join(SHOTS, "intelligence-dark.png"), { width: 1100, height: 820 });
  await theme("light");
  await sleep(250);
  await h.screenshot(path.join(SHOTS, "intelligence-light.png"), { width: 1100, height: 820 });

  // A changed link drops the old result.
  await fill(URL_INPUT, `${srv.base}/other.zip`);
  await sleep(100);
  check("editing the link drops the old analysis", !(await q(PANEL)));

  // A sign-in, and a missing file.
  await checkLink(`${srv.base}/auth/members.zip`);
  check("sign-in required is a problem", (await attr(PANEL, "data-health")) === "problem" && (await q("[data-testid=intel-finding-authRequired]")));
  await checkLink(`${srv.base}/404/gone.zip`);
  check("a missing file names the error", (await text("[data-testid=intel-finding-serverError]"))?.includes("HTTP 404"), await text("[data-testid=intel-warnings]"));
  await page(`(document.querySelector('dialog[aria-labelledby=add-title]').close(), true)`);
  await sleep(100);
  check("closing the dialog drops the analysis", !(await q(PANEL)));

  // ---- the confirmation dialog of a link handed over (clipboard / browser) -------------------------------------
  const p = (await invoke("review_link", { url: `${srv.base}/media/movie.mkv` })).ok;
  check("a link handed over waits for the user", !!p?.token, JSON.stringify(p));
  await waitFor(() => q("[data-testid=pending-dialog] [data-testid=intel]"), "the analysis in the confirmation dialog");
  check("the confirmation dialog says resume is supported", (await text("[data-testid=pending-resume]")) === "Supported");
  check("its panel shows the strategy and storage", (await q("[data-testid=pending-dialog] [data-testid=intel-strategy]")) && (await q("[data-testid=pending-dialog] [data-testid=intel-storage]")));
  check("file, type and size are not repeated in its panel", !(await q("[data-testid=pending-dialog] [data-testid=intel-file]")));
  await click("[data-testid=pending-cancel]");
  await waitFor(async () => !(await q("[data-testid=pending-dialog]")), "the dialog to close");
  check("still nothing in the list", (await invoke("list_downloads", { filter: {} })).ok.length === 0);

  // ---- hostile IPC ----------------------------------------------------------------------------------------------
  const rejected = async (args, why) => {
    const r = await invoke("analyze_link", args);
    check(`rejected: ${why}`, !!r.err && r.ok === undefined, JSON.stringify(r));
  };
  await rejected({ request: { url: `${srv.base}/a.zip`, headers: [["Cookie", "x"]] } }, "extra field (headers) on a check");
  await rejected({ request: { url: "file:///etc/passwd" } }, "a local file");
  await rejected({ request: { url: "javascript:alert(1)" } }, "a script link");
  check("url: null only cancels", (await invoke("analyze_link", { request: { url: null } })).ok === null);

  const csp = (await page("window.__csp")).ok;
  const errs = (await page("window.__errs")).ok;
  check("no CSP violations or script errors", csp.length === 0 && errs.length === 0, JSON.stringify({ csp, errs }));
} catch (e) {
  console.error("\nE2E ABORTED:", e);
  fail();
} finally {
  srv.close();
  await h.cleanup();
}

finish();
