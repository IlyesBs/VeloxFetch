/*
 * VeloxFetch extension: background script (service worker in Chromium, event page in Firefox).
 *
 * Does exactly four things:
 *   1. "Download with VeloxFetch" in the right-click menu for links, images, video and audio;
 *   2. takes over the browser's own downloads (on by default, can be turned off in the popup): the
 *      browser's copy is dropped at once and VeloxFetch asks the user; if VeloxFetch cannot take the
 *      link, the browser downloads it again itself;
 *   3. serves the popup (status, settings, hand over several links);
 *   4. video detection (on by default, like other download managers; can be turned off in the popup):
 *      notes the videos the browser loads in each tab and runs the "Download video" bar (videobar.js).
 * Every hand-over goes through the native messaging host `app.velox.fetch`; there is no other
 * channel out of the extension and nothing here is ever evaluated or executed.
 *
 * Trust rules:
 *   - Messages from other parts of the extension are validated (sender must be our own popup page,
 *     closed message set, strict fields). Web pages cannot message us: there is no
 *     `externally_connectable` and no `onMessageExternal` listener. The video bar (our own content
 *     script, registered only while detection is on) can only ask what was detected in its own tab
 *     and pick one of those items by id: it never supplies a link.
 *   - URLs are data: only http(s), no credentials; everything is re-validated by the host and the app.
 *   - Cookies are only read when the user turned that setting on, only for the download's own URL,
 *     only after the browser granted us access to that site, and never from a private window.
 */
"use strict";

if (typeof importScripts === "function") importScripts("lib/validate.js", "lib/media.js");

var V = self.VeloxValidate;
var M = self.VeloxMedia;
var ext = typeof browser !== "undefined" ? browser : chrome;
var HOST = "app.velox.fetch";
var DEFAULTS = { intercept: true, sendCookies: false, detectVideos: true, appVideoBar: true };

function getSettings() {
  return ext.storage.local.get(["intercept", "sendCookies", "detectVideos", "appVideoBar"]).then(function (s) {
    return {
      intercept: s.intercept !== false,
      sendCookies: s.sendCookies === true,
      detectVideos: s.detectVideos !== false,
      // The switch in VeloxFetch's own settings (learnt from its answer to a ping; on until it says otherwise).
      appVideoBar: s.appVideoBar !== false
    };
  }, function () {
    return Object.assign({}, DEFAULTS);
  });
}

function setSettings(patch) {
  return ext.storage.local.set(patch).then(syncDetection).then(getSettings);
}

/** One request to the native host. Always resolves to { ok, code?, id?, filename? }. */
function nativeCall(payload) {
  return ext.runtime.sendNativeMessage(HOST, payload).then(
    function (reply) {
      if (!reply || typeof reply !== "object" || typeof reply.ok !== "boolean") return { ok: false, code: "link_error" };
      if (reply.ok) {
        var r = reply.result || {};
        return {
          ok: true,
          id: typeof r.id === "number" ? r.id : undefined,
          filename: typeof r.filename === "string" ? r.filename.slice(0, 255) : undefined,
          version: typeof r.version === "string" ? r.version.slice(0, 32) : undefined,
          videoBar: typeof r.videoBar === "boolean" ? r.videoBar : undefined
        };
      }
      return { ok: false, code: reply.error && typeof reply.error.code === "string" ? reply.error.code : "link_error" };
    },
    function (err) {
      // The browser reports why it could not reach the host; tell the cases apart, because the
      // remedy differs (connect / reconnect / restart).
      var m = String((err && err.message) || "");
      // Anything else ("Error when communicating with the native messaging host", ...) means the helper
      // program did not start or did not speak: a broken install, not a dropped connection.
      return { ok: false, code: V.hostErrorCode(m), detail: m.slice(0, 120) };
    }
  );
}

function explain(code) {
  return V.describeError(code);
}

function notify(message) {
  if (!ext.notifications) return;
  try {
    ext.notifications.create({ type: "basic", iconUrl: "icons/128.png", title: "VeloxFetch", message: String(message).slice(0, 200) });
  } catch (e) {
    /* notifications are a nicety */
  }
}

function originPattern(url) {
  var u = V.parseHttpUrl(url);
  return u ? u.origin + "/*" : null;
}

/** The Cookie header for `url`, or null: setting off, private window, no access to the site, none. */
function cookiesFor(url, settings, incognito) {
  if (!settings.sendCookies || incognito || !ext.cookies) return Promise.resolve(null);
  var pattern = originPattern(url);
  if (!pattern) return Promise.resolve(null);
  return ext.permissions
    .contains({ origins: [pattern] })
    .then(function (granted) {
      return granted ? ext.cookies.getAll({ url: url }) : [];
    })
    .then(V.cookieHeader, function () {
      return null;
    });
}

