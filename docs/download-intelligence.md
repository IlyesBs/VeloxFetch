# Download Intelligence

A safe look at a link before it is downloaded: what it is, how big it is, whether a stopped download can continue,
how the download will start, whether the file fits, whether the app already has it, and what deserves a warning.
Every label and warning comes from something observed (the link, the server's answer to one quick check, the drive,
the downloads already known). There is no score and no prediction, and the analysis uses no AI or LLM.

Branch `feat_download_intelligence`.

## 1. What was there before

The audit, done before writing code:

| Capability | Status before | Where | What was missing |
|---|---|---|---|
| Resource analysis | **Partial** | `http-client` `probe_with`: `HEAD`, then `Range: bytes=0-0`. It gives the final URL, `Content-Length`, `Content-Type`, `Content-Disposition`, `Accept-Ranges`, the verified range result, `ETag`, `Last-Modified` and `Server`. `manager/pending.rs` used it for the browser confirmation dialog (size, type, a yes/no "resumable", file name). | The dialog showed only size, type and yes/no. Redirects, HTTPS, validators, sign-in, temporary links and streams were not reported. The Add dialog had no check at all. |
| Strategy | **Not exposed** | `downloader-core` `planner::connection_limits` (start and ceiling per size) plus the adaptive controller at run time. | Nothing told the user before the start. |
| Resume | **Partial** | `ResourceInfo::supports_parallel()` (range verified and size known), shown as yes/no. | No four-level class. No validator check. A server that announces ranges was not told apart from one that really serves them. |
| Health | **Not implemented** | — | — |
| Disk space | **Partial** | The engine refuses a download that cannot fit *when it starts* (`engine.rs`, `StorageFull`). The dashboard shows the drive's free space. | No check before the download. No FAT32 4 GiB limit. |
| Duplicates | **Partial** | The Add dialog compared the exact URL in the UI store. The batch dialog used `batch::normalize` (fragment removed). | No backend check. No final-URL or same-file (ETag + size) match. Nothing in the confirmation dialog. History was ignored. |
| Sign-in | **Partial** | The engine detects a web page instead of the file (`filename::web_page_instead_of_file`). `explain.rs` words 401 and 403 after a failure. | No classification before the download. |
| Temporary links | **Not implemented** | `diagnostics::redact` hides query values in *logs*. | The dialog showed the raw link, signature included. |
| Security findings | **Not implemented** | — | — |
| Cancellation | **Missing** | The pending probe kept running after the user answered (its result was then dropped). | — |

Nothing was rewritten. The probe, the planner, the adaptive controller, the engine's own space check, the redaction
rules and the dialogs were all reused. The analysis only reads them.

## 2. Design

```
link ──► manager/intelligence.rs (gathers, read-only)
          ├─ downloader.probe_quick   (HEAD + 1-byte range, no retry, 20 s cap)  ── the request a download makes first anyway
          ├─ fs4::available_space      (nearest existing parent of the destination folder)
          ├─ sysinfo disk list         (file system type, only for files ≥ 4 GiB)
          └─ store.list + history(500) (known downloads, for duplicates)
        ──► intelligence.rs  assess(Observed) -> AnalysisDto   (pure: no I/O, no clock, no async)
        ──► PendingDto.analysis (confirmation dialog)  |  analyze_link (Add dialog)
```

- **Pure rules** (`crates/download-manager/src/intelligence.rs`): `assess`, `expiring`, `display_url` and
  `find_duplicate` are functions of their inputs. The clock is passed in (`now_ms`), and so is the connection plan
  (`plan(size)`). All of them are unit-tested without a network.
- **Gathering** (`crates/download-manager/src/manager/intelligence.rs`): `DownloadManager::analyze(url, headers,
  requested_name)` returns `(Option<ResourceInfo>, AnalysisDto)`. The confirmation dialog still gets the raw info
  for its existing fields, so nothing is asked twice.
- **The strategy reuses the engine's rule.** `Downloader::connection_plan(size)` is new and calls
  `planner::connection_limits(size, max_connections, min_segment_size)`, the function the engine calls when a
  download starts. The analysis only *reports* the start; the adaptive controller still decides at run time.

## 3. The rules

### Kind
| Kind | When |
|---|---|
| `videoPage` | `media_engine::is_site_page(url)`. No request is made; the video dialog reads it. |
| `stream` | `.m3u8` / `.mpd` link, or a `Content-Type` of an HLS / DASH description. No request is made for links. |
| `webPage` | `web_page_instead_of_file(info, name-the-link-suggests)`: HTML where a file with another extension was expected, and no attachment name. This is the same rule the engine uses to refuse the download. |
| `file` | Any other answer. |
| `unknown` | The check failed. |

