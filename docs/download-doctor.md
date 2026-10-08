# Download Doctor

When a download fails, or while the engine retries on its own, Download Doctor says:
- what happened;
- where the problem is (the server, the network, this computer, the link, the sign-in, or the content);
- whether retrying can help, and when;
- what else the evidence shows;
- what the user can do.

Every answer comes from what was observed. When the evidence is not enough, the answer is "unknown". The Doctor
only diagnoses: the engine's retry loop stays the only automatic recovery.

Branch `feat_download_doctor`, based on `feat_download_intelligence`.

## 1. What was there before

The audit, done before writing code:

| Capability | Status before | Where |
|---|---|---|
| Automatic retry with backoff, jitter and `Retry-After` | **Done**, kept as it is | `downloader-core/src/retry.rs`: 3 classes (Retryable / NonRetryable / UserActionRequired); 5 retries; a separate budget of 40 retries for 429 / 503 with fewer connections; `Retry-After` honoured up to 300 s; cancellation aborts the wait |
| Plain sentence and stable code for an error | **Partial** | `explain.rs`: covers HTTP statuses, disk full (with numbers), permissions, folder, web page, checksum, file exists, incomplete, redirect refused, DNS, timeout, connection, proxy. Missing: TLS, reset vs refused, redirect loop, a range failure sentence, persistence, invalid link, cancellation |
| Per-download diagnostics | **Done**, extended | `manager/diag.rs`: attempts, retries, HTTP status, peak connections, speed, a timeline (server answer, retries, throttling, resume, restart from zero), a report |
| Safe resume and integrity | **Done**, kept as it is | `engine.rs`: `check_resumable` starts over when the file changed or ranges stopped, after deleting the stale part; a checksum mismatch deletes the part and is never completed; size mismatch fails |
| Root-cause correlation, 5 retry classes, severity, cause location, contributing factors, actions | **Not implemented** | — |
| TLS failures | **Incorrect** | Certificate errors and non-TLS servers were retried 5 times with backoff (6 connections for one check), although they never fix themselves |

## 2. Design

```
engine error + run events + saved diagnostics + record + network reading
   └─► manager/doctor.rs  failure_of(e) → Failure     (structured error, transport causes only)
                          failure_evidence(...) → Evidence (read-only)
   └─► doctor.rs          diagnose(&Evidence) → Diagnosis  (pure)
   └─► DiagRecord.doctor (saved) + Inner.doctor (in memory) → DownloadDto.doctor, DiagnosticsDto.doctor
   └─► UI: DoctorPanel (detail view error card) + live notice while retrying
```

- **Pure rules** in `crates/download-manager/src/doctor.rs`: `Failure`, `Evidence`, `diagnose`, and
  `Failure::from_code` (the codes of `explain::error_code`).
- **Gathering** in `manager/doctor.rs`:
  - `failure_of(&DownloadError)` reads the structured error. For transport errors it reads the error's *source
    chain* only, never the top message, which names the link.
  - `failure_evidence(id, e, progress)` collects the evidence (§4).
  - `doctor_for(rec, progress, retrying)` serves the list (§5).
- **Recording** in `manager/diag.rs` `note_end`:
  - the evidence is gathered before the diagnostics are updated;
  - `same_as_before` and `worked_before` come from the saved record;
  - the diagnosis is saved in `DiagRecord.doctor`, kept in `Inner.doctor`, and written to the timeline as
    `Doctor: <code> - problem with: <Source> - retry: <Class> - <Severity>`, just before the `Failed (...)` line
    (which stays last).
  - `note_start` and deleting a download clear the diagnosis held in memory.
  - A successful run clears the saved diagnosis and `last_failure`.

## 3. Failures and their diagnosis

`Failure` is the observed failure. `Category` is the diagnosis after correlation.