/** Ask (from a user gesture) for access to the sites we are about to read cookies for. */
function requestSiteAccess(urls, settings) {
  if (!settings.sendCookies || !ext.permissions) return Promise.resolve();
  var seen = {};
  var origins = [];
  urls.forEach(function (u) {
    var p = originPattern(u);
    if (p && !seen[p]) {
      seen[p] = true;
      origins.push(p);
    }
  });
  if (origins.length === 0) return Promise.resolve();
  return ext.permissions.contains({ origins: origins }).then(function (has) {
    return has ? undefined : ext.permissions.request({ origins: origins }).then(function () {}, function () {});
  }, function () {});
}

/** Hand `items` ([{url, filename?}]) over. Resolves to [{url, ok, ...}]. */
function handoff(items, source, context) {
  return getSettings().then(function (settings) {
    var one = function (item) {
      return cookiesFor(item.url, settings, context.incognito).then(function (cookies) {
        var payload = V.buildDownloadPayload({
          url: item.url,
          filename: item.filename,
          media: item.media,
          referrer: context.referrer,
          cookies: cookies,
          userAgent: navigator.userAgent,
          incognito: context.incognito,
          source: source
        });
        if (!payload) return { url: item.url, ok: false, code: "invalid" };
        return nativeCall(payload).then(function (r) {
          r.url = item.url;
          return r;
        });
      });
    };
    // Sequential: each message starts (and ends) a host process, and the app rate-limits bursts.
    return items.reduce(function (chain, item) {
      return chain.then(function (acc) {
        return one(item).then(function (r) {
          acc.push(r);
          return acc;
        });
      });
    }, Promise.resolve([]));
  });
}

// ---------------------------------------------------------------------------- context menu

function createMenu() {
  ext.contextMenus.removeAll(function () {
    ext.contextMenus.create({
      id: "velox-download",
      title: "Download with VeloxFetch",
      contexts: ["link", "image", "video", "audio"]
    });
  });
}
ext.runtime.onInstalled.addListener(createMenu);
if (ext.runtime.onStartup) ext.runtime.onStartup.addListener(createMenu);

ext.contextMenus.onClicked.addListener(function (info, tab) {
  if (info.menuItemId !== "velox-download") return;
  var url = info.linkUrl || info.srcUrl;
  if (!V.parseHttpUrl(url)) {
    notify("Only http and https links can be downloaded.");
    return;
  }
  var context = { referrer: info.frameUrl || info.pageUrl || (tab && tab.url), incognito: !!(tab && tab.incognito) };
  getSettings()
    .then(function (s) {
      return requestSiteAccess([url], s);
    })
    .then(function () {
      return handoff([{ url: url }], "contextMenu", context);
    })
    .then(function (results) {
      var r = results[0];
      notify(r.ok ? (r.id === 0 ? "Sent to VeloxFetch: confirm the download in the app" : "Added to VeloxFetch") + (r.filename ? ": " + r.filename : "") : explain(r.code));
    });
});

// ---------------------------------------------------------------------------- optional takeover

// URLs we re-started in the browser ourselves after VeloxFetch could not take them (never taken over twice).
var restarted = {};

/** Remove the browser's own copy: cancel it, delete a file that already finished, forget the entry. */
function dropBrowserCopy(id) {
  var ignore = function () {};
  return ext.downloads.cancel(id).then(ignore, ignore).then(function () {
    return ext.downloads.removeFile ? ext.downloads.removeFile(id).then(ignore, ignore) : undefined;
  }).then(function () {
    return ext.downloads.erase({ id: id });
  }).then(ignore, ignore);
}

/** Is this browser download one we should take over? (Settings on, http(s), not started by an extension.) */
function shouldTake(item, settings) {
  if (!settings.intercept) return false;
  if (item.byExtensionId) return false; // started by an extension, ours included
  var url = item.finalUrl || item.url;
  if (!V.parseHttpUrl(url)) return false; // blob:, data:, file: ... stay with the browser
  if (restarted[url] && Date.now() - restarted[url] < 60000) return false;
  return true;
}

var taken = {}; // browser download ids already handled (both events can fire for one download)

/**
 * The file name the browser chose (from the server's answer, with the user's sign-in), without any folder:
 * Firefox gives a full path. A site like Google Drive only names the file for a signed-in browser.
 */
function browserFilename(item) {
  var name = typeof item.filename === "string" ? item.filename.split(/[\\/]/).pop() : "";
  return name || undefined;
}

