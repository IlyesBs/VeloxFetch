// Guards for properties of the built extension that must never drift or be widened by accident.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
execFileSync(process.execPath, [join(root, "build.mjs")], { stdio: "ignore" });
const manifest = (t) => JSON.parse(readFileSync(join(root, "dist", t, "manifest.json"), "utf8"));

/** Chromium derives an extension id from the public key: sha256, first 16 bytes, hex mapped 0-9a-f -> a-p. */
function idFromKey(b64) {
  const hex = createHash("sha256").update(Buffer.from(b64, "base64")).digest("hex").slice(0, 32);
  return [...hex].map((c) => String.fromCharCode("a".charCodeAt(0) + parseInt(c, 16))).join("");
}

test("the Chrome extension id equals the id the native host and the app trust", () => {
  const id = idFromKey(manifest("chrome").key);
  const rust = readFileSync(join(root, "..", "..", "crates", "browser-bridge", "src", "caller.rs"), "utf8");
  const trusted = /CHROME_EXTENSION_ID: &str = "([a-p]{32})"/.exec(rust)?.[1];
  assert.equal(id, trusted, "manifest key and CHROME_EXTENSION_ID have drifted apart");
});

test("the Firefox add-on id equals the id the host trusts", () => {
  const rust = readFileSync(join(root, "..", "..", "crates", "browser-bridge", "src", "caller.rs"), "utf8");
  const trusted = /FIREFOX_EXTENSION_ID: &str = "([^"]+)"/.exec(rust)?.[1];
  assert.equal(manifest("firefox").browser_specific_settings.gecko.id, trusted);
});

test("Firefox: what goes to the app is declared (Firefox's built-in data consent), exactly that", () => {
  // Firefox counts what an add-on hands to a native application. The link and the page it came from
  // (browsingActivity, websiteContent) and, if allowed, that site's cookies (authenticationInfo) go to the
  // VeloxFetch app on the same computer; nothing goes anywhere else, so nothing is optional and nothing more.
  assert.deepEqual(manifest("firefox").browser_specific_settings.gecko.data_collection_permissions, {
    required: ["browsingActivity", "websiteContent", "authenticationInfo"],
  });
});

test("the host name matches the one the app registers", () => {
  const rust = readFileSync(join(root, "..", "..", "crates", "browser-bridge", "src", "caller.rs"), "utf8");
  const host = /HOST_NAME: &str = "([^"]+)"/.exec(rust)?.[1];
  assert.match(readFileSync(join(root, "src", "background.js"), "utf8"), new RegExp(`HOST = "${host.replace(/\./g, "\\.")}"`));
});

for (const target of ["chrome", "firefox"]) {
  test(`${target}: least privilege`, () => {
    const m = manifest(target);
    assert.equal(m.manifest_version, 3);
    const allowed = new Set(["contextMenus", "nativeMessaging", "downloads", "storage", "activeTab", "scripting", "cookies", "notifications", "webRequest"]);
    for (const p of m.permissions) assert.ok(allowed.has(p), `unexpected permission ${p}`);
    // Video detection is on by default (like other download managers): it needs `webRequest` (it only
    // *reads* response headers, never blocks or changes them) and http(s) site access for the bar.
    assert.deepEqual(m.host_permissions, ["http://*/*", "https://*/*"]);
    assert.equal(m.content_scripts, undefined, "no content script up front: the video bar is registered at run time while detection is on");
    assert.equal(m.externally_connectable, undefined, "web pages and other extensions cannot message us");
    assert.equal(m.web_accessible_resources, undefined);
    assert.equal(m.content_security_policy, undefined, "the strict MV3 default applies");
    assert.equal(m.commands, undefined);
    assert.equal(m.optional_permissions, undefined);
    assert.equal(m.optional_host_permissions, undefined);
    for (const p of ["<all_urls>", "webRequestBlocking", "declarativeNetRequest", "tabs", "history", "management", "proxy", "debugger", "webNavigation"]) {
      assert.ok(!JSON.stringify(m).includes(`"${p}"`), `${p} must not be requested`);
    }
  });
}

test("only Chrome pins a key; only Firefox has an add-on id", () => {
  assert.ok(manifest("chrome").key && !manifest("chrome").browser_specific_settings);
  assert.ok(!manifest("firefox").key && manifest("firefox").browser_specific_settings.gecko.id);
});

