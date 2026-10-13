# Resume integrity: segment re-verification and source-change detection

How VeloxFetch keeps a paused, interrupted or crashed download from ever combining bytes that do not belong
together. It covers bytes of two versions of the file, changed local bytes, and a segment map with a gap. It also
explains how a problem is found before the end of a long download, not only by the final checksum.

Code:
* `crates/downloader-core/src/verify.rs`: segment hashes, re-verification, segment-map check;
* `engine.rs`: `checkpoint`, `reverify_if_touched`, `check_resumable`, the worker's `Content-Range` total check, and the
  check before finishing;
* `crates/http-client/src/lib.rs`: `get_range_with`, probe, `HttpError::SourceChanged`;
* `crates/persistence/src/lib.rs`: schema v7;
* Download Doctor: `doctor.rs`, `manager/doctor.rs`, `explain.rs`, `manager/diag.rs`.

## 1. Audit (what was there, what changed)

| Capability | Before | Now |
|---|---|---|
| Source identity saved with the download (size, `ETag`, `Last-Modified`) | **Done** | Unchanged |
| Resume only the same version | **Done**: size + strong `ETag` (a weak one never counts), else `Last-Modified`. No validator at all: "cannot be validated" → start over. | A segment-map check (§5). October 2026: without a strong `ETag`, saved-byte samples decide instead of starting over (§2) |
| `If-Range` on every ranged request | **Done**: a server that honours it answers `200` when the file changed → refused | Unchanged |
| `206` required, `Content-Range` start = asked, end ≤ asked | **Done** | Unchanged |
| `Content-Range` **total** = the download's size | **Missing**: another size was not noticed mid-download | Checked by every worker → `source_changed` |
| Answer `ETag` ≠ the download's strong `ETag` | **Missing**: a server ignoring `If-Range` could send a new version with `206` and it was written among the saved bytes | Refused → `source_changed` |
| Compressed answers (`Content-Encoding`) | **Partial**: `Accept-Encoding: identity` was asked, but a server sending gzip anyway was accepted | A compressed ranged answer is refused. A compressed probe never enables ranges. |
| Truncated body | **Done** (`ShortBody`, retried from where it stopped) | Unchanged |
| Overlong body | **Done**: never more than the asked bytes are written (`pump_keep` limit). Extra bytes are not written. | Unchanged |
| Counters durable before they are saved | **Done**: `fsync` before the store commit | Unchanged; hashes join the same commit |
| Crash recovery of the `.part` file | **Partial**: size and counters checked, but completed segments were trusted as written. A local change or a disk fault in a finished segment went unnoticed until a checksum (if any). | Per-segment SHA-256, re-verified when the file was touched (§3) |
| Segment map with a gap or overlap | **Partial**: only "more bytes than the segment holds" was checked | Full coverage check at resume and before finishing (§5) |
| Doctor / diagnostics | **Done** for range and checksum failures | `source_changed`, and timeline lines for re-verification |

## 2. Source identity and range validation (every ranged answer)

Before a single byte of a ranged answer is written (`get_range_with` and the worker), all of these must hold:
1. The status is `206`. A `200` to a range is refused (`range_not_honored`): never appended.
2. There is no `Content-Encoding`, or it is `identity`. Otherwise the answer is refused: ranges of compressed bytes are
   not the file's.
3. If the download has a **strong** `ETag` (sent as `If-Range`) and the answer carries a strong `ETag`, they are
   equal. Otherwise `source_changed`. Weak or missing tags prove nothing either way and are not used.
4. `Content-Range` is present, starts exactly at the asked offset, and ends at or before the asked end. Otherwise
   `range_not_honored`.
5. The `Content-Range` **total** equals the download's size. Otherwise `source_changed`.
6. The body is written from that offset, at most the asked length. A shorter body is a `ShortBody` retry from the
   last written byte.

