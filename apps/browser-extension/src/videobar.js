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
 * Idle it is a small island (only its download button); hovered or focused it opens with a spring, and closes again a
 * moment after the pointer leaves.
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
    ":host{all:initial;position:fixed;z-index:2147483647;top:0;left:0;display:none;font:13px/1.3 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;-webkit-font-smoothing:antialiased}",
    // The island: centred on its place, so it grows and shrinks from its middle.
    ".wrap{position:absolute;left:0;top:0;width:max-content;transform:translateX(-50%);display:flex;flex-direction:column;align-items:center}",
    ".bar{position:relative;display:flex;align-items:center;gap:2px;min-height:38px;box-sizing:border-box;padding:4px;border-radius:19px;overflow:hidden;color:#fff;" +
      "background:linear-gradient(180deg,rgba(30,30,36,.97),rgba(6,6,10,.97));" +
      "box-shadow:inset 0 0 0 1px rgba(255,255,255,.08),inset 0 1px 0 rgba(255,255,255,.14),0 14px 34px -8px rgba(0,0,0,.6),0 2px 8px rgba(0,0,0,.3);" +
      "backdrop-filter:blur(16px) saturate(170%);-webkit-backdrop-filter:blur(16px) saturate(170%);" +
      "transition:width .56s cubic-bezier(.3,1.32,.4,1),box-shadow .35s ease;animation:pop .55s cubic-bezier(.3,1.45,.4,1) both}",
    ".bar:hover{box-shadow:inset 0 0 0 1px rgba(147,197,253,.22),inset 0 1px 0 rgba(255,255,255,.16),0 18px 40px -8px rgba(0,0,0,.65),0 0 24px -6px rgba(59,130,246,.45)}",
    "@keyframes pop{from{transform:scale(.35);opacity:0;filter:blur(8px)}60%{opacity:1}to{transform:none;opacity:1;filter:none}}",
    ".out .bar,.out .menu{animation:gone .2s cubic-bezier(.4,0,1,1) both}",
    "@keyframes gone{to{transform:scale(.5);opacity:0;filter:blur(6px)}}",
    "button{all:unset;cursor:pointer;display:flex;align-items:center;gap:7px;box-sizing:border-box;min-height:30px;padding:0 12px;border-radius:999px;color:#fff;white-space:nowrap;transition:background .2s ease,transform .18s ease,filter .2s ease}",
    "button:hover,button:focus-visible{background:rgba(255,255,255,.12)}",
    "button:active{transform:scale(.94)}",
    ".go{background:linear-gradient(180deg,#5b93ff,#2563eb);font-weight:600;letter-spacing:.01em;box-shadow:inset 0 0 0 1px rgba(255,255,255,.2),0 6px 16px -4px rgba(37,99,235,.7)}",
    ".go:hover,.go:focus-visible{background:linear-gradient(180deg,#6ea0ff,#2f6df0);filter:brightness(1.05)}",
    ".x{padding:0 10px;opacity:.7;font-size:16px}.x:hover{opacity:1}",
    ".grip{cursor:grab;padding:0 4px 0 8px;opacity:.55;touch-action:none}.grip:hover,.grip:focus-visible{opacity:1;background:none}",
    ".dragging,.dragging .grip{cursor:grabbing}",
    ".note{padding:0 12px;max-width:320px;white-space:normal}",
    ".note.busy{background:linear-gradient(90deg,rgba(255,255,255,.45) 0,#fff 50%,rgba(255,255,255,.45) 100%);background-size:200% 100%;-webkit-background-clip:text;background-clip:text;color:transparent;animation:shine 1.3s linear infinite}",
    "@keyframes shine{to{background-position:-200% 0}}",
    // Idle: only the download button, round, breathing softly. Open: the rest fades in from a blur.
    ".island .grip,.island .label,.island .x,.island .note{display:none}",
    ".island .go{width:30px;padding:0;justify-content:center;animation:breathe 2.6s ease-in-out infinite}",
    "@keyframes breathe{0%,100%{box-shadow:inset 0 0 0 1px rgba(255,255,255,.2),0 0 0 0 rgba(59,130,246,.55)}50%{box-shadow:inset 0 0 0 1px rgba(255,255,255,.2),0 0 0 7px rgba(59,130,246,0)}}",
    ".bar:not(.island) .grip,.bar:not(.island) .label,.bar:not(.island) .x,.bar:not(.island) .note{animation:reveal .42s cubic-bezier(.2,.8,.2,1) .09s both}",
    "@keyframes reveal{from{opacity:0;filter:blur(5px);transform:scale(.86)}to{opacity:1;filter:none;transform:none}}",
    ".leaving .grip,.leaving .label,.leaving .x,.leaving .note{animation:none;opacity:0;filter:blur(4px);transition:opacity .12s ease,filter .12s ease}",
    ".menu{margin-top:8px;display:flex;flex-direction:column;gap:2px;padding:5px;border-radius:16px;max-width:420px;background:linear-gradient(180deg,rgba(30,30,36,.97),rgba(6,6,10,.97));" +
      "box-shadow:inset 0 0 0 1px rgba(255,255,255,.08),0 16px 38px -8px rgba(0,0,0,.6);transform-origin:top center;animation:drop .38s cubic-bezier(.3,1.35,.4,1) both}",
    "@keyframes drop{from{opacity:0;transform:scale(.7) translateY(-8px);filter:blur(4px)}to{opacity:1;transform:none;filter:none}}",
    ".menu button{border-radius:10px;overflow:hidden;text-overflow:ellipsis}",
    "svg{width:15px;height:15px;flex:none}",
    "[hidden]{display:none!important}",
    "@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}"
  ].join("\n");
  var wrap = document.createElement("div");
  wrap.className = "wrap";
  var bar = document.createElement("div");
  bar.className = "bar island";
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
  goText.className = "label";
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
  var drag = null; // a drag going on: { id, dx, dy, x, y, moved, onGrip } (the pointer's offset from the bar's middle and top, where it started)
  var expanded = false; // open (hovered, or saying something) or the idle island
  var hovering = false;
  var idleTimer = 0;
  var shrinkTimer = 0;
  var morphTimer = 0;
  var hideAnim = 0;
  var openWidth = 0; // the open bar's width, last measured: the island's place is the open bar's middle
  var still = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
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
    var w = roomWidth();
    var h = bar.offsetHeight || 38;
    // The bar is placed by its middle: it grows and shrinks from there.
    var cx = r.right - w / 2 - 10;
    var top = r.top + 10;
    if (spot) {
      // Its centre at the same place on the video (a note that widens it for a moment does not move it), inside it.
      cx = Math.max(r.left + w / 2, Math.min(r.right - w / 2, r.left + spot.fx * r.width));
      top = Math.max(r.top, Math.min(r.bottom - h, r.top + spot.fy * r.height - h / 2));
    }
    moveTo(cx, top);
  }

  /** The width the bar may take: the open bar's, so the island opens without leaving the screen. */
  function roomWidth() {
    return Math.max(bar.offsetWidth || 0, openWidth || 190);
  }

  function moveTo(cx, top) {
    var w = roomWidth();
    var h = bar.offsetHeight || 38;
    cx = Math.max(w / 2 + 4, Math.min(innerWidth - w / 2 - 4, cx));
    top = Math.max(4, Math.min(innerHeight - h - 4, top));
    host.style.setProperty("transform", "translate(" + Math.round(cx) + "px," + Math.round(top) + "px)", "important");
    return { cx: cx, top: top };
  }

  /** Change what the bar shows, its width following with a spring (from its middle). */
  function morph(update) {
    var from = bar.offsetWidth; // its layout width (the pop-in animation scales it)
    bar.style.width = "";
    update();
    var to = bar.offsetWidth;
    if (!bar.classList.contains("island") && to) openWidth = to;
    clearTimeout(morphTimer);
    if (from && Math.abs(to - from) > 0.5 && !still) {
      bar.style.width = from + "px";
      void bar.offsetWidth; // start from the old width
      bar.style.width = to + "px";
      morphTimer = setTimeout(function () {
        bar.style.width = "";
      }, 600);
    }
    if (target) place();
  }

  /** Open (hovered, focused, saying something) or back to the idle island. */
  function setOpen(open) {
    clearTimeout(shrinkTimer);
    if (open === expanded) return;
    expanded = open;
    if (open) {
      morph(function () {
        bar.classList.remove("island", "leaving");
      });
    } else {
      // The words fade first, then it closes.
      bar.classList.add("leaving");
      shrinkTimer = setTimeout(function () {
        morph(function () {
          bar.classList.remove("leaving");
          bar.classList.add("island");
        });
      }, still ? 0 : 130);
    }
  }

  function idleLater() {
    clearTimeout(idleTimer);
    idleTimer = setTimeout(function () {
      if (hovering || busy || drag || !menu.hidden || !note.hidden) return idleLater();
      setOpen(false);
    }, 1100);
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
    var update = function () {
      note.textContent = text || "";
      note.hidden = !text;
      note.classList.toggle("busy", busy);
    };
    if (text && !expanded) {
      expanded = true;
      clearTimeout(shrinkTimer);
      morph(function () {
        update();
        bar.classList.remove("island", "leaving");
      });
    } else if (expanded) morph(update);
    else update();
    if (!text) idleLater();
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
      clearTimeout(hideAnim);
      wrap.classList.remove("out");
      var appearing = host.style.display !== "block";
      if (appearing && !busy) {
        // Each time it appears: the idle island, popping in.
        expanded = false;
        bar.classList.add("island");
        bar.classList.remove("leaving");
        bar.style.width = "";
      }
      goText.textContent = label(s);
      go.title = label(s);
      if (!busy) setNote("");
      host.style.setProperty("display", "block", "important");
      if (!openWidth) measureOpen();
      place();
      later();
    });
  }

  /** The open bar's width, measured once without showing it (the island is placed by the open bar's middle). */
  function measureOpen() {
    var island = bar.classList.contains("island");
    bar.style.visibility = "hidden";
    bar.classList.remove("island");
    openWidth = bar.offsetWidth || openWidth;
    if (island) bar.classList.add("island");
    bar.style.visibility = "";
  }

  function hide(now) {
    clearTimeout(hideTimer);
    if (now) {
      menu.hidden = true;
      menu.textContent = "";
      target = null;
      hovering = false;
      if (host.style.display !== "block" || still) return host.style.setProperty("display", "none", "important");
      // It shrinks away, then goes.
      wrap.classList.add("out");
      clearTimeout(hideAnim);
      hideAnim = setTimeout(function () {
        if (target) return; // shown again meanwhile
        host.style.setProperty("display", "none", "important");
        wrap.classList.remove("out");
      }, 200);
    }
  }

  function later() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(function () {
      if (!menu.hidden || busy || drag || hovering) return later();
      hide(true);
    }, 3500);
  }

  function download(itemId) {
    busy = true;
    menu.hidden = true;
    menu.textContent = "";
    setNote("Sending to VeloxFetch…");
    note.classList.add("busy");
    ask({ type: "bar:download", itemId: itemId }).then(function (r) {
      busy = false;
      note.classList.remove("busy");
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
    drag = { id: ev.pointerId, dx: ev.clientX - (b.left + b.width / 2), dy: ev.clientY - b.top, x: ev.clientX, y: ev.clientY, moved: false, onGrip: ev.target !== bar };
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
    var cx = ev.clientX - drag.dx;
    var top = ev.clientY - drag.dy;
    if (target && target.isConnected) {
      // Over the video only: where it is dropped is where it stays.
      var r = target.getBoundingClientRect();
      var w = bar.offsetWidth;
      var h = bar.offsetHeight;
      cx = Math.max(r.left + w / 2, Math.min(r.right - w / 2, cx));
      top = Math.max(r.top, Math.min(r.bottom - h, top));
    }
    moveTo(cx, top);
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
  // Hovered or focused: it opens. Left alone: back to the island after a moment.
  bar.addEventListener("pointerenter", function (ev) {
    if (!ev.isTrusted) return;
    hovering = true;
    setOpen(true);
  });
  bar.addEventListener("pointerleave", function () {
    hovering = false;
    idleLater();
    later();
  });
  bar.addEventListener("focusin", function () {
    setOpen(true);
  });
  bar.addEventListener("focusout", idleLater);

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
    moveTo(b.left + b.width / 2 + step[0], b.top + step[1]);
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
