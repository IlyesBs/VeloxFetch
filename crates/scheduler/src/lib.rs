//! Time-window scheduling: "start at 22:00, stop at 07:00".
//!
//! Pure logic with no I/O and no clock of its own: every function takes the current local time, so
//! it is deterministic and testable. The download manager decides what to *do* at a boundary.

pub use chrono;
use chrono::{Datelike, Duration, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleError {
    #[error("invalid time '{0}' (expected HH:MM, 24-hour)")]
    BadTime(String),
    #[error("start and stop are the same time; the window would be empty or always open")]
    EmptyWindow,
    #[error("select at least one day")]
    NoDays,
}

/// A daily window. `days[0]` is Monday … `days[6]` is Sunday and refers to the day the window
/// *starts* on, so an overnight window starting Friday 22:00 runs until Saturday 07:00.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub enabled: bool,
    /// "HH:MM" local time at which downloading may begin.
    pub start: String,
    /// "HH:MM" local time at which the queue stops. Earlier than `start` means overnight.
    pub stop: String,
    pub days: [bool; 7],
}

impl Default for Schedule {
    fn default() -> Self {
        Self { enabled: false, start: "22:00".into(), stop: "07:00".into(), days: [true; 7] }
    }
}

pub fn parse_time(s: &str) -> Result<NaiveTime, ScheduleError> {
    let bad = || ScheduleError::BadTime(s.chars().take(16).collect());
    let (h, m) = s.split_once(':').ok_or_else(bad)?;
    let digits = |p: &str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit());
    if !digits(h) || !digits(m) {
        return Err(bad());
    }
    NaiveTime::from_hms_opt(h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?, 0).ok_or_else(bad)
}

impl Schedule {
    /// Check the fields. A disabled schedule is still validated so that saved data is always sane.
    pub fn validate(&self) -> Result<(), ScheduleError> {
        let (a, b) = (parse_time(&self.start)?, parse_time(&self.stop)?);
        if a == b {
            return Err(ScheduleError::EmptyWindow);
        }
        if !self.days.iter().any(|d| *d) {
            return Err(ScheduleError::NoDays);
        }
        Ok(())
    }

    fn bounds(&self) -> Option<(NaiveTime, NaiveTime)> {
        Some((parse_time(&self.start).ok()?, parse_time(&self.stop).ok()?))
    }

    fn day_on(&self, d: chrono::Weekday) -> bool {
        self.days[d.num_days_from_monday() as usize]
    }

    /// Is downloading allowed at `now`? A disabled (or unparsable) schedule never restricts anything.
    pub fn is_open(&self, now: NaiveDateTime) -> bool {
        if !self.enabled {
            return true;
        }
        let Some((start, stop)) = self.bounds() else { return true };
        let t = now.time();
        let today = now.date().weekday();
        if start < stop {
            self.day_on(today) && t >= start && t < stop
        } else {
            // Overnight: the evening part belongs to today, the morning part to yesterday's window.
            (self.day_on(today) && t >= start) || (self.day_on(today.pred()) && t < stop)
        }
    }

