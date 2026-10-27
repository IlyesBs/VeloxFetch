/*
 * Pure validation helpers for the VeloxFetch extension. No browser APIs in here, so the same file
 * runs in the service worker, the popup and Node's test runner.
 *
 * Rules (mirroring crates/browser-bridge, which validates again on the other side):
 *  - URLs are DATA. Only http/https, no credentials, no control characters, bounded length.
 *    Nothing received is ever evaluated, executed or turned into a command.
 *  - Messages between the extension's own parts are a closed set with strict field lists.
 *  - Header-bound text (cookies, user agent) is printable ASCII only.
 *  - Validation rejects or omits; it never "repairs" hostile input into something acceptable.
 */
(function (root) {
  "use strict";

  var MAX_URL = 8192;
  var MAX_FILENAME = 255;
  var MAX_REFERRER = 2048;
  var MAX_COOKIE = 16 * 1024;
  var MAX_UA = 512;
  var MAX_ITEMS = 50;

  var SOURCES = ["contextMenu", "intercept", "popup", "linkList", "videoBar"];
  /** What a video hand-over is: a stream description, or a video page (absent = an ordinary file). */
  var MEDIA = ["stream", "page"];

  function hasControl(s) {
    for (var i = 0; i < s.length; i++) {
      var c = s.charCodeAt(i);
      if (c < 0x20 || (c >= 0x7f && c <= 0x9f)) return true;
    }
    return false;
  }

  /** A parsed URL for a plain http(s) address, or null. */
  function parseHttpUrl(s, max) {
    if (typeof s !== "string" || s.length === 0 || s.length > (max || MAX_URL) || hasControl(s)) return null;
    var u;
    try {
      u = new URL(s);
    } catch (e) {
      return null;
    }
    if (u.protocol !== "http:" && u.protocol !== "https:") return null;
    if (!u.hostname) return null;
    if (u.username || u.password) return null;
    return u;
  }

  function isPrintableAscii(s) {
    for (var i = 0; i < s.length; i++) {
      var c = s.charCodeAt(i);
      if (c < 0x20 || c > 0x7e) return false;
    }
    return true;
  }

  /** A plain file name or null (the app sanitises again; this only refuses obvious paths). */
  function cleanFilename(s) {
    if (typeof s !== "string" || s.length === 0 || s.length > MAX_FILENAME) return null;
    if (s === "." || s === ".." || hasControl(s)) return null;
    if (/[\\/:*?"<>|]/.test(s)) return null;
    return s;
  }

  /** "a=b; c=d" from cookie objects, or null when nothing usable / too large / not plain ASCII. */
  function cookieHeader(cookies) {
    if (!Array.isArray(cookies) || cookies.length === 0) return null;
    var parts = [];
    for (var i = 0; i < cookies.length; i++) {
      var c = cookies[i];
      if (!c || typeof c.name !== "string" || typeof c.value !== "string") continue;
      var piece = c.name + "=" + c.value;
      // Skip anything that could not travel in a header; never alter it.
      if (!isPrintableAscii(piece) || piece.indexOf(";") !== -1) continue;
      parts.push(piece);
    }
    var header = parts.join("; ");
    if (header.length === 0 || header.length > MAX_COOKIE) return null;
    return header;
  }

  /**
   * The payload for the native host, built from what the browser knows. Optional fields that fail
   * validation are left out (the download still works, just without that hint); a bad URL fails.
   * Cookies are never included for private windows.
   */
  function buildDownloadPayload(input) {
    var url = parseHttpUrl(input && input.url);
    if (!url) return null;
    var source = SOURCES.indexOf(input.source) !== -1 ? input.source : "popup";
    var out = { command: "download", url: url.href, source: source };
    if (MEDIA.indexOf(input.media) !== -1) out.media = input.media;

    var name = input.filename && cleanFilename(input.filename);
    if (name) out.filename = name;

    var ref = parseHttpUrl(input.referrer, MAX_REFERRER);
    if (ref) {
      ref.hash = "";
      out.referrer = ref.href;
    }
    var incognito = input.incognito === true;
    if (incognito) out.incognito = true;

    if (!incognito && typeof input.cookies === "string" && input.cookies.length > 0 && input.cookies.length <= MAX_COOKIE && isPrintableAscii(input.cookies)) {
      out.cookies = input.cookies;
    }
    if (typeof input.userAgent === "string" && input.userAgent.length > 0 && input.userAgent.length <= MAX_UA && isPrintableAscii(input.userAgent)) {
      out.userAgent = input.userAgent;
    }
    return out;
  }

  var EXTENSIONS = [
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "iso", "img", "dmg", "pkg", "exe", "msi", "msix", "appx", "deb", "rpm", "apk",
    "appimage", "bin", "jar", "pdf", "epub", "mobi", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "csv", "mp3", "flac", "wav", "aac", "ogg",
    "m4a", "mp4", "mkv", "avi", "mov", "webm", "wmv", "flv", "m4v", "torrent"
  ];

  /** Does this link look like a file download (by extension or the `download` attribute)? */
  function isDownloadWorthy(url, hasDownloadAttr) {
    var u = parseHttpUrl(url);
    if (!u) return false;
    if (hasDownloadAttr) return true;
    var m = /\.([a-z0-9]{1,8})$/i.exec(u.pathname);
    return !!m && EXTENSIONS.indexOf(m[1].toLowerCase()) !== -1;
  }

  // ------------------------------------------------------------------ internal messages

  function isPlainObject(v) {
    return v !== null && typeof v === "object" && !Array.isArray(v);
  }

  function onlyKeys(obj, allowed) {
    var keys = Object.keys(obj);
    for (var i = 0; i < keys.length; i++) if (allowed.indexOf(keys[i]) === -1) return false;
    return true;
  }

  /**
   * Validate a message sent to the background script by another part of this extension.
   * `ctx` = { runtimeId, popupUrl }. Returns { ok: true, value } or { ok: false, error }.
   *
   * The sender must be this very extension (never a web page or another extension) and, for
   * anything that acts, the popup page itself.
   */
  function validateInternalMessage(msg, sender, ctx) {
    if (!isPlainObject(sender) || sender.id !== ctx.runtimeId) return { ok: false, error: "untrusted sender" };
    if (!isPlainObject(msg) || typeof msg.type !== "string") return { ok: false, error: "malformed message" };
    var fromPopup = typeof sender.url === "string" && sender.url === ctx.popupUrl;
    if (!fromPopup) return { ok: false, error: "untrusted sender" };

    switch (msg.type) {
      case "ping":
      case "getSettings":
        if (!onlyKeys(msg, ["type"])) return { ok: false, error: "unexpected fields" };
        return { ok: true, value: { type: msg.type } };
      case "setSettings": {
        if (!onlyKeys(msg, ["type", "settings"]) || !isPlainObject(msg.settings)) return { ok: false, error: "unexpected fields" };
        if (!onlyKeys(msg.settings, ["intercept", "sendCookies", "detectVideos"])) return { ok: false, error: "unexpected fields" };
        var s = {};
        var names = ["intercept", "sendCookies", "detectVideos"];
        for (var i = 0; i < names.length; i++) {
          if (msg.settings[names[i]] !== undefined) {
            if (typeof msg.settings[names[i]] !== "boolean") return { ok: false, error: "settings must be booleans" };
            s[names[i]] = msg.settings[names[i]];
          }
        }
        return { ok: true, value: { type: "setSettings", settings: s } };
      }
      case "getMedia":
      case "downloadMedia": {
        var fields = msg.type === "getMedia" ? ["type", "tabId"] : ["type", "tabId", "itemId"];
        if (!onlyKeys(msg, fields) || !Number.isInteger(msg.tabId) || msg.tabId < 0) return { ok: false, error: "unexpected fields" };
        if (msg.type === "getMedia") return { ok: true, value: { type: "getMedia", tabId: msg.tabId } };
        if (typeof msg.itemId !== "string" || !/^(page|m\d{1,6})$/.test(msg.itemId)) return { ok: false, error: "unknown item" };
        return { ok: true, value: { type: "downloadMedia", tabId: msg.tabId, itemId: msg.itemId } };
      }
      case "handoff": {
        if (!onlyKeys(msg, ["type", "items", "source", "tabId"])) return { ok: false, error: "unexpected fields" };
        if (!Array.isArray(msg.items) || msg.items.length === 0 || msg.items.length > MAX_ITEMS) return { ok: false, error: "1 to " + MAX_ITEMS + " items expected" };
        var src = SOURCES.indexOf(msg.source) !== -1 ? msg.source : null;
        if (!src) return { ok: false, error: "unknown source" };
        var items = [];
        for (var j = 0; j < msg.items.length; j++) {
          var it = msg.items[j];
          if (!isPlainObject(it) || !onlyKeys(it, ["url", "filename"])) return { ok: false, error: "malformed item" };
          if (!parseHttpUrl(it.url)) return { ok: false, error: "only http and https links can be downloaded" };
          items.push({ url: it.url, filename: typeof it.filename === "string" ? it.filename : undefined });
        }
        var tabId = Number.isInteger(msg.tabId) ? msg.tabId : undefined;
        return { ok: true, value: { type: "handoff", items: items, source: src, tabId: tabId } };
      }
      default:
        return { ok: false, error: "unknown message type" };
    }
  }

  var ERROR_TEXT = {
    host_missing: "This browser is not connected to VeloxFetch yet. Open VeloxFetch, go to Settings > Browser integration and press Connect.",
    host_forbidden: "This browser is not allowed to use the VeloxFetch helper. In VeloxFetch > Settings > Browser integration press Disconnect, then Connect again.",
    host_exited: "The VeloxFetch helper stopped unexpectedly. Restart VeloxFetch and try again.",
    host_failed: "The VeloxFetch helper program could not start. Open VeloxFetch (it repairs the connection when it starts), then try again. If it still fails, press Connect again in Settings > Browser integration.",
    app_not_running: "VeloxFetch is not running and could not be started.",
    auth_failed: "VeloxFetch could not verify this browser. Restart VeloxFetch and try again.",
    link_error: "The connection to VeloxFetch was lost. Try again.",
    invalid: "That link cannot be downloaded.",
    invalid_input: "VeloxFetch refused that link.",
    bad_fields: "That request was not understood.",
    too_large: "That request was too large.",
    rate_limited: "Too many requests. Wait a moment and try again.",
    internal: "VeloxFetch could not add the download."
  };

  /** A short, user-facing explanation. Never echoes hostile text from the reply. */
  function describeError(code) {
    return Object.prototype.hasOwnProperty.call(ERROR_TEXT, code) ? ERROR_TEXT[code] : "Something went wrong talking to VeloxFetch.";
  }

  /**
   * Why the browser could not reach the helper, from its own error text. Chromium: "Specified native messaging host
   * not found.", "Access to ... is forbidden.", "Native host has exited.", "Failed to start native messaging host.".
   * Firefox: "No such native application app.velox.fetch" (not connected), "An unexpected error occurred".
   */
  function hostErrorCode(message) {
    var m = String(message || "");
    if (/not found|no such native application/i.test(m)) return "host_missing";
    if (/forbidden/i.test(m)) return "host_forbidden";
    if (/exited|disconnected port/i.test(m)) return "host_exited";
    // Anything else ("Error when communicating with the native messaging host", ...) means the helper program did
    // not start or did not speak: a broken install, not a dropped connection.
    return "host_failed";
  }

  var api = {
    MAX_ITEMS: MAX_ITEMS,
    MAX_URL: MAX_URL,
    MAX_COOKIE: MAX_COOKIE,
    parseHttpUrl: parseHttpUrl,
    cleanFilename: cleanFilename,
    cookieHeader: cookieHeader,
    buildDownloadPayload: buildDownloadPayload,
    isDownloadWorthy: isDownloadWorthy,
    validateInternalMessage: validateInternalMessage,
    describeError: describeError,
    hostErrorCode: hostErrorCode
  };

  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.VeloxValidate = api;
})(typeof self !== "undefined" ? self : this);
