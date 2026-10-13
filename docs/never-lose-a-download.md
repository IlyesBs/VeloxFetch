# Never lose a download (October 2026)

A download that fails because of the network or a server's passing trouble is no longer left failed: it is tried
again on its own, from its saved bytes, until it finishes or its tries run out. Code:
`crates/download-manager/src/manager/recovery.rs`.

## 1. Before

* Inside one run, the engine retries a failed request 5 times (backoff 0.5 s to 8 s, about 15 s in all; 40 times for
  429 / 503). Then the download was **Failed**, and only the user's Retry continued it.
* The network dropping for a minute, Wi-Fi changing, or the computer going to sleep therefore left downloads failed
  until someone noticed. A night download that met a 2-minute outage at 01:00 was still failed in the morning.

## 2. What it does now

| Situation | What happens |
|---|---|
| A failure the network or the server may get over: no connection, DNS, timeout, a dropped connection, a part cut short, HTTP 408 / 425 / 429 / 5xx | the download waits, then is tried again on its own: 10 s, 30 s, 1 min, 2 min, 5 min, then every 10 min, **20 tries** at most (about 3 hours online) |
| The computer is offline | no try is spent and nothing is sent; the card says "No internet connection: it continues on its own when the connection is back"; it starts as soon as the computer is online |
| The computer slept | the one-second loop sees a gap of 30 s or more (either clock: Linux stops the monotonic clock in sleep, Windows does not); every waiting download is tried at once |
| A download running when the computer slept | its connections stall; the engine notices within 30 s (`read_timeout`), retries, and if the network is not back yet the download waits for it as above |
| The tries run out | it stays failed; Download Doctor explains it; the saved bytes stay for Retry |
| A failure that needs the user (sign in, 403 / 401 / 407, 404 / 410, disk full, no permission, another file, a web page, a checksum) | never tried on its own (unchanged) |

Every try continues the saved bytes, with the resume checks of `docs/resume-integrity.md` (a changed file starts
over, never mixed).

**The user always wins.** Retry tries now and counts from the start again. **Stop trying** (the card's menu; the
same as Pause) stops the tries and keeps the saved bytes; the download is then paused. Cancel and Remove forget it.
Download again starts over as before.

**In memory only.** After the app restarts, a failed download waits for the user: it may have failed days ago.
(Downloads that were *running* when the app closed are paused and resume as before.)

## 3. How it shows

* The card, under the error: "Trying again on its own in 30 s" (counting down every second), "Trying again now…",
  or "No internet connection: …". In English, French and Arabic.
* The Queue page shows such a download as **Retrying**, not as failed.
* Download Doctor gives the time of the next try (`retryAtMs`).
* The diagnostics timeline: "Trying again on its own (try 3 of 20)", then the run's own lines.
* `DownloadDto.autoRetry`: `{ atMs, waitingForNetwork, tries, maxTries }`, `null` when not waiting.

## 4. Design

* `finish()` (end of every run): a failure whose `explain::error_code` is one of the passing troubles
  (`recovery::recoverable`) is put in `Recovery.waiting` with its next time. Success, pause and cancel clear it.
* The queue loop (every second) calls `recovery_turn(online, seen_offline)`. "Offline" is the same judgement the
  Smart Download Queue uses (the routing table, confirmed twice; a download that makes progress proves the network
  works). The network is looked at every 5 seconds while something waits.
* A due download is started by the same path as the queue's own starts (`start_inner`), so it waits for a free slot,
  respects the queues' rules and limits, and is counted in diagnostics.
* No new command: "Stop trying" is `pause_download`, which now accepts a failed download that is waiting.
* `ManagerConfig.recovery_delays` and `recovery_max_tries` (tests use milliseconds). `DownloadManager::system_resumed`
  can be called by an operating-system wake event too.
* A stall ("the server stopped sending") is now classed `timeout` (it was `other`).

## 5. Safety

* Only the download's own address and headers are used again (nothing new is sent anywhere).
* Bounded: at most 20 tries, at least 10 s apart, and only for failures the network or the server may get over; a
  server that keeps answering 5xx costs one probe and a few requests every 10 minutes.
* Offline: nothing at all is sent.
* A try that finds another file starts over cleanly (the resume checks), never joins two versions.

## 6. Tests

* `recovery.rs` unit tests: which codes are tried again; the waits (10 s … 10 min, about 3 hours in all); a sleep
  seen on either clock, not when the clock is set back.
* `explain.rs`: a stall is `timeout`.
* `tests/integration/tests/recovery.rs` (7): a server in trouble is tried until it finishes, from the saved bytes, no
  start over; offline, no try is spent and nothing is asked, then it continues when the network is back; after a
  wake-up a download due in an hour is tried at once; it gives up after its tries and the Doctor announces no next
  try, then the user's Retry finishes; 403 and 404 are never tried; Pause stops the tries and keeps the bytes (a
  failed download that is not waiting still cannot be paused); Cancel and Remove forget it.
* `autoRetry.test.ts` (4): the countdown, the offline line, nothing for others, Retrying in the queue.
* `timeline.test.ts`: the new line in French and Arabic.
* `desktop-usability.mjs` (+4, the real app): a server answering 502 fails, the card counts down, its menu offers
  Stop trying, and once the server is fine it finishes with no click.
