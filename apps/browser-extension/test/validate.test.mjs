import assert from "node:assert/strict";
import test from "node:test";
import V from "../src/lib/validate.js";

const CTX = { runtimeId: "abc", popupUrl: "chrome-extension://abc/popup.html" };
const POPUP = { id: "abc", url: "chrome-extension://abc/popup.html" };

test("only plain http(s) URLs are accepted, as data", () => {
  for (const ok of ["https://example.com/a.zip", "http://127.0.0.1:8080/x?y=1#z", "https://xn--r8jz45g.jp/a"]) {
    assert.ok(V.parseHttpUrl(ok), ok);
  }
  for (const bad of [
    "javascript:alert(1)", "data:text/html,<script>1</script>", "file:///C:/Windows/win.ini", "ftp://a.test/x", "chrome://settings",
    "chrome-extension://abc/popup.html", "about:blank", "blob:https://a.test/uuid", "view-source:https://a.test/", "vbscript:x",
    "https://user:pass@a.test/", "https://a.test/a\nb", "https://a.test/a\u0000b", "https://a.test/\u007f", "\\\\server\\share",
    "calc.exe", "", " ", null, undefined, 5, {}, [], "https://", "https://a.test/" + "a".repeat(9000),
  ]) {
    assert.equal(V.parseHttpUrl(bad), null, JSON.stringify(bad));
  }
});

test("file names must be plain names", () => {
  for (const ok of ["a.zip", "My File (1).tar.gz", "r\u00e9sum\u00e9.pdf"]) assert.equal(V.cleanFilename(ok), ok);
  for (const bad of ["", ".", "..", "a/b", "a\\b", "C:x.exe", "a*b", "a?b", 'a"b', "a<b", "a|b", "a\nb", "a\0b", "x".repeat(256), null, 5]) {
    assert.equal(V.cleanFilename(bad), null, JSON.stringify(bad));
  }
});

test("cookie header: only safe pairs, never altered", () => {
  assert.equal(V.cookieHeader([{ name: "sid", value: "1" }, { name: "t", value: "dark" }]), "sid=1; t=dark");
  // Unsafe pairs are skipped whole; they are never "cleaned up".
  assert.equal(V.cookieHeader([{ name: "a", value: "b\r\nX-Evil: 1" }, { name: "ok", value: "1" }]), "ok=1");
  assert.equal(V.cookieHeader([{ name: "a", value: "\u00e9" }]), null);
  assert.equal(V.cookieHeader([{ name: "a", value: "b; c=d" }]), null, "a ';' would smuggle a second cookie");
  assert.equal(V.cookieHeader([]), null);
  assert.equal(V.cookieHeader(null), null);
  assert.equal(V.cookieHeader([{ name: "big", value: "x".repeat(V.MAX_COOKIE + 1) }]), null, "too large: dropped, not truncated");
  assert.equal(V.cookieHeader([{ name: 1, value: 2 }, null, {}]), null);
});

test("the payload matches the host's schema and drops bad optional fields instead of failing", () => {
  const p = V.buildDownloadPayload({
    url: "https://example.com/a.zip", filename: "a.zip", referrer: "https://example.com/page#frag", cookies: "sid=1", userAgent: "Mozilla/5.0", source: "contextMenu",
  });
  assert.deepEqual(p, {
    command: "download", url: "https://example.com/a.zip", source: "contextMenu", filename: "a.zip",
    referrer: "https://example.com/page", cookies: "sid=1", userAgent: "Mozilla/5.0",
  });
  const keys = new Set(["command", "url", "source", "filename", "referrer", "cookies", "userAgent", "incognito"]);
  assert.ok(Object.keys(p).every((k) => keys.has(k)), "no fields the host does not know");

  const sloppy = V.buildDownloadPayload({ url: "https://a.test/x", filename: "../evil", referrer: "javascript:1", cookies: "a\r\nb", userAgent: "ua\n", source: "weird" });
  assert.deepEqual(sloppy, { command: "download", url: "https://a.test/x", source: "popup" }, "bad hints are omitted, unknown source falls back");
  assert.equal(V.buildDownloadPayload({ url: "file:///x", source: "popup" }), null, "a bad URL is a failure");
  assert.equal(V.buildDownloadPayload(null), null);
});

