//! Dates, times and timestamps, `Draco::DateTime` (spec
//! `docs/re/calc-values.md` 4).
//!
//! A date is a day number (1 = 1/1/0001, proleptic Gregorian, through
//! 12/31/4000 = 1460970); a time is signed elapsed seconds with microseconds;
//! a timestamp is seconds since 1/1/0001 00:00:00. Fractional seconds are
//! truncated toward zero to microseconds (4.1). Values parsed from text keep
//! the details that survive for display (4.5): a date's separator and a
//! time's style.

mod format;
mod parse;

pub use parse::type_change_text;

pub(crate) use parse::loose_date_fields;

use crate::number::Number;
use std::cmp::Ordering;

/// The last day: 12/31/4000.
pub const MAX_DAY: i64 = 1_460_970;
const MICROS_PER_SECOND: i128 = 1_000_000;
const SECONDS_PER_DAY: i128 = 86_400;
/// The last timestamp: 12/31/4000 23:59:59.999999, in microseconds.
pub const MAX_TIMESTAMP_MICROS: i128 = MAX_DAY as i128 * SECONDS_PER_DAY * MICROS_PER_SECOND - 1;
/// Day number of 1/1/1970.
const UNIX_EPOCH_DAY: i64 = 719_163;
/// Times hold their hours in 32 bits (`DateTime::mHour`): a time of 2^31
/// hours or more is error 501 (`Time(2147483648;0;0)`, `GetAsTime(10^20)`,
/// `Time(2147483647;0;0) + 3600`).
const MAX_TIME_MICROS: i128 = (1 << 31) * 3_600 * MICROS_PER_SECOND;

/// How a time displays (`DateTime::mTimeStyle`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TimeStyle {
    /// `h:mm:ss`, hours unbounded, seconds always shown: times from `Time()`,
    /// arithmetic, numbers and text without AM/PM.
    #[default]
    Elapsed,
    /// The locale's time of day, zero seconds omitted (`1:30 PM`, `13:00`):
    /// the time of a timestamp, text with AM/PM.
    TimeOfDay,
    /// A bare hour with AM/PM (`1 PM`).
    HourOnly,
}

/// A date.
#[derive(Clone, Copy, Debug)]
pub struct Date {
    day: u32,
    sep: Option<char>,
}

/// A time (elapsed seconds, microsecond resolution).
#[derive(Clone, Copy, Debug)]
pub struct Time {
    micros: i128,
    style: TimeStyle,
}

/// A timestamp.
#[derive(Clone, Copy, Debug)]
pub struct Timestamp {
    micros: i128,
    style: TimeStyle,
    sep: Option<char>,
}

/// Days since 1/1/1970 of a proleptic Gregorian date (month 1..=12).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a count of days since 1/1/1970.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub(crate) fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

