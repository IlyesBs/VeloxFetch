# Use a new link (expired links, sessions and mirrors)

A paused or failed download can continue from a **fresh link**: a signed link that expired, a browser session that
ended, a mirror that went away. The bytes already saved are kept **only when the new link is proven to give the
same file**. Anything less is reported to the user, and nothing changes: the saved bytes, the partial file and the
download's link stay as they were.

The point of the rules: a new link to a *different* file (same name, even same size) must never fill the gaps of
the old one. That would give a damaged file shown as complete.

Code:
* rules: `crates/download-manager/src/relink.rs` (pure, unit-tested);
* flow: `crates/download-manager/src/manager/relink.rs` (`DownloadManager::use_new_link`);
* sample request: `Downloader::sample_range` (`crates/downloader-core/src/engine.rs`);
* UI: `apps/desktop/src/newLink.ts`, `components/NewLinkDialog.tsx`, and the button in `PendingDialog.tsx`.

## 1. What was there before (audit)

| Capability | Before | Now |
|---|---|---|
| Detect 401 / 403 / 404 / expired links / ended sessions | **Done** by Download Doctor (`doctor.rs`): `link_expired`, `session_expired`, `access_denied`, `not_found`. It already tells an expired link from a real refusal (it uses the link's stated expiry, whether the download worked before, and the browser session). | Unchanged (reused) |
| Keep progress when a link expires | **Done.** A 401 / 403 / 404 stops the download as Failed. Its synced segments are kept. | Unchanged |
| Resume only the same version | **Done.** A resume checks the size and the strong `ETag` (else `Last-Modified`); every range request carries `If-Range`. A changed file starts over, never stitched. | Unchanged |
| Range answers checked (`206`, `Content-Range` start and end) | **Done** (`get_range_with`) | Reused for the samples |
| Credentials never sent to another origin on a redirect | **Done** (`http-client`) | Unchanged; also applied to a new link (§5) |
| **Give a stopped download a new link** | **Missing.** Download Doctor said "get a new link" but there was no way to use one. Re-adding the link made a second download from zero. | "Use a new link…" (§2) |
| Identity of a new link vs. the saved bytes | **Missing** | §3 |
| A browser re-click continuing the stopped download | **Missing.** The confirm dialog only offered to retry the old (expired) link. | §2 |

## 2. Where it is

* **Download card menu** (paused or failed): **Use a new link…** opens a dialog. Paste the fresh link and choose
  **Check and continue**.
* **Download Doctor** (Details page of a failed download): when its advice includes "get a new link", a **Use a new
  link…** button opens the same dialog.
* **Browser confirmation dialog:** when a link handed over by the browser matches a **paused or failed** download in
  the list (Smart Duplicate Detection: same link, same redirect target, or the same file under another link), the
  dialog says so and offers **Continue the stopped download with this link**. The browser's fresh session (cookies,
  referrer) is used for that link. The request is answered only if the link is used. Otherwise it stays in the
  dialog and can still be started as a new download.

While checking: *"New source found. Checking that it is the same file before resuming…"*. Results (texts `nl.*` in
en / fr / ar):

| Result | Says | Offers |
|---|---|---|
| Used, bytes kept | "Link replaced. Resuming from the saved progress." + the evidence | (closes) |
| Used, nothing was saved yet | "Link replaced. The download is starting." | (closes) |
| Likely the same file | "This is likely the same file, but the server does not confirm it." | **Continue anyway**, **Download it as a new file** |
| Another file | "This link gives a different file." + why | **Download it as a new file** |
| Cannot continue (no ranges / no validator / no proof) | "The saved progress cannot be continued from this link." + why | **Download it as a new file** |
| The same link and session | "This is the link the download already uses." + "get a fresh link or sign in again" | — |
| The link failed | "The new link did not work." + the server's code | — |

Every result that does not switch says **"Your saved progress (N) is kept."**

## 3. Is it the same file? (`relink::identify`)