### Resume (`ResumeSupport`)
| Value | Evidence |
|---|---|
| `supported` | `206` with `Content-Range: bytes 0-0/N` (verified), N > 0, **and** an `ETag` or `Last-Modified`. The engine resumes with `If-Range`, so a changed file is noticed. |
| `likelySupported` | Verified ranges and a known size, but no validator. Resume works unless the file changes on the server (finding `resumeUnverified`). |
| `notSupported` | The whole file (`200`) came back for a 1-byte request, or ranges were verified but the total is unknown. The engine downloads such files in one go and cannot resume them (finding `noResume`). |
| `unknown` | No answer, or an ambiguous one (for example a `206` without a usable `Content-Range`). |

`Accept-Ranges: bytes` alone never counts: the server's *behaviour* decides (`rangeClaimed` vs `rangeVerified` in the result).

### Strategy
| Value | When |
|---|---|
| `parallel` (+ `connections`) | A file with verified ranges and a known size, and an engine ceiling above 1. |
| `single` | A file whose size is known but which cannot be split (no ranges), or one too small to split (ceiling 1, under 1 MB). |
| `conservative` | Size unknown, a slow or failing server, a web page, or no answer. One connection and no assumptions. |
| `stream` | A stream or video page (the video dialog). |

### Sign-in (`Access`)
| Value | Evidence |
|---|---|
| `authRequired` | `401`. Also: redirected to a sign-in page (see below) although cookies or `Authorization` were sent, meaning the session given was not enough. |
| `browserSessionRequired` | A web page came back after a redirect to a sign-in address (path segment starting with `login`, `signin`, `sign-in`, `sign_in`, `logon`, `auth`, `sso`, `oauth`, or a host starting with `login.`, `signin.`, `accounts.`, `auth.`, `sso.`), and no credentials were sent. |
| `public` | A file came back and no credentials were sent. |
| `unknown` | Everything else. `403` alone proves nothing (it can mean an expired link, a region block or hot-link protection), so it gets the `accessRefused` finding but no access class. A file fetched *with* the browser's session gets `unknown` plus `sessionSent`: whether the session is needed cannot be known without a second request, and none is made. |

### Temporary links (`expiring`)
Conservative on purpose. A link counts as temporary only if:
- it has a parameter only signed links carry: `X-Amz-Signature`, `X-Amz-Expires`, `X-Amz-Credential`,
  `X-Amz-Security-Token`, `X-Goog-Signature`, `X-Goog-Expires`, `X-Goog-Credential`, `GoogleAccessId`,
  `Key-Pair-Id`, `hdnts`, `hdnea`, `__gda__`; or
- it has an expiry-named parameter (`expires`, `expire`, `expiry`, `expiration`, `exp`, `validto`, `valid_until`,
  `se`) next to a secret-named one (`diagnostics::is_secret_name`, or `hash`, `md5`, `hmac`, `st`); or
- it has `policy` together with `signature`.

Both the link and the address it redirected to are checked. The expiry time is read only when the link states it
plainly: Unix seconds in `expires` / `expire` / `expiry` / `exp` / `validto`, or `X-Amz-Date` / `X-Goog-Date` plus
`*-Expires` seconds (capped at 7 days). Signing schemes are never decoded beyond that. An expiry already in the past
is `expiredLink` (problem); otherwise it is `temporaryLink` (warning).

### Storage (`StorageCheck`)
Measured on the nearest existing parent of the folder the file will go to (`predicted_dir`, so sorting by type is
included).

| Value | When |
|---|---|
| `unavailable` | The drive could not be measured. |
| `tooLargeForDrive` | The size is ≥ 4 GiB and the drive is FAT (`vfat`, `fat`, `fat12/16/32`, `msdos`). This is checked before free space, because it fails whatever the free space. exFAT and NTFS are not affected. |
| `insufficient` | size > free. |
| `low` | It fits, but uses more than 90 % of the free space. |
| `ok` | It fits. |
| `cannotVerify` | The size is unknown (or it is a stream): "Cannot verify the final requirement". |

The drive list (`sysinfo::Disks`, refresh kind `nothing()`) is read only for files of 4 GiB or more. This keeps the
check fast and avoids touching every mount point for ordinary files.

### Duplicates (`find_duplicate`)
Cancelled downloads never match. A file name alone never matches.

| Reason | Match |
|---|---|
| `sameLink` | The same link after `batch::normalize` (fragment removed; the URL parser already lower-cases the scheme and host and drops default ports). |
| `sameFinalLink` | The address the link redirected to equals a known download's link. |
| `sameFile` | Same site (last two host labels, `www.` ignored), the same **strong** `ETag` (weak `W/` tags prove nothing), and the same size. This catches the same file under another signed link. |

