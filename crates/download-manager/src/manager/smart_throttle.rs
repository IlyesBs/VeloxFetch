//! Smart throttle: downloads give way to what the user is doing, and take the whole connection when nothing
//! else needs it.
//!
//! * A listed program (a game, a video call) runs: the gaming policy applies ([`GamePolicy`]): downloads keep the
//!   chosen share of the connection (adaptive, the default), a fixed low limit, or pause (the queue holds them and
//!   starts nothing). Seen once is enough to apply it; it is lifted when the program has been gone for two scans
//!   in a row (a program restarting, or a launcher handing over to the game, does not flip it), and nothing the
//!   user set is changed, so lifting it restores exactly what was there. A failed scan keeps it for
//!   [`SCAN_FAIL_GRACE`] (no burst of traffic over a game because one scan failed), never longer.
//! * Automatic mode (on by default): the computer's own traffic is measured. When something else uses the
//!   connection while the user is at the computer (browsing, a video), downloads slow down so it stays
//!   responsive; heavy traffic while the user is away slows them less; when the network is free or the
//!   computer is idle, they speed back up, step by step. Smoothing, two thresholds (on / off), a short
//!   confirmation before slowing and a longer calm before speeding up keep the speed from jumping around.
//!   Downloads never go below the reserved minimum: they never stop.
//! * The other traffic is the computer's received bytes minus VeloxFetch's own, both counted over the same
//!   window (between two observations), with an allowance for packet headers: a stale reading or a speed
//!   that just changed never looks like someone else using the line.
//! * The cap is a share of the speed downloads really had just before slowing (at most the best speed
//!   seen), so it always bites: a share of a best speed reached long ago on a faster server may be above
//!   what downloads get today, and would change nothing.
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
const AUTO_KEY: &str = "smart_throttle.auto";
const MIN_KEY: &str = "smart_throttle.min_bps";
const POLICY_KEY: &str = "smart_throttle.game_policy";
const GAME_LIMIT_KEY: &str = "smart_throttle.game_limit_bps";
/// The low-bandwidth policy's limit until the user picks one.
const DEFAULT_GAME_LIMIT_BPS: u64 = 512 * 1024;
/// Scans in a row without a listed program before its policy is lifted.
const GONE_AFTER_SCANS: u32 = 2;
/// The programs cannot be listed: a policy in force stays this long, then is lifted (never stuck).
pub const SCAN_FAIL_GRACE: Duration = Duration::from_secs(15);
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
/// Never slower than this, so downloads keep moving (and resume quickly when the cap lifts). The user can
/// reserve more (or a little less, down to [`MIN_FLOOR_BPS`]).
const FLOOR_BPS: u64 = 256 * 1024;
const MIN_FLOOR_BPS: u64 = 32 * 1024;
const MAX_FLOOR_BPS: u64 = 1024 * 1024 * 1024;
/// Used until VeloxFetch has seen how fast this connection is.
const UNKNOWN_PEAK_LIMIT_BPS: u64 = 1024 * 1024;
/// Two measurements closer than this reuse the last window's figures (too short to be exact).
const METER_MIN: Duration = Duration::from_millis(1500);
/// A gap longer than this (the computer slept, the watcher stopped) starts the measurement over.
const METER_MAX: Duration = Duration::from_secs(30);
/// What the network counters add to the bytes VeloxFetch receives: Ethernet, IP and TCP headers (about
/// 4.5 % at full-size packets) and the secure layer's records.
const HEADER_OVERHEAD: f64 = 1.06;

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
    /// Automatic mode: follow the computer's other traffic and the user's presence.
    auto: bool,
    /// The least downloads keep, whatever happens (bytes/s).
    floor: u64,
    ctl: Controller,
    reason: Reason,
    /// Other traffic (smoothed) and whether the user is away, as last seen (for the status line).
    other_bps: Option<u64>,
    user_idle: Option<bool>,
    /// What downloads do while a listed program runs, and the low policy's limit.
    policy: GamePolicy,
    game_limit: u64,
    /// Whether a listed program counts as running (debounced).
    watch: GameWatch,
    /// The pause policy holds the queue (as last decided).
    hold: bool,
    /// The previous measurement: when, the computer's received bytes, VeloxFetch's received bytes.
    meter: Option<(Instant, u64, u64)>,
    /// The last window's other traffic and VeloxFetch's speed (bytes/s).
    last_window: (f64, f64),
    /// What downloads were getting lately while nothing held them back (bytes/s; 0 = not known yet).
    recent_bps: f64,
}

impl SmartState {
    /// The other traffic and VeloxFetch's own speed since the previous measurement (bytes/s), from the computer's
    /// received bytes `nic` (`None`: no counters) and VeloxFetch's received bytes `mine`, counted at `now`.
    fn measure(&mut self, now: Instant, nic: Option<u64>, mine: u64) -> (Option<f64>, Option<f64>) {
        let Some(nic) = nic else {
            self.meter = None;
            return (None, None);
        };
        match self.meter {
            Some((at, _, _)) if now.duration_since(at) < METER_MIN => (Some(self.last_window.0), Some(self.last_window.1)),
            Some((at, pn, pm)) if now.duration_since(at) <= METER_MAX && nic >= pn => {
                let secs = now.duration_since(at).as_secs_f64();
                let mine_bps = mine.saturating_sub(pm) as f64 / secs;
                // Signed on purpose: while downloads are capped, the system takes data in ahead of VeloxFetch reading
                // it, so one window shows a little more than VeloxFetch's share and the next a little less. Clipping
                // each window at zero would only keep the "more" (see `Controller::step_with`).
                let other = (nic - pn) as f64 / secs - mine_bps * HEADER_OVERHEAD;
                self.meter = Some((now, nic, mine));
                self.last_window = (other, mine_bps);
                (Some(other), Some(mine_bps))
            }
            // The first measurement (or after a long gap, or counters that went back): nothing to compare yet.
            _ => {
                self.meter = Some((now, nic, mine));
                self.last_window = (0.0, 0.0);
                (Some(0.0), None)
            }
        }
    }

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
            auto: get(AUTO_KEY).as_deref() != Some("false"),
            floor: get(MIN_KEY)
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|v| (MIN_FLOOR_BPS..=MAX_FLOOR_BPS).contains(v))
                .unwrap_or(FLOOR_BPS),
            policy: get(POLICY_KEY).and_then(|s| serde_json::from_str::<GamePolicy>(&format!("\"{s}\"")).ok()).unwrap_or_default(),
            game_limit: get(GAME_LIMIT_KEY)
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|v| (MIN_FLOOR_BPS..=MAX_FLOOR_BPS).contains(v))
                .unwrap_or(DEFAULT_GAME_LIMIT_BPS),
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

