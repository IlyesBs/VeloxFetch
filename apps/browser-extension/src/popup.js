/*
 * Popup. Everything from pages or from the background script is treated as untrusted text: it is
 * only ever shown with `textContent` (never as HTML) and links are only ever *sent*, never followed
 * or opened by this script.
 */
"use strict";

var V = self.VeloxValidate;
var ext = typeof browser !== "undefined" ? browser : chrome;

function $(id) {
  return document.getElementById(id);
}

function say(el, text, cls) {
  el.textContent = text;
  el.className = cls || "";
  el.hidden = !text;
}

function send(message) {
  return ext.runtime.sendMessage(message);
}

// Runs inside the page (via scripting.executeScript). It only READS the DOM and returns plain
// data; the page cannot make it do anything else, and the result is validated before use.
function collectLinks() {
  var out = [];
  var anchors = document.querySelectorAll("a[href]");
  for (var i = 0; i < anchors.length && out.length < 500; i++) {
    var a = anchors[i];
    out.push({
      href: String(a.href),
      text: String(a.textContent || "").replace(/\s+/g, " ").trim().slice(0, 120),
      download: a.hasAttribute("download")
    });
  }
  return out;
}

var links = []; // [{ url, label, worthy }]

function render() {
  var showAll = $("show-all").checked;
  var list = $("links");
  list.textContent = "";
  var shown = 0;
  links.forEach(function (l, i) {
    if (!showAll && !l.worthy) return;
    shown++;
    var li = document.createElement("li");
    var cb = document.createElement("input");
    cb.type = "checkbox";
    cb.dataset.index = String(i);
    cb.checked = l.worthy;
    cb.id = "l" + i;
    var box = document.createElement("span");
    var label = document.createElement("label");
    label.htmlFor = cb.id;
    label.textContent = l.label;
    var small = document.createElement("small");
    small.textContent = l.url;
    box.append(label, small);
    li.append(cb, box);
    list.append(li);
  });
  say($("scan-note"), shown === 0 ? "No downloadable links found. Try “Show all links”." : "", "note");
  $("send").disabled = shown === 0;
}

async function scan() {
  say($("result"), "");
  try {
    var tabs = await ext.tabs.query({ active: true, currentWindow: true });
    var tab = tabs[0];
    if (!tab || !V.parseHttpUrl(tab.url || "")) {
      say($("scan-note"), "This page cannot be scanned (only ordinary web pages can).", "note");
      return;
    }
    var res = await ext.scripting.executeScript({ target: { tabId: tab.id }, func: collectLinks });
    var raw = res && res[0] && Array.isArray(res[0].result) ? res[0].result : [];
    var seen = {};
    links = [];
    raw.forEach(function (r) {
      if (!r || typeof r.href !== "string") return;
      var u = V.parseHttpUrl(r.href);
      if (!u || seen[u.href]) return;
      seen[u.href] = true;
      var label = (typeof r.text === "string" && r.text) || decodeURIComponent(u.pathname.split("/").pop() || u.hostname).slice(0, 120);
      links.push({ url: u.href, label: label, worthy: V.isDownloadWorthy(u.href, r.download === true) });
    });
    $("found").hidden = false;
    render();
    $("scan").dataset.tab = String(tab.id);
  } catch (e) {
    say($("scan-note"), "Could not read this page.", "note");
  }
}

async function sendSelected() {
  var boxes = Array.prototype.slice.call($("links").querySelectorAll("input[type=checkbox]:checked"));
  var items = boxes
    .slice(0, V.MAX_ITEMS)
    .map(function (b) {
      return { url: links[Number(b.dataset.index)].url };
    });
  if (items.length === 0) {
    say($("result"), "Select at least one link.", "note");
    return;
  }
  $("send").disabled = true;
  // Asking for site access must happen right here, inside the click.
  var settings = (await send({ type: "getSettings" })).settings || {};
  if (settings.sendCookies) {
    var origins = {};
    items.forEach(function (i) {
      var u = V.parseHttpUrl(i.url);
      if (u) origins[u.origin + "/*"] = true;
    });
    try {
      await ext.permissions.request({ origins: Object.keys(origins) });
    } catch (e) {
      /* declined: links are sent without cookies */
    }
  }
  var tabId = Number($("scan").dataset.tab);
  var reply = await send({ type: "handoff", items: items, source: "linkList", tabId: Number.isInteger(tabId) ? tabId : undefined });
  $("send").disabled = false;
  if (!reply || !reply.ok) {
    say($("result"), "Could not send the links.", "note");
    return;
  }
  var okCount = reply.results.filter(function (r) {
    return r.ok;
  }).length;
  var firstError = reply.results.find(function (r) {
    return !r.ok;
  });
  say(
    $("result"),
    okCount + " of " + reply.results.length + " sent to VeloxFetch." + (firstError ? " " + firstError.error : ""),
    "note"
  );
}

