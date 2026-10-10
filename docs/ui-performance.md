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

