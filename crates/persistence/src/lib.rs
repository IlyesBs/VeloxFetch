//! SQLite persistence for VeloxFetch: downloads, segments, queue, settings, history,
//! scheduler jobs. Implements [`DownloadStore`] for the download engine and provides
//! crash recovery for downloads that were interrupted while running.
//!
//! Durability model: WAL journal, foreign keys on. The engine fsyncs the `.part` file
//! *before* committing progress here, so a crash may lose recent progress but the
//! database never claims bytes the disk does not have.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use downloader_core::{part_path, DownloadError, DownloadRecord, DownloadStatus, DownloadStore, Priority, Result, Segment, SegmentStatus};
use rusqlite::{params, Connection, OptionalExtension};

/// v2: scheduling + bandwidth settings per download.
const SCHEMA_V2: &str = "
ALTER TABLE downloads ADD COLUMN priority    INTEGER NOT NULL DEFAULT 2;
ALTER TABLE downloads ADD COLUMN category    TEXT;
ALTER TABLE downloads ADD COLUMN speed_limit INTEGER;
";

/// v3: what the server told us, shown in the download detail view.
const SCHEMA_V3: &str = "
ALTER TABLE downloads ADD COLUMN content_type TEXT;
ALTER TABLE downloads ADD COLUMN server       TEXT;
";

/// v4: request headers handed over by a browser (JSON array of [name, value]; NULL = none).
const SCHEMA_V4: &str = "
ALTER TABLE downloads ADD COLUMN request_headers TEXT;
";

/// v5: what happened to each download (attempts, retries, last HTTP status, timeline), as JSON written by the
/// download manager with secrets already removed. Goes away with the download.
const SCHEMA_V5: &str = "
CREATE TABLE diagnostics (
    download_id INTEGER PRIMARY KEY REFERENCES downloads(id) ON DELETE CASCADE,
    data        TEXT    NOT NULL,
    updated_at  INTEGER NOT NULL
);
";

/// v6: Smart Download Queue. The `queue` table (unused until now) says which queue a download is in, its place
/// in it, when it should start, and whether the user chose the queue (then big-file rules never move it). A
/// download without a row is in the main queue (id 1), in the order it was added.
const SCHEMA_V6: &str = "
ALTER TABLE queue ADD COLUMN queue_id INTEGER NOT NULL DEFAULT 1;
ALTER TABLE queue ADD COLUMN start_at TEXT;
ALTER TABLE queue ADD COLUMN pinned   INTEGER NOT NULL DEFAULT 0;
";

/// v7: segment re-verification. Each completed segment's hash (`sha256:<hex>`, NULL = not hashed: older versions or
/// not complete) and the `.part` file's stamp when the hashes were saved (NULL = unknown: re-hash before trusting).
const SCHEMA_V7: &str = "
ALTER TABLE segments  ADD COLUMN hash       TEXT;
ALTER TABLE downloads ADD COLUMN part_stamp TEXT;
";

/// v8: network usage (issue #45): bytes VeloxFetch received from the network, per local day (`YYYY-MM-DD`). Never
/// tied to a download: deleting downloads or clearing the history keeps it; an update or a reinstall keeps the
/// database, so it keeps counting.
const SCHEMA_V8: &str = "
CREATE TABLE network_usage (
    day   TEXT    PRIMARY KEY,
    bytes INTEGER NOT NULL
);
";

/// Applied in order; `PRAGMA user_version` records how many have run.
const MIGRATIONS: &[&str] = &[SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8];
const SCHEMA_VERSION: i32 = MIGRATIONS.len() as i32;

const SCHEMA_V1: &str = "
CREATE TABLE downloads (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    url             TEXT    NOT NULL,
    filename        TEXT    NOT NULL,
    destination     TEXT    NOT NULL,
    total_size      INTEGER,
    downloaded_size INTEGER NOT NULL DEFAULT 0,
    status          TEXT    NOT NULL,
    etag            TEXT,
    last_modified   TEXT,
    resumable       INTEGER NOT NULL DEFAULT 0,
    checksum        TEXT,
    last_error      TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX idx_downloads_status ON downloads(status);

CREATE TABLE segments (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    download_id   INTEGER NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    idx           INTEGER NOT NULL,
    start_offset  INTEGER NOT NULL,
    end_offset    INTEGER NOT NULL,
    downloaded    INTEGER NOT NULL DEFAULT 0,
    status        TEXT    NOT NULL,
    retry_count   INTEGER NOT NULL DEFAULT 0,
    UNIQUE (download_id, idx)
);

CREATE TABLE queue (
    download_id INTEGER PRIMARY KEY REFERENCES downloads(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    priority    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    download_id INTEGER,
    url         TEXT    NOT NULL,
    filename    TEXT    NOT NULL,
    destination TEXT    NOT NULL,
    size        INTEGER,
    outcome     TEXT    NOT NULL,
    finished_at INTEGER NOT NULL
);

CREATE TABLE scheduler_jobs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    download_id INTEGER REFERENCES downloads(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    run_at      INTEGER,
    cron        TEXT,
    payload     TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1
);
";

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn db_err(e: rusqlite::Error) -> DownloadError {
    DownloadError::Store(e.to_string())
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: i64,
    pub download_id: Option<i64>,
    pub url: String,
    pub filename: String,
    pub destination: PathBuf,
    pub size: Option<u64>,
    pub outcome: String,
    pub finished_at: i64,
}

/// Where a download sits in the Smart Download Queue (see `SCHEMA_V6`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub download_id: i64,
    pub queue_id: i64,
    /// Lower runs first (after priority).
    pub position: i64,
    /// Local time `YYYY-MM-DDTHH:MM` at which it starts on its own.
    pub start_at: Option<String>,
    /// The user put it in this queue.
    pub pinned: bool,
}

/// What the queue needs to know about a download that is not finished, without loading its segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueRow {
    pub id: i64,
    pub status: DownloadStatus,
    pub priority: Priority,
    pub total_size: Option<u64>,
    pub downloaded: u64,
    pub resumable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// State and `.part` are consistent; resume continues from the saved segments.
    ResumeReady { downloaded: u64 },
    /// Saved state can't be trusted; progress was reset and the download will start over.
    Restart { reason: String },
    /// The file had already been moved into place; only the status was stale.
    AlreadyFinished,
}

#[derive(Debug, Clone)]
pub struct RecoveryReport {
    pub id: i64,
    pub filename: String,
    pub action: RecoveryAction,
}

