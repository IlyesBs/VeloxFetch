//! Smart throttle: while a game or a video call is running, downloads slow down so they do not spoil it;
//! when it closes, full speed (every connection) comes back at once.
//!
//! This module only holds the settings and the decision. Finding out which programs are running is the
//! desktop app's job (`src-tauri/src/smart_throttle.rs`), which calls [`DownloadManager::smart_throttle_observe`]
//! every few seconds. The cap goes into its own bucket of the shared rate limits
//! ([`downloader_core::RateLimits::set_smart`]), which every download passes through: the engine, its
//! connections and the user's own speed limits are untouched, and lifting the cap restores them exactly.
//! Video jobs obey it too: stream pieces pass through the same buckets, and yt-dlp gets the cap as its
//! `--limit-rate` (restarted, continuing its partial files, when the cap comes or goes; see `media.rs`).
//!
//! Safe by default: off until the user turns it on; anything unexpected (the scan failed, nothing was
//! reported for a while) means full speed, never a download stuck slow.

use super::*;

const ENABLED_KEY: &str = "smart_throttle.enabled";
const APPS_KEY: &str = "smart_throttle.apps";
const SHARE_KEY: &str = "smart_throttle.share";
/// The best speed seen, kept across restarts so the cap is right from the first minute.
const PEAK_KEY: &str = "smart_throttle.peak";
/// Below this total nothing is really downloading: the learned speed is left alone.
const LEARN_MIN_BPS: f64 = 64.0 * 1024.0;
/// While downloading below the best speed seen, it is forgotten slowly (half in about 35 minutes of
/// downloading), so a move to a slower connection is learned; never while idle.
const PEAK_DECAY: f64 = 0.999;
/// Linux cuts process names to 15 bytes (`/proc/<pid>/comm`).
const LINUX_NAME_LEN: usize = 15;

/// Programs that need the connection more than downloads do. Names as the system shows them, without
/// `.exe`; compared without case. (Discord is left out on purpose: many people keep it open all day.)
pub const DEFAULT_APPS: &[&str] = &[
    "League of Legends",
    "VALORANT-Win64-Shipping",
    "cs2",
    "dota2",
    "FortniteClient-Win64-Shipping",
    "r5apex",
    "RocketLeague",
    "GTA5",
    "Overwatch",
    "EA SPORTS FC 25",
    "eFootball",
    "PUBG",
    "TslGame",
    "Zoom",
    "zoom.us",
    "ms-teams",
    "Teams",
];
const DEFAULT_SHARE: u8 = 25;
const MAX_APPS: usize = 64;
const MAX_APP_NAME: usize = 64;
/// Never slower than this, so downloads keep moving (and resume quickly when the cap lifts).
const FLOOR_BPS: u64 = 256 * 1024;
/// Used until VeloxFetch has seen how fast this connection is.
const UNKNOWN_PEAK_LIMIT_BPS: u64 = 1024 * 1024;

#[derive(Debug, Default)]
pub(super) struct SmartState {
    enabled: bool,
    apps: Vec<String>,
    share: u8,
    /// The best total speed seen while not throttled (slowly forgotten while downloading).
    peak_bps: f64,
    /// The value last written to the settings.
    saved_peak: f64,
    active_app: Option<String>,
    limit: Option<u64>,
    /// The last scan worked (`false` = the desktop app could not list programs: full speed).
    scan_ok: bool,
}

impl SmartState {
    pub(super) fn load(store: &SqliteStore) -> Self {
        let get = |k: &str| store.get_setting(k).ok().flatten();
        let apps = get(APPS_KEY)
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .and_then(|v| clean_apps(&v).ok())
            .unwrap_or_else(|| DEFAULT_APPS.iter().map(|s| s.to_string()).collect());
        let peak = get(PEAK_KEY).and_then(|s| s.parse::<f64>().ok()).filter(|p| p.is_finite() && *p >= 0.0).unwrap_or(0.0);
        Self {
            enabled: get(ENABLED_KEY).as_deref() == Some("true"),
            apps,
            share: get(SHARE_KEY).and_then(|s| s.parse::<u8>().ok()).filter(|v| (5..=90).contains(v)).unwrap_or(DEFAULT_SHARE),
            peak_bps: peak,
            saved_peak: peak,
            scan_ok: true,
            ..Default::default()
        }
    }
}

/// A program name as compared: no `.exe`, no surrounding spaces, lower case.
pub fn normalize_app(name: &str) -> String {
    let n = name.trim();
    let n = if n.len() > 4 && n[n.len() - 4..].eq_ignore_ascii_case(".exe") { &n[..n.len() - 4] } else { n };
    n.to_lowercase()
}

