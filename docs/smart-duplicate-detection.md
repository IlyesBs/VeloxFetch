# Smart Duplicate Detection

When a link is about to be downloaded, VeloxFetch checks whether the app already has that resource and tells the
user before anything happens. Duplicate detection is advice only. It never deletes, overwrites, cancels or replaces
anything, and downloading another copy on purpose always stays possible.

It keeps two questions apart:

* **Duplicate resource**: is this the same file as a download the app knows? This is proven by the link, the server's
  validators or a checksum, never by the name.
* **Destination conflict**: is the file name already taken in the folder? Two files can share a name and have
  different contents. A taken name is never treated as a duplicate, and the new download is saved under the next
  free name.

## 1. What was there before (audit)

| Capability | Before | Now |
|---|---|---|
| Add bar / Add dialog prompt | **Partial**: an exact text comparison with the list in the window (no normalisation, history ignored), and one generic message whatever the state | **Fixed**: asks Rust (`check_duplicate`): normalised link, list and history, checksum, file state. A different message and different actions for each state. The window-only comparison is kept as a fallback |
| Pre-download analysis (Check link, browser confirmation) | **Partial**: `find_duplicate` with `sameLink` / `sameFinalLink` / `sameFile`, but it claimed a match even when the server's file had changed | **Improved**: confidence levels, version comparison (ETag / size / date), checksum, file state |
| Confidence levels | **Missing** | `exact` / `likely` / `possible`. No match is `null` |
| Same link, changed version | **Incorrect**: reported as the same file | `possible` + `changed: true`: "the copy you have may be an older version" |
| Checksums | **Missing** | The same checksum proves the same content under any link. A different checksum on the same link means another content |
| Finished file still there? | **Missing** | One metadata read: `present` / `missing` / `changed` / `unknown` |
| Destination name taken | **Partial**: the engine already never overwrote (`name (1)`), but the user was not told | `nameConflict` in the analysis: "another file named X, its contents may differ, saved as X (1)" |
| Concurrent identical requests | **Done** (engine name claims) | Unchanged. A new test covers it |
| Batch dialog ("already in your list") | **Done** (`batch::normalize`) | Unchanged |
| Destination safety (no overwrite, path checks) | **Done** (`claim_name`, `confine`, roots) | Unchanged |

## 2. Identity and confidence rules

`crates/download-manager/src/intelligence.rs` → `find_duplicate(url, info, checksum, known)`. It is a pure function:
no I/O. The evidence is tried strongest first:

| Evidence | Reason | Confidence |
|---|---|---|
| The same checksum (normalised `algo:hex`, case ignored) as a known download, whatever the link | `sameChecksum` (or `sameLink` if the link matches too) | `exact` |
| Same link, but another checksum given now | `sameLink` / `sameFinalLink` | `possible`, `changed` |
| Same link (or the redirect target equals a known link) and the server's answer proves the same version: equal **strong** ETag, or equal size **and** `Last-Modified` | `sameLink` / `sameFinalLink` | `exact` |
| Same link and the server describes another version: another strong ETag, another size, or another date | `sameLink` / `sameFinalLink` | `possible`, `changed` |
| Same link, version not comparable (no server answer, as in the Add bar, or not enough validators) | `sameLink` / `sameFinalLink` | `likely` |
| Another link on the same site (last two host labels) with the same strong ETag **and** size, and no conflicting checksum | `sameFile` | `likely` |
| Anything else: same name only, a weak ETag (`W/`), size only, another site | no match | — |

Further rules:

* A **cancelled** download never matches. A stored link that does not parse is skipped, not an error.
* **Order** when several downloads match: running, queued or paused first; then completed; then failed; then
  history-only. Among equals, the surer match comes first.
* The confidence is never higher than the evidence. A same-link match without the server's answer is at most
  `likely`. Metadata alone (size, type) never makes a match `exact`.
* **Unknown**: if the database cannot be read, there is no match (`null`) and the download is added as usual.
  Detection never blocks a download.

### URL normalisation (conservative)

`batch::normalize`: the WHATWG parser (the `url` crate) lower-cases the scheme and host, drops the default port and
resolves `.`/`..` path segments. The `#fragment` is then removed: it is never sent to the server, so it is not part
of the resource.