test("private windows never send cookies", () => {
  const p = V.buildDownloadPayload({ url: "https://a.test/x", cookies: "sid=1", incognito: true, source: "popup" });
  assert.equal(p.cookies, undefined);
  assert.equal(p.incognito, true);
});

test("download-worthy links", () => {
  for (const [url, attr, want] of [
    ["https://a.test/file.zip", false, true], ["https://a.test/x.ISO?dl=1", false, true], ["https://a.test/video.MP4", false, true],
    ["https://a.test/page.html", false, false], ["https://a.test/about", false, false], ["https://a.test/about", true, true],
    ["javascript:void(0)", true, false], ["mailto:a@b.c", false, false], ["https://a.test/dir/", false, false],
  ]) {
    assert.equal(V.isDownloadWorthy(url, attr), want, url);
  }
});

test("internal messages: only our own popup, only the closed set, strict fields", () => {
  const ok = (m, s = POPUP) => V.validateInternalMessage(m, s, CTX);

  assert.deepEqual(ok({ type: "ping" }), { ok: true, value: { type: "ping" } });
  assert.ok(ok({ type: "getSettings" }).ok);
  assert.deepEqual(ok({ type: "setSettings", settings: { intercept: true } }).value.settings, { intercept: true });
  const h = ok({ type: "handoff", items: [{ url: "https://a.test/a.zip" }], source: "linkList", tabId: 3 });
  assert.ok(h.ok);
  assert.equal(h.value.tabId, 3);

  // Wrong senders: a web page, another extension, our own non-popup pages, nothing.
  for (const sender of [
    { id: "other", url: CTX.popupUrl }, { id: "abc", url: "https://evil.test/" }, { id: "abc", url: "chrome-extension://abc/other.html" },
    { id: "abc" }, { url: CTX.popupUrl }, { id: "abc", url: CTX.popupUrl + "?x=1" }, {}, null, undefined, "abc",
  ]) {
    assert.equal(V.validateInternalMessage({ type: "ping" }, sender, CTX).ok, false, String(JSON.stringify(sender)));
  }

  // Wrong messages.
  for (const m of [
    null, undefined, "ping", 5, [], {}, { type: 5 }, { type: "exec" }, { type: "openTab", url: "https://a.test" }, { type: "ping", extra: 1 },
    { type: "setSettings" }, { type: "setSettings", settings: { intercept: "yes" } }, { type: "setSettings", settings: { evil: true } },
    { type: "handoff", items: [], source: "popup" }, { type: "handoff", items: "x", source: "popup" },
    { type: "handoff", items: [{ url: "javascript:alert(1)" }], source: "popup" },
    { type: "handoff", items: [{ url: "https://a.test/", path: "C:\\x" }], source: "popup" },
    { type: "handoff", items: [{ url: "https://a.test/" }], source: "hackers" },
    { type: "handoff", items: [{ url: "https://a.test/" }], source: "popup", extra: 1 },
    { type: "handoff", items: Array.from({ length: V.MAX_ITEMS + 1 }, () => ({ url: "https://a.test/" })), source: "popup" },
    { type: "__proto__" }, JSON.parse('{"type":"ping","__proto__":{"x":1}}'),
  ]) {
    assert.equal(ok(m).ok, false, JSON.stringify(m));
  }
  assert.ok(ok({ type: "handoff", items: Array.from({ length: V.MAX_ITEMS }, () => ({ url: "https://a.test/" })), source: "popup" }).ok, "the limit itself is allowed");
});

test("error text never echoes hostile input", () => {
  assert.match(V.describeError("app_not_running"), /not running/);
  assert.match(V.describeError("host_missing"), /press Connect/);
  assert.match(V.describeError("host_forbidden"), /Disconnect, then Connect/);
  assert.match(V.describeError("host_exited"), /Restart VeloxFetch/);
  assert.match(V.describeError("host_failed"), /could not start/);
  const evil = "<img src=x onerror=alert(1)>";
  assert.ok(!V.describeError(evil).includes("<"));
  assert.ok(!V.describeError("__proto__").includes("undefined"));
  assert.equal(typeof V.describeError("constructor"), "string");
});