Evidence is gathered with at most **three small requests** to the new link:
1. the usual check (`probe_quick`, `bytes=0-0`, no retries, 20 s limit);
2. at most two **64 KiB samples** (`sample_range`, `Range` + `206` + `Content-Range` checked), compared byte for byte
   with the saved partial file at the same offsets. The samples are taken only inside bytes the download recorded
   as written and synced: the start of the first saved bytes and the end of the furthest ones.

The whole file is never downloaded or hashed to identify it.

Verdicts, strongest contradiction first. Any proof of another file wins over every match.

| Verdict | When |
|---|---|
| `differentResource` | the new link gives a **web page** where the file was not one; **another size**; another **strong `ETag`**; another `Last-Modified` **on the same host** (mirrors copy files at other times, so a date only counts on the same server); **sampled bytes differ** |
| `unknown` | the size is not known on either side; or only the size matches |
| `verifiedSame` | same size **and** the same strong `ETag` (and, when bytes are saved, the samples match) |
| `likelySame` | same size, nothing contradicts, and the samples match (or, with nothing sampled, the same `Last-Modified`) |

Never enough on their own: a matching **file name**, a matching **size**, a **weak** `ETag` (`W/"…"`), a missing
`ETag`. Different links may give the same file (signed links, mirrors), and the same link may give another file over
time. The verdict never depends on the link's text.

## 4. When are saved bytes reused? (`relink::decide`)

| | Auto (Check and continue) | Confirm (Continue anyway) |
|---|---|---|
| `verifiedSame` | **yes** | yes |
| `likelySame` | no: asks (offers Continue anyway) | **yes** |
| `unknown` | no | no |
| `differentResource` | no | no (never offered) |

**Blockers.** Whatever the verdict, saved bytes are not continued from a link that:
* does not answer ranged requests correctly (`noRanges`): a `200` to a range, or a wrong `Content-Range`. Bytes are
  never appended blindly;
* gives neither a strong `ETag` nor a `Last-Modified` (`noValidator`). The engine could not notice a change during
  the download (`If-Range`), and its resume check would start over.

**Nothing saved yet** (refused before the first byte, or 0 bytes): nothing can be damaged. The download simply runs
from the new link, unless the link is proven to give another file (`differentResource` is never applied: the record
keeps its name and place).

### Switching (only when it is decided)