/// Whether a listed program counts as running, from one scan after another: on at the first sighting, off after
/// [`GONE_AFTER_SCANS`] scans without it, and kept through failed scans for at most [`SCAN_FAIL_GRACE`].
#[derive(Debug, Default, Clone)]
pub struct GameWatch {
    active: Option<String>,
    misses: u32,
    failing_since: Option<Instant>,
}

impl GameWatch {
    /// One scan: `Some(found)` (the listed program found, if any) or `None` (the programs could not be listed).
    /// Returns the program that counts as running.
    pub fn update(&mut self, scan: Option<Option<String>>, now: Instant) -> Option<&str> {
        match scan {
            Some(Some(name)) => {
                // Another listed program while one runs: the same policy goes on (no lift in between).
                *self = GameWatch { active: Some(name), ..Default::default() };
            }
            Some(None) => {
                self.failing_since = None;
                self.misses = self.misses.saturating_add(1);
                if self.misses >= GONE_AFTER_SCANS {
                    self.active = None;
                }
            }
            None => {
                let since = *self.failing_since.get_or_insert(now);
                if now.duration_since(since) >= SCAN_FAIL_GRACE {
                    self.active = None;
                }
            }
        }
        self.active.as_deref()
    }
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
#[cfg(test)]
pub fn cap_for(peak_bps: f64, share: u8) -> u64 {
    cap_with_floor(peak_bps, share, FLOOR_BPS)
}

/// The speed the caps are a share of: what downloads got lately while free (`recent`), at most the best speed
/// seen; the best speed alone while nothing recent is known.
pub fn reference_speed(peak: f64, recent: f64) -> f64 {
    if recent < LEARN_MIN_BPS {
        peak
    } else if peak >= 1.0 {
        recent.min(peak)
    } else {
        recent
    }
}

/// The smart bucket's limit while a listed program runs, by policy. Pause: the queue holds the downloads; the
/// reserved minimum is for what cannot be paused that way (video jobs), so nothing runs at full speed.
fn game_cap(policy: GamePolicy, reference: f64, share: u8, floor: u64, low: u64) -> u64 {
    match policy {
        GamePolicy::Adaptive => cap_with_floor(reference, share, floor),
        GamePolicy::Low => low,
        GamePolicy::Pause => floor,
    }
}

fn cap_with_floor(peak_bps: f64, share: u8, floor: u64) -> u64 {
    if peak_bps < 1.0 {
        return UNKNOWN_PEAK_LIMIT_BPS.max(floor);
    }
    ((peak_bps * f64::from(share) / 100.0) as u64).max(floor)
}

// ------------------------------------------------------------------------------------------ automatic mode

/// What Smart throttle is doing, for the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reason {
    #[default]
    Off,
    /// Full speed: the computer is idle.
    FullIdle,
    /// Full speed: downloads are the only real traffic.
    FullFree,
    /// Slowed: the user is using the internet.
    Browsing,
    /// Slowed (less): heavy traffic while the user is away.
    Busy,
    /// Slowed: a listed program runs.
    App,
    /// Speeding back up, step by step.
    Easing,
    /// Not enough information (no traffic counters): full speed.
    Unknown,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::Off => "off",
            Reason::FullIdle => "fullIdle",
            Reason::FullFree => "fullFree",
            Reason::Browsing => "browsing",
            Reason::Busy => "busy",
            Reason::App => "app",
            Reason::Easing => "easing",
            Reason::Unknown => "unknown",
        }
    }
}

/// One observation (every few seconds).
#[derive(Debug, Clone, Copy, Default)]
pub struct Signals {
    /// The computer's download traffic that is not VeloxFetch's (bytes/s); `None` when unknown.
    pub other_bps: Option<f64>,
    /// How long since the user last touched the keyboard or mouse; `None` when the system does not say.
    pub idle: Option<Duration>,
    /// VeloxFetch's own download speed over the same window (bytes/s), when measured: a small share of it may
    /// be counting noise, never "someone else".
    pub mine_bps: Option<f64>,
    /// A speed limit holds downloads back (Smart throttle's cap, the user's own limits): the system then takes
    /// data in by bursts ahead of VeloxFetch reading it, and a window can be off by much more.
    pub paced: bool,
}

/// The user counts as away after this long without input.
pub const IDLE_AFTER: Duration = Duration::from_secs(120);
/// Other traffic above this while the user is at the computer: they are using the internet (a page loading, a
/// video, a call). Background chatter (a chat app, a sync client idling) stays below it.
const ACTIVE_ON_BPS: f64 = 96.0 * 1024.0;
/// Of VeloxFetch's own speed, this share of "other traffic" may be measuring noise (counters read a moment
/// apart, retransmissions): it never counts as someone using the line.
const NOISE_SHARE: f64 = 0.03;
/// The same while a limit paces downloads: the system's receive buffers fill and empty by bursts, so a 3 s
/// window can be off by the whole speed (it evens out over time; smoothed, about a tenth is left, measured on
/// a real line).
const NOISE_SHARE_PACED: f64 = 0.3;
/// Heavy traffic (whether or not the user is there): at least this, or this share of the connection.
const BUSY_ON_BPS: f64 = 512.0 * 1024.0;
const BUSY_ON_SHARE: f64 = 0.15;
/// Thresholds to leave a state are lower than to enter it (no flapping around one value).
const OFF_RATIO: f64 = 0.5;
/// Smoothing of the measured traffic (share of the newest reading).
const SMOOTHING: f64 = 0.5;
/// Readings in a row before slowing down (a single burst changes nothing).
const CONFIRM: u8 = 2;
/// Calm readings until the first step up (the cap is held for the ones before); doubled while the network keeps
/// changing.
const CALM: u32 = 4;
/// Speeding up: this much more per reading, until full speed.
const STEP_UP: f64 = 1.5;

