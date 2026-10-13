# Smart Download Queue

Several download queues, each with a few plain rules, on top of the queue VeloxFetch already had. The goal is to be
simpler than a traditional download manager: one page, a tab per queue, ready-made setups, and every download says in
plain words what it is doing and why.

Branch `feat_smart_download_queue`.

## 1. What was there before (kept as it was)

The analysis, done before writing code:

| Piece | Where | What it already did |
|---|---|---|
| Run slots | `downloader-core/src/scheduler.rs` (`SlotScheduler`) | At most *N* downloads at once. A free slot goes to the highest priority (5 levels), then first come, first served. A global gate stops new starts. "Prefer" lets downloads the queue paused go first again. |
| Queue control | `download-manager/src/manager/queue.rs` | Max downloads at once and connections per download. Pause or resume the whole queue (optionally pausing running downloads, which are remembered and resumed). One daily time window ("22:00 to 07:00"). The power action after everything finishes. |
| Time logic | `crates/scheduler` | `Schedule`: is the window open, and when does it next change (pure, tested with a fake clock). |
| Per download | `manager.rs` | Priority, start / pause / resume / retry, and "Download later" (added without starting, `idle`). |
| Idle time and traffic | Smart Throttle (`smart_throttle.rs`, `sysio.rs`, `SystemIntegration::user_idle`) | Idle time per OS, the computer's other traffic, and whether Smart Throttle is slowing downloads right now. |
| UI | `views/QueueView.tsx`, `components/QueueControls.tsx`, `ScheduleSettings.tsx` | The queue page (running, waiting by priority, paused), the queue controls card, the schedule panel in Settings. |

**Missing:**
- more than one queue, and rules per queue;
- a start time for one download;
- the user's own order (move up / down);
- network and connection-type awareness;
- states that say *why* a download waits;
- "Next download";
- a timeline.

## 2. Integration approach

**Reused:**
- the one slot scheduler, with the same slots and priorities;
- the global pause, the global window and the power action, untouched;
- Smart Throttle's idle time, traffic reading and "slowing now" state;
- `SystemIntegration`, for the OS;
- the store, events and design system the UI already uses.

**Added in layers, each testable alone:**

1. **Rules, as pure logic** (`crates/scheduler/src/queue_rules.rs`). Given the time, idle time, connection,
   online state and other traffic, it says whether the queue is open, or why not. It needs no I/O and no clock
   of its own.
2. **The slot scheduler learns two things:**
   - `set_policy(rank, blocked)`: the user's order (applied after priority) and the downloads whose queue is
     closed (they keep waiting; the next one goes instead);
   - the **express lane**: a High or Highest download that only waits because less urgent downloads hold every
     slot gets *one* extra slot, so a big low-priority file never blocks an urgent one. Off by default in the
     scheduler; the manager turns it on.
3. **Database v6.** The `queue` table already existed but was unused. It gains three columns:
   - `queue_id`;
   - `start_at`;
   - `pinned` (the user chose the queue).

   A download without a row is in the main queue, in the order it was added.

   The queues themselves (name and rules) live in the settings (`queues.list`). So does what survives a
   restart: paused, started by hand, and a used start time (`queues.state`). Downloads paused by a queue rule
   are kept in `queues.held`.
4. **The manager** (`download-manager/src/manager/queues.rs`) evaluates every queue once a second, in the same loop
   as the global queue, and acts:
   - it pauses and remembers the running downloads of a queue that closes;
   - it resumes them when the queue opens;
   - it starts downloads whose start time came;
   - in a queue with rules, it starts what was added "for later" once the queue opens;
   - it moves big files to the queue made for them;
   - it gives the slot scheduler the order and the downloads to hold back.

   A download that starts is held back *at once* if its queue is closed (`policy_for_new_run`), so a free slot can
   never be taken first.
5. **The connection** (`system-integration/src/network.rs`). It reads the routing table and the adapter; nothing
   is sent:
   - Linux: `/proc/net/route` and `/sys/class/net`;
   - Windows: `GetBestInterface` and `GetIfEntry2`;
   - macOS: `SCNetworkReachability`, online or not only.
6. **The UI:**
   - the queue page keeps its controls card;
   - it adds a tab per queue, the selected queue's header (state, rule chips, Start / Pause, Edit schedule, Clear
     completed, a ⋯ menu), the "Next download" line, a 24-hour timeline when times are involved, and the rows;
   - the logic sits in `src/smartQueue.ts` (pure, unit-tested);
   - the Add dialog gains a queue choice and *Start: now / later / at a set time*.

**Rules that keep the rest of the app as it was:**
- The main queue has no rule unless the user gives it one. Someone who never opens the queue page sees the old
  behaviour: same slots, same priorities, and "Download later" still waits for the user.
