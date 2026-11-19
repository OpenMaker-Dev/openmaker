//! When a schedule runs, as FileMaker Server's schedule details define it
//! (Claris, "Specifying schedule details", "Examples of repeating
//! schedules"): once at a date and time, or on days (daily, every n days,
//! or on days of the week) from a start date to an optional end date
//! (inclusive), each day at one time or every n minutes or hours from a
//! start time to an end time. Dates and times are the server's local ones.

use std::time::Duration;

use jiff::civil::{Date, DateTime, Time, Weekday};
use jiff::Zoned;

/// The longest gap between days a schedule may have.
const MAX_EVERY_DAYS: u32 = 366;

/// When a schedule runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Repeat {
    /// Once, at this local date and time.
    Once(DateTime),
    /// On `days` at `times`, from `start` to `end` (both included; open
    /// when absent).
    Days {
        days: Days,
        times: Times,
        start: Option<Date>,
        end: Option<Date>,
    },
}

/// The days a repeating schedule runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Days {
    /// Every day.
    Daily,
    /// Every n days, counted from the start date (which is required).
    Every(u32),
    /// On these days of the week.
    Weekly(Vec<Weekday>),
}

/// When a repeating schedule runs on one of its days.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Times {
    /// At this time.
    At(Time),
    /// At `from`, then every `interval` (whole minutes) while the time is
    /// not after `until`.
    Every {
        interval: Duration,
        from: Time,
        until: Time,
    },
}

impl Repeat {
    /// Why this repeat cannot run as given, if it cannot.
    pub fn check(&self) -> Result<(), String> {
        let Repeat::Days {
            days,
            times,
            start,
            end,
        } = self
        else {
            return Ok(());
        };
        match days {
            Days::Daily => {}
            Days::Every(n) if !(1..=MAX_EVERY_DAYS).contains(n) => {
                return Err(format!("every n days needs n from 1 to {MAX_EVERY_DAYS}"))
            }
            Days::Every(_) if start.is_none() => {
                return Err("every n days needs a start date to count from".into())
            }
            Days::Every(_) => {}
            Days::Weekly(days) if days.is_empty() => {
                return Err("a weekly schedule needs at least one day".into())
            }
            Days::Weekly(_) => {}
        }
        if let Times::Every {
            interval,
            from,
            until,
        } = times
        {
            if interval.is_zero() || interval.as_secs() % 60 != 0 || interval.subsec_nanos() != 0 {
                return Err("the repeat interval must be a whole number of minutes".into());
            }
            if interval.as_secs() > 24 * 3600 {
                return Err("the repeat interval must be at most 24 hours".into());
            }
            if until < from {
                return Err("the end time is before the start time".into());
            }
        }
        if let (Some(start), Some(end)) = (start, end) {
            if end < start {
                return Err("the end date is before the start date".into());
            }
        }
        Ok(())
    }

    /// The first time this repeat runs after `after` (strictly), in
    /// `after`'s time zone; `None` when it never runs again. A run missed
    /// (the server was down, or the schedule's last run had not ended) is
    /// not made up: FileMaker skips it ("Examples of repeating
    /// schedules"). A local time the time zone skips (the change to
    /// daylight saving time) shifts forward by the gap's duration, and
    /// a local time that occurs twice runs at its first instant (jiff's
    /// compatible disambiguation; OpenMaker's choice).
    pub fn next_after(&self, after: &Zoned) -> Option<Zoned> {
        let zone = after.time_zone().clone();
        let at = |date: Date, time: Time| date.to_datetime(time).to_zoned(zone.clone()).ok();
        let later = |z: &Zoned| z.timestamp() > after.timestamp();
        match self {
            Repeat::Once(when) => when.to_zoned(zone.clone()).ok().filter(later),
            Repeat::Days {
                days,
                times,
                start,
                end,
            } => {
                // A gap may move the day's first runs to the next local
                // day: begin the day before.
                let mut date = after.date().yesterday().ok()?;
                if let Some(start) = start {
                    date = date.max(*start);
                }
                // Yesterday, today and the days up to the next one the
                // schedule runs on.
                let span = match days {
                    Days::Daily => 3,
                    Days::Every(n) => *n + 2,
                    Days::Weekly(_) => 9,
                };
                for _ in 0..span {
                    if end.is_some_and(|end| date > end) {
                        return None;
                    }
                    if days.includes(date, *start) {
                        // Shifting a missing local time can put it after
                        // a later wall-clock time. Choose the first
                        // instant, not the first local time.
                        if let Some(next) = times
                            .on()
                            .filter_map(|t| at(date, t))
                            .filter(later)
                            .min_by_key(Zoned::timestamp)
                        {
                            return Some(next);
                        }
                    }
                    date = date.tomorrow().ok()?;
                }
                None
            }
        }
    }
}