/// The share of the connection downloads keep while the user uses the internet, from the chosen mode
/// (Aggressive 10 % -> 35 %, Balanced 25 % -> 50 %, Conservative 50 % -> 75 %).
pub fn keep_while_active(share: u8) -> f64 {
    (f64::from(share) / 100.0 + 0.25).clamp(0.25, 0.9)
}

#[derive(Debug, Default, Clone)]
pub struct Controller {
    other: Option<f64>,
    /// The previous reading as measured (bytes/s, signed).
    prev_raw: Option<f64>,
    active: bool,
    busy: bool,
    pending: u8,
    calm: u32,
    /// Readings since the network last calmed down (`None`: it never was busy); a quick return means an
    /// unsteady network.
    since_calm: Option<u32>,
    unsteady: bool,
    cap: Option<f64>,
}

impl Controller {
    /// One step with the best speed seen as the reference (see [`Self::step_with`]).
    #[cfg(test)]
    pub fn step(&mut self, sig: Signals, peak: f64, share: u8, floor: u64) -> (Option<u64>, Reason) {
        self.step_with(sig, peak, peak, share, floor)
    }

    /// Slowing down: the other traffic is (or was just) above its threshold.
    pub fn wants_slow(&self) -> bool {
        self.active || self.busy
    }

    /// The cap while slowing: the share of `reference` the mode keeps (more while the user is away), never below
    /// the floor.
    fn target(&self, reference: f64, share: u8, floor: f64) -> f64 {
        let keep = keep_while_active(share);
        // The user is away and something else downloads heavily (updates, a stream on another app): slow less.
        let keep = if self.active { keep } else { keep + (1.0 - keep) / 2.0 };
        let known = if reference < 1.0 { UNKNOWN_PEAK_LIMIT_BPS as f64 / keep } else { reference };
        (known * keep).max(floor)
    }

    /// New settings (a mode, a minimum) while a cap is in force: the cap follows at once, without starting over
    /// (starting over would lift it for a few readings, then slow down again).
    pub fn retarget(&mut self, reference: f64, share: u8, floor: u64) -> Option<u64> {
        let cap = self.cap?;
        let cap = if self.wants_slow() { self.target(reference, share, floor as f64) } else { cap.max(floor as f64) };
        self.cap = Some(cap);
        Some(cap as u64)
    }

    /// One step: the new cap (`None` = full speed) and why. `peak`: the best speed seen (how fast the line is);
    /// `reference`: what the caps are a share of (see [`reference_speed`]); `floor`: the least downloads keep;
    /// `share`: the chosen mode.
    pub fn step_with(&mut self, sig: Signals, peak: f64, reference: f64, share: u8, floor: u64) -> (Option<u64>, Reason) {
        let floor = floor as f64;
        let Some(raw) = sig.other_bps else {
            // No counters: nothing to go on, so never hold downloads back.
            *self = Controller::default();
            return (None, Reason::Unknown);
        };
        // Smoothed with its sign, so a window that showed a little more than the truth and the next one a little
        // less cancel out; how far below zero it may go is bounded, so real traffic shows within a reading or two.
        // The last two windows are averaged first: the "more" of one window and the "less" of the next meet there.
        let pair = self.prev_raw.map_or(raw, |p| (p + raw) / 2.0);
        self.prev_raw = Some(raw);
        let floor_other = -(ACTIVE_ON_BPS.max(sig.mine_bps.unwrap_or(0.0) * 0.1));
        let smoothed = match self.other {
            Some(o) => o + SMOOTHING * (pair - o),
            None => pair,
        }
        .max(floor_other);
        self.other = Some(smoothed);
        let other = smoothed.max(0.0);
        let away = sig.idle.is_some_and(|i| i >= IDLE_AFTER);
        let busy_on = BUSY_ON_BPS.max(peak * BUSY_ON_SHARE);
        let noise = if sig.paced { NOISE_SHARE_PACED } else { NOISE_SHARE };
        let active_on = ACTIVE_ON_BPS.max(sig.mine_bps.unwrap_or(0.0) * noise);
        let was_slowing = self.active || self.busy;
        self.active = !away && other > if self.active { active_on * OFF_RATIO } else { active_on };
        self.busy = other > if self.busy { busy_on * OFF_RATIO } else { busy_on };
        let wants_slow = self.active || self.busy;
        if wants_slow && !was_slowing {
            self.unsteady = self.since_calm.is_some_and(|n| n < 10 * CALM);
        }
        self.since_calm = self.since_calm.map(|n| n.saturating_add(1));

        if wants_slow {
            self.calm = 0;
            self.pending = self.pending.saturating_add(1);
            let target = self.target(reference, share, floor);
            if self.pending >= CONFIRM && self.cap.is_none_or(|c| target < c) {
                self.cap = Some(target);
            }
            let reason = if self.active { Reason::Browsing } else { Reason::Busy };
            return (self.cap.map(|c| c as u64), if self.cap.is_some() { reason } else { idle_or_free(away) });
        }

        self.pending = 0;
        if self.calm == 0 && was_slowing {
            self.since_calm = Some(0);
        }
        self.calm = self.calm.saturating_add(1);
        let Some(cap) = self.cap else { return (None, idle_or_free(away)) };
        let needed = if self.unsteady { CALM * 2 } else { CALM };
        if self.calm < needed {
            return (Some(cap as u64), Reason::Easing);
        }
        let next = cap * STEP_UP;
        if reference < 1.0 || next >= reference * 0.95 {
            self.cap = None;
            self.unsteady = false;
            (None, idle_or_free(away))
        } else {
            self.cap = Some(next);
            (Some(next as u64), Reason::Easing)
        }
    }