/// What the `request_headers` column may hold: only sealed values, never the plain text.
const HEADERS_CONTEXT: &str = "downloads.request_headers";

pub struct SqliteStore {
    conn: Mutex<Connection>,
    /// Seals the browser's cookies / an `Authorization` header before they reach the file. Without
    /// one, such secrets are simply not persisted (they live in memory until the download ends).
    sealer: Mutex<Option<std::sync::Arc<secrets::Sealer>>>,
    /// Secrets of downloads that could not be sealed (no sealer): held here, in memory, for as long
    /// as the process runs. They are never written anywhere.
    volatile: Mutex<std::collections::HashMap<i64, Vec<(String, String)>>>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path).map_err(db_err)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL").map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(db_err)?;
        // Deleted and overwritten values are zeroed in the file, so a removed cookie is really gone.
        conn.pragma_update(None, "secure_delete", "ON").map_err(db_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(db_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(db_err)?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(db_err)?;
        if version > SCHEMA_VERSION {
            return Err(DownloadError::Store(format!("database schema v{version} is newer than this build supports (v{SCHEMA_VERSION})")));
        }
        if version < SCHEMA_VERSION {
            let tx = conn.unchecked_transaction().map_err(db_err)?;
            for m in &MIGRATIONS[version.max(0) as usize..] {
                tx.execute_batch(m).map_err(db_err)?;
            }
            tx.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(db_err)?;
            tx.commit().map_err(db_err)?;
        }
        Ok(Self { conn: Mutex::new(conn), sealer: Mutex::new(None), volatile: Mutex::new(Default::default()) })
    }

    /// Start sealing secrets with `sealer`, and deal with anything stored in the clear by an older
    /// version: it is sealed (or, with no sealer, dropped). Returns how many rows were rewritten.
    pub fn set_sealer(&self, sealer: Option<std::sync::Arc<secrets::Sealer>>) -> Result<usize> {
        *self.sealer.lock().unwrap_or_else(|p| p.into_inner()) = sealer.clone();
        let conn = self.conn();
        let rows: Vec<(i64, String)> = conn
            .prepare("SELECT id, request_headers FROM downloads WHERE request_headers IS NOT NULL AND request_headers NOT LIKE 's1:%'")
            .map_err(db_err)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_err)?
            .collect::<std::result::Result<_, _>>()
            .map_err(db_err)?;
        for (id, plain) in &rows {
            let value = sealer
                .as_ref()
                .filter(|_| serde_json::from_str::<Vec<(String, String)>>(plain).is_ok())
                .map(|s| s.seal(plain.as_bytes(), HEADERS_CONTEXT));
            conn.execute("UPDATE downloads SET request_headers = ?2 WHERE id = ?1", params![id, value]).map_err(db_err)?;
        }
        if !rows.is_empty() {
            // Old copies of the clear text may still sit in free pages or the write-ahead log.
            let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
            conn.execute_batch("VACUUM").map_err(db_err)?;
        }
        Ok(rows.len())
    }

    fn volatile(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<i64, Vec<(String, String)>>> {
        self.volatile.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A record whose secrets could not be read from the file gets the in-memory copy, if any.
    fn with_volatile(&self, mut rec: DownloadRecord) -> DownloadRecord {
        if rec.request_headers.is_empty() {
            if let Some(h) = rec.id.and_then(|id| self.volatile().get(&id).cloned()) {
                rec.request_headers = h;
            }
        }
        rec
    }

    fn sealer(&self) -> Option<std::sync::Arc<secrets::Sealer>> {
        self.sealer.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn list(&self) -> Result<Vec<DownloadRecord>> {
        let conn = self.conn();
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM downloads ORDER BY id")
            .map_err(db_err)?
            .query_map([], |r| r.get(0))
            .map_err(db_err)?
            .collect::<std::result::Result<_, _>>()
            .map_err(db_err)?;
        let sealer = self.sealer();
        ids.into_iter()
            .filter_map(|id| load_locked(&conn, id, sealer.as_deref()).transpose())
            .map(|r| r.map(|rec| self.with_volatile(rec)))
            .collect()
    }

    /// Remove a download and its segments (the caller deletes the `.part` file).
    pub fn delete(&self, id: i64) -> Result<()> {
        failpoint::hit("delete.before");
        self.volatile().remove(&id);
        self.conn().execute("DELETE FROM downloads WHERE id = ?1", [id]).map_err(db_err)?;
        Ok(())
    }

    /// A finished file that is not a row of the download list (a video saved by the video subsystem).
    pub fn add_history(&self, url: &str, filename: &str, destination: &Path, size: u64) -> Result<i64> {
        failpoint::hit_io("store.write").map_err(|e| DownloadError::Store(e.to_string()))?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO history (download_id, url, filename, destination, size, outcome, finished_at)
             VALUES (NULL, ?1, ?2, ?3, ?4, 'completed', ?5)",
            params![url, filename, destination.to_string_lossy(), size as i64, now()],
        )
        .map_err(db_err)?;
        Ok(conn.last_insert_rowid())
    }

    pub fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>> {
        let conn = self.conn();
        let mut st = conn
            .prepare(
                "SELECT id, download_id, url, filename, destination, size, outcome, finished_at
                 FROM history ORDER BY id DESC LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = st.query_map([limit], history_row).map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// One line of the history.
    pub fn history_entry(&self, id: i64) -> Result<Option<HistoryEntry>> {
        self.conn()
            .query_row(
                "SELECT id, download_id, url, filename, destination, size, outcome, finished_at FROM history WHERE id = ?1",
                [id],
                history_row,
            )
            .optional()
            .map_err(db_err)
    }

    /// Forget one line of the history (`Some(id)`) or all of it (`None`). Files are not touched, and the
    /// sizes stay counted in the lifetime total. Returns how many lines went.
    pub fn forget_history(&self, id: Option<i64>) -> Result<usize> {
        failpoint::hit_io("store.write").map_err(|e| DownloadError::Store(e.to_string()))?;
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db_err)?;
        let (filter, args): (&str, Vec<i64>) = match id {
            Some(id) => ("WHERE id = ?1", vec![id]),
            None => ("", vec![]),
        };
        let gone: i64 = tx
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(size), 0) FROM history {filter}{} outcome = 'completed'",
                    if id.is_some() { " AND" } else { "WHERE" }
                ),
                rusqlite::params_from_iter(args.iter()),
                |r| r.get(0),
            )
            .map_err(db_err)?;
        let n = tx.execute(&format!("DELETE FROM history {filter}"), rusqlite::params_from_iter(args.iter())).map_err(db_err)?;
        if gone > 0 {
            tx.execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(value AS INTEGER) + CAST(excluded.value AS INTEGER) AS TEXT)",
                rusqlite::params![FORGOTTEN_BYTES_KEY, gone.to_string()],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok(n)
    }

    /// Total size of everything ever completed (sum over history, plus lines the user removed from it).
    pub fn history_total_bytes(&self) -> Result<u64> {
        let conn = self.conn();
        let n: i64 =
            conn.query_row("SELECT COALESCE(SUM(size), 0) FROM history WHERE outcome = 'completed'", [], |r| r.get(0)).map_err(db_err)?;
        let forgotten: i64 = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [FORGOTTEN_BYTES_KEY], |r| r.get::<_, String>(0))
            .optional()
            .map_err(db_err)?
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        Ok(n.max(0).saturating_add(forgotten.max(0)) as u64)
    }

    /// Add `bytes` received from the network on `day` (`YYYY-MM-DD`, local time) to the network usage.
    pub fn add_network_usage(&self, day: &str, bytes: u64) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        failpoint::hit_io("store.write").map_err(|e| DownloadError::Store(e.to_string()))?;
        let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
        self.conn()
            .execute(
                "INSERT INTO network_usage(day, bytes) VALUES(?1, ?2)
                 ON CONFLICT(day) DO UPDATE SET bytes = CASE WHEN bytes > 9223372036854775807 - excluded.bytes
                     THEN 9223372036854775807 ELSE bytes + excluded.bytes END",
                params![day, bytes],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Network usage: everything ever counted, on `day` (`YYYY-MM-DD`) and in `month` (`YYYY-MM`).
    pub fn network_usage(&self, day: &str, month: &str) -> Result<NetworkUsage> {
        let conn = self.conn();
        // TOTAL (a float) never fails on overflow as SUM does; a u64 cast saturates.
        let one = |sql: &str, arg: Option<&str>| -> Result<u64> {
            let n: f64 = match arg {
                Some(a) => conn.query_row(sql, [a], |r| r.get(0)),
                None => conn.query_row(sql, [], |r| r.get(0)),
            }
            .map_err(db_err)?;
            Ok(n.max(0.0) as u64)
        };
        let sum =
            |filter: &str, arg: &str| one(&format!("SELECT TOTAL(bytes) FROM network_usage {filter}"), (!arg.is_empty()).then_some(arg));
        Ok(NetworkUsage { total: sum("", "")?, today: sum("WHERE day = ?1", day)?, month: sum("WHERE substr(day, 1, 7) = ?1", month)? })
    }

    /// Save the diagnostics of a download (JSON, secrets already removed by the caller). A download that no
    /// longer exists is ignored.
    pub fn save_diagnostics(&self, download_id: i64, data: &str) -> Result<()> {
        failpoint::hit_io("store.write").map_err(|e| DownloadError::Store(e.to_string()))?;
        self.conn()
            .execute(
                "INSERT INTO diagnostics(download_id, data, updated_at)
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM downloads WHERE id = ?1)
                 ON CONFLICT(download_id) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at",
                params![download_id, data, now()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn diagnostics(&self, download_id: i64) -> Result<Option<String>> {
        self.conn().query_row("SELECT data FROM diagnostics WHERE download_id = ?1", [download_id], |r| r.get(0)).optional().map_err(db_err)
    }

    /// Number of downloads per status, e.g. `[("downloading", 2), ("queued", 5)]`.
    pub fn status_counts(&self) -> Result<Vec<(String, u64)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT status, COUNT(*) FROM downloads GROUP BY status").map_err(db_err)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))).map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        failpoint::hit_io("store.write").map_err(|e| DownloadError::Store(e.to_string()))?;
        failpoint::hit("settings.before");
        let r = self
            .conn()
            .execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(db_err);
        failpoint::hit("settings.after");
        r.map(drop)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn().query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional().map_err(db_err)
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn().execute("DELETE FROM settings WHERE key = ?1", [key]).map_err(db_err)?;
        Ok(())
    }

    /// All settings whose key starts with `prefix` (as `(key, value)`).
    pub fn settings_with_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT key, value FROM settings WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key").map_err(db_err)?;
        let rows = st.query_map([prefix], |r| Ok((r.get(0)?, r.get(1)?))).map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Every saved queue placement.
    pub fn placements(&self) -> Result<Vec<Placement>> {
        let conn = self.conn();
        let mut st =
            conn.prepare("SELECT download_id, queue_id, position, start_at, pinned FROM queue ORDER BY download_id").map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok(Placement {
                    download_id: r.get(0)?,
                    queue_id: r.get(1)?,
                    position: r.get(2)?,
                    start_at: r.get(3)?,
                    pinned: r.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Save where a download sits. A download that no longer exists is ignored.
    pub fn set_placement(&self, p: &Placement) -> Result<()> {
        failpoint::hit_io("store.write").map_err(|e| DownloadError::Store(e.to_string()))?;
        self.conn()
            .execute(
                "INSERT INTO queue (download_id, position, priority, queue_id, start_at, pinned)
                 SELECT ?1, ?2, 0, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM downloads WHERE id = ?1)
                 ON CONFLICT(download_id) DO UPDATE SET position = excluded.position, queue_id = excluded.queue_id,
                     start_at = excluded.start_at, pinned = excluded.pinned",
                params![p.download_id, p.position, p.queue_id, p.start_at, p.pinned as i64],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Move every download of queue `from` to queue `to` (a queue was deleted). Returns how many moved.
    pub fn move_queue(&self, from: i64, to: i64) -> Result<usize> {
        self.conn().execute("UPDATE queue SET queue_id = ?2, pinned = 0 WHERE queue_id = ?1", params![from, to]).map_err(db_err)
    }

    /// The downloads that are not finished (queued, running, paused or failed), oldest first.
    pub fn queue_rows(&self) -> Result<Vec<QueueRow>> {
        let conn = self.conn();
        let mut st = conn
            .prepare(
                "SELECT id, status, priority, total_size, downloaded_size, resumable FROM downloads
                 WHERE status IN ('queued', 'downloading', 'verifying', 'paused', 'failed') ORDER BY id",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, status, prio, total, done, resumable) = row.map_err(db_err)?;
            let Some(status) = DownloadStatus::parse(&status) else { continue };
            out.push(QueueRow {
                id,
                status,
                priority: Priority::from_i64(prio),
                total_size: total.map(|t| t.max(0) as u64),
                downloaded: done.max(0) as u64,
                resumable: resumable != 0,
            });
        }
        Ok(out)
    }

    /// Structural check of the database file (`integrity_check` + `foreign_key_check`). A killed
    /// process must never leave anything but a sound file behind: the crash tests call this.
    pub fn integrity_check(&self) -> Result<()> {
        let conn = self.conn();
        let rows = |sql: &str| -> Result<Vec<String>> {
            let mut st = conn.prepare(sql).map_err(db_err)?;
            let v =
                st.query_map([], |r| r.get::<_, String>(0)).map_err(db_err)?.collect::<std::result::Result<Vec<_>, _>>().map_err(db_err)?;
            Ok(v)
        };
        let problems = rows("PRAGMA integrity_check")?;
        if problems != ["ok"] {
            return Err(DownloadError::Store(format!("integrity check failed: {}", problems.join("; "))));
        }
        let orphans = rows("SELECT \"table\" FROM pragma_foreign_key_check")?;
        if !orphans.is_empty() {
            return Err(DownloadError::Store(format!("rows without a parent in: {}", orphans.join(", "))));
        }
        Ok(())
    }

    /// Crash recovery. Call once at application start, before resuming anything.
    ///
    /// Downloads left in `Downloading`/`Verifying` were interrupted (crash, power loss, kill).
    /// Each is checked against its `.part` file and turned into `Paused` (resumable) or reset
    /// so it restarts cleanly. Segment statuses are normalized so resume never trusts a
    /// half-finished state.
    pub fn recover_interrupted(&self) -> Result<Vec<RecoveryReport>> {
        let interrupted: Vec<DownloadRecord> =
            self.list()?.into_iter().filter(|r| matches!(r.status, DownloadStatus::Downloading | DownloadStatus::Verifying)).collect();
        let mut reports = Vec::new();
        for mut rec in interrupted {
            let part = part_path(&rec.destination);
            let part_len = std::fs::metadata(&part).ok().map(|m| m.len());
            let action = if part_len.is_none() && rec.destination.is_file() {
                // Crashed between rename and the final status write.
                rec.status = DownloadStatus::Completed;
                RecoveryAction::AlreadyFinished
            } else if let Some(reason) = self.untrustworthy(&rec, part_len) {
                let _ = std::fs::remove_file(&part);
                for s in &mut rec.segments {
                    s.downloaded = 0;
                    s.status = SegmentStatus::Pending;
                }
                rec.downloaded = 0;
                rec.status = DownloadStatus::Paused;
                RecoveryAction::Restart { reason }
            } else {
                for s in &mut rec.segments {
                    s.status = if s.is_complete() { SegmentStatus::Completed } else { SegmentStatus::Pending };
                }
                rec.downloaded = rec.segments.iter().map(|s| s.downloaded).sum();
                rec.status = DownloadStatus::Paused;
                RecoveryAction::ResumeReady { downloaded: rec.downloaded }
            };
            self.save(&rec)?;
            if action == RecoveryAction::AlreadyFinished {
                if let Some(id) = rec.id {
                    self.complete(id)?;
                }
            }
            reports.push(RecoveryReport { id: rec.id.unwrap_or_default(), filename: rec.filename.clone(), action });
        }
        Ok(reports)
    }

    fn untrustworthy(&self, rec: &DownloadRecord, part_len: Option<u64>) -> Option<String> {
        if !rec.resumable || rec.segments.is_empty() {
            return Some("transfer is not resumable".into());
        }
        match (part_len, rec.total_size) {
            (None, _) => return Some(".part file is missing".into()),
            (Some(l), Some(t)) if l != t => return Some(format!(".part is {l} bytes, expected {t}")),
            (_, None) => return Some("total size unknown".into()),
            _ => {}
        }
        if rec.segments.iter().any(|s| s.downloaded > s.len()) {
            return Some("segment state inconsistent".into());
        }
        None
    }
}

impl DownloadStore for SqliteStore {
    fn destination_in_use(&self, destination: &Path, except: Option<i64>) -> Result<bool> {
        // Windows and macOS file systems ignore case.
        let sql = if cfg!(any(windows, target_os = "macos")) {
            "SELECT 1 FROM downloads WHERE lower(destination) = lower(?1) AND id != ?2 AND status NOT IN ('completed', 'cancelled') LIMIT 1"
        } else {
            "SELECT 1 FROM downloads WHERE destination = ?1 AND id != ?2 AND status NOT IN ('completed', 'cancelled') LIMIT 1"
        };
        let found = self
            .conn()
            .query_row(sql, params![destination.to_string_lossy(), except.unwrap_or(-1)], |_| Ok(()))
            .optional()
            .map_err(db_err)?;
        Ok(found.is_some())
    }

    fn load(&self, id: i64) -> Result<Option<DownloadRecord>> {
        Ok(load_locked(&self.conn(), id, self.sealer().as_deref())?.map(|rec| self.with_volatile(rec)))
    }

    fn save(&self, rec: &DownloadRecord) -> Result<i64> {
        failpoint::hit_io("store.write").map_err(|e| DownloadError::Store(e.to_string()))?;
        let headers = encode_headers(&rec.request_headers, self.sealer().as_deref());
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db_err)?;
        let ts = now();
        let dest = rec.destination.to_string_lossy().to_string();
        let id = match rec.id {
            None => {
                tx.execute(
                    "INSERT INTO downloads (url, filename, destination, total_size, downloaded_size, status, etag,
                        last_modified, resumable, checksum, last_error, created_at, updated_at,
                        priority, category, speed_limit, content_type, server, request_headers, part_stamp)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                    params![
                        rec.url,
                        rec.filename,
                        dest,
                        rec.total_size.map(|v| v as i64),
                        rec.downloaded as i64,
                        rec.status.as_str(),
                        rec.etag,
                        rec.last_modified,
                        rec.resumable,
                        rec.checksum,
                        rec.last_error,
                        ts,
                        rec.priority.as_i64(),
                        rec.category,
                        rec.speed_limit.map(|v| v as i64),
                        rec.content_type,
                        rec.server,
                        headers,
                        rec.part_stamp
                    ],
                )
                .map_err(db_err)?;
                tx.last_insert_rowid()
            }
            Some(id) => {
                let n = tx
                    .execute(
                        "UPDATE downloads SET url=?2, filename=?3, destination=?4, total_size=?5, downloaded_size=?6,
                            status=?7, etag=?8, last_modified=?9, resumable=?10, checksum=?11, last_error=?12,
                            updated_at=?13, priority=?14, category=?15, speed_limit=?16, content_type=?17, server=?18,
                            request_headers=?19, part_stamp=?20 WHERE id=?1",
                        params![
                            id,
                            rec.url,
                            rec.filename,
                            dest,
                            rec.total_size.map(|v| v as i64),
                            rec.downloaded as i64,
                            rec.status.as_str(),
                            rec.etag,
                            rec.last_modified,
                            rec.resumable,
                            rec.checksum,
                            rec.last_error,
                            ts,
                            rec.priority.as_i64(),
                            rec.category,
                            rec.speed_limit.map(|v| v as i64),
                            rec.content_type,
                            rec.server,
                            headers,
                            rec.part_stamp
                        ],
                    )
                    .map_err(db_err)?;
                if n == 0 {
                    return Err(DownloadError::Store(format!("download {id} does not exist")));
                }
                id
            }
        };
        tx.execute("DELETE FROM segments WHERE download_id = ?1 AND idx >= ?2", params![id, rec.segments.len() as i64]).map_err(db_err)?;
        {
            let mut st = tx
                .prepare_cached(
                    "INSERT INTO segments (download_id, idx, start_offset, end_offset, downloaded, status, retry_count, hash)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(download_id, idx) DO UPDATE SET start_offset = excluded.start_offset,
                        end_offset = excluded.end_offset, downloaded = excluded.downloaded,
                        status = excluded.status, retry_count = excluded.retry_count, hash = excluded.hash",
                )
                .map_err(db_err)?;
            for s in &rec.segments {
                // A hash only ever describes a complete segment (never saved for one that is not).
                let hash = s.hash.as_deref().filter(|_| s.is_complete());
                st.execute(params![
                    id,
                    s.id as i64,
                    s.start as i64,
                    s.end as i64,
                    s.downloaded as i64,
                    s.status.as_str(),
                    s.retry_count,
                    hash
                ])
                .map_err(db_err)?;
            }
        }
        failpoint::hit("store.before_commit"); // everything written, nothing committed: must vanish
        tx.commit().map_err(db_err)?;
        failpoint::hit("store.after_commit");
        if self.sealer().is_none() {
            let mut v = self.volatile();
            if rec.request_headers.is_empty() {
                v.remove(&id);
            } else {
                v.insert(id, rec.request_headers.clone());
            }
        }
        Ok(id)
    }

    fn complete(&self, id: i64) -> Result<()> {
        failpoint::hit("history.before");
        let conn = self.conn();
        conn.execute(
            "INSERT INTO history (download_id, url, filename, destination, size, outcome, finished_at)
             SELECT id, url, filename, destination, COALESCE(total_size, downloaded_size), 'completed', ?2
             FROM downloads WHERE id = ?1",
            params![id, now()],
        )
        .map_err(db_err)?;
        failpoint::hit("history.after");
        Ok(())
    }
}

/// `None` (SQL NULL) when there is nothing to store, so ordinary downloads keep a NULL column.
/// Settings key: bytes of completed downloads whose history lines were removed (kept in the lifetime total).
const FORGOTTEN_BYTES_KEY: &str = "stats.forgotten_bytes";

/// Bytes received from the network, as saved ([`SqliteStore::network_usage`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetworkUsage {
    pub total: u64,
    pub today: u64,
    pub month: u64,
}

fn history_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: r.get(0)?,
        download_id: r.get(1)?,
        url: r.get(2)?,
        filename: r.get(3)?,
        destination: PathBuf::from(r.get::<_, String>(4)?),
        size: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        outcome: r.get(6)?,
        finished_at: r.get(7)?,
    })
}

