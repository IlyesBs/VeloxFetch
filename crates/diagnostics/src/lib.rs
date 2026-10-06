//! Local diagnostics for VeloxFetch: a log file on the user's own disk and the text reports built from it.
//!
//! Nothing here sends anything anywhere. Every line goes through [`Redactor`] before it is kept, so
//! `Authorization` / `Cookie` values, passwords, tokens, the user name and password in a link and every value
//! of a link's query string never reach the log file, the in-memory ring the UI reads, or a report.

pub mod log;
pub mod redact;

pub use log::{LogConfig, LogHandle, LogLine};
pub use redact::{is_secret_name, redact, Redactor, MASK};

/// `1536` -> `1.5 KB` (binary units, like file managers show).
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Unix milliseconds as `2026-09-24 10:11:12Z` (UTC; no time-zone database needed).
pub fn utc_timestamp(unix_ms: i64, with_millis: bool) -> String {
    let secs = unix_ms.div_euclid(1000);
    let ms = unix_ms.rem_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, mi, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    if with_millis {
        format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}.{ms:03}Z")
    } else {
        format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}Z")
    }
}

/// The user's home folder (hidden as `~` in logs and reports), if the environment names an absolute one.
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from).filter(|p| p.is_absolute())
}

/// Milliseconds since 1970 now.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_and_sizes() {
        assert_eq!(utc_timestamp(0, false), "1970-01-01 00:00:00Z");
        assert_eq!(utc_timestamp(1_790_000_000_123, true), "2026-09-21 14:13:20.123Z");
        assert_eq!(utc_timestamp(951_782_400_000, false), "2000-02-29 00:00:00Z");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(10), "10 B");
    }
}