    /// The automatic cap is dropped (mode turned off, a listed program took over...).
    pub fn reset(&mut self) {
        *self = Controller::default();
    }
}

fn idle_or_free(away: bool) -> Reason {
    if away {
        Reason::FullIdle
    } else {
        Reason::FullFree
    }
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
            auto: s.auto,
            min_bps: s.floor,
            reason: if s.enabled { s.reason.code() } else { "off" }.to_string(),
            other_bps: s.other_bps,
            user_idle: s.user_idle,
            peak_bps: (s.peak_bps >= 1.0).then_some(s.peak_bps as u64),
            reference_bps: Some(reference_speed(s.peak_bps, s.recent_bps)).filter(|r| *r >= 1.0).map(|r| r as u64),
            game_policy: s.policy,
            game_limit_bps: s.game_limit,
            game_hold: s.hold,
        }
    }

    pub async fn set_smart_throttle(&self, req: SetSmartThrottle) -> Result<SmartThrottleDto, ManagerError> {
        let apps = req.apps.as_deref().map(clean_apps).transpose()?;
        if let Some(p) = req.share_percent {
            if !(5..=90).contains(&p) {
                return Err(ManagerError::invalid("the share must be between 5 and 90 percent"));
            }
        }
        if let Some(m) = req.min_bps {
            if !(MIN_FLOOR_BPS..=MAX_FLOOR_BPS).contains(&m) {
                return Err(ManagerError::invalid("the reserved minimum must be between 32 KB/s and 1 GB/s"));
            }
        }
        if let Some(m) = req.game_limit_bps {
            if !(MIN_FLOOR_BPS..=MAX_FLOOR_BPS).contains(&m) {
                return Err(ManagerError::invalid("the gaming speed limit must be between 32 KB/s and 1 GB/s"));
            }
            self.inner.store.set_setting(GAME_LIMIT_KEY, &m.to_string())?;
        }
        if let Some(p) = req.game_policy {
            let code = serde_json::to_string(&p).unwrap_or_default();
            self.inner.store.set_setting(POLICY_KEY, code.trim_matches('"'))?;
        }
        if let Some(m) = req.min_bps {
            self.inner.store.set_setting(MIN_KEY, &m.to_string())?;
        }
        if let Some(on) = req.auto {
            self.inner.store.set_setting(AUTO_KEY, if on { "true" } else { "false" })?;
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
            if let Some(m) = req.min_bps {
                s.floor = m;
            }
            if let Some(on) = req.auto {
                s.auto = on;
            }
            if let Some(p) = req.game_policy {
                s.policy = p;
            }
            if let Some(l) = req.game_limit_bps {
                s.game_limit = l;
            }
            if !s.enabled || !s.auto {
                s.ctl.reset();
            }
            !s.enabled
                || req.auto == Some(false)
                || req.min_bps.is_some()
                || req.share_percent.is_some()
                || req.game_policy.is_some()
                || req.game_limit_bps.is_some()
        };
        if release {
            // Off (or a new mode / minimum): applied now, not at the next scan. Off means full speed at once.
            let enabled = self.inner.smart.lock().unwrap().enabled;
            if !enabled {
                self.smart_throttle_observe(Some(&[]));
            } else {
                self.reapply_smart_cap();
            }
        }
        tracing::info!(enabled = ?req.enabled, "smart throttle settings changed");
        Ok(self.smart_throttle())
    }

    /// What the desktop app found running (`None` = it could not tell: a gaming policy in force stays for
    /// [`SCAN_FAIL_GRACE`], then full speed). Applies or lifts the cap.
    /// Without activity information: the automatic mode keeps full speed (see [`Self::smart_throttle_observe_activity`]).
    pub fn smart_throttle_observe(&self, running: Option<&[String]>) {
        self.smart_throttle_observe_signals(running, Signals::default());
    }

    /// The desktop app's observation with the user's idle time; the computer's other traffic is measured here.
    /// Reads the network counters: call it from a blocking thread.
    pub fn smart_throttle_observe_activity(&self, running: Option<&[String]>, idle: Option<Duration>) {
        // Both counters at the same moment, so both rates cover the same window.
        let nic = self.inner.sysio.net_received_total();
        let mine = self.velox_received_total();
        let (other_bps, mine_bps) = self.inner.smart.lock().unwrap().measure(Instant::now(), nic, mine);
        let paced = self.inner.limits.any_limit() || self.inner.active.lock().unwrap().values().any(|a| a.control.speed_limit().is_some());
        self.smart_throttle_observe_signals(running, Signals { other_bps, idle, mine_bps, paced });
    }

    /// Bytes VeloxFetch received from the network since it started, exact at this moment: the session total
    /// (added up at each progress tick and by video jobs) plus what running downloads received since their
    /// last tick.
    fn velox_received_total(&self) -> u64 {
        let live: Vec<(i64, u64)> = self.inner.active.lock().unwrap().iter().map(|(id, a)| (*id, a.progress.received())).collect();
        let trackers = self.inner.trackers.lock().unwrap();
        let counted = self.inner.session_bytes.load(Ordering::Relaxed);
        counted + live.iter().map(|(id, r)| r.saturating_sub(trackers.get(id).map_or(0, |t| t.last_received))).sum::<u64>()
    }

    /// Smart Throttle is slowing downloads right now (the queue lets Low-priority downloads wait meanwhile).
    pub(super) fn smart_is_limiting(&self) -> bool {
        let s = self.inner.smart.lock().unwrap();
        s.enabled && s.limit.is_some()
    }

    fn velox_download_bps(&self) -> f64 {
        // Ordinary downloads and video jobs (streams, video pages) together: both are capped.
        let files: f64 = self.inner.trackers.lock().unwrap().values().map(|t| t.speed).sum();
        files + self.media_live().0 as f64
    }

    /// The decision, from what was observed (tests give the signals directly).
    pub fn smart_throttle_observe_signals(&self, running: Option<&[String]>, sig: Signals) {
        let total = self.velox_download_bps();
        let (changed, limit, reason, save_peak) = {
            let mut s = self.inner.smart.lock().unwrap();
            s.scan_ok = running.is_some();
            let found = if s.enabled {
                let scan = running.map(|r| find_running(&s.apps, r).map(str::to_string));
                s.watch.update(scan, Instant::now()).map(str::to_string)
            } else {
                s.watch = GameWatch::default();
                None
            };
            s.other_bps = sig.other_bps.map(|o| o.max(0.0) as u64);
            s.user_idle = sig.idle.map(|i| i >= IDLE_AFTER);
            let (share, floor, peak) = (s.share, s.floor, s.peak_bps);
            let reference = reference_speed(peak, s.recent_bps);
            let (limit, reason) = if !s.enabled {
                s.ctl.reset();
                (None, Reason::Off)
            } else if found.is_some() {
                // A listed program wins; the automatic cap starts over when it closes.
                s.ctl.reset();
                (Some(game_cap(s.policy, reference, share, floor, s.game_limit)), Reason::App)
            } else if s.auto {
                s.ctl.step_with(sig, peak, reference, share, floor)
            } else {
                (None, if sig.idle.is_some_and(|i| i >= IDLE_AFTER) { Reason::FullIdle } else { Reason::FullFree })
            };
            if limit.is_none() {
                // Learn how fast this connection can go (only while nothing holds it back).
                s.peak_bps = learn_peak(s.peak_bps, total);
                // And what downloads get these days, while nobody else uses the line (the caps' reference).
                if !s.ctl.wants_slow() && total >= LEARN_MIN_BPS {
                    s.recent_bps = if s.recent_bps < 1.0 { total } else { s.recent_bps + 0.5 * (total - s.recent_bps) };
                }
            }
            // Saved when it moved by a tenth, not every scan.
            let save_peak = ((s.peak_bps - s.saved_peak).abs() > s.saved_peak.max(1.0) * 0.1).then_some(s.peak_bps);
            if let Some(p) = save_peak {
                s.saved_peak = p;
            }
            let changed = s.active_app != found || s.limit != limit;
            let hold = found.is_some() && s.policy == GamePolicy::Pause;
            let hold_changed = s.hold != hold;
            s.hold = hold;
            s.active_app = found;
            s.limit = limit;
            s.reason = reason;
            (changed || hold_changed, limit, reason, save_peak)
        };
        if let Some(p) = save_peak {
            if let Err(e) = self.inner.store.set_setting(PEAK_KEY, &format!("{p:.0}")) {
                tracing::debug!(error = %e, "could not save the learned speed");
                // only a head start next time
            }
        }
        if changed {
            self.inner.limits.set_smart(limit);
            self.spawn_game_hold_sync();
            match limit {
                Some(l) => tracing::info!(reason = reason.code(), limit_bps = l, "smart throttle: downloads slowed"),
                None => tracing::info!(reason = reason.code(), "smart throttle: full speed"),
            }
        }
    }

    /// A new mode, minimum or gaming policy: an active cap follows at once (a listed program's cap, or the
    /// automatic one), and the queue is held or let go to match the policy.
    fn reapply_smart_cap(&self) {
        let limit = {
            let mut s = self.inner.smart.lock().unwrap();
            let reference = reference_speed(s.peak_bps, s.recent_bps);
            let (share, floor) = (s.share, s.floor);
            s.hold = s.reason == Reason::App && s.policy == GamePolicy::Pause;
            let limit = match s.reason {
                Reason::App => Some(game_cap(s.policy, reference, share, floor, s.game_limit)),
                // The automatic cap follows the new settings now, and keeps its state (no lift-then-slow).
                _ if s.limit.is_some() && s.auto => s.ctl.retarget(reference, share, floor),
                _ => None,
            };
            s.limit = limit;
            limit
        };
        self.inner.limits.set_smart(limit);
        self.spawn_game_hold_sync();
    }

    /// The pause policy wants the queue held right now.
    pub(super) fn smart_holds_queue(&self) -> bool {
        let s = self.inner.smart.lock().unwrap();
        s.enabled && s.hold
    }

    /// Bring the queue in line with the pause policy, in the background (the observation runs on a blocking thread
    /// and must not wait for downloads to pause). Without a runtime (some unit tests) there is nothing to hold.
    fn spawn_game_hold_sync(&self) {
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let m = self.clone();
            rt.spawn(async move { m.sync_game_hold().await });
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

    const PEAK: f64 = 100_000_000.0; // 100 MB/s
    const KB: f64 = 1024.0;

    fn sig(other: f64, idle_secs: u64) -> Signals {
        Signals { other_bps: Some(other), idle: Some(Duration::from_secs(idle_secs)), ..Default::default() }
    }

    #[test]
    fn browsing_slows_downloads_after_a_confirmation_and_never_below_the_floor() {
        let mut c = Controller::default();
        assert_eq!(c.step(sig(0.0, 0), PEAK, 25, FLOOR_BPS), (None, Reason::FullFree), "downloads alone: full speed");
        // One burst of browsing traffic: confirmed first, nothing changes yet.
        assert_eq!(c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS).0, None);
        let (cap, why) = c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS);
        assert_eq!(why, Reason::Browsing);
        assert_eq!(cap, Some((PEAK * 0.5) as u64), "Balanced keeps half the connection while browsing");
        // The reserved minimum always wins over the share.
        let mut slow = Controller::default();
        for _ in 0..3 {
            slow.step(sig(400.0 * KB, 0), 400_000.0, 10, 5_000_000);
        }
        assert_eq!(slow.step(sig(400.0 * KB, 0), 400_000.0, 10, 5_000_000).0, Some(5_000_000));
    }

    #[test]
    fn the_modes_keep_more_or_less_of_the_connection() {
        assert!((keep_while_active(10) - 0.35).abs() < 1e-9, "Aggressive");
        assert!((keep_while_active(25) - 0.5).abs() < 1e-9, "Balanced");
        assert!((keep_while_active(50) - 0.75).abs() < 1e-9, "Conservative");
        assert!(keep_while_active(90) <= 0.9);
    }

    #[test]
    fn the_speed_comes_back_step_by_step_after_a_calm_and_does_not_jump_around() {
        let mut c = Controller::default();
        for _ in 0..3 {
            c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS);
        }
        let low = c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS).0.unwrap();
        // Traffic wobbling around the threshold (between the on and off levels) changes nothing.
        for other in [60.0, 120.0, 60.0, 90.0] {
            let (cap, why) = c.step(sig(other * KB, 0), PEAK, 25, FLOOR_BPS);
            assert_eq!((cap, why), (Some(low), Reason::Browsing), "{other} KB/s");
        }
        // Calm: the smoothed traffic fades for a few readings (still slowed), then the cap is held for a while
        // (easing), then it grows by steps, then full speed.
        let mut caps = Vec::new();
        for _ in 0..40 {
            let (cap, why) = c.step(sig(0.0, 0), PEAK, 25, FLOOR_BPS);
            match why {
                Reason::Browsing => assert_eq!(cap, Some(low), "never lower while it calms down"),
                Reason::Easing => caps.push(cap.unwrap()),
                Reason::FullFree => break,
                other => panic!("{other:?}"),
            }
        }
        assert!(caps.len() >= CALM as usize - 1, "held during the calm: {caps:?}");
        assert!(caps.windows(2).all(|w| w[1] >= w[0]), "only up while calm: {caps:?}");
        assert!(caps.windows(2).all(|w| (w[1] as f64) <= w[0] as f64 * STEP_UP + 1.0), "no jump: {caps:?}");
        assert_eq!(c.step(sig(0.0, 0), PEAK, 25, FLOOR_BPS), (None, Reason::FullFree));
    }

    /// Readings the cap is held (easing) after the network calms down, before it first goes up.
    fn held_before_rising(c: &mut Controller) -> usize {
        let mut held = 0;
        let mut first = None;
        for _ in 0..60 {
            let (cap, why) = c.step(sig(0.0, 0), PEAK, 25, FLOOR_BPS);
            if why != Reason::Easing {
                continue; // still fading
            }
            match first {
                None => first = cap,
                Some(f) if cap != Some(f) => return held,
                _ => {}
            }
            held += 1;
        }
        held
    }

    #[test]
    fn an_unsteady_network_waits_longer_before_speeding_up() {
        let busy = |c: &mut Controller| {
            for _ in 0..3 {
                c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS);
            }
        };
        let mut steady = Controller::default();
        busy(&mut steady);
        let normal = held_before_rising(&mut steady);
        assert_eq!(normal, CALM as usize - 1, "held, then up on the last calm reading");

        let mut c = Controller::default();
        busy(&mut c);
        // Calms down briefly, then busy again: the network is unsteady.
        while c.step(sig(0.0, 0), PEAK, 25, FLOOR_BPS).1 == Reason::Browsing {}
        busy(&mut c);
        assert_eq!(held_before_rising(&mut c), (CALM * 2) as usize - 1, "twice the calm");
    }

    #[test]
    fn an_idle_computer_gets_full_speed_and_heavy_background_traffic_is_slowed_less() {
        let mut c = Controller::default();
        // Light traffic while the user is away (a sync client): not browsing, full speed, computer idle.
        for _ in 0..4 {
            assert_eq!(c.step(sig(200.0 * KB, 600), PEAK, 25, FLOOR_BPS), (None, Reason::FullIdle));
        }
        // Heavy traffic while away: slowed, but less than while browsing.
        let mut cap = None;
        for _ in 0..4 {
            cap = c.step(sig(30_000_000.0, 600), PEAK, 25, FLOOR_BPS).0;
        }
        assert_eq!(c.step(sig(30_000_000.0, 600), PEAK, 25, FLOOR_BPS).1, Reason::Busy);
        assert_eq!(cap, Some((PEAK * 0.75) as u64));
    }

    #[test]
    fn no_counters_means_full_speed() {
        let mut c = Controller::default();
        for _ in 0..3 {
            c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS);
        }
        assert!(c.cap.is_some());
        assert_eq!(c.step(Signals::default(), PEAK, 25, FLOOR_BPS), (None, Reason::Unknown));
        // Unknown idle time: the user counts as present (browsing still detected).
        let mut c = Controller::default();
        for _ in 0..2 {
            c.step(Signals { other_bps: Some(400.0 * KB), ..Default::default() }, PEAK, 25, FLOOR_BPS);
        }
        assert_eq!(c.step(Signals { other_bps: Some(400.0 * KB), ..Default::default() }, PEAK, 25, FLOOR_BPS).1, Reason::Browsing);
    }

    #[test]
    fn other_traffic_is_measured_over_the_same_window_as_velox_own() {
        let mut s = SmartState::default();
        let t0 = Instant::now();
        const MB: u64 = 1024 * 1024;
        // First measurement: nothing to compare with, so nobody else is seen (never a stale reading).
        assert_eq!(s.measure(t0, Some(1000 * MB), 500 * MB), (Some(0.0), None));
        // 3 s later: the computer received 9.54 MB, VeloxFetch 9 MB (3 MB/s) -> the rest is packet headers, not traffic.
        let (other, mine) = s.measure(t0 + Duration::from_secs(3), Some(1000 * MB + 9 * MB * 106 / 100), 509 * MB);
        assert!(other.unwrap().abs() < 1024.0, "headers are not someone else: {other:?}");
        assert!((mine.unwrap() - 3.0 * MB as f64).abs() < 1.0);
        // Then another program receives 1 MB/s on top.
        let (other, _) =
            s.measure(t0 + Duration::from_secs(6), Some(1000 * MB + 9 * MB * 106 / 100 + 9 * MB * 106 / 100 + 3 * MB), 518 * MB);
        assert!((other.unwrap() - MB as f64).abs() < 0.05 * MB as f64, "{other:?}");
        // Asked again right away: the last window's figures (a window this short would not be exact).
        assert_eq!(s.measure(t0 + Duration::from_millis(6200), Some(u64::MAX / 2), 518 * MB).0, other);
        // A long gap (the computer slept) or counters that went back start over.
        assert_eq!(s.measure(t0 + Duration::from_secs(60), Some(2000 * MB), 600 * MB), (Some(0.0), None));
        assert_eq!(s.measure(t0 + Duration::from_secs(63), Some(10 * MB), 601 * MB), (Some(0.0), None));
        // No counters: unknown.
        assert_eq!(s.measure(t0 + Duration::from_secs(66), None, 602 * MB), (None, None));
    }

    #[test]
    fn a_velox_speed_change_is_never_seen_as_someone_else() {
        // VeloxFetch goes from 3 MB/s to 0.5 MB/s between two observations. Both counters cover the same window, so
        // the drop cancels out (the old way compared a reading of the past second with the speed of now).
        let mut s = SmartState::default();
        let t0 = Instant::now();
        let mb = 1024.0 * 1024.0;
        let (mut nic, mut mine) = (0u64, 0u64);
        s.measure(t0, Some(nic), mine);
        for (i, speed) in [3.0, 3.0, 0.5, 0.5, 2.0].iter().enumerate() {
            let got = (speed * mb * 3.0) as u64;
            mine += got;
            nic += (got as f64 * 1.05) as u64;
            let (other, _) = s.measure(t0 + Duration::from_secs(3 * (i as u64 + 1)), Some(nic), mine);
            assert!(other.unwrap() <= 0.0, "reading {i}: {other:?}");
        }
    }

    #[test]
    fn the_cap_is_a_share_of_what_downloads_get_now_not_of_an_old_best() {
        // Best speed seen: 100 MB/s (another server, last week). Today's download gets 4 MB/s.
        assert_eq!(reference_speed(PEAK, 4.0 * 1024.0 * 1024.0), 4.0 * 1024.0 * 1024.0);
        assert_eq!(reference_speed(PEAK, 0.0), PEAK, "nothing recent: the best speed");
        assert_eq!(reference_speed(PEAK, 2.0 * PEAK), PEAK, "never above the best speed");
        assert_eq!(reference_speed(0.0, 4.0 * 1024.0 * 1024.0), 4.0 * 1024.0 * 1024.0);
        let mut c = Controller::default();
        let now = 4.0 * 1024.0 * 1024.0;
        let mut cap = None;
        for _ in 0..3 {
            cap = c.step_with(sig(400.0 * KB, 0), PEAK, now, 25, FLOOR_BPS).0;
        }
        assert_eq!(cap, Some((now * 0.5) as u64), "Balanced: half of today's 4 MB/s, so it really slows downloads");
    }

    #[test]
    fn new_settings_move_the_cap_without_lifting_it() {
        let mut c = Controller::default();
        for _ in 0..3 {
            c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS);
        }
        // Aggressive while browsing: 35 % at once, and the next reading keeps it (no full-speed gap in between).
        assert_eq!(c.retarget(PEAK, 10, FLOOR_BPS), Some((PEAK * 0.35) as u64));
        assert_eq!(c.step(sig(400.0 * KB, 0), PEAK, 10, FLOOR_BPS), (Some((PEAK * 0.35) as u64), Reason::Browsing));
        // A higher minimum is kept.
        assert_eq!(c.retarget(PEAK, 10, 60_000_000), Some(60_000_000));
        // Nothing capped: nothing to move.
        assert_eq!(Controller::default().retarget(PEAK, 10, FLOOR_BPS), None);
    }

    #[test]
    fn traffic_buffered_ahead_of_a_capped_download_cancels_out() {
        // Capped at 512 KB/s, the system's counters run a little ahead of VeloxFetch one window and behind the next:
        // +400 KB/s, -400 KB/s, ... That is nobody else, and the cap must still lift once the network calms down.
        let mut c = Controller::default();
        for _ in 0..3 {
            c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS);
        }
        assert!(c.cap.is_some());
        let mut lifted = false;
        for i in 0..40 {
            let wobble = if i % 2 == 0 { 400.0 * KB } else { -400.0 * KB };
            let mine = Some(512.0 * KB);
            if c.step(Signals { other_bps: Some(wobble), idle: Some(Duration::ZERO), mine_bps: mine, paced: true }, PEAK, 25, FLOOR_BPS)
                .0
                .is_none()
            {
                lifted = true;
                break;
            }
        }
        assert!(lifted, "the wobble kept downloads slow");
        // A long run of negative readings cannot hide real traffic for long.
        let mut c = Controller::default();
        for _ in 0..20 {
            c.step(sig(-5_000_000.0, 0), PEAK, 25, FLOOR_BPS);
        }
        let mut readings = 0;
        while c.step(sig(400.0 * KB, 0), PEAK, 25, FLOOR_BPS).0.is_none() {
            readings += 1;
            assert!(readings < 5, "real browsing was hidden");
        }
    }

    #[test]
    fn a_capped_download_on_a_real_line_is_not_taken_for_browsing() {
        // Readings measured on a real line while downloads were capped at ~1 MB/s and nothing else ran: the
        // computer's counters (KB/s) swing around VeloxFetch's steady speed. Averaged, they match.
        let nic = [1693.0, 734.0, 887.0, 1596.0, 926.0, 549.0, 2138.0, 150.0, 1815.0];
        let mine = [1220.0, 1043.0, 925.0, 1075.0, 1075.0, 840.0, 1135.0, 943.0, 1184.0];
        let mut c = Controller::default();
        for _ in 0..3 {
            c.step(sig(3000.0 * KB, 0), PEAK, 25, FLOOR_BPS); // the other download that caused the cap
        }
        // The other download stops: once the smoothed traffic has faded, the swings never bring "browsing" back,
        // and the cap lifts.
        let (mut calmed, mut lifted) = (false, false);
        'readings: for _ in 0..4 {
            for (n, m) in nic.iter().zip(mine) {
                let s = Signals {
                    other_bps: Some((n - m * HEADER_OVERHEAD) * KB),
                    idle: Some(Duration::ZERO),
                    mine_bps: Some(m * KB),
                    paced: true,
                };
                let (cap, why) = c.step(s, PEAK, 25, FLOOR_BPS);
                if calmed {
                    assert_ne!(why, Reason::Browsing, "the swings were taken for browsing again");
                }
                calmed |= why != Reason::Browsing;
                if cap.is_none() {
                    lifted = true;
                    break 'readings;
                }
            }
        }
        assert!(lifted, "the cap lifted");
    }

    #[test]
    fn measuring_noise_at_high_speed_is_not_browsing() {
        // 100 MB/s of VeloxFetch: 2 MB/s of "other" is within what the counters can be off by.
        let mut c = Controller::default();
        let noisy = Signals { other_bps: Some(2.0 * 1024.0 * 1024.0), idle: Some(Duration::ZERO), mine_bps: Some(PEAK), paced: false };
        for _ in 0..5 {
            assert_eq!(c.step(noisy, PEAK, 25, FLOOR_BPS).0, None);
        }
        // Light background traffic (a chat app) is not browsing either.
        let mut c = Controller::default();
        for _ in 0..5 {
            assert_eq!(c.step(sig(70.0 * KB, 0), PEAK, 25, FLOOR_BPS).0, None);
        }
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

    // ---------------------------------------------------------------------------------- gaming policy

    fn seen(name: &str) -> Option<Option<String>> {
        Some(Some(name.to_string()))
    }

    #[test]
    fn a_game_counts_at_once_and_is_let_go_after_two_scans_without_it() {
        let t = Instant::now();
        let mut w = GameWatch::default();
        assert_eq!(w.update(Some(None), t), None, "nothing running");
        assert_eq!(w.update(seen("cs2"), t), Some("cs2"), "the first sighting is enough");
        assert_eq!(w.update(Some(None), t), Some("cs2"), "one scan without it: kept (a restart, a launcher handing over)");
        assert_eq!(w.update(Some(None), t), None, "two in a row: gone");
    }

    #[test]
    fn a_game_that_flickers_never_makes_the_policy_flip() {
        let t = Instant::now();
        let mut w = GameWatch::default();
        for _ in 0..20 {
            assert!(w.update(seen("dota2"), t).is_some());
            assert!(w.update(Some(None), t).is_some(), "one miss between sightings never lifts it");
        }
    }

    #[test]
    fn several_games_keep_the_policy_while_any_runs() {
        let t = Instant::now();
        let mut w = GameWatch::default();
        w.update(seen("cs2"), t);
        assert_eq!(w.update(seen("r5apex"), t), Some("r5apex"), "one closed, another runs: still on, no gap");
        assert_eq!(w.update(Some(None), t), Some("r5apex"));
        assert_eq!(w.update(Some(None), t), None);
    }

    #[test]
    fn a_failed_scan_keeps_the_policy_for_a_while_never_for_good() {
        let t = Instant::now();
        let mut w = GameWatch::default();
        assert_eq!(w.update(None, t), None, "no game known: a failed scan finds none");
        w.update(seen("GTA5"), t);
        assert_eq!(w.update(None, t), Some("GTA5"), "uncertain: no burst of traffic over the game");
        assert_eq!(w.update(None, t + SCAN_FAIL_GRACE - Duration::from_secs(1)), Some("GTA5"));
        assert_eq!(w.update(None, t + SCAN_FAIL_GRACE), None, "still failing after the grace: lifted");
        // Back with the game: on again; a failure later starts a new grace.
        let t2 = t + SCAN_FAIL_GRACE * 2;
        assert_eq!(w.update(seen("GTA5"), t2), Some("GTA5"));
        assert_eq!(w.update(None, t2 + Duration::from_secs(3)), Some("GTA5"));
        assert_eq!(w.update(seen("GTA5"), t2 + Duration::from_secs(6)), Some("GTA5"));
        assert_eq!(
            w.update(None, t2 + Duration::from_secs(9) + SCAN_FAIL_GRACE - Duration::from_secs(1)),
            Some("GTA5"),
            "the grace restarted"
        );
    }

    #[test]
    fn each_policy_has_its_own_cap_and_none_is_zero() {
        let mb = 1024.0 * 1024.0;
        assert_eq!(game_cap(GamePolicy::Adaptive, 40.0 * mb, 25, FLOOR_BPS, 999), (10.0 * mb) as u64, "the mode's share, as before");
        assert_eq!(game_cap(GamePolicy::Low, 40.0 * mb, 25, FLOOR_BPS, 300_000), 300_000, "the chosen limit");
        assert_eq!(
            game_cap(GamePolicy::Pause, 40.0 * mb, 25, FLOOR_BPS, 300_000),
            FLOOR_BPS,
            "the queue pauses; the rest keeps the minimum"
        );
        for p in [GamePolicy::Adaptive, GamePolicy::Low, GamePolicy::Pause] {
            assert!(game_cap(p, 0.0, 10, MIN_FLOOR_BPS, MIN_FLOOR_BPS) > 0, "{p:?}: zero would mean no limit at all");
        }
    }

    #[test]
    fn unrelated_programs_never_count_as_a_game() {
        let apps: Vec<String> = DEFAULT_APPS.iter().map(|s| s.to_string()).collect();
        let everyday = v(&[
            "steam",
            "steamwebhelper",
            "EpicGamesLauncher",
            "Battle.net",
            "explorer.exe",
            "chrome",
            "firefox",
            "Discord",
            "code",
            "gamemoded",
            "csrss.exe",
            "teamviewer",
            "zoomit",
            "GTA5Launcher",
            "cs2-helper",
            "overwatch_updater",
        ]);
        assert_eq!(find_running(&apps, &everyday), None, "launchers, helpers and look-alikes are not the game");
    }
}
