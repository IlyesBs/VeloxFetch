//! The rules of one download queue: "only at night", "start at 02:00", "only while the computer is idle",
//! "only on Wi-Fi", "pause while the connection is busy", "big files go here".
//!
//! Pure logic, like the rest of this crate: the time, the idle time, the connection and the traffic are
//! passed in, so every decision is deterministic and testable. The download manager does the rest
//! (holding downloads back, pausing and resuming them).
//!
//! Design rules:
//! * A queue with no rule is always open: nothing changes for someone who never sets one.
//! * Something the computer cannot tell (idle time, connection type, traffic) never closes a queue.
//! * Changes that would pause downloads need a few readings in a row (and a running queue keeps going for
//!   a short while), so a blip never starts and stops downloads.

use std::time::Duration;

use chrono::{Datelike, Duration as Span, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};

use crate::{parse_time, Schedule, ScheduleError};

/// How long without keyboard or mouse before the computer counts as idle (the same as Smart Throttle).
pub const IDLE_AFTER: Duration = Duration::from_secs(120);
/// A queue opened by "only when idle" keeps going at least this long, so a passing touch does not pause it.
pub const IDLE_MIN_RUN: Span = Span::seconds(30);
/// Readings above the limit in a row before "the connection is busy" pauses a queue.
pub const BUSY_ENTER: u8 = 3;
/// Readings clearly below the limit in a row before it carries on.
pub const BUSY_LEAVE: u8 = 5;
/// "Clearly below": under this share of the limit.
pub const BUSY_CALM_SHARE: f64 = 0.7;
/// The smallest limit for "pause while the connection is busy" (64 KB/s).
pub const MIN_BUSY_LIMIT: u64 = 64 * 1024;
/// The smallest size for "big files go to this queue" (1 MB).
pub const MIN_LARGE_FILE: u64 = 1024 * 1024;

/// When the queue may download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum When {
    /// Whenever the other rules allow.
    #[default]
    Anytime,
    /// Only inside a daily window, e.g. 23:00 to 07:00 (downloads pause when it ends).
    #[serde(rename_all = "camelCase")]
    Between { start: String, stop: String, days: [bool; 7] },
    /// From this time on the chosen days until everything in the queue is done.
    #[serde(rename_all = "camelCase")]
    StartAt { time: String, days: [bool; 7] },
}

/// Which connections the queue may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionRule {
    #[default]
    Any,
    Wifi,
    Wired,
    NotMobile,
}

/// What kind of connection the computer is using right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NetKind {
    Wifi,
    Wired,
    Mobile,
    /// A VPN, a virtual adapter, or a system that does not say.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct QueueRules {
    pub when: When,
    pub only_when_idle: bool,
    pub connection: ConnectionRule,
    /// Pause the queue while traffic other than its own is above this (bytes/s).
    pub pause_above_bps: Option<u64>,
    /// Downloads bigger than this (bytes) are moved to this queue once their size is known.
    pub large_files_over: Option<u64>,
}

/// Why a queue does not start downloads right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    /// The user paused it.
    Paused,
    /// Outside its time window, or before its start time; `opens_at` is the next opening when there is one.
    Time { opens_at: Option<NaiveDateTime> },
    /// No network.
    Offline,
    /// Not the connection the queue wants.
    Connection,
    /// Waiting for the computer to be idle.
    NotIdle,
    /// Other traffic is above the queue's limit.
    Busy,
}

impl Closed {
    /// Downloads already running are paused for this (and resumed when it reopens). No network is left to the
    /// engine's own retries: pausing would not help.
    pub fn pauses_running(self) -> bool {
        !matches!(self, Closed::Offline)
    }
}

/// What the rules are judged on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inputs {
    pub now: NaiveDateTime,
    /// Time since the last keyboard / mouse use (`None`: the system does not say).
    pub idle: Option<Duration>,
    /// `None`: not known.
    pub net: Option<NetKind>,
    /// `Some(false)`: no network (already confirmed by the caller).
    pub online: Option<bool>,
    /// Traffic that is not this queue's own (bytes/s, smoothed), `None`: not measured.
    pub other_bps: Option<u64>,
}

/// What a queue remembers between two evaluations. `used_until` is saved: a start time at or before it has
/// already been used (the queue ran out of work after it, or the rule was set after it).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleState {
    pub used_until: Option<NaiveDateTime>,
    busy: bool,
    busy_hits: u8,
    calm_hits: u8,
    idle_opened_at: Option<NaiveDateTime>,
}