impl Days {
    fn includes(&self, date: Date, start: Option<Date>) -> bool {
        match self {
            Days::Daily => true,
            Days::Every(n) => start
                .and_then(|start| start.until(date).ok())
                .is_some_and(|span| span.get_days() >= 0 && span.get_days() % *n as i32 == 0),
            Days::Weekly(days) => days.contains(&date.weekday()),
        }
    }
}

impl Times {
    /// The times of one day, in order.
    fn on(&self) -> impl Iterator<Item = Time> + '_ {
        let (first, step, last) = match self {
            Times::At(time) => (seconds(*time), 1, seconds(*time)),
            Times::Every {
                interval,
                from,
                until,
            } => (
                seconds(*from),
                interval.as_secs().max(60) as i64,
                seconds(*until),
            ),
        };
        (0..)
            .map(move |k| first + k * step)
            .take_while(move |s| *s <= last)
            .filter_map(|s| {
                Time::new((s / 3600) as i8, (s / 60 % 60) as i8, (s % 60) as i8, 0).ok()
            })
    }
}

/// A time of day in whole seconds.
fn seconds(time: Time) -> i64 {
    i64::from(time.hour()) * 3600 + i64::from(time.minute()) * 60 + i64::from(time.second())
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    use super::*;

    fn utc(y: i16, m: i8, d: i8, h: i8, min: i8) -> Zoned {
        date(y, m, d)
            .at(h, min, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
    }

    fn times(repeat: &Repeat, from: Zoned, count: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut now = from;
        for _ in 0..count {
            match repeat.next_after(&now) {
                Some(next) => {
                    out.push(next.datetime().to_string());
                    now = next;
                }
                None => break,
            }
        }
        out
    }

    fn at(h: i8, m: i8) -> Times {
        Times::At(Time::new(h, m, 0, 0).unwrap())
    }

    /// Claris's "Examples of repeating schedules": created at 4:30 PM on a
    /// Friday (2021-12-17 is one), daily at 5:00 PM until 12/31/2021: today
    /// first, then every day, the end date included.
    #[test]
    fn daily_runs_from_today_through_the_end_date() {
        let repeat = Repeat::Days {
            days: Days::Daily,
            times: at(17, 0),
            start: Some(date(2021, 12, 17)),
            end: Some(date(2021, 12, 31)),
        };
        let runs = times(&repeat, utc(2021, 12, 17, 16, 30), 20);
        assert_eq!(runs.len(), 15);
        assert_eq!(runs[0], "2021-12-17T17:00:00");
        assert_eq!(runs[1], "2021-12-18T17:00:00");
        assert_eq!(runs[14], "2021-12-31T17:00:00");
    }

    /// The weekly example: every Friday at 5:00 PM from that Friday; the
    /// end date is a Friday and runs.
    #[test]
    fn weekly_runs_on_its_days() {
        let repeat = Repeat::Days {
            days: Days::Weekly(vec![Weekday::Friday]),
            times: at(17, 0),
            start: Some(date(2021, 12, 17)),
            end: Some(date(2021, 12, 31)),
        };
        assert_eq!(
            times(&repeat, utc(2021, 12, 17, 16, 30), 5),
            [
                "2021-12-17T17:00:00",
                "2021-12-24T17:00:00",
                "2021-12-31T17:00:00"
            ]
        );
        let workdays = Repeat::Days {
            days: Days::Weekly(vec![Weekday::Monday, Weekday::Wednesday]),
            times: at(8, 0),
            start: None,
            end: None,
        };
        assert_eq!(
            times(&workdays, utc(2026, 10, 29, 12, 0), 3),
            [
                "2026-11-02T08:00:00",
                "2026-11-04T08:00:00",
                "2026-11-09T08:00:00"
            ]
        );
    }

    /// The every-n-days example: every three days at 5:00 PM from Friday:
    /// Friday, then Monday, with no end.
    #[test]
    fn every_n_days_counts_from_the_start_date() {
        let repeat = Repeat::Days {
            days: Days::Every(3),
            times: at(17, 0),
            start: Some(date(2021, 12, 17)),
            end: None,
        };
        assert_eq!(
            times(&repeat, utc(2021, 12, 17, 16, 30), 3),
            [
                "2021-12-17T17:00:00",
                "2021-12-20T17:00:00",
                "2021-12-23T17:00:00"
            ]
        );
        // Asked late, it still counts from the start date.
        assert_eq!(
            times(&repeat, utc(2021, 12, 21, 0, 0), 1),
            ["2021-12-23T17:00:00"]
        );
        // Before the start date, nothing runs earlier.
        assert_eq!(
            times(&repeat, utc(2021, 12, 1, 0, 0), 1),
            ["2021-12-17T17:00:00"]
        );
    }

    #[test]
    fn repeats_within_a_day_and_skips_missed_runs() {
        let repeat = Repeat::Days {
            days: Days::Daily,
            times: Times::Every {
                interval: Duration::from_secs(15 * 60),
                from: Time::new(9, 0, 0, 0).unwrap(),
                until: Time::new(10, 0, 0, 0).unwrap(),
            },
            start: None,
            end: None,
        };
        assert_eq!(
            times(&repeat, utc(2026, 10, 29, 8, 0), 6),
            [
                "2026-10-29T09:00:00",
                "2026-10-29T09:15:00",
                "2026-10-29T09:30:00",
                "2026-10-29T09:45:00",
                "2026-10-29T10:00:00",
                "2026-10-30T09:00:00"
            ]
        );
        // Asked at 9:50 (the 9:15 to 9:45 runs were missed): the next is
        // 10:00, the missed ones are not made up.
        assert_eq!(
            times(&repeat, utc(2026, 10, 29, 9, 50), 1),
            ["2026-10-29T10:00:00"]
        );
        // Every 2 hours from 1:00 to 6:00: 1, 3, 5.
        let hours = Repeat::Days {
            days: Days::Daily,
            times: Times::Every {
                interval: Duration::from_secs(2 * 3600),
                from: Time::new(1, 0, 0, 0).unwrap(),
                until: Time::new(6, 0, 0, 0).unwrap(),
            },
            start: None,
            end: None,
        };
        assert_eq!(
            times(&hours, utc(2026, 10, 29, 0, 0), 4),
            [
                "2026-10-29T01:00:00",
                "2026-10-29T03:00:00",
                "2026-10-29T05:00:00",
                "2026-10-30T01:00:00"
            ]
        );
    }

    #[test]
    fn once_runs_once_if_it_is_still_ahead() {
        let repeat = Repeat::Once(date(2026, 11, 1).at(6, 30, 0, 0));
        assert_eq!(
            times(&repeat, utc(2026, 10, 29, 0, 0), 3),
            ["2026-11-01T06:30:00"]
        );
        assert!(repeat.next_after(&utc(2026, 11, 1, 6, 30)).is_none());
    }

    /// Local times follow the server's time zone across daylight saving
    /// time: a time in the spring gap shifts forward, a time in the
    /// autumn overlap once.
    #[test]
    fn daylight_saving_time() {
        let zone = TimeZone::get("America/New_York").unwrap();
        let repeat = Repeat::Days {
            days: Days::Daily,
            times: Times::Every {
                interval: Duration::from_secs(30 * 60),
                from: Time::new(1, 0, 0, 0).unwrap(),
                until: Time::new(3, 0, 0, 0).unwrap(),
            },
            start: None,
            end: None,
        };
        // 2026-03-08: 2:00 to 3:00 does not exist.
        let spring = date(2026, 3, 8)
            .at(0, 0, 0, 0)
            .to_zoned(zone.clone())
            .unwrap();
        let runs: Vec<String> = {
            let mut now = spring;
            (0..5)
                .map(|_| {
                    now = repeat.next_after(&now).unwrap();
                    now.to_string()
                })
                .collect()
        };
        assert_eq!(
            runs,
            [
                "2026-03-08T01:00:00-05:00[America/New_York]",
                "2026-03-08T01:30:00-05:00[America/New_York]",
                "2026-03-08T03:00:00-04:00[America/New_York]",
                "2026-03-08T03:30:00-04:00[America/New_York]",
                "2026-03-09T01:00:00-04:00[America/New_York]",
            ]
        );
        // 2026-11-01: 1:00 to 2:00 happens twice; each time runs once.
        let autumn = date(2026, 11, 1).at(0, 0, 0, 0).to_zoned(zone).unwrap();
        let mut now = autumn;
        let runs: Vec<String> = (0..5)
            .map(|_| {
                now = repeat.next_after(&now).unwrap();
                now.to_string()
            })
            .collect();
        assert_eq!(
            runs,
            [
                "2026-11-01T01:00:00-04:00[America/New_York]",
                "2026-11-01T01:30:00-04:00[America/New_York]",
                "2026-11-01T02:00:00-05:00[America/New_York]",
                "2026-11-01T02:30:00-05:00[America/New_York]",
                "2026-11-01T03:00:00-05:00[America/New_York]",
            ]
        );
    }

    #[test]
    fn spring_gap_runs_are_ordered_by_instant() {
        let zone = TimeZone::get("America/New_York").unwrap();
        let repeat = Repeat::Days {
            days: Days::Daily,
            times: Times::Every {
                interval: Duration::from_secs(45 * 60),
                from: Time::new(1, 30, 0, 0).unwrap(),
                until: Time::new(3, 45, 0, 0).unwrap(),
            },
            start: None,
            end: None,
        };
        let before = date(2026, 3, 8).at(0, 0, 0, 0).to_zoned(zone).unwrap();
        // The missing 02:15 shifts to 03:15, after the valid 03:00 run.
        assert_eq!(
            times(&repeat, before, 4),
            [
                "2026-03-08T01:30:00",
                "2026-03-08T03:00:00",
                "2026-03-08T03:15:00",
                "2026-03-08T03:45:00",
            ]
        );
    }

    #[test]
    fn checks() {
        let days = |days, times, start, end| Repeat::Days {
            days,
            times,
            start,
            end,
        };
        assert!(days(Days::Every(2), at(1, 0), None, None).check().is_err());
        assert!(days(Days::Every(0), at(1, 0), Some(date(2026, 1, 1)), None)
            .check()
            .is_err());
        assert!(days(Days::Weekly(vec![]), at(1, 0), None, None)
            .check()
            .is_err());
        let every = |minutes: u64, from: i8, until: i8| Times::Every {
            interval: Duration::from_secs(minutes * 60),
            from: Time::new(from, 0, 0, 0).unwrap(),
            until: Time::new(until, 0, 0, 0).unwrap(),
        };
        assert!(days(Days::Daily, every(0, 1, 2), None, None)
            .check()
            .is_err());
        assert!(days(Days::Daily, every(5, 3, 2), None, None)
            .check()
            .is_err());
        assert!(days(Days::Daily, every(5, 2, 3), None, None)
            .check()
            .is_ok());
        assert!(days(
            Days::Daily,
            at(1, 0),
            Some(date(2026, 2, 1)),
            Some(date(2026, 1, 1))
        )
        .check()
        .is_err());
    }
}