/**
 * Take over one browser download: drop the browser's copy, then hand the link to VeloxFetch. Resolves as
 * soon as the browser's copy is gone (the hand-over continues in the background).
 */
function takeOver(item) {
  if (taken[item.id]) return Promise.resolve();
  taken[item.id] = true;
  var url = item.finalUrl || item.url;
  var dropped = dropBrowserCopy(item.id);
  handoff([{ url: url, filename: browserFilename(item) }], "intercept", { referrer: item.referrer, incognito: !!item.incognito }).then(function (results) {
    var r = results[0];
    if (r.ok) return;
    // VeloxFetch could not take it: give it back to the browser so the download is not lost.
    notify(explain(r.code) + " The browser will download it instead.");
    restarted[url] = Date.now();
    ext.downloads.download({ url: url }).then(function () {}, function () {});
  });
  return dropped;
}

if (ext.downloads && ext.downloads.onDeterminingFilename) {
  // Chromium (Chrome, Edge, Brave). This is the moment the browser is about to decide the file name and,
  // if the user asked for it, open its own "Save as" window. It waits for every listener to answer, so
  // cancelling the download HERE means that window never appears. (Doing it in onCreated is too late:
  // the window is already on its way.) We must always answer, or the browser would wait for us.
  ext.downloads.onDeterminingFilename.addListener(function (item, suggest) {
    var answered = false;
    var answer = function () {
      if (answered) return;
      answered = true;
      try {
        suggest();
      } catch (e) {
        /* the download may already be cancelled */
      }
    };
    getSettings().then(function (settings) {
      if (!shouldTake(item, settings)) return answer();
      // The browser's copy is cancelled and erased: there is nothing left to name. Answering for it would
      // only make the browser complain ("Invalid downloadId").
      answered = true;
      return takeOver(item);
    }).then(answer, answer);
    return true; // we answer asynchronously
  });
} else if (ext.downloads && ext.downloads.onCreated) {
  // Firefox has no such hook: cancel as soon as the download exists.
  ext.downloads.onCreated.addListener(function (item) {
    getSettings().then(function (settings) {
      if (shouldTake(item, settings)) takeOver(item);
    });
  });
}

// ---------------------------------------------------------------------------- video detection

var DETECT = { permissions: ["webRequest"], origins: ["http://*/*", "https://*/*"] };
var BAR_ID = "velox-videobar";
var detectOn = null; // null until the settings were read; responses seen before that wait in `early`
var detectReady = null; // the first syncDetection(): bar messages that woke the worker wait for it
var early = [];
var listening = false;
var tabMedia = {}; // tabId -> { page, items: [{ id, kind, url, label, size }], next }

function hasDetectPermission() {
  if (!ext.permissions) return Promise.resolve(false);
  return ext.permissions.contains(DETECT).then(function (v) {
    return v === true;
  }, function () {
    return false;
  });
}

function pageKey(url) {
  return typeof url === "string" ? url.split("#")[0] : "";
}

/** A tab's list, from memory or (after the background script was restarted) from session storage. */
function loadTab(tabId) {
  if (tabMedia[tabId]) return Promise.resolve(tabMedia[tabId]);
  var key = "tab:" + tabId;
  var store = ext.storage && ext.storage.session;
  var read = store ? store.get(key).then(function (r) {
    return r && r[key];
  }, function () {
    return null;
  }) : Promise.resolve(null);
  return read.then(function (saved) {
    if (!tabMedia[tabId]) {
      var ok = saved && Array.isArray(saved.items) && Number.isInteger(saved.next);
      tabMedia[tabId] = ok ? { page: String(saved.page || ""), items: saved.items.slice(0, M.MAX_ITEMS), next: saved.next } : { page: "", items: [], next: 1 };
    }
    return tabMedia[tabId];
  });
}

function saveTab(tabId) {
  var store = ext.storage && ext.storage.session;
  if (!store || !tabMedia[tabId]) return;
  var o = {};
  o["tab:" + tabId] = tabMedia[tabId];
  store.set(o).then(function () {}, function () {});
}

function badge(tabId, count) {
  if (!ext.action) return;
  try {
    ext.action.setBadgeBackgroundColor({ tabId: tabId, color: "#2563eb" });
    ext.action.setBadgeText({ tabId: tabId, text: count > 0 ? String(Math.min(count, 99)) : "" });
  } catch (e) {
    /* the tab may be gone */
  }
}

function forgetTab(tabId) {
  delete tabMedia[tabId];
  if (ext.storage && ext.storage.session) ext.storage.session.remove("tab:" + tabId).then(function () {}, function () {});
}

