# The publisher's checksum (feature #6, v2.0.0)

Many sites publish a checksum list next to their files: `SHA256SUMS` / `SHA512SUMS` (Ubuntu, Debian and most Linux
mirrors), `sha256sums.asc` (the Linux kernel), `SHASUMS256.txt` (Node), `<file>.sha256` (LibreOffice and others).
VeloxFetch finds that list by itself while the file downloads and checks the file against it before it is finished.
A file that does not match is never kept. Nothing has to be typed or copied.

## What the user sees

| Where | What |
|---|---|
| Add dialog, "Check link" (Download Intelligence) | An **Integrity** row: "Publisher's checksum found (SHA256SUMS, SHA-256): the file will be verified", or "No publisher's checksum next to this file: only its size will be checked". |
| Download card | While it downloads: **Will be verified** (blue). Finished: **Verified** (green shield), with a tooltip naming the list and the kind of digest. Briefly **Looking for a checksum…** while the list is asked. |
| Completed list, History | **Verified**. In a very tight row (the smallest window) only the green shield shows; its words stay in the tooltip and the accessible name. |
| Details page | An **Integrity** card: Verified (against which list, SHA-256 / SHA-512, the digest itself with a copy button), Will be verified, **Damaged** (did not match: not kept, Retry downloads it again) or **Not verified** (no checksum known: only its size was checked). The timeline says "Checking the file against the publisher's checksum (SHA256SUMS)" and "The file matches the publisher's checksum (SHA256SUMS)". |
| A damaged file | The card's error: "The file arrived damaged: it does not match the checksum its publisher lists next to it, so it was not kept. Retry downloads it again." The Download Doctor files it under integrity. It is never tried again on its own (that would download the whole file again and again). |
| Settings › Folders › **File integrity** | On by default. What it does, and what it does not prove. |

A checksum the user gives in the Add dialog always wins: the publisher's list is then not even asked for.

## How it works

1. **When a download starts** (`manager/publisher.rs`, `look_for_publisher_checksum`), unless it has the user's own
   checksum or the setting is off, `DownloadControl::set_publisher` hands the engine a channel that says Looking /
   Found / None. The lists are asked only **once the server has answered and the transfer has begun** (a server that
   refuses the file or cannot be reached is never asked for more), alongside the transfer, never slowing it.
2. **The lists asked** (`candidates`, `ask_lists`), in the link's own folder only:
   * first the folder's shared lists, `SHA512SUMS`, `SHA256SUMS`, `SHA256SUMS.txt`, `sha256sums.txt`,
     `sha256sum.txt`, `SHASUMS256.txt`, `sha256sums.asc`, all at once (about one round trip). They are kept 10
     minutes for every file of that folder (16 folders, 8 MiB at most), and lookups in one folder wait for each other,
     so **a batch of a hundred files asks for them once**;
   * then, only if none of them names the file, the file's own lists `<file>.sha256`, `.sha512`, `.sha256sum`,
     `.sha512sum` (at once). After two files in a row of a folder had none, they are no longer asked there (a site
     without lists gets a handful of requests, not hundreds);
   * the first list in that order that names the file wins. The answer for a link is kept 10 minutes (64 links), so
     the Add dialog's check and the download ask once.
3. **Reading a list** (`downloader_core::integrity::find_in_list`): GNU lines (`<hex>  name`, `<hex> *name`,
   `./name`, a path ending in `/name`), BSD lines (`SHA256 (name) = <hex>`), a lone digest in a list made for one file;
   SHA-512 wins over SHA-256; signature armour is skipped. The name must be exactly the link's file name. The same name
   with two different digests is ambiguous: never guessed (no check).
4. **Before the file is finished** (engine, `publisher_checksum`), the engine waits for the answer (at most 20 s; a
   pause or cancel wins at once), then checks the `.part` file as for a user checksum. A match: the checksum and
   `checksum_source = publisher:<list>` are saved with the download and copied to its history line (`verified`). A
   mismatch: the `.part` file is deleted, the download fails with `publisher_checksum_mismatch`.
5. **Downloaded again:** a publisher's checksum from an earlier run is forgotten and looked up again (the site may
   have a new version).

## Safety and what it proves

