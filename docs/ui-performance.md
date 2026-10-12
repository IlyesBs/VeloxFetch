# Interface smoothness and start-up

How the desktop window stays smooth while downloads run, and how it appears quickly at launch. Measured on Linux
(KDE Plasma, Wayland, RTX 5080, screens at 144 Hz with 1.15 scaling and 180 Hz at 1x) and on a virtual display
(Xvfb, software drawing); the test that measures it is `tests/e2e/desktop-perf.mjs`.

## 1. What was slow, and why

| Symptom | Cause (measured) |
|---|---|
| The dashboard dropped to 23 fps (45 ms per frame) as soon as downloads ran, scrolling the same | Every running card animates its connection matrix (up to 64 cells), so the page repaints every frame. On Linux the web view (WebKitGTK) paints with its CPU rasteriser, where **box shadows** (soft ones above all, but even crisp ones on large rounded panels) and the **window-wide backdrop** (five radial gradients under see-through panels) cost almost the whole frame. Hiding the matrix, all shadows or all animations each brought it back to ~60 fps; layer tricks (`will-change`, `translateZ`, `contain`) and WebKit's renderer switches changed nothing |
| Worse at a fractional screen scale (Wayland, e.g. 1.15) | GTK 3 draws such windows at 2x and lets the compositor shrink them: four times the pixels to paint (65 ms per frame, 17 fps scrolling) |
| No more than 60 fps on a 120 / 144 / 240 Hz screen (Linux) | WebKitGTK's own ceiling (GTK 3): a bare WebKit page outside the app measures 62 fps on the 144 / 180 Hz screens. Not something the app can lift; on Windows (WebView2) the screen's rate is used |
| A dark window for a while at launch | The window is created hidden and shown when the interface is on the page. "On the page" was a React passive effect, which WebKit holds back in a hidden window: committed at 58 ms, run at 311 ms. Then WebKit needs ~300 ms after the window appears to draw its first frame (its drawing surface is set up on first show), with the window's own colour (dark, also for the light theme) showing meanwhile |

## 2. The fixes

* **Linux: no shadows** (`styles.css`, the block "Linux: no box shadows"): `:root[data-platform="linux"] *` has no
  box shadow and no drop-shadow filter. What a shadow meant is drawn another way: the rings of a selected tab, mode,
  page, pill and the focus ring of a field or the link bar become **outlines**; the glass panels' top highlight
  becomes a lighter **top border** (`:where(...)`, so hover and selected borders still win). Windows and macOS keep
  their shadows (their web views paint them on the GPU).