- Something the computer cannot tell never holds a download back: unknown idle time, an unknown or VPN
  connection, or traffic that is not measured.
- Only downloads paused *by a queue rule* are resumed by it. A download the user paused or started by hand is the
  user's (`user_took_over`).
- The queues never touch the global pause, the global window or the power action. Both layers must agree before a
  download starts.

## 3. Rules a queue can have

| Rule | Choices | Effect |
|---|---|---|
| When | Anytime · Between *from*–*to* on chosen days · Starting at *time* on chosen days | **Between:** downloads run only inside the window; running ones pause when it ends and resume when it opens again. An overnight window belongs to the day it starts. **Starting at:** the queue opens at that time and stays open until everything in it is done, then waits for the next start time. A start time missed while the app was closed starts the queue at the next launch. A start time that already passed when the rule is saved waits for the next day. |
| Only while the computer is idle | on / off | Opens after 2 minutes without keyboard or mouse (the same threshold as Smart Throttle). When the user comes back it keeps going at least 30 s, then pauses. If the system does not report idle time, the rule is not applied and the editor says so. |
| Connection | Any · Wi-Fi only · Cable only · Not on mobile data | Closes only on a connection that is *known* to be wrong. A VPN or virtual adapter counts as unknown and never blocks. |
| Pause when busy | Never · above 256 KB/s … 20 MB/s | "Other traffic" is everything on the computer minus this queue's own downloads, so other queues count. It is smoothed (×0.5 per reading). It pauses after 3 readings above the limit in a row, and carries on after 5 readings below 70 % of it. |
| Big files go here | Off · larger than 100 MB … 5 GB | Once a download's size is known and less than a quarter of it is done, it moves to this queue: <ul><li>if the queue is closed, the download pauses there, but only if it can resume (one that cannot resume stays where it is);</li><li>a download whose queue the user chose is never moved.</li></ul> |

**Always on, for every queue:**
- **No network:** after 2 readings in a row, nothing new starts. Running downloads are left to the engine's own
  retries. A download that is moving data proves the network works.
- **Smart Throttle:** while it is slowing downloads, Low and Lowest downloads wait for the line to be free. Nothing
  running is stopped.
- **Express lane:** described above.

**Ready-made setups** in the editor:

| Setup | Rules |
|---|---|
| Night downloads | Between 23:00 and 07:00, big files over 500 MB go here |
| When I'm away | Only while idle |
| Start at 2:00 | Starting at 02:00 |
| Light on the connection | Not on mobile data, pause above 2 MB/s |

Limits: 16 queues, names up to 40 characters. The busy limit is at least 64 KB/s and the big-file size at least 1 MB.

## 4. One download

| Action | What happens |
|---|---|
| Priority | High · Normal · Low in the queue page; Highest and Lowest still show when a download has one. "Priority for all" sets a whole queue. |
| Start now | Starts it whatever its queue's rules (only a missing network still stops it). It still uses a slot and its priority. |
| Start later | Pauses it if it runs and clears its start time. |
| Start at… | A date and time, with quick choices: in 1 hour, tonight 23:00, tomorrow 02:00 or 08:00. A running download pauses until then. When the time comes it starts like "Start now". Limited to a year ahead. |
| Move up / down | Trades places with its neighbour of the same priority and state. The order is what the slot scheduler uses, after priority. |
| Move to *queue* | Puts it in that queue (pinned). If its old queue had paused it, it carries on and the new queue's rules decide. |

**The states shown on each row:**

| State | When |
|---|---|
| **Downloading** | It is running. |
| **Retrying** | It is running, but every connection (or the first request) is waiting to try again after an error. Sent live in the progress ticks. |
| **Waiting** | It waits, with the reason on the row: a free slot, the user ("Starts when you press Start"), the network, the right connection, the computer being idle, a busy line, or Smart Throttle. |
| **Scheduled** | It has a start time, or its queue's hours hold it. Also for a download a time window paused, which resumes on its own. |
| **Paused** | The user paused it. |
| **Completed** | It finished. |
| **Failed** | It failed (or was cancelled). |

## 5. The queue page

A row's share of the file is shown with one decimal ("12.0%"), like the cards and the details page
(`formatPercent`). Until October 2026 it showed the raw number ("11.992703192112195%").

**From top to bottom:**
1. The controls card it always had: downloads at once, connections, pause everything.
2. The queue tabs. Each shows the number still to do, a dot while it downloads, and an icon when it is closed.
   *New queue* is at the end.