* **What it proves:** the file was not damaged or cut short on its way (a broken mirror, a bad connection, a disk
  error). **Not** that the site is honest: the list comes from the same site. The interface says "matches the
  publisher's checksum", never "safe". Signatures (`.asc`, `.gpg`) are not checked.
* **https only** (a release build): a plain `http` link is never looked next to. Debug builds accept plain http with
  `VELOX_PUBLISHER_HTTP` for the local test servers; release builds never read it.
* **Same site only:** the lists are asked from the link's own host and port; an answer that redirects elsewhere is
  ignored.
* **Nothing private is sent:** the lists are asked through the same protected client as every download (proxy,
  local-address protection) with **no cookie, sign-in, referrer or custom header**, even when the download itself has
  them. The link's query (a signature, a token) is never part of a list's address.
* **Bounded:** a list larger than 1 MiB, a web page, or one slower than 5 s is ignored; the whole lookup takes at most
  10 s (4 s inside the Add dialog's check); reading stops after 20,000 lines.
* **Stored:** only the list's *name* and the digest, never its address.

## Code

* `crates/downloader-core/src/integrity.rs`: `find_in_list`, `PublisherLookup`.
* `crates/downloader-core/src/engine.rs`: `DownloadControl::set_publisher`, `publisher_checksum`, `PUBLISHER_WAIT`,
  run events `Verifying` / `Verified`, `DownloadOutcome::publisher_list`.
* `crates/download-manager/src/manager/publisher.rs`: the lookup, the setting (`downloads.publisher_checksum`), the
  live state of running downloads.
* `crates/persistence/src/lib.rs`: schema **v9** (`downloads.checksum_source`, `history.verified`; v2.0.0 only).
* `crates/download-manager/src/api.rs`: `VerifyDto` on `DownloadDto` and `HistoryDto`; `intelligence.rs`:
  `AnalysisDto::publisher_checksum`.
* Desktop: commands `get_publisher_check` / `set_publisher_check` (main window only);
  `components/VerifyBadge.tsx`, `components/IntegritySettings.tsx`, the Integrity card in `views/DetailView.tsx`, the
  Integrity row in `components/IntelligencePanel.tsx`; texts `verify.*`, `e.publisherChecksum`, `tl.verif*` in all 8
  languages.

## Tests

* `downloader-core` `integrity`: every list format, never guessing (not listed, similar names, md5 / sha1, two
  digests, a web page, tag and length that disagree, a path in the name, a huge list).
* `downloader-core` `engine`: the answer is waited for; nothing found; a pause wins at once; a lookup that went away.
* `download-manager` `publisher`: lists only next to the file, only over https, never with the link's query.
* `persistence`: who verified a file is kept with it and its history line.
* `tests/integration/tests/publisher.rs` (11): a match (card, history, details, timeline, kept after a restart); a
  damaged file is never kept, Retry with a corrected list; every usual list; no list, too large a list, an ambiguous
  one; a slow list never holds the download up; the user's checksum wins and the setting turns it off; no cookie,
  referrer or sign-in sent, https only in a release; the Add dialog's check and the download's answer reused; "Will be
  verified" while it downloads; a batch of 6 files asks its folder's list once, and a site without lists gets the
  files' own lists asked for 2 files only; a server that refuses the file is never asked for lists.
* `tests/e2e/desktop-integrity.mjs`: the whole path in the real app window (Settings, Add dialog, card, Completed,
  History, details, a damaged file, no list, the switch, a bad request refused), and in all 8 languages the badges fit
  on the card, in Completed and in History from 1360×860 down to 640×480. Writes `docs/screenshots/integrity-dark.png`.
* `tests/e2e/real-publisher-checksum.mjs` (by hand, internet), 2026-10-16 on this line:
  * the Linux kernel `linux-6.10.tar.xz` (145 MB, cdn.kernel.org): found `sha256sums.asc` beforehand, "Will be
    verified" while downloading, Verified (SHA-256) in 64 s; the file on disk has the published digest;
  * the Debian installer `debian-13.7.0-amd64-netinst.iso` (793 MB, mirrors.edge.kernel.org): found `SHA512SUMS` (the
    strongest), Verified (SHA-512) in 483 s; the file on disk has the published digest;
  * `sha256sums.asc` itself (its folder's list does not name it): finished as before, not verified;
  * the History marks the two verified files.