1. The record is re-read and saved in one step (one blocking store call). It must still be **paused or failed** and not running,
   with **exactly the same segment progress** as when the check began. Otherwise nothing is switched ("the download
   changed while the new link was checked"). A cancel, delete, resume or another switch during the check therefore
   wins.
2. The new link, its headers (sealed on disk like any download's) and the new link's validators are saved. The
   strong `ETag` is kept, a weak one never. From then on the engine's resume check and every `If-Range` use the new
   link's validators, which were just shown to describe the same file.
3. The diagnosis is cleared. A diagnostics event is written without the link, e.g. *"New link: the same file
   (confirmed by the server and by the saved bytes). Continuing from N bytes"*.
4. The download starts through the normal path (`start_download`): queue rules, adaptive connections (up to 64) and
   the scheduler apply as for any resume. Only the missing ranges are fetched.

## 5. Progress, integrity, retries, concurrency

* **Nothing saved is ever deleted by this feature.** A refused, failed or cancelled check changes nothing on disk or
  in the record.
* **Verified bytes vs. received bytes.** Only segment counters that were checkpointed after `fsync` are trusted
  (`checkpoint`). Those are also the only bytes sampled. Bytes received but not checkpointed are fetched again.
* **Writes.** The engine's existing segment rules apply unchanged: each range is written at its own offset, its
  `Content-Range` must start where asked and stay inside the segment, a short body is retried, and segments never
  overlap.
* **Final integrity.** Unchanged: the size must match exactly, and a checksum given for the download is verified
  before the file is renamed. A mismatch fails the download, never completes it (tested after a switch).
* **Restart.** The new link is in the record before the download starts again, so after a crash or restart the
  resume uses the new link. The expired one is never asked again.
* **Bounded, no loops.** A check is one user-triggered attempt with no retries. The same link with the same session
  is refused at once (`sameSource`), so an unchanged expired link is never retried. Download Doctor's retry policy
  (`Retry-After`, backoff, no endless retries of 401 / 403 / 404) is unchanged.
* **One check per download at a time.** A second request for the same download is refused while one runs (an
  in-memory claim, released however the check ends). Other downloads are not touched.

**Remaining window (known limitation).** Between the check and the engine's own resume check (moments later), a CDN
may start serving another version. The engine then sees other validators and starts that download over (existing
rule: changed files are never stitched). The bytes are lost but never mixed.

## 6. Session and token security

* **Same origin only for the download's own credentials.** For a pasted link, the download's referrer and user agent
  are kept, but its **cookies and `Authorization` only when the new link has the same origin** (scheme, host, port).
  A mirror elsewhere gets none (tested: no `Cookie` reaches the second server).
* **Browser hand-over:** the browser's headers for *that* link are used, as for any browser download. Private-window
  rules are unchanged (no cookies from a private window).
* A user name and password in a pasted link are taken out of the link into a sealed `Authorization` header, as when
  adding a download.
* Redirects keep the existing rules: credentials are dropped across origins, and there are no hops into local
  addresses.
* **The link is never logged or shown in diagnostics** by this feature. The request's `Debug` output redacts it. The
  stored link follows the existing rule for any download's link (stored as the download's address; reports go
  through the redactor, which hides query secrets: tested with `X-Amz-Signature`).
* TLS verification, URL validation (http/https only, no control characters, length limit) and the download-folder
  confinement (the partial file is read only inside the allowed folders) are unchanged.
* The IPC command `use_new_link` is **main window only** (capability, Rust authorization matrix, destructive-command
  test).

## 7. Platforms

Pure Rust and the shared HTTP client. Windows, Linux and macOS behave the same. If the browser extension is not
installed (or confirmation is off, so browser downloads start directly), the browser path is simply not offered.
The menu and Doctor paths always work.

## 8. Tests

* **Unit** (`relink.rs`, 9): strong ETag + size prove the same file under another link and on a mirror; any proof of
  another file wins (size, ETag, bytes, web page); weak or missing ETag proves nothing; a date counts only on the same
  server; unknown size; the reuse decision table and blockers; sample windows stay inside saved bytes;
  credentials never follow a link to another origin (host and scheme); the same link and session is not a new
  source, a fresh session is.
* **Integration** (`tests/integration/tests/relink.rs`, 11, mocked HTTP server with `/signed/<token>/` links that
  expire):
  * an expired link at partial progress → fresh link verified, completes byte for byte, only the missing parts
    fetched, the expired link never asked again, the secret never in diagnostics;
  * a mirror on another server is used and gets no cookies;
  * another file under the same name is refused (other size; same size and ETag but other bytes; changed ETag; a
    sign-in page): progress and partial file untouched;
  * a link without ranges, without a validator, the same link, a 404, a 403: nothing changes;
  * likely the same file waits for the user, then completes;
  * the browser's fresh session for a waiting link continues the download (no second download, the fresh cookie
    is sent);
  * only stopped downloads; a link or a waiting download is required; only web links; two checks at once: the
    second is refused;
  * cancelling during the check wins;
  * restart after the switch continues from the new link;
  * the final checksum still decides after a switch;
  * a download refused before any byte simply takes the new link.
* **UI** (`newLink.test.ts`, 4): where it is offered, which browser duplicate can be continued, what each outcome
  says and offers (never "continue" for another file), every text in en / fr / ar.
* **End-to-end** (`tests/e2e/desktop-views.mjs`): pause part-way, card menu → Use a new link → the same link is
  refused, a fresh link is proven and used, the download completes byte for byte; unknown id refused.

## 9. Known limitations

* No automatic discovery of a fresh link. The app never scrapes pages, replays private endpoints or computes
  signatures. A fresh link comes from the user (paste) or from the browser extension (a re-click on the page).
  Clipboard content is never taken as a new link on its own.
* `likelySame` needs the user's word. If the file proves different later and has no checksum, the result may be
  damaged. The dialog says so.
* Mirrors that tag the same file with **different strong ETags** are refused (`etagChanged`). This is the safe
  side; the user can download it as a new file.
* The window described at the end of §5.
* Video downloads (the video engine) are not covered: they are restarted by the video tool itself.