var DETECT = { permissions: ["webRequest"], origins: ["http://*/*", "https://*/*"] };

async function activeTab() {
  var tabs = await ext.tabs.query({ active: true, currentWindow: true });
  return tabs[0] && V.parseHttpUrl(tabs[0].url || "") ? tabs[0] : null;
}

/** The videos detected on the current page, each with its own Download button. */
async function showMedia() {
  var list = $("media");
  list.textContent = "";
  list.hidden = true;
  if (!$("detect").checked) return;
  var tab = await activeTab();
  if (!tab) return;
  var r = await send({ type: "getMedia", tabId: tab.id });
  if (!r || !r.ok) return;
  if (r.protected) {
    say($("detect-note"), r.protected + " protects its videos with DRM (copy protection): they cannot be downloaded.", "note");
    return;
  }
  var entries = [];
  if (r.page) entries.push({ id: "page", label: "This video (" + r.page + ")" });
  (r.items || []).forEach(function (it) {
    entries.push({ id: it.id, label: it.label });
  });
  if (entries.length === 0) {
    say($("detect-note"), "No video found on this page yet. Play the video, then open this window again.", "note");
    return;
  }
  say($("detect-note"), "");
  entries.forEach(function (e) {
    var li = document.createElement("li");
    var text = document.createElement("span");
    text.textContent = e.label;
    var b = document.createElement("button");
    b.type = "button";
    b.textContent = "Download";
    b.addEventListener("click", async function () {
      b.disabled = true;
      var res = await send({ type: "downloadMedia", tabId: tab.id, itemId: e.id });
      say($("result"), res && res.ok ? "Sent to VeloxFetch: choose the quality there." : (res && res.error) || "Could not send the video.", "note");
      b.disabled = false;
    });
    li.append(text, b);
    list.append(li);
  });
  list.hidden = false;
}

async function init() {
  var status = $("status");
  try {
    var ping = await send({ type: "ping" });
    if (ping && ping.ok) say(status, "Connected to VeloxFetch" + (ping.version ? " " + ping.version : ""), "ok");
    else say(status, (ping && ping.error) || "Not connected to VeloxFetch.", "bad");
  } catch (e) {
    say(status, "Not connected to VeloxFetch.", "bad");
  }

  var s = (await send({ type: "getSettings" })).settings || {};
  $("intercept").checked = s.intercept !== false;
  $("cookies").checked = s.sendCookies === true;
  $("intercept").addEventListener("change", function (e) {
    send({ type: "setSettings", settings: { intercept: e.target.checked } });
  });
  $("cookies").addEventListener("change", function (e) {
    send({ type: "setSettings", settings: { sendCookies: e.target.checked } });
  });
  var granted = await ext.permissions.contains(DETECT).catch(function () {
    return false;
  });
  $("detect").checked = s.detectVideos !== false && granted;
  if (s.appVideoBar === false) {
    // Turned off in VeloxFetch itself: that switch decides.
    $("detect").disabled = true;
    say($("detect-note"), "The Download video bar is turned off in VeloxFetch (Settings > Browser & clipboard).", "note");
  }
  $("detect").addEventListener("change", async function (e) {
    var on = e.target.checked;
    if (on) {
      // Granted at install; a browser that left it to the user (some Firefox versions) asks here, inside the click.
      var has = await ext.permissions.contains(DETECT).catch(function () {
        return false;
      });
      if (!has) {
        on = await ext.permissions.request(DETECT).catch(function () {
          return false;
        });
      }
      e.target.checked = on;
      say($("detect-note"), on ? "On. A bar appears over videos you can download." : "The browser did not allow it, so detection stays off.", "note");
    } else {
      say($("detect-note"), "");
    }
    await send({ type: "setSettings", settings: { detectVideos: on } });
    showMedia();
  });
  showMedia();
  $("scan").addEventListener("click", scan);
  $("show-all").addEventListener("change", render);
  $("send").addEventListener("click", sendSelected);
  $("select-all").addEventListener("click", function () {
    $("links")
      .querySelectorAll("input[type=checkbox]")
      .forEach(function (b) {
        b.checked = true;
      });
  });
}

init();