At resume (`check_resumable`), before any worker starts:
* the size is the same;
* the version (`resume::validators`): the same strong `ETag` proves it, nothing more is asked. A weak `ETag`, a
  date, or nothing at all only hints at it, so **up to four 64 KiB samples of the saved bytes** (the start of the
  first saved bytes, the end of the furthest, the middle of the largest; only bytes recorded as written) are fetched
  again and compared, after the segment re-check (§3). All equal: continued. Any difference (another strong or weak
  `ETag`, another date, other bytes): another file, start over ("the saved bytes differ from the file on the
  server"). The server cannot be asked (after the usual retries, 30 s per sample): **no byte is thrown away**, the
  download stops like any failure and is checked again on the next try. Before October 2026 a weak `ETag` (nginx with
  compression, Apache, many CDNs) or a missing one always started over;
* the server still answers ranges correctly (verified `bytes=0-0`, uncompressed);
* the `.part` file has the expected size;
* the segment map is sound (§5).

**Join checks (every version of every server, tagged or not).** Bytes written by different requests meet at
joins. Every join is checked against the server's own bytes before the file may finish:
* a request that continues saved bytes (the rest of a part, or a part right after a complete one) starts
  **1 KiB earlier**. Those bytes must equal the saved ones before anything new is written (`check_overlap`, with
  one re-read after 100 ms for a previous writer's last bytes in flight);
* a request that starts where nothing is saved yet records the join as unverified. The request that writes the
  part before it reads **1 KiB past its own end**, only when the part after it already started unchecked, and keeps a SHA-256 of that tail;
* when all parts are done, every unverified join is compared with its tail (`check_joins`). A join without a tail
  (its request was cut short) gets a final look: the 2 KiB around it are fetched again and compared on both sides.

Any difference is `source_changed`. The download stops, no file is finished, and the saved part is marked unusable
(validators cleared, saved first). The retry therefore starts over even when the server kept the same date.
Overhead: 1 KiB per request plus the few joins that need a final look.

A redirect to another address is followed as before. Identity is judged on the answer's validators and size,
never on the address. A file name or an unchanged URL is never taken as proof.

**Mid-download change on `source_changed`:** the download stops. Its siblings are cancelled through the shared
cancellation group and **joined** before the final checkpoint, so no worker can write afterwards. The saved part
and counters are kept. Retry probes again: the server now describes another version, so the saved part is
discarded and the download starts over cleanly. Bytes of two versions are never combined.

## 3. Segment hashes (lifecycle)

States: **SEGMENT_VERIFIED** (the stored hash matches the bytes), **SEGMENT_INVALID** (they differ: the segment is
emptied), unhashed (saved by an older version: counted, kept, reported as uncheckable).

1. **Hashing.** The periodic checkpoint `fsync`s the `.part` file. Then every segment completed since the last
   checkpoint is read back and hashed (SHA-256, streaming, a 1 MiB buffer, so memory is bounded whatever the size).
   The hash is saved **in the same store write** as its counters. A hash therefore always describes durable bytes;
   a segment is never "verified" before its bytes are committed. A hash is kept only while its segment stays complete
   over the **same range** (`carry_hashes`). It is never stored for an incomplete segment (also enforced by the store).
   **Spread over the download (Phase 20, `RunningHashes`):** each checkpoint, after the `fsync`, also reads the bytes
   an unfinished segment received since the previous checkpoint and adds them to that segment's running hash (kept in
   memory for the run); a segment that completes then only reads its last bytes. Hashes made this run are kept for the
   final checkpoint, which saves into another copy of the record and used to read **the whole file again** after the
   last byte (0.5 s per GiB on this machine, more from a disk once the file no longer fits in memory). The result is
   the same SHA-256 of the same synced bytes; a restart (`ProgressHandle::reset_segment`), progress that went back, a
   new plan or a changed range drops the running hashes, and the bytes are read from the segment's first byte again.
2. **Stamp.** With the hashes, the `.part` file's stamp (`<size>:<mtime ns>`) is saved. The last checkpoint of a
   clean stop (pause, failure) runs after every writer has stopped, so its stamp is exact.
3. **Resume** (`reverify_if_touched`):
   * same stamp: the file was not touched since. **Nothing is read**; a clean pause costs nothing;
   * another stamp (a crash after more writes, another program, a disk fault that changed the file) or none: every
     hashed completed segment is read and hashed again. A mismatch **empties that segment only** (counter 0, no hash,
     pending), so only its range is downloaded again. Intact ones are kept.
4. **Invalidation is committed before any writer starts.** The emptied segments are saved first. If the app dies
   before that save, nothing was written yet and the stamp still differs, so the next start runs the same check
   again: idempotent (tested with a real process killed at `reverify.before_save`).
5. **Finishing.** The size check and a given checksum are unchanged and still decide.

**What a hash proves:** that local bytes did not change since they were saved. **It does not prove** that they are
what the server meant. That is the job of §2 (validators, `If-Range`, `Content-Range`) and of a checksum given by a
source more trusted than the server. Without a strong `ETag` or a trusted checksum, VeloxFetch cannot guarantee
end-to-end authenticity, and does not claim it.

## 4. Doctor and diagnostics

| Situation | Code | Doctor |
|---|---|---|
| Another version during the download (answer `ETag`, `Content-Range` total) | `source_changed` | Integrity, **Critical**, retryable: **Restart safely** (Retry starts over cleanly) |
| `200` to a range, wrong `Content-Range`, compressed answer | `range_not_honored` | Range failure: **Restart safely** |
| Truncated body | `incomplete` (retried automatically) | Retry |
| Segment map not complete when finishing | `incomplete` ("cannot finish: …") | Retry downloads what is missing |
| Segments re-checked, all intact / some changed | timeline line (info / warning, with the bytes to fetch again) | — |
| Older segments without a hash | timeline line ("could not be checked") | — |

Timeline reliability (found while testing under load, fixed):
* a flood of retries no longer pushes the run's milestones out. Both the per-run list (40) and the saved timeline
  (60) drop the oldest retry / throttling line first;
* a finished download's diagnostics never lack its last run. The ended run stays readable (`finishing`) until its
  numbers are saved, under the same lock the diagnostics and the average speed are read with.

The UI text for `source_changed` is `e.sourceChanged` (en / fr / ar). Links, cookies and signed query strings never
appear: timeline texts contain only counts and sizes, and reports go through the redactor.

## 5. Segment map

`coverage_problem`: segments must start at 0, follow each other with no gap or overlap, end at the file's size,
and never claim more bytes than they hold.
* **At resume:** a broken map → "stored segment state is inconsistent" → start over (the saved counters cannot be
  trusted).
* **Before finishing** a parallel download: a broken map, or any segment not complete → failure `incomplete`. Never
  renamed into place, never marked completed.

## 6. Crash safety and persistence

* Schema **v7** (backward compatible): `segments.hash` and `downloads.part_stamp`, both nullable. Older records load
  with no hashes and no stamp. Their segments resume as before and are reported as uncheckable.
* Order at every checkpoint: snapshot → `fsync` → hash newly completed segments → stamp → one store transaction
  (counters, hashes, stamp). A crash at any point leaves either the previous state or the new one. A segment is never
  saved complete without its hash, unless hashing failed: then it is saved without a hash and the stamp is cleared,
  so it is re-checked.
* Startup recovery (`recover_interrupted`) keeps the hashes and stamp. The stamp of a crashed run differs, so the
  next resume re-verifies.
* Only the engine's existing writers touch segment state. Invalidation happens before workers are spawned, and a
  mid-run mismatch cancels and joins every worker. No new lock or recovery system.

## 7. Performance

* **Clean pause / resume:** zero extra reads (stamp equal).
* **While downloading:** each completed segment is read back once, at the checkpoint after it completes. That is one
  extra read of the file in total, usually from the page cache, in a blocking thread. Hashing costs about
  1–2 GB/s per core with the SHA extensions.
* **After a crash or an outside change:** one read of the completed bytes. For tens of GB this takes on the order of a
  minute on an SSD; a hard disk is slower. It is the price of not trusting possibly-changed bytes. Bytes that are
  intact are never downloaded again.
* No extra network request when the server gives a strong `ETag`. Otherwise, at resume only: up to four ranged
  requests of 64 KiB (at most 256 KiB), instead of downloading the whole file again.

## 8. Tests

* `verify.rs` (6):
  * a changed segment is found and only it is emptied;
  * older segments are counted, not trusted;
  * a hash follows its exact range;
  * the coverage check catches a gap, an overlap, a missing end and an overclaim;
  * 48 MiB is hashed with a 1 MiB buffer (same digest as one-shot);
  * the stamp changes with the file.
* `http-client` (2 new):
  * ranged answers checked: total passed on; another strong `ETag` → `SourceChanged`; a weak one is not used;
    `gzip` refused and `identity` accepted; `200`, another start, past the end and no `Content-Range` all refused;
  * a compressed probe never enables ranges.
* `tests/integration/tests/reverify.rs` (8, mocked server):
  * a clean pause re-reads nothing, hashes saved for complete segments only;
  * one byte changed in a finished part → only that range is fetched again, byte for byte, said in the timeline;
  * crash state (stale stamp) → all checked, intact kept;
  * older records without hashes still resume and say so;
  * same size, new content and new `ETag` mid-download (server ignoring `If-Range`) → `source_changed`, never
    finished; retry → entirely the new version;
  * another total size mid-download → `source_changed`, progress kept;
  * no validator, a weak `ETag` with a date, a weak `ETag` alone: continued once the samples match, only the
    missing bytes downloaded;
  * changed while paused with no validator, the same weak `ETag` and date, or only the same date: the samples
    differ → start over, entirely the new version;
  * the server refuses the comparison: the download fails with every saved byte kept, and the retry continues them;
  * a gap in the saved map → not resumed.
* `tests/integration/tests/crash.rs` (1 new, real process): killed while invalidating a changed segment → detected
  again → finished byte for byte, no stray files.
* Existing suites cover the unchanged parts: crash, pause / resume, retry, cancel, checksum and range.

## 9. Coverage

| Situation | How it is caught |
|---|---|
| Another version, strong `ETag` | `If-Range` (`200`) and the answer's `ETag`, on every request |
| Another size | the `Content-Range` total, on every request; the size check at resume |
| Another version, same size, **no tag or only a date** (during a run or while paused) | the join checks at every join (1 KiB overlaps and tails), before finishing |
| A version change while paused, **weak or no tag** | saved-byte samples at resume; then the join checks |
| A version change after a `source_changed` | the saved part is never continued (validators cleared) |
| Local change of a finished part | its hash, re-checked when the `.part` stamp differs |
| Local change that restores the exact size and time | joins next to that part, plus the final size and checksum |
| Gap, overlap, missing part | the segment map, at resume and before finishing |
| Sequential transfers (no ranges) | one answer is one version; a drop restarts from byte 0 |

A file from a server that gives no version information is only known to be consistent at every join, not in
every byte between joins. That is all the bytes received can show. A checksum given for the file (Add dialog) proves
every byte, and is verified before the file is finished.
