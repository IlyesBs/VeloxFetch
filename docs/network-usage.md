# Network usage (issue #45)

The dashboard's **Network usage** card says how much VeloxFetch has received from the network: the big number is
everything ever counted, the two rows below it **today** and **this month** (the computer's local date). The tooltip also
gives what was received since VeloxFetch was opened.

Before, the card showed only the bytes received since launch: it went back to 0 at every restart, update or
reinstall, and the lifetime total next to it was tied to the downloads list.

## What counts

* Every byte a download or a video receives from the network (`DownloadManager::count_received`, called where the
  session counter was fed: a download's progress tick, a download's end, a video's progress). Resumed downloads count
  only the new bytes; bytes already on disk never count again.
* It belongs to no download: deleting downloads, deleting their files, or clearing the history changes nothing.

## How it is kept

* `crates/persistence/src/lib.rs`, schema **v8**: the table `network_usage(day TEXT PRIMARY KEY, bytes INTEGER)`, one
  row per local day (`YYYY-MM-DD`). `add_network_usage` adds to the day's row (saturating at the largest number SQLite
  holds, never wrapping); `network_usage(day, month)` returns ever / today / this month (`TOTAL`, which never fails on
  overflow).
* `crates/download-manager/src/manager/usage.rs`: received bytes wait in memory and are saved at most every 5 s
  (`SAVE_EVERY`), at once when the day changes, and when the app closes (`shutdown`). A save that fails keeps its bytes
  for the next one. The numbers shown are the saved ones plus the bytes not saved yet, so the card moves every tick; a
  new day or month starts from the bytes not saved yet.
* The date comes from the manager's clock (`ManagerConfig::clock`, the computer's local time), so tests can move it.
* The database lives in the app data folder, which an update or a reinstall keeps, so the count goes on. Only
  removing the app's data (or the database) starts it again from 0.
* At most the last 5 s are lost if the app is killed (a crash, power cut).

## Interface

* `apps/desktop/src/components/BentoCards.tsx`, card `tile-network`: the total (`network-total`), then two rows
  "Today" / "This month" with their numbers (`network-period`, `network-today`, `network-month`; two rows, so the
  numbers never get cut, even "1023.9 MB" in the narrowest card, in every language), tooltip with the session's
  bytes. `MetricsDto` carries `network_total`, `network_today`, `network_month` (and `session_downloaded`, still used
  by the tooltip).
* Texts: `bento.networkToday`, `bento.networkMonth`, `bento.networkTip` in every language.

## Security and privacy

Only numbers of bytes per day are stored: no address, file name or link. Nothing is sent anywhere.

## Tests

* `persistence`: `network_usage_is_kept_by_day_and_outlives_downloads_and_history` (per day, month, after deleting
  downloads and the history, after reopening, never wraps).
* `usage.rs`: `bytes_not_saved_yet_count_and_a_new_day_or_month_starts_from_them`.
* `tests/integration/tests/phase5.rs`: `network_usage_is_kept_after_deleting_downloads_clearing_history_and_restarting`
  (a real download, delete it, clear the history, restart the manager on the next day of the next month).
* `tests/e2e/desktop-ipc.mjs` (+4 checks): counted while downloading; kept after the app is closed and opened again
  (and keeps counting); kept after deleting every download and clearing the history; the card shows it.
* `tests/e2e/real-network-usage.mjs` (by hand, internet): a real 145 MB https download; the app counted 145.1 MB for a
  145.1 MB file while Linux's counters saw 152.7 MB received in all (other programs included); kept after closing and
  opening the app, and after deleting every download (and its file) and clearing the history.
* Upgrade: a copy of a real 1.0.0 database (schema v7) opened by this build becomes v8 with every download kept.