/// The first configured program that is running, by its configured name. A running name of exactly 15
/// bytes may be a Linux name cut short ("League of Legen"): it matches a longer configured name it starts.
pub fn find_running<'a>(apps: &'a [String], running: &[String]) -> Option<&'a str> {
    let cut: Vec<String> = running.iter().filter(|r| r.trim().len() == LINUX_NAME_LEN).map(|r| normalize_app(r)).collect();
    let running: std::collections::HashSet<String> = running.iter().map(|r| normalize_app(r)).collect();
    apps.iter()
        .find(|a| {
            let a = normalize_app(a);
            running.contains(&a) || (a.len() > LINUX_NAME_LEN && cut.iter().any(|c| a.starts_with(c.as_str())))
        })
        .map(String::as_str)
}

/// The best speed after seeing `total` (bytes/s) while nothing held downloads back.
pub fn learn_peak(peak: f64, total: f64) -> f64 {
    if total < LEARN_MIN_BPS {
        peak // idle or nearly: nothing to learn, and nothing forgotten
    } else {
        (peak * PEAK_DECAY).max(total)
    }
}

fn clean_apps(apps: &[String]) -> Result<Vec<String>, ManagerError> {
    if apps.len() > MAX_APPS {
        return Err(ManagerError::invalid(format!("at most {MAX_APPS} programs")));
    }
    let mut out: Vec<String> = Vec::new();
    for a in apps {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        if a.chars().count() > MAX_APP_NAME
            || a.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        {
            return Err(ManagerError::invalid("a program name is too long or has characters a program name cannot have"));
        }
        if !out.iter().any(|o| normalize_app(o) == normalize_app(a)) {
            out.push(a.to_string());
        }
    }
    Ok(out)
}

/// The cap while a listed program runs: the chosen share of the best speed seen, never below the floor.
pub fn cap_for(peak_bps: f64, share: u8) -> u64 {
    if peak_bps < 1.0 {
        return UNKNOWN_PEAK_LIMIT_BPS;
    }
    ((peak_bps * f64::from(share) / 100.0) as u64).max(FLOOR_BPS)
}

impl DownloadManager {
    pub fn smart_throttle_enabled(&self) -> bool {
        self.inner.smart.lock().unwrap().enabled
    }

    pub fn smart_throttle(&self) -> SmartThrottleDto {
        let s = self.inner.smart.lock().unwrap();
        SmartThrottleDto {
            enabled: s.enabled,
            apps: s.apps.clone(),
            share_percent: s.share,
            active_app: s.active_app.clone(),
            limit_bps: s.limit,
            scan_ok: s.scan_ok,
        }
    }

    pub async fn set_smart_throttle(&self, req: SetSmartThrottle) -> Result<SmartThrottleDto, ManagerError> {
        let apps = req.apps.as_deref().map(clean_apps).transpose()?;
        if let Some(p) = req.share_percent {
            if !(5..=90).contains(&p) {
                return Err(ManagerError::invalid("the share must be between 5 and 90 percent"));
            }
        }
        if let Some(a) = &apps {
            self.inner.store.set_setting(APPS_KEY, &serde_json::to_string(a).unwrap_or_default())?;
        }
        if let Some(p) = req.share_percent {
            self.inner.store.set_setting(SHARE_KEY, &p.to_string())?;
        }
        if let Some(on) = req.enabled {
            self.inner.store.set_setting(ENABLED_KEY, if on { "true" } else { "false" })?;
        }
        let release = {
            let mut s = self.inner.smart.lock().unwrap();
            if let Some(a) = apps {
                s.apps = a;
            }
            if let Some(p) = req.share_percent {
                s.share = p;
            }
            if let Some(on) = req.enabled {
                s.enabled = on;
            }
            !s.enabled
        };
        if release {
            // Off: full speed now, not at the next scan.
            self.smart_throttle_observe(Some(&[]));
        }
        tracing::info!(enabled = ?req.enabled, "smart throttle settings changed");
        Ok(self.smart_throttle())
    }