| Observed | Category | Problem with | Retry class |
|---|---|---|---|
| DNS / connect / timeout **and** the system reports no route (`online == Some(false)`) | `offline` | network | retryAfterUserAction |
| DNS, online or unknown | `dns` | network | unknown (a wrong link or a DNS fault: text cannot tell) |
| timeout, `Stalled`, `SlowToAnswer` | `timeout` | network | retryable |
| reset, broken pipe, closed, unexpected EOF, body error | `connectionReset` | network | retryable |
| refused / unreachable (connect error) | `connectionFailed` | network | retryable |
| certificate refused, server not speaking TLS, handshake alert | `tls` | server | notRetryable |
| 401 | `authRequired` (`sessionExpired` if data had arrived with a session) | account | retryAfterUserAction |
| 403 | `linkExpired` if a signed link's stated expiry passed, or a signed link that worked before; `sessionExpired` if it worked with a session; else `accessDenied` | link / account / server | retryAfterUserAction |
| 404 / 410 | `linkExpired` if the signed link's expiry passed; else `notFound` | link | notRetryable (retryAfterUserAction when expired) |
| 407 | `proxyAuth` | network | retryAfterUserAction |
| 408 | `requestTimeout` | server | retryable |
| 409 | `conflict` | server | unknown |
| 416, range not honoured mid-transfer | `rangeFailure` | server | retryable (Retry re-checks the server and restarts safely) |
| 429 | `rateLimited` | server | retryableWithDelay (+ `retryAfterSecs`) |
| 500 / 502 / 503 / 504 | `serverTemporary` | server | retryableWithDelay (+ `retryAfterSecs`) |
| other 5xx | `serverError` | server | notRetryable |
| other status | `httpOther` | server | unknown |
| too many redirects, loop, refused redirect | `redirect` | link | notRetryable |
| disk full, quota (`numbers` when the engine gave them) | `diskFull` | this computer | retryAfterUserAction |
| permission denied, file in use | `permission` | this computer | retryAfterUserAction |
| folder outside the allowed ones | `folderNotAllowed` | this computer | retryAfterUserAction |
| other I/O | `writeFailed` | this computer | unknown |
| file exists | `fileExists` | this computer | retryAfterUserAction |
| checksum mismatch | `integrity`, retryable once; the same failure twice → notRetryable | content | — |
| short body, size mismatch | `incomplete` | server | retryable |
| state store error | `persistence` | this computer | unknown |
| web page instead of the file | `sessionExpired` if it worked with a session, else `webPage` | account / link | retryAfterUserAction |
| bad scheme, bad URL, bad header | `invalidResource` | link | notRetryable |
| bad proxy | `proxyInvalid` | network | retryAfterUserAction |
| cancelled | `cancelled` | — | notRetryable (severity info) |
| anything else | `unknown` | unknown | unknown |

The network is blamed only with evidence: a network failure, or `offline` from the system's reading. A failure on
the disk, a server status, or the content is never put on the network, whatever happened before.

## 4. Evidence

| Field | From |
|---|---|
| `failure` | `failure_of(e)` |
| `retry_after_secs` | `HttpError::Status { retry_after }` (0 ignored) |
| `run_retries` | `ProgressHandle::retries()` |
| `drops` | the run's `Retrying` events whose error code is timeout / connection / incomplete |
| `throttled`, `started_over` | the run's `Throttled` / `StartingOver` events |
| `worked_before` | bytes received in this run, the record's `downloaded`, or bytes from earlier runs (`DiagRecord.bytes`) |
| `same_as_before` | `DiagRecord.last_failure` (kept across starts, unlike `error_code`) equals this failure's code |
| `session` | the record's request headers include `Cookie` / `Authorization`, or the link has a user name |
| `temporary_link`, `link_expired` | `intelligence::expiring(url)` (the Download Intelligence rule) and its stated expiry vs now |
| `online` | `system.network()` read now, only after DNS / connect / timeout failures. It reads the routing table only, sends nothing, and runs off the async threads |
| `resumable`, `downloaded` | the record |
| `space` | "N bytes needed, M available" in the engine's message |
| `retry_at_ms` | while retrying: the last `Retrying` event's time plus its delay |