/// Secrets go to disk sealed or not at all: with no sealer nothing is stored.
fn encode_headers(h: &[(String, String)], sealer: Option<&secrets::Sealer>) -> Option<String> {
    if h.is_empty() {
        return None;
    }
    let json = serde_json::to_string(h).ok()?;
    sealer.map(|s| s.seal(json.as_bytes(), HEADERS_CONTEXT))
}

/// Corrupt, unreadable (another key, no key) or missing data means "no extra headers"; it must
/// never make a download unloadable, and clear text is never accepted back.
fn decode_headers(s: Option<String>, sealer: Option<&secrets::Sealer>) -> Vec<(String, String)> {
    let Some(s) = s else { return Vec::new() };
    let Some(plain) = sealer.and_then(|k| k.open(&s, HEADERS_CONTEXT)) else { return Vec::new() };
    serde_json::from_slice(&plain).unwrap_or_default()
}

fn load_locked(conn: &Connection, id: i64, sealer: Option<&secrets::Sealer>) -> Result<Option<DownloadRecord>> {
    let rec = conn
        .query_row(
            "SELECT url, filename, destination, total_size, downloaded_size, status, etag, last_modified,
                    resumable, checksum, last_error, priority, category, speed_limit, content_type, server,
                    request_headers, part_stamp
             FROM downloads WHERE id = ?1",
            [id],
            |r| {
                Ok(DownloadRecord {
                    id: Some(id),
                    url: r.get(0)?,
                    filename: r.get(1)?,
                    destination: PathBuf::from(r.get::<_, String>(2)?),
                    total_size: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    downloaded: r.get::<_, i64>(4)? as u64,
                    status: DownloadStatus::parse(&r.get::<_, String>(5)?).unwrap_or(DownloadStatus::Failed),
                    etag: r.get(6)?,
                    last_modified: r.get(7)?,
                    resumable: r.get(8)?,
                    checksum: r.get(9)?,
                    last_error: r.get(10)?,
                    segments: Vec::new(),
                    priority: Priority::from_i64(r.get(11)?),
                    category: r.get(12)?,
                    speed_limit: r.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                    content_type: r.get(14)?,
                    server: r.get(15)?,
                    request_headers: decode_headers(r.get::<_, Option<String>>(16)?, sealer),
                    part_stamp: r.get(17)?,
                })
            },
        )
        .optional()
        .map_err(db_err)?;
    let Some(mut rec) = rec else { return Ok(None) };
    let mut st = conn
        .prepare_cached(
            "SELECT idx, start_offset, end_offset, downloaded, status, retry_count, hash
             FROM segments WHERE download_id = ?1 ORDER BY idx",
        )
        .map_err(db_err)?;
    rec.segments = st
        .query_map([id], |r| {
            Ok(Segment {
                id: r.get::<_, i64>(0)? as usize,
                start: r.get::<_, i64>(1)? as u64,
                end: r.get::<_, i64>(2)? as u64,
                downloaded: r.get::<_, i64>(3)? as u64,
                status: SegmentStatus::parse(&r.get::<_, String>(4)?).unwrap_or(SegmentStatus::Pending),
                retry_count: r.get(5)?,
                hash: r.get(6)?,
            })
        })
        .map_err(db_err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db_err)?;
    Ok(Some(rec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(dest: PathBuf) -> DownloadRecord {
        let mut segs = vec![Segment::new(0, 0, 499), Segment::new(1, 500, 999)];
        segs[0].downloaded = 500;
        segs[0].status = SegmentStatus::Completed;
        segs[1].downloaded = 100;
        segs[1].status = SegmentStatus::Downloading;
        DownloadRecord {
            id: None,
            url: "http://x/f".into(),
            filename: "f".into(),
            destination: dest,
            total_size: Some(1000),
            downloaded: 600,
            status: DownloadStatus::Downloading,
            etag: Some("\"e\"".into()),
            last_modified: None,
            resumable: true,
            checksum: Some("sha256:00".into()),
            last_error: None,
            segments: segs,
            priority: Priority::High,
            category: Some("iso".into()),
            speed_limit: Some(2_000_000),
            content_type: Some("application/zip".into()),
            server: Some("nginx".into()),
            request_headers: vec![("referer".into(), "https://a.test/".into()), ("cookie".into(), "sid=1".into())],
            part_stamp: None,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-db-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_update_and_segment_shrink() {
        let s = SqliteStore::open_in_memory().unwrap();
        let mut r = rec(PathBuf::from("/tmp/f"));
        let id = s.save(&r).unwrap();
        let got = s.load(id).unwrap().unwrap();
        assert_eq!(got.segments.len(), 2);
        assert_eq!((got.segments[1].downloaded, got.segments[1].status), (100, SegmentStatus::Downloading));
        assert_eq!(got.etag.as_deref(), Some("\"e\""));
        assert_eq!((got.priority, got.category.as_deref(), got.speed_limit), (Priority::High, Some("iso"), Some(2_000_000)));

        r.id = Some(id);
        r.segments.truncate(1);
        r.status = DownloadStatus::Paused;
        s.save(&r).unwrap();
        let got = s.load(id).unwrap().unwrap();
        assert_eq!((got.segments.len(), got.status), (1, DownloadStatus::Paused));
        assert!(s.load(9999).unwrap().is_none());
    }

    #[test]
    fn totals_and_counts() {
        let s = SqliteStore::open_in_memory().unwrap();
        let a = s.save(&rec(PathBuf::from("/tmp/a"))).unwrap();
        let mut r = rec(PathBuf::from("/tmp/b"));
        r.status = DownloadStatus::Paused;
        s.save(&r).unwrap();
        s.complete(a).unwrap();
        assert_eq!(s.history_total_bytes().unwrap(), 1000);
        let mut counts = s.status_counts().unwrap();
        counts.sort();
        assert_eq!(counts, vec![("downloading".to_string(), 1), ("paused".to_string(), 1)]);
    }

    #[test]
    fn network_usage_is_kept_by_day_and_outlives_downloads_and_history() {
        let d = tmp("usage");
        let db = d.join("v.db");
        {
            let s = SqliteStore::open(&db).unwrap();
            s.add_network_usage("2026-09-30", 500).unwrap();
            s.add_network_usage("2026-10-14", 1000).unwrap();
            s.add_network_usage("2026-10-15", 200).unwrap();
            s.add_network_usage("2026-10-15", 300).unwrap();
            s.add_network_usage("2026-10-15", 0).unwrap();
            // Downloads and the history go: the usage stays.
            let id = s.save(&rec(d.join("f"))).unwrap();
            s.complete(id).unwrap();
            s.delete(id).unwrap();
            s.forget_history(None).unwrap();
        }
        // The app closed and opened again (an update, a reinstall that keeps the data): still there.
        let s = SqliteStore::open(&db).unwrap();
        assert_eq!(s.network_usage("2026-10-15", "2026-10").unwrap(), NetworkUsage { total: 2000, today: 500, month: 1500 });
        assert_eq!(s.network_usage("2026-11-01", "2026-11").unwrap(), NetworkUsage { total: 2000, today: 0, month: 0 });
        // Never wraps around.
        s.add_network_usage("2026-10-16", u64::MAX).unwrap();
        s.add_network_usage("2026-10-16", u64::MAX).unwrap();
        let u = s.network_usage("2026-10-16", "2026-10").unwrap();
        assert!(u.today >= i64::MAX as u64 / 2 && u.total >= u.today, "{u:?}");
    }

    #[test]
    fn history_settings_and_delete() {
        let s = SqliteStore::open_in_memory().unwrap();
        let id = s.save(&rec(PathBuf::from("/tmp/f"))).unwrap();
        s.complete(id).unwrap();
        assert_eq!(s.history(10).unwrap()[0].size, Some(1000));
        s.set_setting("k", "1").unwrap();
        s.set_setting("k", "2").unwrap();
        assert_eq!(s.get_setting("k").unwrap().as_deref(), Some("2"));
        s.delete(id).unwrap();
        assert!(s.load(id).unwrap().is_none());
    }

    #[test]
    fn reopening_persists_and_schema_is_versioned() {
        let d = tmp("reopen");
        let db = d.join("v.db");
        let id = SqliteStore::open(&db).unwrap().save(&rec(d.join("f"))).unwrap();
        let s = SqliteStore::open(&db).unwrap();
        assert!(s.load(id).unwrap().is_some());
        let v: i32 = s.conn().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn diagnostics_are_kept_per_download_and_go_with_it() {
        let d = tmp("diag");
        let s = SqliteStore::open(d.join("d.db")).unwrap();
        let id = s.save(&rec(d.join("a.bin"))).unwrap();
        assert_eq!(s.diagnostics(id).unwrap(), None);
        s.save_diagnostics(id, r#"{"attempts":1}"#).unwrap();
        s.save_diagnostics(id, r#"{"attempts":2}"#).unwrap();
        assert_eq!(s.diagnostics(id).unwrap().as_deref(), Some(r#"{"attempts":2}"#));
        // Unknown downloads are ignored rather than creating orphans.
        s.save_diagnostics(9999, "{}").unwrap();
        assert_eq!(s.diagnostics(9999).unwrap(), None);
        s.delete(id).unwrap();
        assert_eq!(s.diagnostics(id).unwrap(), None);
        s.integrity_check().unwrap();
    }

    #[test]
    fn queue_placements_are_saved_moved_and_go_with_the_download() {
        let d = tmp("placement");
        let s = SqliteStore::open(d.join("q.db")).unwrap();
        let a = s.save(&rec(d.join("a.bin"))).unwrap();
        let b = s.save(&rec(d.join("b.bin"))).unwrap();
        assert!(s.placements().unwrap().is_empty(), "no row: main queue, order added");
        let pa = Placement { download_id: a, queue_id: 2, position: -5, start_at: Some("2026-03-03T02:00".into()), pinned: true };
        s.set_placement(&pa).unwrap();
        s.set_placement(&Placement { download_id: b, queue_id: 2, position: 7, start_at: None, pinned: false }).unwrap();
        s.set_placement(&Placement { download_id: 9999, queue_id: 2, position: 1, start_at: None, pinned: false }).unwrap();
        assert_eq!(s.placements().unwrap().len(), 2, "a download that does not exist is ignored");
        assert_eq!(s.placements().unwrap()[0], pa);
        // Updated in place.
        s.set_placement(&Placement { position: 3, ..pa.clone() }).unwrap();
        assert_eq!(s.placements().unwrap()[0].position, 3);
        // A deleted queue hands its downloads to another one, no longer pinned.
        assert_eq!(s.move_queue(2, 1).unwrap(), 2);
        assert!(s.placements().unwrap().iter().all(|p| p.queue_id == 1 && !p.pinned));
        s.delete(a).unwrap();
        assert_eq!(s.placements().unwrap().len(), 1, "the row goes with the download");
        let rows = s.queue_rows().unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![b]);
        assert_eq!(rows[0].priority, Priority::High);
        let mut done = s.load(b).unwrap().unwrap();
        done.status = DownloadStatus::Completed;
        s.save(&done).unwrap();
        assert!(s.queue_rows().unwrap().is_empty(), "finished downloads are not the queue's business");
        s.integrity_check().unwrap();
    }

    #[test]
    fn upgrades_a_v1_database_in_place() {
        let d = tmp("migrate");
        let db = d.join("old.db");
        {
            // A database exactly as the previous release created it.
            let c = Connection::open(&db).unwrap();
            c.execute_batch(SCHEMA_V1).unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
            c.execute(
                "INSERT INTO downloads (url, filename, destination, status, created_at, updated_at)
                 VALUES ('http://x/old', 'old.bin', '/tmp/old.bin', 'paused', 1, 1)",
                [],
            )
            .unwrap();
        }
        let s = SqliteStore::open(&db).unwrap();
        s.set_sealer(Some(std::sync::Arc::new(secrets::Sealer::from_key([1; 32])))).unwrap();
        let old = s.load(1).unwrap().unwrap();
        assert_eq!((old.priority, old.category, old.speed_limit), (Priority::Normal, None, None));
        let v: i32 = s.conn().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // The diagnostics table exists for old downloads too.
        s.save_diagnostics(1, r#"{"attempts":1}"#).unwrap();
        assert_eq!(s.diagnostics(1).unwrap().as_deref(), Some(r#"{"attempts":1}"#));
        // Newly saved rows use the new columns.
        let id = s.save(&rec(d.join("n.bin"))).unwrap();
        let fresh = s.load(id).unwrap().unwrap();
        assert_eq!(
            (fresh.priority, fresh.content_type.as_deref(), fresh.server.as_deref()),
            (Priority::High, Some("application/zip"), Some("nginx"))
        );
        assert_eq!((old.content_type, old.server), (None, None));
        assert!(old.request_headers.is_empty(), "a database from before v4 has no request headers");
        assert_eq!(fresh.request_headers.len(), 2);
        assert_eq!(fresh.request_headers[1], ("cookie".to_string(), "sid=1".to_string()));
        // Corrupt header data must never make a download unloadable.
        s.conn().execute("UPDATE downloads SET request_headers = 'not json' WHERE id = ?1", [id]).unwrap();
        assert!(s.load(id).unwrap().unwrap().request_headers.is_empty());
    }

    fn sealer(b: u8) -> Option<std::sync::Arc<secrets::Sealer>> {
        Some(std::sync::Arc::new(secrets::Sealer::from_key([b; 32])))
    }

    /// Every byte a database leaves on disk (main file, write-ahead log, shared memory).
    fn disk_bytes(db: &std::path::Path) -> Vec<u8> {
        let mut all = Vec::new();
        for ext in ["", "-wal", "-shm"] {
            let mut p = db.as_os_str().to_owned();
            p.push(ext);
            all.extend(std::fs::read(p).unwrap_or_default());
        }
        all
    }

    fn contains(hay: &[u8], needle: &str) -> bool {
        hay.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn cookies_are_sealed_on_disk_and_never_stored_without_a_sealer() {
        let d = tmp("sealed");
        let db = d.join("s.db");
        let mut r = rec(d.join("a.bin"));
        r.request_headers = vec![("cookie".into(), "sid=TOPSECRETCOOKIE".into()), ("authorization".into(), "Basic SECRETBASIC==".into())];
        let id;
        {
            let s = SqliteStore::open(&db).unwrap();
            s.set_sealer(sealer(1)).unwrap();
            id = s.save(&r).unwrap();
            assert_eq!(s.load(id).unwrap().unwrap().request_headers, r.request_headers, "round trip while the key is known");
            // A second save (progress checkpoints do this constantly) keeps working and stays sealed.
            let mut again = s.load(id).unwrap().unwrap();
            again.downloaded = 5;
            s.save(&again).unwrap();
        }
        let bytes = disk_bytes(&db);
        for secret in ["TOPSECRETCOOKIE", "SECRETBASIC", "sid=", "authorization"] {
            assert!(!contains(&bytes, secret), "{secret} reached the database file in the clear");
        }
        // Reopen with the same key: still readable. With another key or none: no headers, no failure.
        let s = SqliteStore::open(&db).unwrap();
        s.set_sealer(sealer(1)).unwrap();
        assert_eq!(s.load(id).unwrap().unwrap().request_headers, r.request_headers);
        s.set_sealer(sealer(2)).unwrap();
        assert!(s.load(id).unwrap().unwrap().request_headers.is_empty(), "another key");
        let s2 = SqliteStore::open(&db).unwrap();
        assert!(s2.load(id).unwrap().unwrap().request_headers.is_empty(), "no key at all");
        // A store that never got a sealer keeps secrets in memory only: usable now, never on disk.
        let id2 = s2.save(&rec(d.join("b.bin"))).unwrap();
        assert_eq!(s2.load(id2).unwrap().unwrap().request_headers.len(), 2, "available in this process");
        assert!(!contains(&disk_bytes(&db), "sid=1"));
        // What is stored carries the sealed marker.
        let sealed: String = s.conn().query_row("SELECT request_headers FROM downloads WHERE id=?1", [id], |r| r.get(0)).unwrap();
        assert!(secrets::is_sealed(&sealed));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn clear_text_left_by_an_older_version_is_sealed_or_erased_and_scrubbed_from_the_file() {
        let d = tmp("legacy");
        let db = d.join("l.db");
        let id;
        {
            let s = SqliteStore::open(&db).unwrap();
            id = s.save(&rec(d.join("a.bin"))).unwrap();
            // What versions before Phase 11 wrote.
            s.conn()
                .execute("UPDATE downloads SET request_headers = ?2 WHERE id = ?1", params![id, r#"[["cookie","sid=LEGACYCOOKIE"]]"#])
                .unwrap();
        }
        assert!(contains(&disk_bytes(&db), "LEGACYCOOKIE"), "the fixture really is in the clear");
        let s = SqliteStore::open(&db).unwrap();
        assert!(s.load(id).unwrap().unwrap().request_headers.is_empty(), "clear text is never read back");
        assert_eq!(s.set_sealer(sealer(1)).unwrap(), 1);
        assert_eq!(
            s.load(id).unwrap().unwrap().request_headers,
            vec![("cookie".to_string(), "sid=LEGACYCOOKIE".to_string())],
            "kept, now sealed"
        );
        assert!(!contains(&disk_bytes(&db), "LEGACYCOOKIE"), "no copy of the clear text is left in the file or log");
        assert_eq!(s.set_sealer(sealer(1)).unwrap(), 0, "nothing left to migrate");
        drop(s);

        // With no credential store at all the old secrets are simply erased.
        let id = {
            let s = SqliteStore::open(&db).unwrap();
            let id = s.save(&rec(d.join("c.bin"))).unwrap();
            s.conn()
                .execute("UPDATE downloads SET request_headers = ?2 WHERE id = ?1", params![id, r#"[["cookie","sid=ERASEME"]]"#])
                .unwrap();
            id
        };
        let s = SqliteStore::open(&db).unwrap();
        assert_eq!(s.set_sealer(None).unwrap(), 1);
        assert!(s.load(id).unwrap().unwrap().request_headers.is_empty());
        assert!(!contains(&disk_bytes(&db), "ERASEME"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn recovery_resume_restart_and_finished() {
        let d = tmp("recover");
        let s = SqliteStore::open_in_memory().unwrap();

        // consistent .part -> ResumeReady, completed segment kept, in-flight one reset to Pending
        let good = d.join("good.bin");
        std::fs::write(part_path(&good), vec![0u8; 1000]).unwrap();
        let g = s.save(&rec(good)).unwrap();

        // missing .part -> Restart, progress zeroed
        let missing = s.save(&rec(d.join("missing.bin"))).unwrap();

        // wrong-size .part -> Restart and the bad file is removed
        let bad = d.join("bad.bin");
        std::fs::write(part_path(&bad), vec![0u8; 10]).unwrap();
        let b = s.save(&rec(bad.clone())).unwrap();

        // destination exists, .part gone, status Verifying -> AlreadyFinished
        let done = d.join("done.bin");
        std::fs::write(&done, b"x").unwrap();
        let mut r = rec(done);
        r.status = DownloadStatus::Verifying;
        let f = s.save(&r).unwrap();

        // Paused stays untouched
        let mut p = rec(d.join("paused.bin"));
        p.status = DownloadStatus::Paused;
        s.save(&p).unwrap();

        let reports = s.recover_interrupted().unwrap();
        assert_eq!(reports.len(), 4);
        let act = |id| reports.iter().find(|r| r.id == id).unwrap().action.clone();
        assert_eq!(act(g), RecoveryAction::ResumeReady { downloaded: 600 });
        assert!(matches!(act(missing), RecoveryAction::Restart { .. }));
        assert!(matches!(act(b), RecoveryAction::Restart { .. }));
        assert_eq!(act(f), RecoveryAction::AlreadyFinished);
        assert!(!part_path(&bad).exists());

        let g = s.load(g).unwrap().unwrap();
        assert_eq!(g.status, DownloadStatus::Paused);
        assert_eq!(g.segments[0].status, SegmentStatus::Completed);
        assert_eq!(g.segments[1].status, SegmentStatus::Pending);
        let m = s.load(missing).unwrap().unwrap();
        assert_eq!((m.downloaded, m.segments[0].downloaded), (0, 0));
        assert_eq!(s.load(f).unwrap().unwrap().status, DownloadStatus::Completed);
        assert_eq!(s.history(10).unwrap().len(), 1);

        // Idempotent: nothing left to recover.
        assert!(s.recover_interrupted().unwrap().is_empty());
    }

    #[test]
    fn a_destination_is_in_use_only_while_its_download_is_unfinished() {
        let s = SqliteStore::open_in_memory().unwrap();
        let dest = PathBuf::from("/dl/movie.mkv");
        let mut rec = rec(dest.clone());
        rec.status = DownloadStatus::Paused;
        let id = s.save(&rec).unwrap();
        assert!(s.destination_in_use(&dest, None).unwrap());
        assert!(!s.destination_in_use(&dest, Some(id)).unwrap(), "a download does not block itself");
        assert!(!s.destination_in_use(Path::new("/dl/other.mkv"), None).unwrap());
        rec.id = Some(id);
        for (status, used) in [
            (DownloadStatus::Queued, true),
            (DownloadStatus::Failed, true),
            (DownloadStatus::Completed, false),
            (DownloadStatus::Cancelled, false),
        ] {
            rec.status = status;
            s.save(&rec).unwrap();
            assert_eq!(s.destination_in_use(&dest, None).unwrap(), used, "{status:?}");
        }
    }
}