pub(crate) fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The proleptic Gregorian year of a day number of any size (`YearName`
/// reads numbers past 12/31/4000 as dates: `YearName(1460971;0)` is year
/// 4001).
pub(crate) fn year_of_day(day: i64) -> i64 {
    civil_from_days(day - UNIX_EPOCH_DAY).0
}

/// The day number of any proleptic Gregorian date, in range or not (1/4/0000
/// is -362).
pub(crate) fn day_number_of(year: i64, month: u32, day: u32) -> i64 {
    days_from_civil(year, month, day) + UNIX_EPOCH_DAY
}

impl Date {
    /// The date of a day number, if in 1..=1460970.
    pub fn from_day(day: i64) -> Option<Date> {
        (1..=MAX_DAY).contains(&day).then_some(Date {
            day: day as u32,
            sep: None,
        })
    }

    /// A valid calendar date with a year in 1..=4000.
    pub fn from_ymd(year: i64, month: u32, day: u32) -> Option<Date> {
        if !(1..=4000).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
        {
            return None;
        }
        Date::from_day(days_from_civil(year, month, day) + UNIX_EPOCH_DAY)
    }

    /// `Date(month; day; year)` with overflowing and negative components
    /// rolled over (`DateTime::SetNormalizedDate`, spec 4.2):
    /// `Date(2;29;2023)` = 3/1/2023, `Date(0;1;2020)` = 12/1/2019,
    /// `Date(1;400;2020)` = 2/3/2021. `None` when the result is outside
    /// 1/1/0001..12/31/4000 (error 500).
    pub fn normalized(month: i64, day: i64, year: i64) -> Option<Date> {
        let months = year.checked_mul(12)?.checked_add(month.checked_sub(1)?)?;
        let y = months.div_euclid(12);
        let m = (months.rem_euclid(12) + 1) as u32;
        if !(-1_000_000..=1_000_000).contains(&y) {
            return None;
        }
        let first = days_from_civil(y, m, 1) + UNIX_EPOCH_DAY;
        Date::from_day(first.checked_add(day.checked_sub(1)?)?)
    }

    /// The day number.
    pub fn day_number(self) -> i64 {
        i64::from(self.day)
    }

    /// (year, month, day).
    pub fn ymd(self) -> (i64, u32, u32) {
        civil_from_days(self.day_number() - UNIX_EPOCH_DAY)
    }

    /// The day of the year, 1 for January 1.
    pub fn day_of_year(self) -> u32 {
        let year = self.ymd().0;
        let first = days_from_civil(year, 1, 1) + UNIX_EPOCH_DAY;
        (self.day_number() - first + 1) as u32
    }

    /// 1 for Sunday through 7 for Saturday (1/1/0001 is a Monday).
    pub fn weekday(self) -> u32 {
        (self.day % 7) + 1
    }

    /// The separator the date was typed with, if it came from text.
    pub fn separator(self) -> Option<char> {
        self.sep
    }

    /// The same day displayed with `sep` (or the locale's for `None`).
    pub fn with_separator(self, sep: Option<char>) -> Date {
        Date { sep, ..self }
    }

    /// The day number as a number.
    pub fn to_number(self) -> Number {
        Number::from_i64(self.day_number())
    }

    /// The date of a number of days: truncated toward zero, `None` outside
    /// the range (spec 4.4: `GetAsDate(1.9)` = 1/1/0001, `GetAsDate(0)` is
    /// error 500).
    pub fn from_number(n: &Number) -> Option<Date> {
        Date::from_day(n.int().to_i64()?)
    }

    /// Midnight of this day as a timestamp.
    pub fn to_timestamp(self) -> Timestamp {
        Timestamp {
            micros: i128::from(self.day_number() - 1) * SECONDS_PER_DAY * MICROS_PER_SECOND,
            style: TimeStyle::TimeOfDay,
            sep: self.sep,
        }
    }
}

impl PartialEq for Date {
    fn eq(&self, other: &Date) -> bool {
        self.day == other.day
    }
}

impl Eq for Date {}

impl PartialOrd for Date {
    fn partial_cmp(&self, other: &Date) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Date {
    fn cmp(&self, other: &Date) -> Ordering {
        self.day.cmp(&other.day)
    }
}

/// Seconds (exact decimal) to microseconds, truncated toward zero.
pub(crate) fn micros_of(seconds: &Number) -> Option<i128> {
    seconds.to_scaled_i128(6)
}

/// Microseconds to seconds as a number.
pub(crate) fn seconds_of(micros: i128) -> Number {
    Number::from_scaled_i128(micros, 6)
}

impl Time {
    pub fn from_micros(micros: i128, style: TimeStyle) -> Time {
        Time { micros, style }
    }

    /// The elapsed time of a number of seconds (spec 4.4:
    /// `GetAsTime(100000)` = 27:46:40); `None` for `?` or 2^31 hours or
    /// more either way (`GetAsTime(-7730941132800)` is error 501).
    pub fn from_seconds(seconds: &Number) -> Option<Time> {
        let micros = micros_of(seconds)?;
        (micros.abs() < MAX_TIME_MICROS).then(|| Time::from_micros(micros, TimeStyle::Elapsed))
    }

    /// `Time(hours; minutes; seconds)`: overflow and negatives allowed
    /// (`Time(-1;30;0)` = -0:30:00, `Time(1.5;0;0)` = 1:30:00). The hours
    /// and minutes count in whole seconds: `hours * 3600 + minutes * 60` is
    /// truncated toward zero before the seconds, fraction and all, are
    /// added (`Time(1/3;0;0)` = 0:19:59, `Time(0;1/7;0.5)` = 0:00:08.5).
    /// `None` outside -2^31..2^31 hours; unlike other times, exactly -2^31
    /// hours is allowed (`Time(-2147483648;0;-1)`).
    pub fn from_hms(hours: &Number, minutes: &Number, seconds: &Number) -> Option<Time> {
        let p = crate::number::MAX_PRECISION;
        let whole = hours
            .mul(&Number::from_i64(3600), p)
            .add(&minutes.mul(&Number::from_i64(60), p), p)
            .int();
        let micros = micros_of(&whole.add(seconds, p))?;
        let hour = 3_600 * MICROS_PER_SECOND;
        (micros > -MAX_TIME_MICROS - hour && micros < MAX_TIME_MICROS)
            .then(|| Time::from_micros(micros, TimeStyle::Elapsed))
    }

    /// Whole hours, minutes and seconds of the elapsed time, each with the
    /// time's sign (`Hour`, `Minute`, `Seconds`; `-1:30:05` is -1, -30 and
    /// -5), the seconds with their fraction.
    pub fn hms(self) -> (i64, i64, Number) {
        let minute = 60 * MICROS_PER_SECOND;
        let hours = self.micros / (60 * minute);
        let minutes = self.micros / minute % 60;
        (
            hours as i64,
            minutes as i64,
            seconds_of(self.micros % minute),
        )
    }

    pub fn micros(self) -> i128 {
        self.micros
    }

    pub fn style(self) -> TimeStyle {
        self.style
    }

    pub fn with_style(self, style: TimeStyle) -> Time {
        Time { style, ..self }
    }

    /// Seconds as a number.
    pub fn to_number(self) -> Number {
        seconds_of(self.micros)
    }

    /// `1/1/0001` plus this time, if in range (`GetAsTimestamp(Time(1;0;0))`
    /// = 1/1/0001 1:00 AM; a time keeps its style when a declared result
    /// converts it).
    pub fn to_timestamp(self, style: TimeStyle) -> Option<Timestamp> {
        Timestamp::from_micros(self.micros, style)
    }
}

impl PartialEq for Time {
    fn eq(&self, other: &Time) -> bool {
        self.micros == other.micros
    }
}

impl Eq for Time {}

impl Timestamp {
    /// A timestamp of microseconds since 1/1/0001, if in range.
    pub fn from_micros(micros: i128, style: TimeStyle) -> Option<Timestamp> {
        (0..=MAX_TIMESTAMP_MICROS)
            .contains(&micros)
            .then_some(Timestamp {
                micros,
                style,
                sep: None,
            })
    }

    /// `Timestamp(date; time)`: the time added to midnight of the date, so a
    /// time beyond 24 hours rolls the date (`Timestamp(Date(1;1;2001);
    /// Time(25;0;0))` = 1/2/2001 1:00 AM); `None` out of range.
    pub fn from_date_time(date: Date, time: Time) -> Option<Timestamp> {
        let midnight = date.to_timestamp().micros;
        let mut ts = Timestamp::from_micros(midnight + time.micros, TimeStyle::TimeOfDay)?;
        ts.sep = date.sep;
        Some(ts)
    }

    /// The timestamp of a number of seconds since 1/1/0001.
    pub fn from_seconds(seconds: &Number) -> Option<Timestamp> {
        Timestamp::from_micros(micros_of(seconds)?, TimeStyle::TimeOfDay)
    }

    pub fn micros(self) -> i128 {
        self.micros
    }

    pub fn style(self) -> TimeStyle {
        self.style
    }

    pub fn separator(self) -> Option<char> {
        self.sep
    }

    pub fn with_separator(self, sep: Option<char>) -> Timestamp {
        Timestamp { sep, ..self }
    }

    /// Seconds since 1/1/0001 as a number.
    pub fn to_number(self) -> Number {
        seconds_of(self.micros)
    }

    /// The date part.
    pub fn date(self) -> Date {
        let day = self.micros.div_euclid(SECONDS_PER_DAY * MICROS_PER_SECOND) + 1;
        Date {
            day: day as u32,
            sep: self.sep,
        }
    }

    /// The time of day (spec 4.4: displayed in the locale's time-of-day
    /// format).
    pub fn time_of_day(self) -> Time {
        Time::from_micros(
            self.micros.rem_euclid(SECONDS_PER_DAY * MICROS_PER_SECOND),
            TimeStyle::TimeOfDay,
        )
    }
}

impl PartialEq for Timestamp {
    fn eq(&self, other: &Timestamp) -> bool {
        self.micros == other.micros
    }
}

impl Eq for Timestamp {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_numbers() {
        assert_eq!(Date::from_ymd(1, 1, 1).unwrap().day_number(), 1);
        assert_eq!(Date::from_ymd(2001, 1, 1).unwrap().day_number(), 730_486);
        assert_eq!(Date::from_ymd(1970, 1, 1).unwrap().day_number(), 719_163);
        assert_eq!(Date::from_ymd(4000, 12, 31).unwrap().day_number(), MAX_DAY);
        assert!(Date::from_ymd(4001, 1, 1).is_none());
        assert!(Date::from_ymd(2023, 2, 29).is_none());
        let a = Date::from_ymd(1600, 1, 1).unwrap().day_number();
        let b = Date::from_ymd(1500, 1, 1).unwrap().day_number();
        assert_eq!(a - b, 36_524);
        assert_eq!(Date::from_day(730_486).unwrap().ymd(), (2001, 1, 1));
        assert_eq!(Date::from_ymd(1, 1, 1).unwrap().weekday(), 2);
        assert_eq!(Date::from_ymd(1582, 10, 15).unwrap().weekday(), 6);
    }

    #[test]
    fn normalization() {
        let n = |m, d, y| Date::normalized(m, d, y).map(|d| d.ymd());
        assert_eq!(n(2, 29, 2023), Some((2023, 3, 1)));
        assert_eq!(n(13, 1, 2020), Some((2021, 1, 1)));
        assert_eq!(n(0, 1, 2020), Some((2019, 12, 1)));
        assert_eq!(n(-1, 1, 2020), Some((2019, 11, 1)));
        assert_eq!(n(1, 0, 2020), Some((2019, 12, 31)));
        assert_eq!(n(1, 400, 2020), Some((2021, 2, 3)));
        assert_eq!(n(1, 1, 0), None);
        assert_eq!(n(1, 1, 4001), None);
    }

    #[test]
    fn timestamps() {
        let d = Date::from_ymd(2001, 1, 1).unwrap();
        let ts = d.to_timestamp();
        assert_eq!(ts.micros(), 63_113_904_000 * 1_000_000);
        let t = Time::from_seconds(&Number::from_i64(25 * 3600)).unwrap();
        let rolled = Timestamp::from_date_time(d, t).unwrap();
        assert_eq!(rolled.date().ymd(), (2001, 1, 2));
        assert_eq!(rolled.time_of_day().micros(), 3600 * 1_000_000);
        assert!(Timestamp::from_micros(-1, TimeStyle::TimeOfDay).is_none());
    }

    #[test]
    fn time_construction() {
        let n = |s: &str| match s.strip_prefix('-') {
            Some(abs) => Number::from_literal(abs, '.').unwrap().neg(),
            None => Number::from_literal(s, '.').unwrap(),
        };
        let third = Number::from_i64(1)
            .div(&Number::from_i64(3), crate::number::DEFAULT_PRECISION)
            .unwrap();
        let zero = Number::zero();
        let hms = |h: &Number, m: &Number, s: &Number| Time::from_hms(h, m, s).map(Time::micros);
        // Hours and minutes count in whole seconds, seconds keep fractions.
        assert_eq!(hms(&third, &zero, &zero), Some(1_199_000_000));
        assert_eq!(hms(&third, &third, &zero), Some(1_219_000_000));
        assert_eq!(hms(&zero, &third, &third), Some(19_333_333));
        assert_eq!(hms(&n("-0.5"), &zero, &n("0.25")), Some(-1_799_750_000));
        assert_eq!(hms(&zero, &n("-0.01"), &n("0.5")), Some(500_000));
        // 32-bit hours.
        let max = n("2147483647");
        assert!(hms(&max, &n("59"), &n("59.999999")).is_some());
        assert!(hms(&max, &n("60"), &zero).is_none());
        assert!(hms(&n("-2147483648"), &zero, &n("-1")).is_some());
        assert!(hms(&n("-2147483649"), &zero, &zero).is_none());
        assert!(Time::from_seconds(&n("7730941132799.999999")).is_some());
        assert!(Time::from_seconds(&n("7730941132800")).is_none());
        assert!(Time::from_seconds(&n("-7730941132800")).is_none());
        let t = Time::from_hms(&n("-1"), &n("-30"), &n("-5.25")).unwrap();
        let (h, m, s) = t.hms();
        assert_eq!((h, m, s.to_text_with('.')), (-1, -30, "-5.25".to_string()));
    }

    #[test]
    fn calendar_helpers() {
        assert_eq!(Date::from_ymd(2024, 12, 31).unwrap().day_of_year(), 366);
        assert_eq!(Date::from_ymd(1900, 3, 1).unwrap().day_of_year(), 60);
        assert_eq!(year_of_day(MAX_DAY), 4000);
        assert_eq!(year_of_day(MAX_DAY + 1), 4001);
    }
}
