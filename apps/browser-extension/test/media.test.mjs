import assert from "node:assert/strict";
import test from "node:test";
import M from "../src/lib/media.js";
import V from "../src/lib/validate.js";

const h = (pairs) => Object.entries(pairs).map(([name, value]) => ({ name, value }));
const resp = (over) => ({ tabId: 3, statusCode: 200, type: "media", url: "https://cdn.test/v/movie.mp4", responseHeaders: h({ "Content-Type": "video/mp4", "Content-Length": "50000000" }), ...over });

test("stream descriptions are detected by type or by name", () => {
  for (const r of [
    resp({ url: "https://cdn.test/a/master.m3u8?token=1", type: "xmlhttprequest", responseHeaders: h({ "content-type": "application/vnd.apple.mpegurl" }) }),
    resp({ url: "https://cdn.test/a/index", type: "xmlhttprequest", responseHeaders: h({ "Content-Type": "application/x-mpegURL; charset=utf-8" }) }),
    resp({ url: "https://cdn.test/a/manifest.mpd", type: "other", responseHeaders: h({ "content-type": "text/plain" }) }),
    resp({ url: "https://cdn.test/a/x", type: "xmlhttprequest", responseHeaders: h({ "content-type": "application/dash+xml" }) }),
  ]) {
    assert.equal(M.classifyResponse(r)?.kind, "stream", r.url);
  }
});

test("a real video file played by the page is detected, with its total size", () => {
  const f = M.classifyResponse(resp({ statusCode: 206, responseHeaders: h({ "content-type": "video/mp4", "content-range": "bytes 0-1023/734003200", "content-length": "1024" }) }));
  assert.equal(f.kind, "file");
  assert.equal(f.size, 734003200);
  assert.match(f.label, /^Video · movie\.mp4 · 700 MB$/);
  assert.equal(M.classifyResponse(resp({ url: "https://cdn.test/s/song.mp3", responseHeaders: h({ "content-type": "audio/mpeg", "content-length": "5000000" }) })).label.slice(0, 5), "Audio");
});

test("pieces, previews, failures and things that are not video are ignored", () => {
  for (const r of [
    resp({ type: "xmlhttprequest" }), // a player stitching pieces together fetches them with scripts
    resp({ url: "https://cdn.test/seg/12.ts", responseHeaders: h({ "content-type": "video/mp2t", "content-length": "2000000" }) }),
    resp({ url: "https://cdn.test/seg/12.m4s", responseHeaders: h({ "content-type": "video/iso.segment" }) }),
    resp({ responseHeaders: h({ "content-type": "video/mp4", "content-length": "100000" }) }), // a preview / ad
    resp({ statusCode: 404 }),
    resp({ statusCode: 302 }),
    resp({ tabId: -1 }), // not from a tab (the extension itself, a service worker)
    resp({ url: "https://rr1---sn-x.googlevideo.com/videoplayback?itag=22", responseHeaders: h({ "content-type": "video/mp4", "content-length": "90000000" }) }),
    resp({ url: "https://cdn.test/page.html", responseHeaders: h({ "content-type": "text/html" }) }),
    resp({ url: "https://cdn.test/x.mp4", responseHeaders: h({ "content-type": "text/html" }) }), // an error page named .mp4
    resp({ url: "javascript:alert(1)" }),
    resp({ url: "https://u:p@cdn.test/a.mp4" }),
    resp({ url: "file:///C:/a.mp4" }),
    null,
    {},
    "x",
  ]) {
    assert.equal(M.classifyResponse(r), null, JSON.stringify(r));
  }
});

test("a tab keeps one stream per host (the master playlist), no duplicates, and a bounded list", () => {
  const list = [];
  let id = 1;
  const add = (it) => M.addItem(list, it, id) && ++id;
  add({ kind: "stream", url: "https://cdn.test/v/master.m3u8", label: "HLS" });
  add({ kind: "stream", url: "https://cdn.test/v/720p.m3u8", label: "HLS" }); // a quality of the same video
  add({ kind: "stream", url: "https://other.test/v/master.m3u8", label: "HLS" });
  add({ kind: "file", url: "https://cdn.test/a.mp4?t=1", label: "f" });
  add({ kind: "file", url: "https://cdn.test/a.mp4?t=2", label: "f" }); // the same file asked for again
  assert.deepEqual(list.map((x) => x.url), ["https://cdn.test/v/master.m3u8", "https://other.test/v/master.m3u8", "https://cdn.test/a.mp4?t=1"]);
  assert.deepEqual(list.map((x) => x.id), ["m1", "m2", "m3"]);
  for (let i = 0; i < 100; i++) add({ kind: "file", url: `https://h${i}.test/a.mp4`, label: "f" });
  assert.equal(list.length, M.MAX_ITEMS);
  assert.deepEqual(Object.keys(M.publicItem(list[0])).sort(), ["id", "kind", "label", "size"], "the bar never learns a link");
});