**Contributing factors** are added only when shown by the evidence:
- `offline`;
- `unstableConnection`: 2 or more drops while data was arriving, for network, server or unknown causes only;
- `serverThrottled`;
- `repeatedRetries`: 3 or more retries;
- `workedBefore`: for session, expired, denied or not-found causes;
- `sessionUsed`: for account or link causes;
- `temporaryLink`;
- `sameAsBefore`;
- `startedOver`;
- `noResume`: the server cannot resume and a retry would start from zero.

**Severity:**
- `warning` while the engine retries;
- `info` when cancelled;
- `critical` for integrity and persistence problems;
- `recoverable` when retryable (or unknown);
- `blocking` otherwise.

## 5. Where it shows

- **`DownloadDto.doctor`:**
  - failed: the diagnosis made when it failed (in memory). After a restart, a diagnosis is rebuilt from the saved
    error text and the record, with less evidence.
  - downloading and `retrying`: a live diagnosis with `retryAtMs`.
  - otherwise `None`.

  The list and ticks are unchanged, so normal downloads carry nothing.
- **`DiagnosticsDto.doctor`:** the saved diagnosis while the download is failed.
- **Detail view:**
  - under the error sentence, `DoctorPanel` shows the headline and a severity chip, where the problem is, the space
    numbers ("free about X"), whether retrying helps or the wait the server asked for, the factors, and the advice;
  - the error card's Retry button is labelled "Restart safely" or "Retry now" when that fits;
  - "Open download folder" is shown for a full disk;
  - while the engine retries, an amber notice says "Server temporarily unavailable (HTTP 503) · the server, not
    your connection · Retrying on its own in 30 s".
- **Actions are only real ones:**
  - buttons: Retry (existing `retry_download`), Sign in (existing dialog, `needs: signIn`), Open download folder
    (existing `open_download_folder`), Cancel (existing);
  - advice only (no button): sign in again from the browser, get a fresh link, check the connection, check
    Settings > Network, close the program using the file or choose another folder.
- Texts: 74 `doc.*` keys in `locales/{en,fr,ar}.json`. `explain.rs` has new codes: `redirect_loop`,
  `range_not_honored` (now a sentence), `persistence`, `invalid_request`, `cancelled`.

## 6. Retry intelligence

- The engine's retry loop is unchanged except for one fix:
  - `HttpError::is_retryable` now refuses transport errors whose causes say `invalid peer certificate`,
    `certificate verify failed` or `InvalidContentType` (a server that does not speak TLS);
  - helper `tls_will_fail_again`, exposed as `HttpError::is_tls_permanent`;
  - other handshake failures stay retryable.

  The effect: a bad certificate makes 2 connections (`HEAD` plus the ranged `GET`) instead of 6.
- `Retry-After` is still honoured by the engine (capped at 300 s). The Doctor reports it (`retryAfterSecs`) and
  starts no retry of its own: no new retry loop, no retry storm.
- Not retried by the user's Retry without a change:
  - 404 / 410 / other 5xx / redirect / invalid link (`notRetryable`);
  - a second identical checksum mismatch (`notRetryable`).

  The Retry button stays available, because a fixed server may still work, but the panel says it will not help.
- **Range / resume:** the engine's `check_resumable` re-evaluates on Retry and starts over safely (it deletes the
  stale part and plans a new transfer). Incompatible data is never appended.
- **Integrity:** the part is deleted on a mismatch and the download is never marked completed (checked again by a
  test).

## 7. Safety and performance

- **Read-only:** the Doctor never retries, starts, writes, deletes or requests anything. This is checked by the
  policy test "Download Doctor".
- **No secrets:**
  - the diagnosis holds codes, classes, counts and byte numbers only (no free text; checked by the policy test);
  - transport errors are read from their causes, not from the top message that names the link;
  - the timeline line goes through the existing redaction;
  - nothing is logged by the Doctor.