    /// What the desktop app found running (`None` = it could not tell: full speed). Applies or lifts the cap.
    pub fn smart_throttle_observe(&self, running: Option<&[String]>) {
        // Ordinary downloads and video jobs (streams, video pages) together: both are capped.
        let files: f64 = self.inner.trackers.lock().unwrap().values().map(|t| t.speed).sum();
        let total = files + self.media_live().0 as f64;
        let (changed, limit, app, save_peak) = {
            let mut s = self.inner.smart.lock().unwrap();
            s.scan_ok = running.is_some();
            let found = match running {
                Some(r) if s.enabled => find_running(&s.apps, r).map(str::to_string),
                _ => None,
            };
            if found.is_none() {
                // Learn how fast this connection can go (only while nothing holds it back).
                s.peak_bps = learn_peak(s.peak_bps, total);
            }
            // Saved when it moved by a tenth, not every scan.
            let save_peak = ((s.peak_bps - s.saved_peak).abs() > s.saved_peak.max(1.0) * 0.1).then_some(s.peak_bps);
            if let Some(p) = save_peak {
                s.saved_peak = p;
            }
            let limit = found.as_ref().map(|_| cap_for(s.peak_bps, s.share));
            let changed = s.active_app != found || s.limit != limit;
            s.active_app = found.clone();
            s.limit = limit;
            (changed, limit, found, save_peak)
        };
        if let Some(p) = save_peak {
            if let Err(e) = self.inner.store.set_setting(PEAK_KEY, &format!("{p:.0}")) {
                tracing::debug!(error = %e, "could not save the learned speed");
                // only a head start next time
            }
        }
        if changed {
            self.inner.limits.set_smart(limit);
            match &app {
                Some(a) => tracing::info!(program = %a, limit_bps = limit.unwrap_or(0), "smart throttle on: a listed program is running"),
                None => tracing::info!("smart throttle off: full speed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn programs_are_matched_without_case_or_exe() {
        let apps = v(&["League of Legends", "Zoom", "cs2"]);
        assert_eq!(find_running(&apps, &v(&["explorer.exe", "ZOOM.EXE"])), Some("Zoom"));
        assert_eq!(find_running(&apps, &v(&["League of Legends.exe"])), Some("League of Legends"));
        assert_eq!(find_running(&apps, &v(&["cs2"])), Some("cs2"));
        assert_eq!(find_running(&apps, &v(&["cs22.exe", "zoomit.exe", "exe"])), None, "whole names only");
        assert_eq!(find_running(&[], &v(&["zoom.exe"])), None);
        assert_eq!(normalize_app("  Zoom.EXE "), "zoom");
        assert_eq!(normalize_app(".exe"), ".exe");
    }

    #[test]
    fn linux_names_cut_to_15_bytes_still_match() {
        let apps = v(&["League of Legends", "FortniteClient-Win64-Shipping", "cs2"]);
        assert_eq!(find_running(&apps, &v(&["League of Legen"])), Some("League of Legends"));
        assert_eq!(find_running(&apps, &v(&["FortniteClient-"])), Some("FortniteClient-Win64-Shipping"));
        assert_eq!(find_running(&apps, &v(&["League of Lege"])), None, "only a full 15-byte name is a cut one");
        assert_eq!(find_running(&apps, &v(&["League of Other"])), None);
    }

    #[test]
    fn the_learned_speed_is_kept_while_idle_and_follows_downloads() {
        let peak = 50_000_000.0;
        // Hours with nothing downloading: unchanged, so a game later does not crawl at the floor.
        let mut p = peak;
        for _ in 0..5000 {
            p = learn_peak(p, 0.0);
        }
        assert_eq!(p, peak);
        assert_eq!(learn_peak(peak, 80_000_000.0), 80_000_000.0, "a faster speed is learned at once");
        let slower = (0..2000).fold(peak, |p, _| learn_peak(p, 10_000_000.0));
        assert!(slower < peak && slower >= 10_000_000.0, "a slower connection is learned slowly: {slower}");
    }

    #[test]
    fn the_cap_is_a_share_of_the_best_speed_with_a_floor() {
        assert_eq!(cap_for(0.0, 25), UNKNOWN_PEAK_LIMIT_BPS);
        assert_eq!(cap_for(100_000_000.0, 25), 25_000_000);
        assert_eq!(cap_for(500_000.0, 10), FLOOR_BPS);
    }

    #[test]
    fn program_lists_are_checked() {
        assert_eq!(
            clean_apps(&v(&[" Zoom ", "zoom.exe", "", "cs2"])).unwrap(),
            v(&["Zoom", "cs2"]),
            "trimmed, duplicates and blanks dropped"
        );
        for bad in ["a/b", "..\\x", "C:", "a\nb", &"x".repeat(65)] {
            assert!(clean_apps(&v(&[bad])).is_err(), "{bad:?}");
        }
        let many: Vec<String> = (0..65).map(|i| format!("app{i}")).collect();
        assert!(clean_apps(&many).is_err());
    }
}