function onHeaders(d) {
  if (detectOn === null) {
    if (early.length < 200) early.push(d);
    return;
  }
  if (!detectOn) return;
  var item = M.classifyResponse(d);
  if (!item) return;
  loadTab(d.tabId).then(function (t) {
    if (M.addItem(t.items, item, t.next)) {
      t.next++;
      saveTab(d.tabId);
      badge(d.tabId, t.items.length);
    }
  });
}

function listen() {
  if (listening || !ext.webRequest || !ext.webRequest.onHeadersReceived) return;
  ext.webRequest.onHeadersReceived.addListener(onHeaders, { urls: DETECT.origins, types: ["media", "xmlhttprequest", "other"] }, ["responseHeaders"]);
  listening = true;
}

function registerBar() {
  if (!ext.scripting || !ext.scripting.registerContentScripts) return Promise.resolve();
  return ext.scripting.getRegisteredContentScripts({ ids: [BAR_ID] }).then(function (list) {
    if (list && list.length > 0) return;
    return ext.scripting.registerContentScripts([{ id: BAR_ID, matches: DETECT.origins, js: ["videobar.js"], runAt: "document_idle", allFrames: false }]).then(injectOpenTabs);
  }).then(function () {}, function () {});
}

/** A registered script only reaches pages loaded from now on: give the bar to the pages already open too
 * (the script itself refuses to run twice in one page). */
function injectOpenTabs() {
  if (!ext.tabs || !ext.tabs.query || !ext.scripting.executeScript) return;
  return ext.tabs.query({ url: DETECT.origins }).then(function (tabs) {
    tabs.forEach(function (t) {
      if (t.discarded || !V.parseHttpUrl(t.url || "")) return;
      ext.scripting.executeScript({ target: { tabId: t.id }, files: ["videobar.js"] }).then(function () {}, function () {});
    });
  }, function () {});
}

function unregisterBar() {
  if (!ext.scripting || !ext.scripting.unregisterContentScripts) return Promise.resolve();
  return ext.scripting.getRegisteredContentScripts({ ids: [BAR_ID] }).then(function (list) {
    if (list && list.length > 0) return ext.scripting.unregisterContentScripts({ ids: [BAR_ID] });
  }).then(function () {}, function () {});
}

/** Detection runs only while the setting is on AND the browser granted the permission for it. */
function syncDetection() {
  return Promise.all([getSettings(), hasDetectPermission()]).then(function (r) {
    detectOn = r[0].detectVideos && r[0].appVideoBar && r[1];
    var queued = early;
    early = [];
    if (detectOn) {
      listen();
      queued.forEach(onHeaders);
      return registerBar();
    }
    Object.keys(tabMedia).forEach(function (id) {
      badge(Number(id), 0);
      forgetTab(Number(id));
    });
    return unregisterBar();
  });
}

// Registered at once (not after a promise) so a detected video can wake the background script up.
if (ext.webRequest) listen();
detectReady = syncDetection().then(function () {}, function () {});

/** Learn the app's "Download video bar" switch from a ping answer and apply it. */
function adoptAppSettings(r) {
  if (!r || !r.ok || typeof r.videoBar !== "boolean") return Promise.resolve();
  return getSettings().then(function (s) {
    if (s.appVideoBar === r.videoBar) return;
    return ext.storage.local.set({ appVideoBar: r.videoBar }).then(syncDetection);
  });
}

var lastAppCheck = 0;
/** Ask the app (at most once a minute; a ping never starts the app). */
function checkApp(force) {
  if (!force && Date.now() - lastAppCheck < 60000) return Promise.resolve();
  lastAppCheck = Date.now();
  return nativeCall({ command: "ping" }).then(adoptAppSettings, function () {});
}
checkApp(true);
if (ext.permissions && ext.permissions.onRemoved) ext.permissions.onRemoved.addListener(syncDetection);
if (ext.permissions && ext.permissions.onAdded) ext.permissions.onAdded.addListener(syncDetection);

if (ext.tabs && ext.tabs.onUpdated) {
  // A new page (the address changed, not just its #part): start a new list.
  ext.tabs.onUpdated.addListener(function (tabId, info) {
    if (info.status === "complete") checkApp(false);
    if (!info.url) return;
    loadTab(tabId).then(function (t) {
      if (pageKey(t.page) === pageKey(info.url)) return;
      t.page = info.url;
      t.items = [];
      saveTab(tabId);
      badge(tabId, 0);
    });
  });
}
if (ext.tabs && ext.tabs.onRemoved) ext.tabs.onRemoved.addListener(forgetTab);

