# Game mode (feature #7, v2.0.0)

Smart throttle already gives way to a game or a call **when it sees a listed program run**. That needs Smart throttle
on, the program in the list, and a few seconds for the scan. Game mode is the one-click version: the user says "I am
playing (or in a call) now", and downloads give way at once, whatever level is chosen (even Fast), whatever runs, until
it ends. No Settings page to open.

## What the user sees

| Where | What |
|---|---|
| Status bar | A **Game mode** button (a gamepad). One click: on. While on it is a lit amber pill, "Game mode on · 1 h 42 min" (only the gamepad and the time left in a narrow window; the words are in its tooltip and accessible name). One click: off. |
| Banner, every screen | While on: "**Game mode on** · downloads keep 612 KB/s · 1 h 42 min left", with **+1 hour** (when it has an end) and **Turn off**. Like the power-action banner, it is under the title bar on every screen. |
| The whole window | The accent turns the Smart throttle amber (`html[data-throttle]`) and `html[data-game="on"]` is set; the sidebar gauge shows Game mode's cap; its status line says "Game mode · downloads limited to 256 KB/s". |
| Tray icon menu | A **Game mode** item with a tick: one click on (for the usual time) or off. The tick follows changes made in the window or by time running out (within 3 s). Windows / macOS: the icon's tooltip says "VeloxFetch · Game mode on". |
| Keyboard | **Ctrl+G** (⌘G on macOS) turns it on or off from any screen (listed in Settings › Keyboard shortcuts). |
| Settings › Speed › Smart throttle | A **Game mode** card at the top: what it does, Turn on now / Turn off, what downloads will do, and "One click turns it on for" **1 hour / 2 hours (default) / 4 hours / Until I turn it off**. The policy below it is now labelled "While a listed program runs or Game mode is on". |
| Queue view | With the pause policy: "Paused for Game mode · 2 will resume when it ends". |
| When it ends by itself | Downloads speed back up; a desktop notification "Game mode ended — Downloads are back to their usual speed" when notifications are on and the window is not in front. |

## What downloads do

Exactly the gaming policy the user already chose (Settings › Speed › Smart throttle), so there is one place to say it:

| Policy | In Game mode |
|---|---|
| Give way (default) | downloads keep a quarter of the speed they had (at least the reserved minimum, 256 KB/s by default; 1 MB/s until the line's speed is known) |
| Low speed | the chosen limit (512 KB/s by default) |
| Pause | running downloads pause (progress kept), nothing new starts; only what Game mode paused resumes when it ends (the user's own pauses stay) |

Game mode wins over every level (Fast, Smart, Manual), over the Fast profile and over several networks at once: it is
the user's own click, now. Nothing the user set is changed: turning it off (or its end) gives back exactly what was in
force before (Fast's full speed, Manual's level, Smart's automatic mode, a listed program's own policy at the next
scan). The user's own speed limits and queue pause stay in force throughout (the lowest limit wins).

## How it works

* **State** (`manager/smart_throttle.rs`): `SmartState::game_mode: Option<GameUntil>` (`TurnedOff` or `At(unix ms)`,
  wall-clock, so a computer that slept still ends it on time) and `game_minutes` (how long one click turns it on for).
  Saved as `game_mode.until` (`0` = until turned off) and `game_mode.minutes`; a value that cannot be read is off.
* **Decision**: `smart_throttle_observe_signals` and `reapply_smart_cap` put Game mode first: `Reason::Game`, the
  policy's cap (`game_cap`) in the Smart bucket of the shared rate limits, and with the pause policy `hold` (the queue's
  `game_hold` gate, the same as a listed program's). Game mode that has ended is cleared at the next observation (the
  desktop watcher observes every 3 s, also while Smart throttle is off) and its setting deleted.
* **Command**: `set_game_mode(SetGameMode { on, minutes, add_minutes, default_minutes })` (main window only, unknown
  fields refused). `on: true` for the usual time, `minutes` for another one (`0` = until turned off); `add_minutes`
  makes it longer (never more than a day from now; on, for that long, when it was off); `default_minutes` the usual
  time. At most 24 hours. The state comes back in `SmartThrottleDto.game_mode { on, until_ms, minutes }`.
* **At start** (`resume_game_mode`): Game mode still on from before applies before the first queue evaluation (the
  pause policy's gate closed from the first moment); one that ended while the app was closed is off.
* **Desktop** (`src-tauri/src/desktop.rs`, `smart_throttle.rs`): the tray's `CheckMenuItem` "game" calls
  `set_game_mode`; `sync_tray_game` keeps the tick (and the tooltip) in line every 3 s; the watcher notices an end by
  time and calls `notify_game_mode_ended`.
* **Window** (`components/GameMode.tsx`): `GameModeButton` (status bar), `GameModeBanner` (under the title bar),
  `GameModeSettings` (Settings card), `toggleGameMode` (also Ctrl+G in `App.tsx`); `throttle.ts`'s
  `throttleStore.game` shows the change at once and the app's answer replaces it (a refused one brings the real state
  back), `gameOn`, `gameLine`, `gameSecondsLeft`, `formatLeft`. Texts `gm.*`, `tray.gameMode*`, `keys.game`,
  `qc.gameMode`, `dur.m` in all 8 languages.
* **Status bar in long languages**: to make room, the "downloaded" total and the profile switch show from 768 px,
  "No tracking, no telemetry" (the shield stays) from 1280 px; nothing is pushed out of the window in any language
  from 640 px (it was, in German and Russian, before).

## Safety

* Nothing new is looked at or sent: Game mode does not scan programs or reach the network; it only sets the cap and
  the queue gate that Smart throttle already had. The end notification is a local desktop notification.
* Never stuck: an end time is always kept (unless the user chose "until I turn it off", shown as such everywhere);
  at most a day; an unreadable saved value is off; an ended one is forgotten.
* Only the main window can change it (`set_game_mode` in `main-window.json` only); the tray item runs inside the app.

## Tests

* `download-manager` unit test: the saved end is read back safely (`0`, a time, garbage, negative) and ends on time.
* `tests/integration/tests/game_mode.rs` (5): one click slows a real download with Smart throttle off and off gives
  full speed back (file intact); the pause policy holds the queue and lets go only what it held; it wins over Manual, a
  listed program and the Fast profile and gives each back; the usual time, +1 hour, a day at most, refused requests;
  kept across a restart (the queue held from the first moment), ended while closed (what it paused resumes), ended
  while running (let go, forgotten), a broken saved value.
* `apps/desktop/src/throttle.test.ts` (+4): the status line in every case, time left, one click shown at once then the
  app's answer, the window's marks, a refused change.
* `tests/security/policy.test.mjs` (+1): main window only, strict request, a day at most, never on for ever by accident,
  nothing scanned or sent.
* `tests/e2e/desktop-game-mode.mjs` (new suite): the status bar button, the banner on every screen, +1 hour, Turn off,
  the amber window, the gauge, the real speed held and given back, Settings (card, usual time, shortcut list), Ctrl+G,
  the pause policy and the queue's words, kept after a restart, every text fits in all 8 languages from 1360×860 to
  640×480 with nothing pushed out of the status bar, refused requests, no JavaScript errors. Writes
  `docs/screenshots/game-mode.png`.
* By hand, on this PC's KDE Plasma (2026-10-16): the real tray menu read over D-Bus shows "Game mode" with a tick;
  clicking it turns Game mode on for two hours (tick on) and off (tick off); a Game mode ending 25 s after start
  unticks itself on time, is forgotten, and the desktop shows "Game mode ended — Downloads are back to their usual
  speed".
* `tests/e2e/real-game-mode.mjs` (by hand, internet), 2026-10-16: the Linux kernel from cdn.kernel.org at
  2.8 MB/s; one click: give way, cap 415–612 KB/s (a quarter of the speed), the download held at 380 KB/s once what was
  on its way drained; low 256 KB/s: 262–264 KB/s; pause: paused with its progress kept; off: back to 2.1 MB/s.