impl RuleState {
    /// The state saved at the last run (only `used_until` is kept).
    pub fn restored(used_until: Option<NaiveDateTime>) -> Self {
        Self { used_until, ..Default::default() }
    }
}

fn time_of(s: &str) -> Option<NaiveTime> {
    parse_time(s).ok()
}

/// The start times of a "start at" rule in `(after, until]`, oldest first.
pub fn start_times(time: &str, days: &[bool; 7], after: NaiveDateTime, until: NaiveDateTime) -> Vec<NaiveDateTime> {
    let Some(t) = time_of(time) else { return Vec::new() };
    let mut out = Vec::new();
    let mut date = after.date();
    while date <= until.date() {
        let at = date.and_time(t);
        if at > after && at <= until && days[date.weekday().num_days_from_monday() as usize] {
            out.push(at);
        }
        date += Span::days(1);
    }
    out
}

/// The last start time at or before `now` (looking back a week), if any.
pub fn last_start(time: &str, days: &[bool; 7], now: NaiveDateTime) -> Option<NaiveDateTime> {
    start_times(time, days, now - Span::days(8), now).pop()
}

/// The next start time strictly after `now`.
pub fn next_start(time: &str, days: &[bool; 7], now: NaiveDateTime) -> Option<NaiveDateTime> {
    start_times(time, days, now, now + Span::days(8)).into_iter().next()
}

impl QueueRules {
    /// Check the fields (the window and start times, the limits).
    pub fn validate(&self) -> Result<(), RuleError> {
        match &self.when {
            When::Anytime => {}
            When::Between { start, stop, days } => {
                Schedule { enabled: true, start: start.clone(), stop: stop.clone(), days: *days }.validate()?;
            }
            When::StartAt { time, days } => {
                parse_time(time)?;
                if !days.iter().any(|d| *d) {
                    return Err(ScheduleError::NoDays.into());
                }
            }
        }
        if self.pause_above_bps.is_some_and(|v| v < MIN_BUSY_LIMIT) {
            return Err(RuleError::BusyLimit);
        }
        if self.large_files_over.is_some_and(|v| v < MIN_LARGE_FILE) {
            return Err(RuleError::LargeFile);
        }
        Ok(())
    }

