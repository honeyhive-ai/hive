//! Scheduled / triggered agents — the time math.
//!
//! A scheduled agent fires a prompt at a recurring time (e.g. "every 30
//! minutes" or "daily at 09:00"). This module holds only the pure, clock-free
//! decision — *is this trigger due to fire right now, given when it last
//! fired?* — so it's fully unit-testable. The runtime owns the actual tick
//! loop, persistence, and dispatch (which reuses the normal turn path).
//!
//! All times are `time::OffsetDateTime`. `DailyAt` is evaluated in whatever
//! offset the caller's `now` carries, so passing a local-offset `now` gives
//! local wall-clock behavior; passing UTC gives UTC.

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, Time};

/// How a scheduled agent recurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleTrigger {
    /// Fire every `every_secs` seconds. A value of 0 is treated as 1s.
    Interval { every_secs: u64 },
    /// Fire once per calendar day at `hour`:`minute` (wall clock, in `now`'s
    /// offset). Out-of-range values never fire.
    DailyAt { hour: u8, minute: u8 },
}

impl ScheduleTrigger {
    /// Whether this trigger should fire at `now`, given the instant it last
    /// fired (`None` = never fired yet).
    ///
    /// - `Interval`: due when at least the interval has elapsed since the last
    ///   run (and always due if it has never run).
    /// - `DailyAt`: due when the most recent `HH:MM` boundary at or before
    ///   `now` is *after* the last run — i.e. we haven't fired since today's
    ///   (or last night's) scheduled time passed.
    pub fn is_due(&self, last_run: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
        match *self {
            ScheduleTrigger::Interval { every_secs } => {
                let every = every_secs.max(1) as i64;
                match last_run {
                    None => true,
                    Some(prev) => (now - prev).whole_seconds() >= every,
                }
            }
            ScheduleTrigger::DailyAt { hour, minute } => {
                let Ok(at) = Time::from_hms(hour, minute, 0) else {
                    return false; // invalid time → never fires
                };
                // The most recent occurrence of `at` at or before `now`: today's
                // boundary if it has already passed, otherwise yesterday's.
                let today = now.replace_time(at);
                let boundary = if today <= now { today } else { today - Duration::days(1) };
                match last_run {
                    None => true,
                    Some(prev) => prev < boundary,
                }
            }
        }
    }