test("no source file can evaluate or inject strings as code or markup", () => {
  const bad = [/\beval\s*\(/, /new\s+Function\b/, /\.innerHTML\b/, /\.outerHTML\s*=/, /document\.write/, /insertAdjacentHTML/, /setTimeout\s*\(\s*["'`]/, /setInterval\s*\(\s*["'`]/, /\bimportScripts\s*\(\s*[^"')]/];
  const walk = (d) => readdirSync(d).flatMap((f) => (statSync(join(d, f)).isDirectory() ? walk(join(d, f)) : [join(d, f)]));
  for (const f of walk(join(root, "src")).filter((f) => /\.(js|html)$/.test(f))) {
    const s = readFileSync(f, "utf8");
    for (const re of bad) assert.ok(!re.test(s), `${f} matches ${re}`);
    if (f.endsWith(".html")) assert.ok(!/<script(?![^>]*\bsrc=)/i.test(s) && !/\son\w+\s*=/i.test(s), `${f} has inline script`);
  }
});

test("the extension has exactly one way out: the native host", () => {
  const src = ["background.js", "popup.js", "videobar.js", "lib/media.js"].map((f) => readFileSync(join(root, "src", f), "utf8")).join("\n");
  for (const re of [/\bfetch\s*\(/, /XMLHttpRequest/, /WebSocket/, /sendBeacon/, /EventSource/, /tabs\.create/, /windows\.create/, /tabs\.update/]) {
    assert.ok(!re.test(src), `unexpected ${re}`);
  }
  assert.match(src, /sendNativeMessage/);
});

test("the video bar is registered at run time only, in top frames, and only from our own file", () => {
  const bg = readFileSync(join(root, "src", "background.js"), "utf8");
  const calls = bg.match(/registerContentScripts\(\[\{.*?\}\]\)/g) ?? [];
  assert.equal(calls.length, 1, "exactly one registration");
  assert.match(calls[0], /js: \["videobar\.js"\]/);
  assert.match(calls[0], /allFrames: false/);
  // Besides the registration, only the pages already open when it is registered get the same file.
  const injections = bg.match(/executeScript\([^;]*/g) ?? [];
  assert.equal(injections.length, 1, "one injection, for the open tabs");
  assert.match(injections[0], /files: \["videobar\.js"\]/);
  assert.match(bg, /registerContentScripts\(\[\{.*?\}\]\)\.then\(injectOpenTabs\)/);
  // It is only registered while the popup's setting AND VeloxFetch's own switch are on AND the browser granted the permission.
  assert.match(bg, /detectOn = r\[0\]\.detectVideos && r\[0\]\.appVideoBar && r\[1\]/);
  // The app's switch is only learnt from its ping answer; the popup cannot set it.
  assert.match(bg, /ext\.storage\.local\.set\(\{ appVideoBar: r\.videoBar \}\)/);
});

test("the video bar cannot be read or driven by the page, and never sends a link", () => {
  const bar = readFileSync(join(root, "src", "videobar.js"), "utf8");
  assert.match(bar, /attachShadow\(\{ mode: "closed" \}\)/);
  const sends = bar.match(/type: "bar:[a-z]+"/g) ?? [];
  assert.deepEqual([...new Set(sends)].sort(), ['type: "bar:download"', 'type: "bar:state"']);
  assert.ok(!/url\s*:/.test(bar.replace(/\/\*[\s\S]*?\*\//g, "")), "no link is ever put in a message");
  assert.ok((bar.match(/isTrusted/g) ?? []).length >= 3, "every click handler checks isTrusted");
  for (const re of [/\.src/, /currentSrc/, /\.href/, /postMessage/, /addEventListener\("message"/]) {
    assert.ok(!re.test(bar), `the bar must not use ${re}`);
  }
});

test("the site lists are the same in the extension, the app and the UI", async () => {
  const rust = readFileSync(join(root, "..", "..", "crates", "media-engine", "src", "site.rs"), "utf8");
  const between = (s, from, to) => s.slice(s.indexOf(from), s.indexOf(to, s.indexOf(from)));
  const rustSites = [...between(rust, "pub const VIDEO_SITES", "];").matchAll(/\("([^"]+)", &\[([^\]]*)\]\)/g)].map((m) => [m[1], [...m[2].matchAll(/"([^"]+)"/g)].map((x) => x[1])]);
  const rustProtected = [...between(rust, "pub const PROTECTED_SITES", "];").matchAll(/\("([^"]+)", "([^"]+)"\)/g)].map((m) => [m[1], m[2]]);
  const M = (await import("../src/lib/media.js")).default;
  assert.deepEqual(M.VIDEO_SITES, rustSites);
  assert.deepEqual(M.PROTECTED_SITES, rustProtected);
  const ui = readFileSync(join(root, "..", "desktop", "src", "format.ts"), "utf8");
  const uiSites = [...between(ui, "const VIDEO_SITES", "];").matchAll(/\["([^"]+)", \[([^\]]*)\]\]/g)].map((m) => [m[1], [...m[2].matchAll(/"([^"]+)"/g)].map((x) => x[1])]);
  assert.deepEqual(uiSites, rustSites);
});