3. The selected queue:
   - its state in words, e.g. "Scheduled · starts Wed 23:00", "Waiting until the computer is idle", or "Started
     by hand · runs until everything is done";
   - its rules as chips;
   - its actions:

     | Action | Effect |
     |---|---|
     | **Start** | Runs the queue now whatever its schedule, until it is empty, and starts its paused and "later" downloads. |
     | **Pause** | Pauses its running downloads; Start resumes them. |
     | **Edit schedule** | Opens the editor. |
     | **Clear completed** | Removes its finished downloads from the list; the files stay. |
     | **⋯ menu** | Priority for all, and moving the queue earlier or later. The queue order breaks ties between equal priorities across queues. |
4. **Next download:** which download starts next and when:
   - "starts now";
   - "in about 4 min", from the first running download expected to finish;
   - its start time or the queue's opening time;
   - or the reason it waits.
5. **Timeline:** only when the queue has a time rule or a download with a start time. It covers the next 24 hours
   from now, with the hours the queue may download as bars and set start times as dots.
6. **The rows**, in the order they will run:
   1. running;
   2. waiting (by priority, then place);
   3. scheduled (soonest first);
   4. paused;
   5. failed;
   6. finished (newest first).

**Look and feel:** the existing cards, chips, tabs, mode buttons and pills, the `rise` animation, live dots and
progress bars. It is the same design in light and dark, and mirrored in Arabic. The open tab is remembered with the
other layout preferences (`prefs.ts`).

## 6. Commands (IPC)

All five are main-window only:
- registered in `lib.rs`, `build.rs`, `capabilities/main-window.json` and `authz.rs` (75 commands);
- checked by the policy test.

| Command | Takes | Returns |
|---|---|---|
| `get_queues` | nothing | `QueuesDto` |
| `save_queue` | `{ id?, name, rules }` (strict) | `QueuesDto` |
| `delete_queue` | `id` (not the main queue; its downloads go to the main queue) | `QueuesDto` |
| `queue_action` | `id`, `{ kind: start \| pause \| clearCompleted \| moveUp \| moveDown \| priority }` | `QueuesDto` |
| `place_download` | `{ id, queueId?, start?: now \| later \| at, step?: up \| down \| top }` (strict) | `DownloadDto` |

**Also new:**
- `NewDownload` takes `queueId` and `startAt`;
- `DownloadDto` gains `queueId`, `position`, `startAt`, `retrying` and `held`;
- progress ticks carry `retrying`;
- a new `queues` event is sent when a queue's state, the network or the order changes.

## 7. Tests

| Suite | What |
|---|---|
| `crates/scheduler` `queue_rules` (11) | Windows, start times (missed, already passed, used up), days, offline, connection rules, idle with its grace, busy hysteresis, validation, strict reading, timeline spans |
| `downloader-core` `scheduler` (+5) | The user's order, blocked waiters, express lane (one extra slot, only behind less urgent downloads, off by default), a priority raised while waiting releases the right slot |
| `persistence` (+1) | Places saved, updated, moved with a deleted queue, gone with the download; `queue_rows` |
| `system-integration` `network` (3) | Default route (IPv4 / IPv6, down and unreachable routes), adapters told apart (Wi-Fi, cable, mobile, VPN, bridge, virtual), the real reading never fails |
| `download-manager` `queues` (3) | Times, defaults, broken saved queues ignored |
| `tests/integration/tests/smart_queue.rs` (14) | Regression guard (no queue = as before), night window, start-at queue, own start time + start now, pause / start queue + priority for all, idle and Wi-Fi rules, offline, order (and both moved downloads announced), express lane, Smart Throttle giving way, big-file routing, restart, delete, clear completed, bad requests |
| `apps/desktop/src/smartQueue.test.ts` (12) | States, reasons, order, moves, next download, timeline, quick times, rule chips, editor checks, store |
| `apps/desktop/src/selectors.test.ts` (1) | No store selector builds a new array or object on each read (that loops React; found by the e2e test) |
| `tests/security/policy.test.mjs` (+1) | Main window only, strict requests, validated rules, no socket or request to read the connection, no power action from the queues |
| `tests/e2e/desktop-smart-queue.mjs` (48 checks) | A running row's percentage has one decimal (fails with the raw number). The real app: tabs, editor and setups, a closed window holding a download, Start / Pause, Start at with the timeline, Start now, moving with the arrows changing which one gets the slot, the Add dialog, Clear completed, delete, hostile IPC, no CSP or script errors |

## 8. Limits and next steps

- **macOS** reports online / offline but not the connection type, so the Wi-Fi and cable rules never block there.
- **Big files** are moved once their size is known. That is after the server answers, so a big file may use a slot
  for a moment before it moves.
- **"Start now"** marks are kept in memory. After a restart, a download that was started by hand follows its queue's
  rules again.
- **Not tested on a real machine yet:** Windows and macOS connection detection. It is checked by cross-compiling.