    /// The next moment (strictly after `now`) at which [`is_open`](Self::is_open) flips, if any.
    /// Looks at every boundary in the next nine days, so it is cheap and exact.
    pub fn next_change(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        if !self.enabled {
            return None;
        }
        let (start, stop) = self.bounds()?;
        let now = now.with_nanosecond(0)?;
        let current = self.is_open(now);
        let mut best: Option<NaiveDateTime> = None;
        for day in 0..=8 {
            let date = now.date() + Duration::days(day);
            for boundary in [start, stop] {
                let at = date.and_time(boundary);
                if at > now && self.is_open(at) != current && best.is_none_or(|b| at < b) {
                    best = Some(at);
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }

    // 2026-03-02 is a Monday.
    fn sched(start: &str, stop: &str, days: [bool; 7]) -> Schedule {
        Schedule { enabled: true, start: start.into(), stop: stop.into(), days }
    }

    #[test]
    fn disabled_schedule_never_restricts() {
        let s = Schedule::default();
        assert!(!s.enabled);
        for h in 0..24 {
            assert!(s.is_open(at(2026, 3, 2, h, 0)));
        }
        assert_eq!(s.next_change(at(2026, 3, 2, 12, 0)), None);
    }

    #[test]
    fn overnight_window_22_to_07() {
        let s = sched("22:00", "07:00", [true; 7]);
        assert!(!s.is_open(at(2026, 3, 2, 21, 59)));
        assert!(s.is_open(at(2026, 3, 2, 22, 0)), "start is inclusive");
        assert!(s.is_open(at(2026, 3, 2, 23, 59)));
        assert!(s.is_open(at(2026, 3, 3, 0, 0)));
        assert!(s.is_open(at(2026, 3, 3, 6, 59)));
        assert!(!s.is_open(at(2026, 3, 3, 7, 0)), "stop is exclusive");
        assert!(!s.is_open(at(2026, 3, 3, 12, 0)));
    }

    #[test]
    fn same_day_window() {
        let s = sched("09:00", "17:30", [true; 7]);
        assert!(!s.is_open(at(2026, 3, 2, 8, 59)));
        assert!(s.is_open(at(2026, 3, 2, 9, 0)));
        assert!(s.is_open(at(2026, 3, 2, 17, 29)));
        assert!(!s.is_open(at(2026, 3, 2, 17, 30)));
        assert!(!s.is_open(at(2026, 3, 2, 23, 0)));
    }

    #[test]
    fn days_refer_to_the_day_the_window_starts() {
        // Only Friday (index 4) starts a window: Fri 22:00 -> Sat 07:00.
        let mut days = [false; 7];
        days[4] = true;
        let s = sched("22:00", "07:00", days);
        assert!(s.is_open(at(2026, 3, 6, 23, 0)), "Friday night");
        assert!(s.is_open(at(2026, 3, 7, 3, 0)), "Saturday early morning belongs to Friday's window");
        assert!(!s.is_open(at(2026, 3, 7, 23, 0)), "Saturday night does not start a window");
        assert!(!s.is_open(at(2026, 3, 6, 3, 0)), "Friday early morning belongs to Thursday's window");
        assert!(!s.is_open(at(2026, 3, 2, 23, 0)), "Monday night");
    }

    #[test]
    fn next_change_finds_the_exact_boundary() {
        let s = sched("22:00", "07:00", [true; 7]);
        assert_eq!(s.next_change(at(2026, 3, 2, 12, 0)), Some(at(2026, 3, 2, 22, 0)));
        assert_eq!(s.next_change(at(2026, 3, 2, 23, 0)), Some(at(2026, 3, 3, 7, 0)));
        assert_eq!(s.next_change(at(2026, 3, 2, 22, 0)), Some(at(2026, 3, 3, 7, 0)), "strictly after now");
        // Only Friday: from Monday noon the next flip is Friday 22:00.
        let mut days = [false; 7];
        days[4] = true;
        assert_eq!(sched("22:00", "07:00", days).next_change(at(2026, 3, 2, 12, 0)), Some(at(2026, 3, 6, 22, 0)));
    }

    #[test]
    fn validation() {
        assert!(sched("22:00", "07:00", [true; 7]).validate().is_ok());
        assert_eq!(sched("22:00", "22:00", [true; 7]).validate(), Err(ScheduleError::EmptyWindow));
        assert_eq!(sched("22:00", "07:00", [false; 7]).validate(), Err(ScheduleError::NoDays));
        for bad in ["24:00", "7:00", "07:60", "ab:cd", "0700", "", "07:00:00", "07:0", "-1:00", "\u{660}\u{667}:\u{660}\u{660}"] {
            assert!(sched(bad, "07:00", [true; 7]).validate().is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn unknown_fields_are_rejected_when_deserialising() {
        let ok = r#"{"enabled":true,"start":"22:00","stop":"07:00","days":[true,true,true,true,true,true,true]}"#;
        assert!(serde_json::from_str::<Schedule>(ok).is_ok());
        let extra = r#"{"enabled":true,"start":"22:00","stop":"07:00","days":[true,true,true,true,true,true,true],"x":1}"#;
        assert!(serde_json::from_str::<Schedule>(extra).is_err());
        let short = r#"{"enabled":true,"start":"22:00","stop":"07:00","days":[true]}"#;
        assert!(serde_json::from_str::<Schedule>(short).is_err());
    }
}