- **Bounded cost:** at most one record read and one network-state read per failure. Nothing at all while
  downloads succeed. The live diagnosis reads the run's events (40 at most) only while retrying.

## 8. Tests

| Suite | What |
|---|---|
| `download-manager` `doctor` unit tests (10) | 503 / 429 / 501 / 408 / 409 classes and `Retry-After`; 404 not retried; 401, 403, an expired session, an expired signed link, a signed link that worked, a sign-in page; offline vs DNS vs timeout vs reset (instability factor), TLS, unknown online state; disk full with numbers, permission not blamed on the network, persistence critical; integrity once retryable, then not; range failure restarts safely with no resume; a live retry is a warning; unknown and cancel; codes round-trip and are safe |
| `download-manager` `explain` (+6 asserts) | `redirect_loop`, `range_not_honored` sentence, `persistence`, `invalid_request`, `cancelled` |
| `tests/integration/tests/doctor.rs` (7) | Real failures: 500 and 429 (server, retry with delay, network not blamed); 401 and a web page; reset, hang, refused port, `.invalid` DNS, and offline (via `DryRunSystem::set_network`); TLS to a plain server, **not retried** (≤ 2 connections; 6 before the fix, verified); 1 PB disk full with numbers; wrong checksum: critical, then not retryable after the second identical failure, never completed, and diagnostics plus timeline carry it; a refused port then a server: Retry completes and the diagnosis goes; successful and cancelled downloads have none |
| `apps/desktop/src/doctor.test.ts` (4) | Lines, countdown, buttons vs advice, every key in en / fr / ar |
| `tests/security/policy.test.mjs` (+1, 42 total) | Pure rules; the glue changes nothing and logs nothing; causes only; no free text in a diagnosis; the TLS rule |
| `tests/e2e/desktop-diagnostics.mjs` (+4 checks) | The real app: a 404 shows the Doctor panel (not found, not retryable, the link, a fresh link), and the timeline names the diagnosis before the failure; screenshot `docs/screenshots/doctor.png` |

Other test files touched: `tests/integration/src/lib.rs` gained `serve_on(addr, data)` (a server on a chosen address,
for the recovery test). `store.test.ts` and `smartQueue.test.ts` fixtures gained `doctor: null`.

**Results on Linux, 2026-10-08:**
- Rust workspace: 641 passed, 0 failed.
- vitest: 76 passed; `tsc` clean; clippy and fmt clean.
- policy: 42/42.
- e2e: `desktop-diagnostics`, `desktop-usability`, `desktop-ipc`, `desktop-views` and `desktop-intelligence` all
  pass.

**Fixed while testing:**
- The first version wrote the Doctor's timeline line *after* the failure line, which broke the existing rule that
  the failure is the last line. It now comes just before it.
- An early version reused the queue loop's network reading, which could be up to 30 s old and report "online" just
  after the connection dropped. It now reads the state at the moment of the failure.

`detail-error-dark.png` and `detail-diagnostics-failed.png` were refreshed because they show the panel.

## 9. Limits and next steps

- **After a restart,** a failed download's diagnosis is rebuilt from its saved error text, so transport detail
  (TLS, reset, DNS from the cause chain) and run history are lost. Opening its diagnostics shows the saved full
  diagnosis.
- **DNS failures while online** stay `unknown`: a wrong address and a DNS outage look the same.
- **No "Change link" or "Change folder" for a failed download** exists in the app, so those are advice, not buttons.
- **TLS:** only certificate refusals and non-TLS servers are treated as permanent. Other handshake failures are
  still retried by the engine.
- **Rollback:** remove `doctor` from `DownloadDto` and `DiagnosticsDto`, the two `doctor.rs` modules, the diag
  fields (`last_failure`, `doctor`: old saved JSON is read with defaults), and the `tls_will_fail_again` clause.
  There is no database migration.