    /// No rule at all: the queue behaves like the plain queue always did.
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }

    pub fn uses_idle(&self) -> bool {
        self.only_when_idle
    }

    pub fn uses_connection(&self) -> bool {
        self.connection != ConnectionRule::Any
    }

    pub fn uses_traffic(&self) -> bool {
        self.pause_above_bps.is_some()
    }

    fn window(&self) -> Option<Schedule> {
        match &self.when {
            When::Between { start, stop, days } => Some(Schedule { enabled: true, start: start.clone(), stop: stop.clone(), days: *days }),
            _ => None,
        }
    }

    /// Set up the state for rules that were just saved: a start time that already passed today does not count.
    pub fn fresh_state(&self, now: NaiveDateTime) -> RuleState {
        RuleState { used_until: matches!(self.when, When::StartAt { .. }).then_some(now), ..Default::default() }
    }

    /// Is the queue open by time alone? `has_work`: something in the queue still has to download.
    fn time_gate(&self, st: &mut RuleState, now: NaiveDateTime, has_work: bool) -> Result<(), Closed> {
        match &self.when {
            When::Anytime => Ok(()),
            When::Between { .. } => {
                let w = self.window().expect("between has a window");
                if w.is_open(now) {
                    Ok(())
                } else {
                    Err(Closed::Time { opens_at: w.next_change(now) })
                }
            }
            When::StartAt { time, days } => {
                let started = last_start(time, days, now).filter(|s| st.used_until.is_none_or(|u| u < *s));
                match started {
                    Some(_) if has_work => Ok(()),
                    Some(_) => {
                        // Everything done: wait for the next start time.
                        st.used_until = Some(now);
                        Err(Closed::Time { opens_at: next_start(time, days, now) })
                    }
                    None => Err(Closed::Time { opens_at: next_start(time, days, now) }),
                }
            }
        }
    }

    /// Judge the rules. `was_open`: the queue was open at the previous evaluation (keeps "only when idle" going
    /// for [`IDLE_MIN_RUN`]).
    pub fn evaluate(&self, st: &mut RuleState, inp: &Inputs, has_work: bool, was_open: bool) -> Result<(), Closed> {
        // The traffic readings are counted every time, so the state is right whenever the other rules allow.
        let busy = self.track_busy(st, inp.other_bps);
        self.time_gate(st, inp.now, has_work)?;
        if inp.online == Some(false) {
            return Err(Closed::Offline);
        }
        if let Some(net) = inp.net {
            let wrong = match self.connection {
                ConnectionRule::Any => false,
                ConnectionRule::Wifi => matches!(net, NetKind::Wired | NetKind::Mobile),
                ConnectionRule::Wired => matches!(net, NetKind::Wifi | NetKind::Mobile),
                ConnectionRule::NotMobile => net == NetKind::Mobile,
            };
            if wrong {
                return Err(Closed::Connection);
            }
        }
        if self.only_when_idle {
            if let Some(idle) = inp.idle {
                let away = idle >= IDLE_AFTER;
                let grace = was_open && st.idle_opened_at.is_some_and(|t| inp.now - t < IDLE_MIN_RUN);
                if away {
                    if !was_open || st.idle_opened_at.is_none() {
                        st.idle_opened_at = Some(inp.now);
                    }
                } else if !grace {
                    st.idle_opened_at = None;
                    return Err(Closed::NotIdle);
                }
            }
        }
        if busy {
            return Err(Closed::Busy);
        }
        Ok(())
    }

    /// Count a traffic reading; true while the connection counts as busy.
    fn track_busy(&self, st: &mut RuleState, other: Option<u64>) -> bool {
        let Some(limit) = self.pause_above_bps else {
            (st.busy, st.busy_hits, st.calm_hits) = (false, 0, 0);
            return false;
        };
        let Some(other) = other else { return st.busy };
        if !st.busy {
            st.busy_hits = if other > limit { st.busy_hits.saturating_add(1) } else { 0 };
            if st.busy_hits >= BUSY_ENTER {
                (st.busy, st.busy_hits, st.calm_hits) = (true, 0, 0);
            }
        } else {
            st.calm_hits = if (other as f64) < limit as f64 * BUSY_CALM_SHARE { st.calm_hits.saturating_add(1) } else { 0 };
            if st.calm_hits >= BUSY_LEAVE {
                (st.busy, st.busy_hits, st.calm_hits) = (false, 0, 0);
            }
        }
        st.busy
    }

    /// The times this queue may download between `now` and `until`, for the timeline: windows as spans, a start
    /// time as a span that runs to `until` (it lasts until the queue is done, which nobody knows in advance).
    pub fn spans(&self, now: NaiveDateTime, until: NaiveDateTime) -> Vec<(NaiveDateTime, NaiveDateTime)> {
        match &self.when {
            When::Anytime => Vec::new(),
            When::Between { .. } => self.window().map(|w| w.spans(now, until)).unwrap_or_default(),
            When::StartAt { time, days } => start_times(time, days, now, until).into_iter().take(1).map(|s| (s, until)).collect(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuleError {
    #[error(transparent)]
    Time(#[from] ScheduleError),
    #[error("the busy-connection limit must be at least 64 KB/s")]
    BusyLimit,
    #[error("the big-file size must be at least 1 MB")]
    LargeFile,
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    // 2026-03-02 is a Monday.
    fn at(d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 3, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }
    fn inputs(now: NaiveDateTime) -> Inputs {
        Inputs { now, idle: None, net: None, online: None, other_bps: None }
    }
    fn night() -> QueueRules {
        QueueRules { when: When::Between { start: "23:00".into(), stop: "07:00".into(), days: [true; 7] }, ..Default::default() }
    }
    fn at_two() -> QueueRules {
        QueueRules { when: When::StartAt { time: "02:00".into(), days: [true; 7] }, ..Default::default() }
    }

    #[test]
    fn a_queue_without_rules_is_always_open_whatever_the_computer_says() {
        let r = QueueRules::default();
        assert!(r.is_plain());
        let mut st = RuleState::default();
        let inp =
            Inputs { now: at(2, 12, 0), idle: Some(Duration::ZERO), net: Some(NetKind::Mobile), online: None, other_bps: Some(u64::MAX) };
        assert_eq!(r.evaluate(&mut st, &inp, true, false), Ok(()));
    }

    #[test]
    fn a_window_opens_and_closes_and_says_when_it_opens() {
        let r = night();
        let mut st = RuleState::default();
        assert_eq!(r.evaluate(&mut st, &inputs(at(2, 22, 0)), true, false), Err(Closed::Time { opens_at: Some(at(2, 23, 0)) }));
        assert_eq!(r.evaluate(&mut st, &inputs(at(2, 23, 30)), true, false), Ok(()));
        assert_eq!(r.evaluate(&mut st, &inputs(at(3, 6, 59)), true, true), Ok(()));
        assert!(matches!(r.evaluate(&mut st, &inputs(at(3, 7, 0)), true, true), Err(Closed::Time { .. })));
        assert!(Closed::Time { opens_at: None }.pauses_running());
    }

    #[test]
    fn a_start_time_set_after_it_passed_waits_for_the_next_day() {
        let r = at_two();
        let mut st = r.fresh_state(at(2, 15, 0));
        assert_eq!(r.evaluate(&mut st, &inputs(at(2, 15, 0)), true, false), Err(Closed::Time { opens_at: Some(at(3, 2, 0)) }));
        assert_eq!(r.evaluate(&mut st, &inputs(at(3, 1, 59)), true, false), Err(Closed::Time { opens_at: Some(at(3, 2, 0)) }));
        assert_eq!(r.evaluate(&mut st, &inputs(at(3, 2, 0)), true, false), Ok(()), "opens at 02:00");
        assert_eq!(r.evaluate(&mut st, &inputs(at(3, 9, 0)), true, true), Ok(()), "and stays open while there is work");
        // Done: closed until the next start, and new work waits for it.
        assert_eq!(r.evaluate(&mut st, &inputs(at(3, 9, 30)), false, true), Err(Closed::Time { opens_at: Some(at(4, 2, 0)) }));
        assert_eq!(r.evaluate(&mut st, &inputs(at(3, 10, 0)), true, false), Err(Closed::Time { opens_at: Some(at(4, 2, 0)) }));
    }

    #[test]
    fn a_start_time_missed_while_the_app_was_closed_still_starts_the_queue() {
        let r = at_two();
        let mut st = r.fresh_state(at(2, 15, 0));
        // The app was closed at 02:00 and opens at 08:00: the queue starts then.
        assert_eq!(r.evaluate(&mut st, &inputs(at(3, 8, 0)), true, false), Ok(()));
    }

    #[test]
    fn start_times_respect_the_days() {
        let mut days = [false; 7];
        days[4] = true; // Friday
        assert_eq!(next_start("02:00", &days, at(2, 12, 0)), Some(at(6, 2, 0)));
        assert_eq!(last_start("02:00", &days, at(7, 1, 0)), Some(at(6, 2, 0)));
        assert_eq!(
            start_times("02:00", &[true; 7], at(2, 2, 0), at(4, 2, 0)),
            vec![at(3, 2, 0), at(4, 2, 0)],
            "after is exclusive, until inclusive"
        );
    }

    #[test]
    fn no_network_closes_without_pausing_and_unknown_never_closes() {
        let r = QueueRules::default();
        let mut st = RuleState::default();
        let mut inp = inputs(at(2, 12, 0));
        inp.online = Some(false);
        assert_eq!(r.evaluate(&mut st, &inp, true, true), Err(Closed::Offline));
        assert!(!Closed::Offline.pauses_running());
        inp.online = None;
        assert_eq!(r.evaluate(&mut st, &inp, true, true), Ok(()));
    }

    #[test]
    fn connection_rules_close_only_on_a_known_wrong_connection() {
        let mut st = RuleState::default();
        let case = |rule: ConnectionRule, net: Option<NetKind>| {
            let r = QueueRules { connection: rule, ..Default::default() };
            let mut inp = inputs(at(2, 12, 0));
            inp.net = net;
            r.evaluate(&mut RuleState::default(), &inp, true, false).is_ok()
        };
        assert!(case(ConnectionRule::Wifi, Some(NetKind::Wifi)));
        assert!(!case(ConnectionRule::Wifi, Some(NetKind::Wired)));
        assert!(!case(ConnectionRule::Wifi, Some(NetKind::Mobile)));
        assert!(case(ConnectionRule::Wifi, Some(NetKind::Other)), "a VPN or an unknown adapter never blocks");
        assert!(case(ConnectionRule::Wifi, None));
        assert!(case(ConnectionRule::Wired, Some(NetKind::Wired)));
        assert!(!case(ConnectionRule::Wired, Some(NetKind::Wifi)));
        assert!(case(ConnectionRule::NotMobile, Some(NetKind::Wifi)));
        assert!(!case(ConnectionRule::NotMobile, Some(NetKind::Mobile)));
        let _ = &mut st;
    }

    #[test]
    fn only_when_idle_opens_after_two_minutes_and_keeps_going_a_little_when_the_user_comes_back() {
        let r = QueueRules { only_when_idle: true, ..Default::default() };
        let mut st = RuleState::default();
        let idle = |now, secs| Inputs { idle: Some(Duration::from_secs(secs)), ..inputs(now) };
        assert_eq!(r.evaluate(&mut st, &idle(at(2, 12, 0), 30), true, false), Err(Closed::NotIdle));
        assert_eq!(r.evaluate(&mut st, &idle(at(2, 12, 2), 120), true, false), Ok(()));
        // The user touches the mouse 10 s later: still running (grace)...
        let t = at(2, 12, 2) + Span::seconds(10);
        assert_eq!(r.evaluate(&mut st, &idle(t, 0), true, true), Ok(()));
        // ...but not after 30 s.
        assert_eq!(r.evaluate(&mut st, &idle(at(2, 12, 3), 5), true, true), Err(Closed::NotIdle));
        // A computer that does not say how long it has been idle never blocks the queue.
        assert_eq!(r.evaluate(&mut st, &inputs(at(2, 12, 4)), true, false), Ok(()));
    }

    #[test]
    fn a_busy_connection_pauses_after_three_readings_and_carries_on_after_five_calm_ones() {
        let r = QueueRules { pause_above_bps: Some(1_000_000), ..Default::default() };
        let mut st = RuleState::default();
        let other = |bps| Inputs { other_bps: Some(bps), ..inputs(at(2, 12, 0)) };
        assert!(r.evaluate(&mut st, &other(2_000_000), true, true).is_ok());
        assert!(r.evaluate(&mut st, &other(2_000_000), true, true).is_ok());
        assert_eq!(r.evaluate(&mut st, &other(2_000_000), true, true), Err(Closed::Busy), "third reading in a row");
        // Unknown traffic keeps the state; slightly below the limit is not calm enough.
        assert_eq!(r.evaluate(&mut st, &inputs(at(2, 12, 0)), true, false), Err(Closed::Busy));
        for _ in 0..10 {
            assert_eq!(r.evaluate(&mut st, &other(900_000), true, false), Err(Closed::Busy));
        }
        for _ in 0..4 {
            assert_eq!(r.evaluate(&mut st, &other(100_000), true, false), Err(Closed::Busy));
        }
        assert_eq!(r.evaluate(&mut st, &other(100_000), true, false), Ok(()), "fifth calm reading");
        // A single spike does not count.
        assert!(r.evaluate(&mut st, &other(5_000_000), true, true).is_ok());
        assert!(r.evaluate(&mut st, &other(100), true, true).is_ok());
        assert!(r.evaluate(&mut st, &other(5_000_000), true, true).is_ok());
    }

    #[test]
    fn validation_and_strict_reading() {
        assert!(night().validate().is_ok());
        assert!(at_two().validate().is_ok());
        let bad = QueueRules { when: When::Between { start: "23:00".into(), stop: "23:00".into(), days: [true; 7] }, ..Default::default() };
        assert!(bad.validate().is_err());
        let bad = QueueRules { when: When::StartAt { time: "2:00".into(), days: [true; 7] }, ..Default::default() };
        assert!(bad.validate().is_err());
        let bad = QueueRules { when: When::StartAt { time: "02:00".into(), days: [false; 7] }, ..Default::default() };
        assert!(bad.validate().is_err());
        assert_eq!(QueueRules { pause_above_bps: Some(10), ..Default::default() }.validate(), Err(RuleError::BusyLimit));
        assert_eq!(QueueRules { large_files_over: Some(10), ..Default::default() }.validate(), Err(RuleError::LargeFile));
        let json = r#"{"when":{"kind":"startAt","time":"02:00","days":[true,true,true,true,true,true,true]},"onlyWhenIdle":true,"connection":"wifi","pauseAboveBps":1048576,"largeFilesOver":null}"#;
        let r: QueueRules = serde_json::from_str(json).unwrap();
        assert!(r.only_when_idle && r.connection == ConnectionRule::Wifi && r.uses_traffic());
        assert_eq!(serde_json::from_str::<QueueRules>("{}").unwrap(), QueueRules::default(), "missing fields mean no rule");
        assert!(serde_json::from_str::<QueueRules>(r#"{"surprise":1}"#).is_err());
    }

    #[test]
    fn timeline_spans() {
        let spans = night().spans(at(2, 12, 0), at(3, 12, 0));
        assert_eq!(spans, vec![(at(2, 23, 0), at(3, 7, 0))]);
        let inside = night().spans(at(3, 1, 0), at(4, 1, 0));
        assert_eq!(inside, vec![(at(3, 1, 0), at(3, 7, 0)), (at(3, 23, 0), at(4, 1, 0))], "a window already open starts now");
        assert_eq!(at_two().spans(at(2, 12, 0), at(3, 12, 0)), vec![(at(3, 2, 0), at(3, 12, 0))]);
        assert!(QueueRules::default().spans(at(2, 12, 0), at(3, 12, 0)).is_empty());
    }
}