/** What the bar / popup may know about a tab: the detected items (without their links), whether the page
 * itself is a video page of a known site, and whether the site protects its videos with DRM. */
function mediaState(tab) {
  var url = tab && tab.url;
  return loadTab(tab.id).then(function (t) {
    var service = M.protectedService(url);
    return {
      ok: true,
      items: service ? [] : t.items.map(M.publicItem),
      page: service ? null : M.videoPageSite(url),
      protected: service
    };
  });
}

/** Hand one detected item (or the page itself) over to the app. */
function downloadMedia(tab, itemId) {
  var service = M.protectedService(tab.url);
  if (service) return Promise.resolve({ ok: false, error: service + " protects its videos with DRM (copy protection): they cannot be downloaded." });
  return loadTab(tab.id).then(function (t) {
    var item = null;
    if (itemId === "page") {
      if (M.videoPageSite(tab.url)) item = { url: pageKey(tab.url), media: "page" };
    } else {
      var found = t.items.filter(function (x) {
        return x.id === itemId;
      })[0];
      if (found) item = { url: found.url, media: found.kind === "stream" ? "stream" : undefined };
    }
    if (!item) return { ok: false, error: "That video is no longer on this page. Play it again and retry." };
    // Cookies (when that setting is on) need access to the video's site, which detection already holds.
    return handoff([item], "videoBar", { referrer: tab.url, incognito: !!tab.incognito }).then(function (results) {
      var r = results[0];
      return r.ok ? { ok: true } : { ok: false, error: explain(r.code) };
    });
  });
}

// ---------------------------------------------------------------------------- popup requests

ext.runtime.onMessage.addListener(function (message, sender, sendResponse) {
  // The video bar (our content script, inside a web page). Our own pages (the popup, even when it is
  // opened in a tab) have an extension address and go through the popup rules below.
  var ownPage = sender && typeof sender.url === "string" && sender.url.indexOf(ext.runtime.getURL("")) === 0;
  if (sender && sender.tab && !ownPage) {
    var bar = M.validateBarMessage(message, sender, ext.runtime.id);
    if (!bar.ok) {
      sendResponse({ ok: false, error: bar.error });
      return false;
    }
    // A message from the bar can be what wakes this worker up: wait until the settings were read.
    var work = detectReady.then(function () {
      if (!detectOn) return { ok: false, error: "Video detection is off." };
      return bar.value.type === "bar:state" ? mediaState(sender.tab) : downloadMedia(sender.tab, bar.value.itemId);
    });
    work.then(sendResponse, function () {
      sendResponse({ ok: false, error: "failed" });
    });
    return true;
  }
  var check = V.validateInternalMessage(message, sender, {
    runtimeId: ext.runtime.id,
    popupUrl: ext.runtime.getURL("popup.html")
  });
  if (!check.ok) {
    sendResponse({ ok: false, error: check.error });
    return false;
  }
  var m = check.value;
  var reply = function (p) {
    p.then(sendResponse, function () {
      sendResponse({ ok: false, error: "failed" });
    });
    return true; // asynchronous response
  };
  switch (m.type) {
    case "ping":
      return reply(nativeCall({ command: "ping" }).then(function (r) {
        lastAppCheck = Date.now();
        return adoptAppSettings(r).then(function () {
          return { ok: r.ok, version: r.version, error: r.ok ? undefined : explain(r.code) };
        });
      }));
    case "getSettings":
      return reply(getSettings().then(function (s) {
        return { ok: true, settings: s };
      }));
    case "setSettings":
      return reply(setSettings(m.settings).then(function (s) {
        return { ok: true, settings: s };
      }));
    case "getMedia":
    case "downloadMedia":
      return reply(
        ext.tabs.get(m.tabId).then(function (tab) {
          if (!tab || !V.parseHttpUrl(tab.url || "")) return { ok: true, items: [], page: null, protected: null };
          return m.type === "getMedia" ? mediaState(tab) : downloadMedia(tab, m.itemId);
        })
      );
    case "handoff":
      return reply(
        (m.tabId === undefined ? Promise.resolve(null) : ext.tabs.get(m.tabId).catch(function () {
          return null;
        })).then(function (tab) {
          var context = { referrer: tab && tab.url, incognito: !!(tab && tab.incognito) };
          return handoff(m.items, m.source, context).then(function (results) {
            return {
              ok: true,
              results: results.map(function (r) {
                return { url: r.url, ok: r.ok, filename: r.filename, error: r.ok ? undefined : explain(r.code) };
              })
            };
          });
        })
      );
    default:
      sendResponse({ ok: false, error: "unknown message type" });
      return false;
  }
});
