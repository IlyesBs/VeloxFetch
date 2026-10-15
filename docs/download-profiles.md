# Download Profiles

A profile is a named **preset over settings the app already has**. It is not a download engine, a controller or a
second configuration system. It holds only the values it changes; every setting it leaves out keeps your own value.
One function decides the values in force, and they are handed to the existing controls.

Your own settings (Settings page, queue limits, Smart throttle switches) are never overwritten by a profile, so
**Default is always exactly your settings**.

## 1. What was there before (audit)

| Capability | Before | Now |
|---|---|---|
| Profiles of any kind | **Missing** (no presets; "Network Modes" do not exist under that name: the closest are Smart throttle's modes and its gaming policy) | Built-in Default / Fast / Balanced / Low resource, plus profiles of your own |
| Settings a profile can set | Existing, each set on its own: global speed limit, downloads at once, connections per download, Smart throttle on / automatic mode | Reused as they are: a profile only overlays them |
| Connections changed while a download runs | **Partial**: applied only to downloads started or resumed afterwards | **Improved**: running downloads follow the new ceiling at their next sample (fewer connections give way, no restart) |
| Retry behaviour | Not user-settable (engine policy) | Not exposed (see limitations) |

## 2. Built-in profiles

| Profile | Speed limit | Downloads at once | Connections | Smart throttle |
|---|---|---|---|---|
| **Default** | yours | yours | yours | yours |
| **Fast** | **none** | yours | up to **64** (the maximum) | held **off** entirely: full speed, never gives way (not to browsing, videos or listed programs); your choice comes back with another profile |
| **Balanced** | yours | yours | yours | automatic mode **on** (this also turns Smart throttle on): full speed while the line is free, gives way while you use it |
| **Low resource** | yours | **1** | **4** | yours |

Notes:

* **Fast** raises the ceiling only. The adaptive controller still starts small and only climbs while more
  connections bring more speed. It still respects a server that refuses connections (refusal hold, give-way). The
  file size limits it too (`planner::connection_limits`: from 10 MB a download may use all 64; smaller files
  fewer, never pieces under 256 KB). No speed or time is promised.
* **Low resource** reduces concurrency only. No CPU, RAM or battery figure is claimed. Integrity, checkpoints,
  pause / resume and crash recovery are unchanged.
* Built-in profiles cannot be edited, renamed or deleted. **Duplicate** one to make your own version. A "restore
  defaults" action is not needed, since built-ins never change.

## 3. Configuration schema and validation

`crates/download-manager/src/profiles.rs`, pure functions:

```text
ProfileSettings {            // every field optional: None = keep the user's setting
  speedLimitBps:  u64        // 0 = no limit; otherwise the global limit's own range (validate::speed_limit)
  maxActive:      u32        // 1..=32 (same range as Settings)
  maxConnections: u32        // 1..=64 (planner::MAX_CONNECTIONS, same as Settings)
  adaptive:       bool       // Smart throttle's automatic mode (true also turns Smart throttle on)
}
```

* Validated in Rust (`validate_settings`, `validate_name`). The UI checks are only a convenience.
* `deny_unknown_fields`: a field the app does not know (say `skipTls`) is refused, never ignored. No profile field
  touches paths, TLS, integrity, credentials, appearance or system settings.
* Names: trimmed, 1–40 characters, no control characters, unique among your profiles (case ignored).
* At most 20 profiles of your own. Ids are `c1`, `c2`… and are never reused.
* An invalid request changes nothing and saves nothing (`profile_action` checks, saves, and only then applies).

## 4. Precedence (one rule)

```text
your settings (Settings page, queue limits, Smart throttle switches)        = base
  + the active profile's values (only where it has one)                     = effective  ← profiles::resolve
  + per-download and per-type speed limits                                  ┐
  + Smart throttle's cap (automatic mode / gaming policy) and its pause     ├ existing layers, unchanged:
  + the queue gate (manual pause, schedule, gaming pause) and priorities    ┘ the lowest limit wins
```

* **Speed:** the profile only sets the limiter's *global* layer (`RateLimits::set_global`). Per-download limits, the
  limits per kind of file and Smart throttle's cap are separate layers, and the lowest one always wins. A profile
  can never lift a per-download limit or a Smart throttle cap.
* **Smart throttle:** the profile's `adaptive` value sits beside your switches (`SmartState::profile`).
  * In force: `on() = your switch || profile == Some(true)`; `automatic() = profile.unwrap_or(your choice)`.
  * Gaming policies and the program list are yours and are never changed. They apply whenever Smart throttle is on.
* **Changes you make while a profile is active** go to your own settings. They take effect where the profile leaves
  that setting alone, and come back fully with Default. Beside the controls a profile replaces, the UI says so: "The
  'Low resource' profile uses 1 while it is active; this is your own setting."
* **Queue and priorities:** unchanged. A profile never starts, resumes, pauses or reorders anything by itself.
* No competing controller: `DownloadManager::apply_profile` is the only place profile values are put in force. It
  runs at start-up, after a profile action, and after you change one of the settings it overlays. When the values in
  force did not change, it does nothing.

## 5. Runtime application

| Setting | Applied | Running downloads |
|---|---|---|
| Speed limit (global layer) | at once | at once (the shared limiter) |
| Downloads at once | at once (`SlotScheduler::set_max_slots`) | a higher number lets downloads *waiting for a slot* start. A lower number never stops anything: it applies as running ones finish. A paused download is not waiting, so it stays paused |
| Connections per download | at once (`Downloader::set_max_connections`) | running transfers read it at every sample (`spawn_sampler` → `AdaptiveController::set_ceiling`). Lower: the extra connections give way after their current piece (`GatePermit::give_way`), with no restart and no lost bytes. Higher: the controller may probe up to it, still only while it pays |
| Smart throttle automatic mode | at once (`smart_profile_changed`: the cap is recomputed or lifted) | at once |

Switching profiles never cancels, restarts or resumes a download. It never discards progress or touches partial
files. Example: Balanced → Fast mid-download keeps the bytes, the requests and the order; only the ceiling and
Smart throttle's mode change.

## 6. Lifecycle and persistence

* Operations (`profile_action`, one command with a tagged `kind`):
  * `select`, `create`, `duplicate` (any profile, built-in included);
  * `update` (rename and/or change settings, profiles of your own only);
  * `delete` (your own only).
* **Deleting the active profile** switches to Default, which is your settings. Downloads and other settings are
  untouched.
* **Stored as one settings value**, `profiles` (JSON `{version: 1, active, custom, nextId}`), written through the
  existing settings store (SQLite, transactional). No migration and no new table. Your existing settings keep their
  own keys and values; nothing is reset to introduce profiles.
* **Loading** (`Saved::parse`):

  | Saved value | Result |
  |---|---|
  | Missing | Default |
  | Unreadable JSON, or a newer version | Default (logged) |
  | Invalid custom entries (bad id or name, values out of range, duplicate ids) | Left out |
  | An active id that no longer exists | Default |
  | `nextId` lower than an existing id | Raised |

  Start-up never fails because of it, and no other setting is changed.
* **Download records** never reference a profile, so renaming or deleting one cannot affect them.

## 7. Interface

* **Settings → Speed → Download profiles:**
  * every profile with what it is for and what it changes ("Up to 4 connections", "Keeps your settings");
  * the active one is marked;
  * Duplicate on every profile; Edit and Delete (asks once more) on your own;
  * "New profile" opens an editor where each setting is "Keep my setting" or a value;
  * an "In force now" summary, and what applies at once to running downloads.
* **Status bar:** the active profile, with a list to switch.
* **Notes beside the controls a profile replaces:** queue limits, global speed limit, Smart throttle (its own
  switches stay as you set them; the dashboard switch is disabled with an explanation while a profile has turned it
  on).
* UI texts in en / fr / ar (`prof.*`). No technology names, no speed promises.

## 8. Security and compatibility

* Two new commands, `get_profiles` and `profile_action`:
  * main window only (`authz`, capability file, build manifest, destructive-commands test list);
  * typed DTOs with `deny_unknown_fields`.
* A profile cannot reach path safety, TLS, integrity checks, credentials or the network settings: those are not
  fields.
* The engine's hard limits still apply: connections 1–64, the planner's size-based ceiling, refusal handling, and
  Smart throttle's reserved minimum.
* Rust ⇄ TypeScript: `ProfileSettings`, `EffectiveSettings`, `ProfileInfo`, `Profiles`, `ProfileAction`, the
  `profiles` event and `SmartThrottle.profileAdaptive` mirror the Rust types.

## 9. Tests and results

| Where | What |
|---|---|
| `download-manager` `profiles::tests` (7) | Default = your settings; built-ins change only what they name; Fast never turns Smart throttle on; set / lift the limit; invalid values, names and unknown fields refused; lifecycle (unique names, duplicate, rename, invalid edit changes nothing, built-ins protected, delete active → Default, ids never reused, the 20 limit); missing / corrupt / newer / partly invalid saved data; DTO order and effective values |
| `downloader-core` `adaptive::tests::the_ceiling_follows_the_settings_while_running` | Lower → down at once, raise → may probe, never below 1 |
| `tests/integration/tests/profiles.rs` (6) | Without profiles nothing changes and downloads work; a profile never overwrites your settings (queue DTO and limits keep yours; your changes come back with Default); switching mid-download lowers the connections without a restart, keeps progress, never asks for the file from byte 0 again, byte-exact result; a manual pause stays paused across profiles, a waiting download may start, Low resource runs one at a time; Balanced / Fast / Default and Smart throttle (your switches untouched), a per-download limit untouched; persistence across a restart, invalid requests not saved, delete active → Default, corrupt saved data and an unknown active id at start-up |
| `apps/desktop/src/profiles.test.ts` (5) | Names in the user's language, lines per setting, what is in force, which settings are replaced, copy names, every key in en / fr / ar |
| e2e `desktop-throttle.mjs` | Real app: Default = base; four built-ins listed; select by click; status bar switch; Balanced turns the automatic mode on while your switch stays off; invalid / built-in delete / unknown field refused over IPC; duplicate; delete the active copy → Default |

Results are in `docs/testing.md` and the commit message.

## 10. Known limitations

* **Retries and the schedule** are not profile fields. Retries are not user-settable today. The schedule is a
  separate feature with its own on / off.
* **Downloads at once:** lowering it does not stop running downloads (by design: no surprise pauses). It applies as
  they finish.
* **Connections:** a download that is not adaptive (one connection by nature, a server without ranges) is not
  affected. A higher ceiling is only used where the file has enough parts (planned when it started).
* **Smart throttle's status text** right after Balanced is selected may say "off" until its next program scan (a
  few seconds), as when turning it on by hand.
* Built-in profiles are fixed. To change one, duplicate it.