Everything else is kept and compared exactly: the path (case-sensitive), every query parameter (`?v=1` ≠ `?v=2`),
signed-URL parameters, tokens, a non-default port and the scheme (`http` ≠ `https`). Two signed links to the same object
differ by their signature. They only match through `sameFile` (strong ETag and size, which needs the server's answer)
or a checksum.

The link is compared exactly as `create_download` would store it. Credentials are split out
(`validate::split_credentials`), Google Drive share links are made direct (`links::direct`), and the same
steps run on both sides.

## 3. State handling

Rust returns a `DuplicateDto`. `apps/desktop/src/duplicates.ts` → `duplicatePrompt` turns it into the prompt. It is a
pure function and every action it offers works:

| Match | Title | Actions (the first is the default button) |
|---|---|---|
| Queued / downloading / verifying | "This file is already downloading" | View download · Download another copy |
| Paused | "This download is paused in your list" | Resume it · View · Download another copy |
| Failed | "An earlier download of this link did not finish" | Try it again · View · Download again (never Open: a failed download is never taken for a usable file) |
| Completed, file present, `exact` | "You already downloaded this file" | Open file (only if `openable`) · Show in folder · View · Download again |
| Completed, file present, `likely`/`possible` | "This appears to have been downloaded already" (+ "whether the file changed since is not known" when `likely`) | same |
| Completed, file missing | "Downloaded before, but the file is no longer in its folder" | Download again · View |
| Completed, file size changed | "Downloaded before, but that file has changed since" | Show in folder · View · Download again |
| Completed, file state unknown | "This appears to have been downloaded already" | View · Download again |
| History only (removed from the list) | as for completed | Open / Show in folder through the history line (`FileTarget::History`); no View |

Notes the prompt adds:

* `changed`: "The file on the server has changed since: the copy you have may be an older version."
* Completed matches: "A new copy gets its own name: nothing is replaced."

The answer never changes the existing download unless the user asks:

* "Resume" and "Try it again" call the normal `resume_download` / `retry_download`, with their usual state rules.
* "Open" and "Show in folder" go through the normal file commands. Those check the download folders and refuse to
  open programs.
* Cancel or Escape adds nothing.

The **file state** (`with_file_state` in `manager/intelligence.rs`) is only looked at for completed and history
matches:

* one `std::fs::metadata` call, never the content;
* `missing` on `NotFound`;
* `changed` if the length differs from the saved size;
* `unknown` on any other error, or when the path is outside the download folders (`AllowedRoots::check`; such a file
  is not looked at and not offered);
* `openable` = present and not `is_risky_to_open` (programs, scripts, shortcuts and web pages are only shown).

## 4. Destination conflicts

* **The engine** (unchanged) never overwrites. `Downloader::claim_name` reserves `name`, `name (1)`, `name (2)`... and
  checks:
  * the files on disk;
  * the in-process claims of running downloads;
  * unfinished downloads in the database (`destination_in_use`).

  A file that appears under the name during the download makes the finished file take the next free name at
  finalisation. `overwrite` is only set by the command-line tool's explicit `--overwrite` flag; the desktop app never sets it.
* **The analysis** (new, `name_conflict` in `manager/intelligence.rs`) predicts this for the user. If
  `predicted_dir/filename` exists, or an unfinished download will write there, it returns
  `NameConflict { saved_as, same_resource }`:
  * `saved_as` is the first free numbered name, up to 50 tried. `null` means a new name will be chosen.
  * `same_resource` is true when the taken name is the matched duplicate's own file. The panel then says "your
    earlier copy keeps the name". Otherwise it says "another file named X is already in this folder (its contents
    may differ)".
  * In both cases the panel says "nothing is replaced".
* A name conflict is **not** a finding and does not change the health: no harm can follow from it.

## 5. Concurrency safeguards

* `check_duplicate` is read-only, so two identical submissions at once can both pass it. This is intentional: a
  duplicate is advice, and two copies may be wanted. The engine then gives them two different files:
  * `claim_name` holds a process-wide mutex over the candidate names;
  * the database check stops a paused or queued download from losing its name.

  Test: `identical_requests_at_once_never_share_or_overwrite_a_file`.
* The Add bar ignores a second press while its duplicate check is running (`asking` ref), so one click never opens
  two prompts.
* `start_download` still claims the id atomically, so the "Resume" answer cannot start a second task.

## 6. Performance

* No network request in `check_duplicate`. The analysis reuses its one existing quick probe.
* Database: `store.list()` plus `store.history(500)`. These are the same queries the analysis already made. No new
  table, index or cache: the match is a linear pass in memory over at most the list plus 500 history rows.
* Disk: one `metadata` call for the single chosen match. The name prediction costs at most 51 `symlink_metadata`
  calls plus indexed `destination_in_use` lookups, and only when the name is taken. No directory scan, no hashing.
* Everything blocking runs in `spawn_blocking`.

## 7. Security and data integrity

* The DTO carries the file name, ids, status, reason, confidence and file state. It never carries the link, the
  path, credentials, cookies or query parameters. Test: `secrets_of_the_link_are_never_part_of_the_answer` checks
  that the signature, token, user name, password and path are absent.
* A download that has not started yet has no file name (it is chosen when it starts). Until October 2026 the match
  was then told with an empty name ("In your list:  · same link", "downloaded before: ."). It is now named by what
  its link suggests: the last part of the path, made safe, else the word "download". Never the whole link, which
  may carry a token (`fix_queue_percent_duplicate_name`; tests
  `a_download_that_has_not_started_yet_is_named_by_its_link`,
  `a_waiting_download_without_a_name_in_its_path_never_shows_its_link`).
* Nothing is deleted, cancelled, paused or overwritten because of a match. The prompt's actions are the existing
  commands, with their existing checks.
* No path from the UI. The file actions use `FileTarget` ids (`download` / `history`), resolved and checked in Rust.
* The new command `check_duplicate` is main-window only. It is in `authz::Command` and the destructive-commands
  test list, and is registered in `build.rs` with its capability permission. `deny_unknown_fields` is on the request.
* Atomic writes, checksums, crash recovery and `confine` are untouched (no engine change).

## 8. Data and IPC

* `check_duplicate(request: { url, checksum? }) -> DuplicateDto | null`
  (`apps/desktop/src-tauri/src/lib.rs`, `api.checkDuplicate`).
* `DuplicateDto`: `id`, `historyId`, `status`, `filename`, `reason` (`sameLink`, `sameFinalLink`, `sameFile`,
  `sameChecksum`), `confidence`, `changed`, `file`, `openable`.
* `AnalysisDto.nameConflict`: `{ savedAs, sameResource } | null`.
* `downloader_core::numbered_name` is re-exported, so the prediction uses the engine's own naming rule.

## 9. Tests and results

| Where | What |
|---|---|
| `download-manager` unit tests (`intelligence::tests`, 6 new + 2 updated) | conservative normalisation (host case, default port, fragment vs. query, path case, scheme, port, signed links, malformed stored link); version rules (same / changed ETag, changed size, size only, date, no server answer); checksums (same under another site, changed on the same link, different elsewhere); order (running first, surer first, failed claims no file); file state mapping |
| `tests/integration/tests/duplicates.rs` (11) | a waiting (never started) download is named by its link, in the answer and in the Check link warning; without a name in its path it is "download", never the link |
| `tests/integration/tests/duplicates.rs` (first 9) | real server + database: finished download found with its file, scheme case and fragment equivalent, another query / another path not a duplicate, Check link `exact` then `possible` after the server's file changes, own-name conflict, a second copy never replaces the first; edited and deleted file; running, paused (state unchanged by the check), failed, cancelled; history-only and after a restart; checksum match under another link, other or invalid checksum; another file with the same name is a name conflict, not a duplicate, and is untouched; two identical requests at once; secrets absent; malformed links refused |
| `apps/desktop/src/duplicates.test.ts` (9) | prompt per state and file state; Open only when openable; a changed version noted; history-only has no View; every combination still offers a download, and every sentence exists in en/fr/ar; window fallback |
| `apps/desktop/src/intelligence.test.ts` | name-conflict lines (other file vs. own copy, with and without a predicted name); new keys in every language |
| `tests/e2e/desktop-usability.mjs` | the real prompt for a finished file says it is there and offers Open, Show in folder, Download again |

Results at commit time are in the commit message and `docs/testing.md`.

## 10. Known limitations

* The Add bar check asks no server (it must be instant), so a same-link match there is `likely`, not `exact`. "Check
  link" in the dialog gives the version comparison.
* The name prediction looks at the folder the link would be sorted into by default. A folder or sub-folder chosen in
  the Add dialog is not part of the check. The engine still never overwrites there.
* History rows keep no ETag or date, so a history-only match is compared by link, size and checksum only. Only the
  last 500 history rows are looked at.
* A file whose size is unchanged but whose content was edited shows as `present`: content is never hashed for
  detection.
* Downloads handed over by the browser extension and links from the batch dialog keep their own flows (the
  confirmation dialog shows the analysis; the batch dialog skips links already in the list).