* **The backdrop as a picture** (`src/aurora.ts`): the same five lights as `.aurora` (a unit test keeps the two
  lists equal), drawn once into a canvas at the screen's pixel density and shown as a `blob:` image; redrawn when
  the theme, the accent (Smart throttle's amber) or the window size changes. The CSS gradients stay underneath as
  the fallback. On every platform.
* **The window appears sooner** (`App.tsx`, `nativeFeel.ts`): "ready" is sent from a layout effect (at commit) by
  a direct call (no `import()` waiting for another task). Shown at ~60 ms of page time instead of ~310 ms.
* **No dark flash** (`lib.rs`, `paint_behind`): the colour behind the page follows the theme the interface uses
  (`--canvas`: `#070d1d` dark, `#f4f6f9` light; the system theme when the interface follows it). Windows 11 (Mica)
  stays see-through. The window's configured colour is the dark canvas (`tauri.conf.json`).
* Tried and dropped (measured worse or no better): showing the window as soon as it is created (launch to first
  paint ~1170 ms against ~830 ms, with a longer empty window); delaying the first data load until the first frame;
  static matrix cells (no gain once shadows and backdrop were fixed, so the animation stays).

## 3. Results

| | Before | After |
|---|---|---|
| Downloads running, real screen, XWayland / 1x | 23 fps | 62 fps |
| Scrolling over them, same | 23 fps | 90 fps |
| Downloads running, native Wayland at 1.15 scale (2x drawing) | 40 fps | 49 fps |
| Scrolling over them, same | 17 fps | 32 fps |
| Virtual display (software), downloads / scrolling | 22 / 21 fps | 62 / 90 fps |
| Window asked to show (page time) | ~310 ms | ~60 ms |
| `desktop-ui.mjs` frame-rate check on Linux | failed (listed as a WebKit gap) | passes |

The scrolling figures come from a harsh synthetic scroll (40 px every 8 ms); a mouse wheel asks for less.

Remaining on Linux: the ~300 ms WebKit takes to draw its first frame after the window appears (in the theme's
colour now, not black), the 60 fps ceiling, and the extra cost of fractional scales on native Wayland.

## 4. Tests

* `apps/desktop/src/perf.test.ts` (8): the backdrop's lights equal the CSS ones, fade to their own colour, are
  painted bottom first as scaled ellipses, and are used by the shell; the window is asked for at commit with a direct
  call; the Linux block turns every shadow off, keeps the meaningful rings as outlines, drops drop-shadow filters and
  keeps the glass highlight as a border.
* `tests/e2e/desktop-perf.mjs` (5 checks, prints the numbers): the window is asked to show within 150 ms of the
  script running, the interface is painted within a second, running downloads keep >= 45 fps and scrolling over
  them >= 40 fps, and on Linux no card or panel paints a shadow. `VELOX_E2E_DISPLAY=:0` measures on the real screen,
  `PERF_WAYLAND=1` on Wayland itself, `PERF_CSS` lays a stylesheet over the page before measuring (to find what
  costs, by hand).

## No repaint every 3 s while Smart throttle is on (October 2026)

`throttle.ts` wrote `<html data-throttle>` on every poll (every 3 s), even with the same value. Any write is an
attribute mutation, so `theme.ts`'s watcher told the backdrop (`aurora.ts`) and the segment monitor that the colours
changed: the full-window background picture was drawn and swapped twice per poll, which showed as a flash. Now the
attribute is written only when it changes, and `onThemeChange` calls back only when `paletteKey` (theme, Smart throttle
on or off, the system's dark mode) changes: "on" and "limiting" share one palette, so slowing downloads repaints
nothing. Tests: `throttle.test.ts`.

## Drop-down lists in the theme's colours (October 2026)

**Problem.** The list a `<select>` opens was drawn by the system, not the page:
* Linux (WebKitGTK): a grey GTK menu;
* macOS: the system menu;
* Windows (WebView2): most styles ignored.

The `option` colours in the CSS could never reach it. The field itself also had its own opaque `--solid` surface,
not the `.input` surface the other fields use, so it looked different from them in dark mode.

**Fix** (`apps/desktop/src/ui/selectMenu.ts`, installed once in `main.tsx`):
* **Every `<select>` stays a real `<select>`:** React state, forms, labels and screen readers are untouched.
* **Opening:** a pointer press or the keys that would open the system list (Enter, Space, F4, Alt+↑/↓) open a menu
  drawn by the app.
* **Choosing:** it sets the value with the native setter and sends a real `change` event, so every `onChange` works
  unchanged. No component changed: all 23 lists get it.
* **Menu behaviour:**
  * arrow keys, Home / End, type-to-find; disabled options skipped; group titles;
  * Escape closes and keeps the value; Tab closes;
  * a press outside, a scroll elsewhere, a resize or leaving the window closes it;
  * it opens above the field when there is no room below, and follows the field's direction (right-to-left in
    Arabic);
  * `role=listbox` / `option`, `aria-selected`, `aria-activedescendant`, `aria-expanded` on the field.
* **Look** (`.select-menu`): the fields' own surface over the solid base,
  `linear-gradient(var(--raised), var(--raised)), var(--solid)`. It is opaque and the same colour as the fields in
  light, dark, glass and solid. The text is `--fg`, the row under the pointer `--accent-soft`, and the chosen row
  `--accent`. No fixed colour.
* **Opt-out:** a `<select data-native-list>` keeps the system list. None uses it today.
* **The field:** `select.input` now uses the `.input` surface like every other field.

**Tests:**
* `selectMenu.test.ts`: installed at start-up; only theme tokens in the menu; the field's surface; keyboard skips
  disabled options.
* e2e `desktop-ui.mjs`, real app, light and dark: the app's list opens, with a background equal to the theme's
  `--solid` under the field's surface layer; Escape keeps the value; a click picks (the profile really changes); the
  keyboard opens and picks.

## Cards no longer flash when a download starts (issue #20, October 2026)

**Symptom.** Starting a download made the cards of the list fade out and back in ("appear and gone"). The
connection grid of a running card also blinked while its speed settled.

**Causes** (measured in the real app, `tests/e2e/desktop-card-flash.mjs`):
1. Every card carried `.rise`, the entrance animation (fade in from opacity 0, 220 ms, `both`). A browser restarts
   an element's CSS animations whenever the element is put back into the page, and React does exactly that:
   * Downloads list (sorted by state): a download going from waiting or paused to running moves up, and React
     re-inserts every card it passes;
   * Active page: a card that changes section (waiting / running / paused) is a new element.
   Each of those cards dropped to ~30 % opacity and faded back in. Smart queue rows had the same class.
2. The grid's pulse speed followed the download's speed through the `--pulse` variable, i.e. the animations'
   **duration**. A new duration moves each animation to another point of its beat: every lit cell jumped at once
   (100–370 jumps in a few seconds of a starting download).

**Fixes.**
1. `ui/entrance.ts`, `useEntrance(key)`: an item gets `.rise` only the first time it is drawn in the window, and loses
   it as soon as the animation ended (`animationend`, or after 600 ms if it was never painted). Moved, re-sorted or
   re-created cards keep their look. Used by `DownloadCard` and `SmartQueueRow`. A brand-new download still fades in
   once.
2. `ThreadMatrix`: the duration stays the stylesheet's (`--pulse: 1.6s`); the pace is set with the animations'
   `playbackRate` (`retime`), which keeps each animation where it is. Only the pulse (`flicker`) is retimed, never
   the cells' colour transition. Without the Web Animations API (not seen: WebView2, WebKitGTK and WKWebView all
   have it) the cells pulse at the base pace.

**Results** (Linux, before → after): existing cards replaying their entrance 2–3 → 0; lowest opacity of an existing
card 0.31 → 1.0; grid jumps 108–371 → 0. No change to the other UI tests (views, usability, ui, queue, smart queue,
perf, redesign).