When several downloads match, the order is: running, waiting or paused first, then completed, then failed, then
history-only. The history adds completed files that are no longer in the list (the last 500 rows), reported with
`id: null`. Checksums are not compared: the server gives none before the download, and no download is made to
compute one.

### Findings and health
Each finding has a `code` and a `level` (`info`, `warning` or `problem`), plus optional values (`detail` = a host,
an error code or a file name; `bytes`; `free`; `atMs`). Findings are sorted with the most serious first.

| Code | Level |
|---|---|
| `insecure` (final address is `http`) | warning |
| `httpsDowngrade` (an `https` link redirected to `http`) | warning |
| `otherSite` (redirected to another site; `detail` = host) | info |
| `authRequired`, `sessionRequired`, `accessRefused` (403), `webPage`, `expiredLink`, `noSpace`, `tooLargeForDrive` | problem |
| `serverError` (`detail` = `http_NNN`) | problem for 4xx except 408 / 425 / 429; warning for those and for 5xx |
| `unreachable` (`detail` = `explain::error_code` of the failure) | warning |
| `slowServer` (no answer within the cap, or `SlowToAnswer`) | info |
| `temporaryLink`, `lowSpace`, `duplicate` | warning |
| `sessionSent`, `noResume`, `resumeUnverified`, `unknownSize`, `storageUnverified`, `stream` | info |

**Health** is a label, not a score:
- `problem` if any finding is a problem;
- otherwise `unknown` if the server could not be checked (and it is not a video);
- otherwise `caution` if any finding is a warning;
- otherwise `good`.

## 4. Safety and side effects

- **Read-only.** The analysis creates, writes, moves and deletes nothing. No setting is changed and no download is
  created. The policy test checks the gathering module for any file-writing or settings call.
- **Bounded network use.** The only network use is `probe_quick`: one `HEAD`, then one `GET` with `Range: bytes=0-0`.
  There are no retries and the whole check is capped at 20 s (`ANALYSIS_TIMEOUT`). The 1-byte body is read only when
  ranges are verified; any other body is dropped unread, so a whole file is never transferred. Links to video pages
  and streams make no request at all. The same hardened client is used (scheme allow-list, redirect rules, local
  address protection, proxy settings).
- **Cancellable.**
  - Confirmation dialog: the task's `AbortHandle` is kept in the pending entry, and answering (`confirm` or
    `dismiss`) aborts it.
  - Add dialog: a new `analyze_link` aborts the one running, `analyze_link({ url: null })` only cancels, and the UI
    cancels when the link is edited or the dialog closes. A cancelled call returns an error that the UI ignores.
- **No secrets.**
  - Failures become a status number or a stable code (`explain::error_code`), never error text, which can contain
    the link.
  - The result carries the final *host* only, never the final address.
  - The dialog's shown link goes through `display_url`, which hides the password of `user:pass@`, every secret-named
    query value, and every value of a signed link except its expiry and date.
  - Cookies and headers are only used to make the request; they are not in the result (checked by an integration
    test).
  - The analysis logs nothing.
- **Never blocks the download.** The analysis only informs. A failed check, a cancelled one, or a problem finding
  never disables Start or Add. The engine keeps its own checks (space at start, web page, retries).
- **Sign-in.** No bypass is ever attempted. Only the credentials the user or browser already gave are sent.

## 5. Data and IPC

- `PendingDto` gains `analysis: Option<AnalysisDto>`. It is `None` while the check runs and for videos. The existing
  `size`, `contentType`, `resumable` and `filename` fields are kept and filled the same way. `url` is now the
  display-safe link (`display_url`); the real link stays in the entry and is what gets downloaded.
- New command **`analyze_link`** (`request: { url: string | null }`, `deny_unknown_fields`) → `AnalysisDto | null`.
  It is main window only and registered in:
  - `lib.rs`;
  - `build.rs`;
  - `capabilities/main-window.json`;
  - `authz.rs` (76 commands);
  - the policy test.

  A `user:pass@` link is split into an `Authorization` header, as the download would do. The advanced fields of the
  Add dialog (cookies, referrer, user agent) are not sent with the check.
- The TypeScript types (`types.ts`: `Analysis`, `Finding`, `FindingCode`, `DuplicateMatch` and the enums) mirror
  `intelligence.rs` one to one.

`AnalysisDto` fields: `kind`, `filename`, `size`, `contentType`, `https`, `redirected`, `finalHost`, `rangeClaimed`,
`rangeVerified`, `etag`, `lastModified`, `resume`, `strategy`, `connections`, `access`, `temporary`, `expiresAtMs`,
`storage`, `freeBytes`, `duplicate`, `health`, `findings`, `errorCode`.

