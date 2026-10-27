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
 *
 * The user can drag it (its handle, or the bar's edge) anywhere over the video, or move it with the arrow keys on
 * the handle; a double click on the handle puts it back. Where it was put is kept as a place on the video (a fraction
 * of its width and height), one for every page: nothing about the page is stored.
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
    ".grip{cursor:grab;padding:6px 4px 6px 8px;opacity:.7;touch-action:none}.grip:hover,.grip:focus-visible{opacity:1}",
    ".dragging,.dragging .grip{cursor:grabbing}",
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
  var grip = document.createElement("button");
  grip.className = "grip";
  grip.type = "button";
  grip.title = "Drag to move (double-click: back to its place)";
  grip.setAttribute("aria-label", "Move the bar");
  var dots = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  dots.setAttribute("viewBox", "0 0 24 24");
  dots.setAttribute("fill", "currentColor");
  [[9, 6], [15, 6], [9, 12], [15, 12], [9, 18], [15, 18]].forEach(function (c) {
    var dot = document.createElementNS("http://www.w3.org/2000/svg", "circle");
    dot.setAttribute("cx", String(c[0]));
    dot.setAttribute("cy", String(c[1]));
    dot.setAttribute("r", "1.8");
    dots.appendChild(dot);
  });
  grip.appendChild(dots);
  var note = document.createElement("span");
  note.className = "note";
  note.hidden = true;
  var close = document.createElement("button");
  close.className = "x";
  close.type = "button";
  close.title = "Hide on this page";
  close.textContent = "×";
  bar.append(grip, go, note, close);
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
  var spot = null; // where the user put the bar: { fx, fy }, its centre's place on the video (0..1 of its size)
  var drag = null; // a drag going on: { id, dx, dy, x, y, moved, onGrip } (the pointer's offset in the bar, where it started)
  var lastTap = 0; // when the handle was last pressed without moving it (two in a row: back to the usual place)
  var SPOT_KEY = "videoBarSpot";

  try {
    Promise.resolve(ext.storage.local.get(SPOT_KEY))
      .then(function (r) {
        spot = validSpot(r && r[SPOT_KEY]);
      })
      .catch(function () {});
  } catch (e) {
    /* the extension was reloaded: keep the default place */
  }

  function validSpot(v) {
    var ok = function (n) {
      return typeof n === "number" && n >= 0 && n <= 1;
    };
    return v && ok(v.fx) && ok(v.fy) ? { fx: v.fx, fy: v.fy } : null;
  }

  function saveSpot(v) {
    spot = v;
    try {
      if (v) Promise.resolve(ext.storage.local.set({ videoBarSpot: v })).catch(function () {});
      else Promise.resolve(ext.storage.local.remove(SPOT_KEY)).catch(function () {});
    } catch (e) {
      /* orphaned copy of the script: the place is kept for this page only */
    }
  }

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
    if (drag) return;
    var r = target.getBoundingClientRect();
    if (r.width < MIN_W || r.height < MIN_H || r.bottom < 0 || r.top > innerHeight) return hide(true);
    var w = wrap.offsetWidth || 180;
    var h = bar.offsetHeight || 36;
    var left = r.right - w - 10;
    var top = r.top + 10;
    if (spot) {
      // Its centre at the same place on the video (a note that widens it for a moment does not move it), inside it.
      left = Math.max(r.left, Math.min(r.right - w, r.left + spot.fx * r.width - w / 2));
      top = Math.max(r.top, Math.min(r.bottom - h, r.top + spot.fy * r.height - h / 2));
    }
    moveTo(left, top);
  }

  function moveTo(left, top) {
    var w = wrap.offsetWidth || 180;
    var h = bar.offsetHeight || 36;
    left = Math.max(4, Math.min(innerWidth - w - 4, left));
    top = Math.max(4, Math.min(innerHeight - h - 4, top));
    host.style.setProperty("transform", "translate(" + Math.round(left) + "px," + Math.round(top) + "px)", "important");
    return { left: left, top: top };
  }

  /** Where the bar is now, as a place on the video (kept for the next videos and pages). */
  function spotHere() {
    if (!target || !target.isConnected) return null;
    var r = target.getBoundingClientRect();
    var b = bar.getBoundingClientRect();
    var clamp = function (n) {
      return Math.max(0, Math.min(1, Math.round(n * 1000) / 1000));
    };
    return { fx: clamp((b.left + b.width / 2 - r.left) / r.width), fy: clamp((b.top + b.height / 2 - r.top) / r.height) };
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
      if (!menu.hidden || busy || drag) return later();
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
  // Dragging: from the handle, or the bar's own edge (not its buttons). The page never sees these pointer events.
  function startDrag(ev) {
    if (!ev.isTrusted || ev.button !== 0 || (ev.target !== grip && ev.target !== bar && !grip.contains(ev.target))) return;
    ev.preventDefault();
    ev.stopPropagation();
    var b = bar.getBoundingClientRect();
    drag = { id: ev.pointerId, dx: ev.clientX - b.left, dy: ev.clientY - b.top, x: ev.clientX, y: ev.clientY, moved: false, onGrip: ev.target !== bar };
    if (drag.onGrip) grip.focus({ preventScroll: true }); // for the arrow keys
    try {
      bar.setPointerCapture(ev.pointerId);
    } catch (e) {
      /* the pointer is already gone */
    }
    bar.classList.add("dragging");
    menu.hidden = true;
  }
  function moveDrag(ev) {
    if (!drag || ev.pointerId !== drag.id || !ev.isTrusted) return;
    ev.preventDefault();
    ev.stopPropagation();
    if (!drag.moved && Math.abs(ev.clientX - drag.x) + Math.abs(ev.clientY - drag.y) < 4) return;
    drag.moved = true;
    moveTo(ev.clientX - drag.dx, ev.clientY - drag.dy);
  }
  function endDrag(ev) {
    if (!drag || ev.pointerId !== drag.id) return;
    ev.stopPropagation();
    var moved = drag.moved;
    var onGrip = drag.onGrip;
    drag = null;
    bar.classList.remove("dragging");
    if (ev.isTrusted && moved) {
      saveSpot(spotHere());
      lastTap = 0;
    } else if (ev.isTrusted && onGrip) {
      // Two presses on the handle in a row: back to the usual place.
      if (Date.now() - lastTap < 450) {
        saveSpot(null);
        lastTap = 0;
      } else lastTap = Date.now();
    }
    place();
    later();
  }
  bar.addEventListener("pointerdown", startDrag);
  bar.addEventListener("pointermove", moveDrag);
  bar.addEventListener("pointerup", endDrag);
  bar.addEventListener("pointercancel", endDrag);
  // A press on the handle or the bar's edge is the bar's, not a click on the page (or its player) below.
  ["click", "dblclick", "mousedown", "mouseup"].forEach(function (t) {
    bar.addEventListener(t, function (ev) {
      if (ev.target === bar || grip.contains(ev.target)) ev.stopPropagation();
    });
  });
  grip.addEventListener("keydown", function (ev) {
    var step = { ArrowLeft: [-16, 0], ArrowRight: [16, 0], ArrowUp: [0, -16], ArrowDown: [0, 16] }[ev.key];
    if (!ev.isTrusted || !step) return;
    ev.preventDefault();
    ev.stopPropagation();
    var b = bar.getBoundingClientRect();
    moveTo(b.left + step[0], b.top + step[1]);
    saveSpot(spotHere());
    later();
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