test("video pages and DRM services are told apart", () => {
  assert.equal(M.videoPageSite("https://www.youtube.com/watch?v=abc"), "youtube.com");
  assert.equal(M.videoPageSite("https://www.youtube.com/"), null);
  assert.equal(M.videoPageSite("https://youtube.com.evil.test/watch?v=1"), null);
  assert.equal(M.protectedService("https://www.netflix.com/watch/81"), "Netflix");
  assert.equal(M.protectedService("https://shahid.mbc.net/ar/player/x"), "Shahid");
  assert.equal(M.protectedService("https://www.stctv.com/"), "STC tv");
  assert.equal(M.protectedService("https://www.amazon.com/gp/video/detail/x"), "Prime Video");
  assert.equal(M.protectedService("https://www.amazon.com/dp/x"), null);
  assert.equal(M.protectedService("https://notnetflix.com/"), null);
});

test("the bar's messages: our own script, a tab's top frame, two message types, item ids only", () => {
  const sender = { id: "ext", tab: { id: 7 }, frameId: 0, url: "https://site.test/watch" };
  assert.deepEqual(M.validateBarMessage({ type: "bar:state" }, sender, "ext"), { ok: true, value: { type: "bar:state", tabId: 7 } });
  assert.deepEqual(M.validateBarMessage({ type: "bar:download", itemId: "m3" }, sender, "ext").value, { type: "bar:download", tabId: 7, itemId: "m3" });
  assert.equal(M.validateBarMessage({ type: "bar:download", itemId: "page" }, sender, "ext").ok, true);
  for (const [msg, s] of [
    [{ type: "bar:state" }, { ...sender, id: "other-extension" }],
    [{ type: "bar:state" }, { ...sender, frameId: 2 }], // an iframe
    [{ type: "bar:state" }, { ...sender, tab: undefined }],
    [{ type: "bar:state" }, { ...sender, url: "chrome://settings" }],
    [{ type: "bar:state", url: "https://evil.test/x" }, sender],
    [{ type: "bar:download", itemId: "m1", url: "https://evil.test/x.exe" }, sender],
    [{ type: "bar:download", itemId: "https://evil.test/x.exe" }, sender],
    [{ type: "bar:download", itemId: 1 }, sender],
    [{ type: "handoff", items: [{ url: "https://evil.test/x" }] }, sender],
    [{ type: "setSettings", settings: { intercept: false } }, sender],
    [null, sender],
    [[], sender],
  ]) {
    assert.equal(M.validateBarMessage(msg, s, "ext").ok, false, JSON.stringify(msg));
  }
});

test("the popup's video messages and the video hint in the payload", () => {
  const ctx = { runtimeId: "abc", popupUrl: "chrome-extension://abc/popup.html" };
  const popup = { id: "abc", url: ctx.popupUrl };
  assert.equal(V.validateInternalMessage({ type: "getMedia", tabId: 4 }, popup, ctx).ok, true);
  assert.equal(V.validateInternalMessage({ type: "downloadMedia", tabId: 4, itemId: "m2" }, popup, ctx).ok, true);
  assert.equal(V.validateInternalMessage({ type: "downloadMedia", tabId: 4, itemId: "https://x" }, popup, ctx).ok, false);
  assert.equal(V.validateInternalMessage({ type: "getMedia", tabId: -1 }, popup, ctx).ok, false);
  assert.equal(V.validateInternalMessage({ type: "getMedia", tabId: 4 }, { id: "abc", url: "https://evil.test/" }, ctx).ok, false);
  assert.deepEqual(V.validateInternalMessage({ type: "setSettings", settings: { detectVideos: true } }, popup, ctx).value.settings, { detectVideos: true });
  assert.equal(V.buildDownloadPayload({ url: "https://www.youtube.com/watch?v=1", source: "videoBar", media: "page" }).media, "page");
  assert.equal(V.buildDownloadPayload({ url: "https://cdn.test/m.m3u8", source: "videoBar", media: "stream" }).source, "videoBar");
  assert.equal(V.buildDownloadPayload({ url: "https://cdn.test/a.mp4", source: "videoBar", media: "exec" }).media, undefined, "unknown hints are dropped");
});
