/*
 * VeloxFetch "Download video" bar. Registered only after the user turned on "Detect videos" in the
 * popup (and granted the browser's permission for it); runs in the top frame of ordinary web pages.
 *
 * It is deliberately dumb and keeps no secrets:
 *  - it never reads a link from the page and never sends one: it asks the background script what the
 *    browser itself loaded for this tab ("bar:state") and says which item to download ("bar:download",
 *    an id the background script made). The background script decides everything else;
 *  - it lives in a closed shadow root, builds its elements with createElement / textContent only, and
 *    acts only on real user clicks (`isTrusted`), so the page can neither read nor press it.
 */
(function () {
  "use strict";
  if (window.top !== window || window.__veloxBar) return;
  window.__veloxBar = true;

  var ext = typeof browser !== "undefined" ? browser : chrome;
  var MIN_W = 220;
  var MIN_H = 124;

  var host = document.createElement("velox-video-bar");
  // Inline and !important, so the page's own style sheets cannot move, show or hide it.
  [["position", "fixed"], ["top", "0"], ["left", "0"], ["z-index", "2147483647"], ["display", "none"], ["margin", "0"], ["padding", "0"]].forEach(function (p) {
    host.style.setProperty(p[0], p[1], "important");
  });
  var shadow = host.attachShadow({ mode: "closed" });
  var style = document.createElement("style");
  style.textContent = [
    ":host{all:initial;position:fixed;z-index:2147483647;top:0;left:0;display:none;font:13px/1.3 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif}",
    ".bar{display:flex;align-items:center;gap:2px;padding:3px;border-radius:999px;background:rgba(17,24,39,.92);color:#fff;box-shadow:0 4px 18px rgba(0,0,0,.35);backdrop-filter:blur(6px)}",
    "button{all:unset;cursor:pointer;display:flex;align-items:center;gap:6px;padding:6px 12px;border-radius:999px;color:#fff;white-space:nowrap}",
    "button:hover,button:focus-visible{background:rgba(255,255,255,.14)}",
    ".go{background:#2563eb;font-weight:600}.go:hover,.go:focus-visible{background:#1d4ed8}",
    ".x{padding:6px 9px;opacity:.75}",
    ".note{padding:6px 12px;max-width:340px;white-space:normal}",
    ".menu{margin-top:6px;display:flex;flex-direction:column;gap:2px;padding:4px;border-radius:12px;background:rgba(17,24,39,.96);box-shadow:0 6px 22px rgba(0,0,0,.4);max-width:420px}",
    ".menu button{border-radius:8px;overflow:hidden;text-overflow:ellipsis}",
    "svg{width:15px;height:15px;flex:none}",
    "[hidden]{display:none!important}"
  ].join("\n");
  var wrap = document.createElement("div");
  var bar = document.createElement("div");
  bar.className = "bar";
  var go = document.createElement("button");
  go.className = "go";
  go.type = "button";
  go.setAttribute("data-velox", "download");
  var icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  icon.setAttribute("viewBox", "0 0 24 24");
  icon.setAttribute("fill", "none");
  icon.setAttribute("stroke", "currentColor");
  icon.setAttribute("stroke-width", "2.4");
  icon.setAttribute("stroke-linecap", "round");
  var path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", "M12 4v11m0 0-4.5-4.5M12 15l4.5-4.5M5 20h14");
  icon.appendChild(path);
  var goText = document.createElement("span");
  goText.textContent = "Download video";
  go.append(icon, goText);
  var note = document.createElement("span");
  note.className = "note";
  note.hidden = true;
  var close = document.createElement("button");
  close.className = "x";
  close.type = "button";
  close.title = "Hide on this page";
  close.textContent = "×";
  bar.append(go, note, close);
  var menu = document.createElement("div");
  menu.className = "menu";
  menu.hidden = true;
  wrap.append(bar, menu);
  shadow.append(style, wrap);

  var state = null; // { items, page, protected } from the background script
  var stateAt = 0;
  var target = null; // the <video> the bar sits on
  var hideTimer = 0;
  var dismissedFor = null; // the page address the user hid the bar on
  var busy = false;

  function ask(message) {
    try {
      return Promise.resolve(ext.runtime.sendMessage(message)).catch(function () {
        return null;
      });
    } catch (e) {
      return Promise.resolve(null); // the extension was reloaded: this copy of the script is orphaned
    }
  }

  function refresh() {
    if (Date.now() - stateAt < 1500 && state) return Promise.resolve(state);
    return ask({ type: "bar:state" }).then(function (r) {
      state = r && r.ok ? r : null;
      stateAt = Date.now();
      return state;
    });
  }

  function worthShowing(s) {
    return !!s && (s.protected || s.page || (Array.isArray(s.items) && s.items.length > 0));
  }

  function bigVideos() {
    var out = [];
    var list = document.getElementsByTagName("video");
    for (var i = 0; i < list.length && i < 50; i++) {
      var r = list[i].getBoundingClientRect();
      if (r.width >= MIN_W && r.height >= MIN_H && r.bottom > 0 && r.right > 0 && r.top < innerHeight && r.left < innerWidth) out.push(list[i]);
    }
    return out;
  }

  function place() {
    if (!target || !target.isConnected) return hide(true);
    var r = target.getBoundingClientRect();
    if (r.width < MIN_W || r.height < MIN_H || r.bottom < 0 || r.top > innerHeight) return hide(true);
    var w = wrap.offsetWidth || 180;
    var top = Math.max(4, Math.min(innerHeight - 44, r.top + 10));
    var left = Math.max(4, Math.min(innerWidth - w - 4, r.right - w - 10));
    host.style.setProperty("transform", "translate(" + Math.round(left) + "px," + Math.round(top) + "px)", "important");
  }

  function setNote(text) {
    note.textContent = text || "";
    note.hidden = !text;
  }

  function label(s) {
    if (s.protected) return s.protected + " (protected)";
    var n = (s.page ? 1 : 0) + (s.items ? s.items.length : 0);
    return n > 1 ? "Download video ▾" : "Download video";
  }

  function show(video) {
    if (document.fullscreenElement || dismissedFor === location.href) return;
    refresh().then(function (s) {
      if (!worthShowing(s)) return hide(true);
      target = video;
      goText.textContent = label(s);
      if (!busy) setNote("");
      host.style.setProperty("display", "block", "important");
      place();
      later();
    });
  }

  function hide(now) {
    clearTimeout(hideTimer);
    if (now) {
      host.style.setProperty("display", "none", "important");
      menu.hidden = true;
      menu.textContent = "";
      target = null;
    }
  }

  function later() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(function () {
      if (!menu.hidden || busy) return later();
      hide(true);
    }, 3500);
  }

  function download(itemId) {
    busy = true;
    menu.hidden = true;
    menu.textContent = "";
    setNote("Sending to VeloxFetch…");
    ask({ type: "bar:download", itemId: itemId }).then(function (r) {
      busy = false;
      setNote(r && r.ok ? "Sent: choose the quality in VeloxFetch" : (r && r.error) || "VeloxFetch did not answer.");
      later();
    });
  }

  function openMenu(s) {
    menu.textContent = "";
    var entries = [];
    if (s.page) entries.push({ id: "page", label: "This video (" + s.page + ")" });
    (s.items || []).forEach(function (it) {
      entries.push({ id: it.id, label: it.label });
    });
    entries.forEach(function (e) {
      var b = document.createElement("button");
      b.type = "button";
      b.textContent = e.label;
      b.addEventListener("click", function (ev) {
        if (ev.isTrusted) download(e.id);
      });
      menu.appendChild(b);
    });
    menu.hidden = false;
    later();
  }

  go.addEventListener("click", function (ev) {
    if (!ev.isTrusted || busy) return;
    stateAt = 0;
    refresh().then(function (s) {
      if (!worthShowing(s)) return hide(true);
      if (s.protected) {
        setNote(s.protected + " protects its videos with DRM (copy protection): they cannot be downloaded.");
        return later();
      }
      var count = (s.page ? 1 : 0) + s.items.length;
      if (count === 1) download(s.page ? "page" : s.items[0].id);
      else if (menu.hidden) openMenu(s);
      else menu.hidden = true;
    });
  });
  close.addEventListener("click", function (ev) {
    if (!ev.isTrusted) return;
    dismissedFor = location.href;
    hide(true);
  });

  // Show the bar while the pointer is over a large video (players often cover the video with their own
  // layers, so this looks at positions rather than at which element is under the pointer).
  var lastMove = 0;
  document.addEventListener(
    "pointermove",
    function (e) {
      if (Date.now() - lastMove < 120) return;
      lastMove = Date.now();
      var vids = bigVideos();
      for (var i = 0; i < vids.length; i++) {
        var r = vids[i].getBoundingClientRect();
        if (e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom) {
          if (target !== vids[i] || host.style.display !== "block") show(vids[i]);
          else later();
          return;
        }
      }
    },
    { passive: true, capture: true }
  );
  // A video that starts playing shows the bar for a moment too.
  document.addEventListener(
    "play",
    function (e) {
      var v = e.target;
      if (v && v.tagName === "VIDEO") {
        var r = v.getBoundingClientRect();
        if (r.width >= MIN_W && r.height >= MIN_H) {
          stateAt = 0;
          show(v);
        }
      }
    },
    true
  );
  addEventListener("scroll", function () {
    if (target) place();
  }, { passive: true, capture: true });
  addEventListener("resize", function () {
    if (target) place();
  });
  document.addEventListener("fullscreenchange", function () {
    if (document.fullscreenElement) hide(true);
  });

  (document.documentElement || document).appendChild(host);
})();
