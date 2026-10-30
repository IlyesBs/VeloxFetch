/*
 * Video detection for the VeloxFetch extension. Pure functions: no browser APIs in here, so the same
 * file runs in the background script and in Node's test runner.
 *
 * What the browser itself fetched is the only source of video links (webRequest response headers):
 * a page cannot hand us a link to send, it can only play videos, which the browser then loads.
 *  - streams: HLS (.m3u8) and DASH (.mpd) descriptions -> the app's video dialog;
 *  - files: real video / audio files the page plays directly (not the small pieces a player
 *    stitches together, which are fetched by scripts, not by the media element);
 *  - video pages of known sites (YouTube...), read by the app through yt-dlp;
 *  - services that protect every video with DRM are recognised and named, never downloaded.
 * The two site lists mirror `VIDEO_SITES` / `PROTECTED_SITES` in crates/media-engine/src/site.rs
 * (a test compares them).
 */
(function (root) {
  "use strict";

  var MAX_ITEMS = 20;
  /** A video file smaller than this is a preview, an ad pixel or a piece of a stream. */
  var MIN_FILE_BYTES = 512 * 1024;

  var VIDEO_SITES = [
    ["youtube.com", ["/watch", "/shorts/", "/embed/", "/live/"]],
    ["youtu.be", []],
    ["youtube-nocookie.com", ["/embed/"]],
    ["vimeo.com", []],
    ["dailymotion.com", ["/video/"]],
    ["dai.ly", []],
    ["twitch.tv", ["/videos/", "/clip/"]],
    ["tiktok.com", ["/video/"]],
    ["x.com", ["/status/"]],
    ["twitter.com", ["/status/"]],
    ["facebook.com", ["/watch", "/videos/", "/reel/"]],
    ["fb.watch", []],
    ["instagram.com", ["/p/", "/reel/", "/tv/"]],
    ["reddit.com", ["/comments/"]],
    ["streamable.com", []],
    ["rumble.com", []],
    ["bilibili.com", ["/video/"]],
    ["bilibili.tv", ["/video/", "/play/"]],
    ["soundcloud.com", []]
  ];

  var PROTECTED_SITES = [
    ["netflix.com", "Netflix"],
    ["shahid.mbc.net", "Shahid"],
    ["shahid.net", "Shahid"],
    ["stctv.com", "STC tv"],
    ["jawwy.tv", "STC tv"],
    ["primevideo.com", "Prime Video"],
    ["disneyplus.com", "Disney+"],
    ["osnplus.com", "OSN+"],
    ["hulu.com", "Hulu"],
    ["max.com", "Max"],
    ["hbomax.com", "Max"],
    ["tv.apple.com", "Apple TV+"],
    ["starzplay.com", "STARZPLAY"],
    ["tod.tv", "TOD"],
    ["paramountplus.com", "Paramount+"],
    ["peacocktv.com", "Peacock"],
    ["crunchyroll.com", "Crunchyroll"],
    ["watchit.com", "WATCH IT"],
    ["spotify.com", "Spotify"]
  ];

  /** Hosts whose media requests are pieces of a page video (handled as the page instead). */
  var PIECE_HOSTS = ["googlevideo.com", "ytimg.com"];

  var HLS_TYPES = ["application/vnd.apple.mpegurl", "application/x-mpegurl", "audio/mpegurl", "audio/x-mpegurl"];
  var DASH_TYPES = ["application/dash+xml"];
  var PIECE_TYPES = ["video/mp2t", "video/iso.segment", "audio/mp2t", "video/vnd.dlna.mpeg-tts"];
  var PIECE_EXTS = ["ts", "m4s", "m4f", "m4v_seg", "cmfv", "cmfa", "aac", "vtt", "key", "init"];
  var FILE_EXTS = ["mp4", "webm", "mkv", "mov", "m4v", "flv", "avi", "ogv", "mp3", "m4a", "ogg", "oga", "opus", "wav", "flac"];

  function hostMatches(host, suffix) {
    return host === suffix || (host.length > suffix.length && host.slice(-suffix.length - 1) === "." + suffix);
  }

  function parse(url) {
    if (typeof url !== "string" || url.length === 0 || url.length > 8192) return null;
    var u;
    try {
      u = new URL(url);
    } catch (e) {
      return null;
    }
    if (u.protocol !== "http:" && u.protocol !== "https:") return null;
    if (!u.hostname || u.username || u.password) return null;
    return u;
  }

  /** The name of a DRM-only streaming service, or null. */
  function protectedService(url) {
    var u = parse(url);
    if (!u) return null;
    var host = u.hostname.toLowerCase();
    if (hostMatches(host, "amazon.com") && u.pathname.indexOf("/gp/video") === 0) return "Prime Video";
    for (var i = 0; i < PROTECTED_SITES.length; i++) if (hostMatches(host, PROTECTED_SITES[i][0])) return PROTECTED_SITES[i][1];
    return null;
  }

  /** The site's short name when `url` is a video page the app reads through yt-dlp, else null. */
  function videoPageSite(url) {
    var u = parse(url);
    if (!u) return null;
    var host = u.hostname.toLowerCase();
    for (var i = 0; i < VIDEO_SITES.length; i++) {
      var h = VIDEO_SITES[i][0];
      var paths = VIDEO_SITES[i][1];
      if (!hostMatches(host, h)) continue;
      var ok = paths.length === 0 ? u.pathname.length > 1 : paths.some(function (p) {
        return u.pathname.indexOf(p) !== -1;
      });
      if (ok) return h;
    }
    return null;
  }

  function header(headers, name) {
    if (!Array.isArray(headers)) return null;
    for (var i = 0; i < headers.length; i++) {
      var h = headers[i];
      if (h && typeof h.name === "string" && h.name.toLowerCase() === name && typeof h.value === "string") return h.value;
    }
    return null;
  }

  /** Total size from `Content-Range: bytes a-b/total`, else `Content-Length` (only for a whole answer). */
  function sizeOf(headers, status) {
    var range = header(headers, "content-range");
    var m = range && /\/(\d{1,15})\s*$/.exec(range);
    if (m) return Number(m[1]);
    var len = header(headers, "content-length");
    if (status === 200 && len && /^\d{1,15}$/.test(len.trim())) return Number(len.trim());
    return null;
  }

  function extOf(u) {
    var last = u.pathname.split("/").pop() || "";
    var m = /\.([a-z0-9_]{1,8})$/i.exec(last);
    return m ? m[1].toLowerCase() : "";
  }

  function nameOf(u) {
    var last = u.pathname.split("/").pop() || "";
    try {
      last = decodeURIComponent(last);
    } catch (e) {
      /* keep it encoded */
    }
    last = last.replace(/[\u0000-\u001f\u007f-\u009f]/g, "").slice(0, 80);
    return last || u.hostname;
  }

  function human(bytes) {
    if (typeof bytes !== "number" || !isFinite(bytes) || bytes <= 0) return "";
    var mb = bytes / (1024 * 1024);
    return mb >= 1024 ? (mb / 1024).toFixed(1) + " GB" : mb >= 10 ? Math.round(mb) + " MB" : mb.toFixed(1) + " MB";
  }

  /**
   * What a response is, from what the browser saw: `{ kind: "stream" | "file", url, label, size }` or
   * null. `d` is a webRequest `onHeadersReceived` detail (url, type, statusCode, tabId, responseHeaders).
   */
  function classifyResponse(d) {
    if (!d || typeof d !== "object" || !(d.tabId >= 0)) return null;
    if (d.statusCode !== 200 && d.statusCode !== 206) return null;
    var u = parse(d.url);
    if (!u) return null;
    var host = u.hostname.toLowerCase();
    if (PIECE_HOSTS.some(function (h) { return hostMatches(host, h); })) return null;
    var ct = (header(d.responseHeaders, "content-type") || "").split(";")[0].trim().toLowerCase();
    var ext = extOf(u);
    if (HLS_TYPES.indexOf(ct) !== -1 || ext === "m3u8") {
      return { kind: "stream", url: u.href, label: "HLS stream · " + nameOf(u), size: null };
    }
    if (DASH_TYPES.indexOf(ct) !== -1 || ext === "mpd") {
      return { kind: "stream", url: u.href, label: "DASH stream · " + nameOf(u), size: null };
    }
    if (PIECE_TYPES.indexOf(ct) !== -1 || PIECE_EXTS.indexOf(ext) !== -1) return null;
    var media = ct.indexOf("video/") === 0 || ct.indexOf("audio/") === 0;
    if (!media && FILE_EXTS.indexOf(ext) === -1) return null;
    if (ct && !media && ct !== "application/octet-stream" && ct !== "binary/octet-stream") return null;
    // Players that stitch pieces together fetch them with scripts; a real file is loaded by the
    // <video> / <audio> element itself ("media").
    if (d.type !== "media") return null;
    var size = sizeOf(d.responseHeaders, d.statusCode);
    if (size !== null && size < MIN_FILE_BYTES) return null;
    var audio = ct.indexOf("audio/") === 0 || ["mp3", "m4a", "oga", "opus", "wav", "flac"].indexOf(ext) !== -1;
    var label = (audio ? "Audio · " : "Video · ") + nameOf(u) + (size ? " · " + human(size) : "");
    return { kind: "file", url: u.href, label: label, size: size };
  }

  /**
   * Add a detected item to a tab's list (mutates `list`, returns true when it changed). Duplicates are
   * dropped, a page keeps one stream description per host (the first one: the master playlist; the
   * qualities it lists are offered by the app), and the list is bounded.
   */
  function addItem(list, item, nextId) {
    if (!Array.isArray(list) || !item) return false;
    for (var i = 0; i < list.length; i++) {
      var x = list[i];
      if (x.url === item.url) return false;
      if (item.kind === "stream" && x.kind === "stream") {
        var a = parse(x.url);
        var b = parse(item.url);
        if (a && b && a.host === b.host) return false;
      }
      if (item.kind === "file" && x.kind === "file") {
        var p = parse(x.url);
        var q = parse(item.url);
        if (p && q && p.host === q.host && p.pathname === q.pathname) return false;
      }
    }
    if (list.length >= MAX_ITEMS) return false;
    list.push({ id: "m" + nextId, kind: item.kind, url: item.url, label: String(item.label).slice(0, 140), size: item.size });
    return true;
  }

  /** What the bar may know about an item: never its link. */
  function publicItem(item) {
    return { id: item.id, kind: item.kind, label: item.label, size: typeof item.size === "number" ? item.size : null };
  }

  /**
   * A message from the video bar (content script). The sender must be this extension running in a tab's
   * top frame of an ordinary web page. The bar can only ask "what is there?" and "download item X":
   * it never supplies a link.
   */
  function validateBarMessage(msg, sender, runtimeId) {
    if (!sender || typeof sender !== "object" || sender.id !== runtimeId) return { ok: false, error: "untrusted sender" };
    if (!sender.tab || !Number.isInteger(sender.tab.id) || sender.tab.id < 0 || sender.frameId !== 0) return { ok: false, error: "untrusted sender" };
    if (!parse(sender.url)) return { ok: false, error: "untrusted sender" };
    if (!msg || typeof msg !== "object" || Array.isArray(msg)) return { ok: false, error: "malformed message" };
    var keys = Object.keys(msg);
    if (msg.type === "bar:state") {
      if (keys.length !== 1) return { ok: false, error: "unexpected fields" };
      return { ok: true, value: { type: "bar:state", tabId: sender.tab.id } };
    }
    if (msg.type === "bar:download") {
      if (keys.length !== 2 || keys.indexOf("itemId") === -1) return { ok: false, error: "unexpected fields" };
      if (typeof msg.itemId !== "string" || !/^(page|m\d{1,6})$/.test(msg.itemId)) return { ok: false, error: "unknown item" };
      return { ok: true, value: { type: "bar:download", tabId: sender.tab.id, itemId: msg.itemId } };
    }
    return { ok: false, error: "unknown message type" };
  }

  var api = {
    MAX_ITEMS: MAX_ITEMS,
    MIN_FILE_BYTES: MIN_FILE_BYTES,
    VIDEO_SITES: VIDEO_SITES,
    PROTECTED_SITES: PROTECTED_SITES,
    protectedService: protectedService,
    videoPageSite: videoPageSite,
    classifyResponse: classifyResponse,
    addItem: addItem,
    publicItem: publicItem,
    human: human,
    validateBarMessage: validateBarMessage
  };

  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.VeloxMedia = api;
})(typeof self !== "undefined" ? self : this);