## 6. User interface

`components/IntelligencePanel.tsx` is a bordered section in the style of the existing dialogs. The header has a shield
icon, the title and a health chip. Then come the rows:
- (`basics` only) File, Type, Size, Resume;
- Download (the strategy);
- Storage;
- Security;
- Already have (only when there is a duplicate);
- Warnings (warning and problem findings, in amber or red, or "None").

**"Why?"** (a `<details>`) lists the evidence: HTTPS or HTTP, the redirect host, the size given or not, ranges proved,
claimed or ignored, a version tag, the date, and then the info-level findings. `src/intelligence.ts` turns the
analysis into these lines. It is pure and unit-tested: every line maps to one field or one finding.

- **Confirmation dialog** (browser or clipboard): the Resume row now shows the four-level value. The panel appears
  under the existing rows without repeating file, type or size. The card scrolls (`max-h-[92vh]`) on small windows.
- **Add dialog**: a "Check link" button under the address shows "Checking the link…", then the full panel (`basics`),
  or "The link could not be checked. You can still download it."

Texts are in `locales/{en,fr,ar}.json` (79 `intel.*` keys) and contain no tool names. Screenshots:
`docs/screenshots/intelligence-{light,dark}.png`. `add-advanced-dark.png` was updated because it now shows the button.

## 7. Tests

| Suite | What |
|---|---|
| `download-manager` `intelligence` unit tests (17) | Ranges with and without a validator; no ranges (also claimed but not honoured); small files not split; unknown size never invented; storage (insufficient, low, OK, unavailable, FAT32 4 GiB); HTTP and HTTPS downgrade; same-site redirect; sign-in (401, sign-in page with and without a session, session sent on a public file, 403 alone); web page; failures (connection, 404, 503, slow); streams; temporary links (S3, CloudFront, Azure, generic, negatives, expiry times); expired vs live; temporary only after a redirect; display link hides secrets; duplicates (name only, fragment, cancelled skipped, active first); same file under another link (strong vs weak ETag, size), final link, history |
| `tests/integration/tests/intelligence.rs` (10) | Real server: normal file (resume, parallel 4, storage OK, the file never requested, nothing written or added); no ranges; 2 redirects; 401, credentials in the link, web page, 500; 1 PB file → insufficient, nothing reserved; duplicates by link, not by name, and by final link; temporary link, signature absent from the result and the dialog; confirmation dialog gets the analysis, cookie absent from the result, the download completes after; failed analysis still lets the download be added; dismissing or a new check cancels at once, no late result, `file://` refused |
| `apps/desktop/src/intelligence.test.ts` (5) | Evidence lines match the fields, failed checks, warnings vs info, values filled in, every key present in en/fr/ar |
| `tests/security/policy.test.mjs` (+1, 41 total) | Main window only, strict request, pure rules, nothing written, one bounded probe, no full download, no error text or log, host only, display link in the dialog, answering aborts |
| `tests/e2e/desktop-intelligence.mjs` (25 checks) | The real app: Check link rows, at most 2 requests, nothing added, Why?, screenshots in both themes, editing drops the result, sign-in, 404, closing drops the result, confirmation dialog panel, hostile IPC (extra field, `file://`, `javascript:`, `null` cancels), no CSP or script errors |

Results on Linux, 2026-10-08:
- Rust workspace: 624 passed, 0 failed (two full runs).
- vitest: 72 passed; `tsc` clean; clippy and fmt clean.
- policy: 41/41.
- e2e: `desktop-intelligence`, `desktop-ipc`, `desktop-folders`, `desktop-usability`, `desktop-media` and
  `desktop-views` all pass.

## 8. Limits and next steps

- **Second request.** Whether a browser session is *required* is not tested with a second request without cookies.
  The rule is one quick check only.
- **Checksums.** Not used for duplicates before the download (the server gives none).
- **Advanced fields.** The Add dialog's check does not send its advanced fields (cookies, referrer, user agent).
  A site that needs them may report `authRequired` or `webPage` while the download itself would work.
- **Sign-in pages without a redirect** (a 200 HTML page at the file's own address) show as `webPage`, not as a sign-in.
- **FAT detection** depends on the file system name the OS reports. Windows (`FAT32`) and Linux (`vfat`) are covered
  by the name list. macOS (`msdos`) is covered by name but not tested on a machine yet.
- **Rollback.** The feature is additive. To revert:
  - remove `analysis` from `PendingDto`;
  - remove the `analyze_link` command (five places and the policy test);
  - remove the two `intelligence.rs` modules.

  The probe, the dialogs and the database are unchanged, and there is no migration.