    /// Reject obviously-invalid triggers (used when accepting user/config
    /// input). Returns a human-readable reason on failure.
    pub fn validate(&self) -> Result<(), String> {
        match *self {
            ScheduleTrigger::Interval { every_secs } => {
                if every_secs == 0 {
                    Err("interval must be at least 1 second".into())
                } else {
                    Ok(())
                }
            }
            ScheduleTrigger::DailyAt { hour, minute } => {
                if hour > 23 || minute > 59 {
                    Err(format!("invalid time {hour:02}:{minute:02}"))
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, PrimitiveDateTime, UtcOffset};

    /// Build an `OffsetDateTime` from components (avoids the `time` `macros`
    /// feature, which the workspace doesn't enable).
    fn dt(y: i32, mo: u8, d: u8, h: u8, mi: u8, s: u8, off_hours: i8) -> OffsetDateTime {
        let date = Date::from_calendar_date(y, Month::try_from(mo).unwrap(), d).unwrap();
        let tm = Time::from_hms(h, mi, s).unwrap();
        let off = UtcOffset::from_hms(off_hours, 0, 0).unwrap();
        PrimitiveDateTime::new(date, tm).assume_offset(off)
    }

    #[test]
    fn interval_fires_first_time_when_never_run() {
        let t = ScheduleTrigger::Interval { every_secs: 600 };
        assert!(t.is_due(None, dt(2026, 6, 22, 12, 0, 0, 0)));
    }

    #[test]
    fn interval_waits_for_full_period() {
        let t = ScheduleTrigger::Interval { every_secs: 600 };
        let last = dt(2026, 6, 22, 12, 0, 0, 0);
        // 9 minutes later → not yet.
        assert!(!t.is_due(Some(last), dt(2026, 6, 22, 12, 9, 0, 0)));
        // exactly 10 minutes → due.
        assert!(t.is_due(Some(last), dt(2026, 6, 22, 12, 10, 0, 0)));
        // well past → due.
        assert!(t.is_due(Some(last), dt(2026, 6, 22, 13, 0, 0, 0)));
    }

    #[test]
    fn interval_zero_is_clamped_not_divide_by_zero() {
        let t = ScheduleTrigger::Interval { every_secs: 0 };
        let last = dt(2026, 6, 22, 12, 0, 0, 0);
        assert!(t.is_due(Some(last), dt(2026, 6, 22, 12, 0, 1, 0)));
    }

    #[test]
    fn daily_fires_first_time_when_never_run() {
        let t = ScheduleTrigger::DailyAt { hour: 9, minute: 0 };
        assert!(t.is_due(None, dt(2026, 6, 22, 9, 30, 0, 0)));
    }

    #[test]
    fn daily_due_once_boundary_passed_then_not_again_same_day() {
        let t = ScheduleTrigger::DailyAt { hour: 9, minute: 0 };
        // Fired yesterday; now it's 09:30 today → today's 09:00 boundary passed
        // and last run predates it → due.
        let last = dt(2026, 6, 21, 9, 0, 30, 0);
        assert!(t.is_due(Some(last), dt(2026, 6, 22, 9, 30, 0, 0)));
        // After firing at 09:30 today, asking again at 10:00 → not due (already
        // ran past today's boundary).
        let ran = dt(2026, 6, 22, 9, 30, 0, 0);
        assert!(!t.is_due(Some(ran), dt(2026, 6, 22, 10, 0, 0, 0)));
    }

    #[test]
    fn daily_not_due_before_todays_boundary() {
        let t = ScheduleTrigger::DailyAt { hour: 9, minute: 0 };
        // It's 08:00; last run was yesterday at 09:00. The most recent boundary
        // is yesterday's 09:00, and we already ran then → not due.
        let last = dt(2026, 6, 21, 9, 0, 0, 0);
        assert!(!t.is_due(Some(last), dt(2026, 6, 22, 8, 0, 0, 0)));
    }

    #[test]
    fn daily_respects_offset_of_now() {
        // 09:00 "due" evaluated in a +02:00 offset: at 09:30+02:00 the local
        // 09:00 boundary has passed.
        let t = ScheduleTrigger::DailyAt { hour: 9, minute: 0 };
        assert!(t.is_due(None, dt(2026, 6, 22, 9, 30, 0, 2)));
        // At 08:30 +02:00 with a run earlier today (07:00) → boundary is
        // yesterday's, already ran since → not due.
        let last = dt(2026, 6, 22, 7, 0, 0, 2);
        assert!(!t.is_due(Some(last), dt(2026, 6, 22, 8, 30, 0, 2)));
    }

    #[test]
    fn daily_invalid_time_never_fires() {
        let t = ScheduleTrigger::DailyAt { hour: 25, minute: 0 };
        assert!(!t.is_due(None, dt(2026, 6, 22, 12, 0, 0, 0)));
    }

    #[test]
    fn validate_rejects_bad_input() {
        assert!(ScheduleTrigger::Interval { every_secs: 0 }.validate().is_err());
        assert!(ScheduleTrigger::Interval { every_secs: 60 }.validate().is_ok());
        assert!(ScheduleTrigger::DailyAt { hour: 24, minute: 0 }.validate().is_err());
        assert!(ScheduleTrigger::DailyAt { hour: 9, minute: 60 }.validate().is_err());
        assert!(ScheduleTrigger::DailyAt { hour: 23, minute: 59 }.validate().is_ok());
    }

    #[test]
    fn serde_tagged_round_trip() {
        let t = ScheduleTrigger::DailyAt { hour: 9, minute: 30 };
        let j = serde_json::to_string(&t).unwrap();
        assert!(j.contains("daily_at"), "got {j}");
        assert_eq!(serde_json::from_str::<ScheduleTrigger>(&j).unwrap(), t);
        let i = ScheduleTrigger::Interval { every_secs: 300 };
        let j = serde_json::to_string(&i).unwrap();
        assert!(j.contains("interval"), "got {j}");
        assert_eq!(serde_json::from_str::<ScheduleTrigger>(&j).unwrap(), i);
    }
}
