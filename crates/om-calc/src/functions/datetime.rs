//! Date, time and timestamp functions: `Date`, `Day`, `DayName`, `DayOfWeek`,
//! `DayOfYear`, `Month`, `MonthName`, `WeekOfYear`, `WeekOfYearFiscal`,
//! `Year`, `Hour`, `Minute`, `Seconds`, `Time`, `Timestamp`.
//!
//! The date-part functions take their argument as a date: an empty argument
//! comes back as it is (`Day("")` is empty text, `Day(GetAsDate(" "))` an
//! empty date), a time gives an empty time without error (`Day(Time(1;0;0))`),
//! and text must parse strictly, so blank text is error 500 (`Day(" ")`)
//! although `GetAsDate(" ")` is merely empty. The time-part functions read
//! a date as its elapsed time (`Hour(Date(1;2;1))` = 24) and blank text as
//! 0 (`Hour(" ")` = 0).

use super::{Function, Keyword};
use crate::datetime::{day_number_of, loose_date_fields, year_of_day, Date, Time, Timestamp};
use crate::{CalcError, Ctx, Number, Value, ValueType};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("Date", date),
    Function::pure("Day", day),
    Function::pure("DayName", day_name),
    Function::pure("DayOfWeek", day_of_week),
    Function::pure("DayOfYear", day_of_year),
    Function::pure("Month", month),
    Function::pure("MonthName", month_name),
    Function::pure("WeekOfYear", week_of_year),
    Function::pure("WeekOfYearFiscal", week_of_year_fiscal),
    Function::pure("Year", year),
    Function::pure("Hour", hour),
    Function::pure("Minute", minute),
    Function::pure("Seconds", seconds),
    Function::pure("Time", time),
    Function::pure("Timestamp", timestamp),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

/// A date argument, or the value a date-part function returns for an
/// argument that has no date.
pub(super) fn date_arg(v: &Value, cx: &Ctx) -> Result<Result<Date, Value>, CalcError> {
    if v.is_empty() {
        return Ok(Err(v.clone()));
    }
    match v {
        Value::Time(_) => Ok(Err(Value::Empty(ValueType::Time))),
        Value::Text(t) => match Date::parse(t.as_str(), cx.locale, cx.today)? {
            Some(d) => Ok(Ok(d)),
            None => Err(CalcError::INVALID_DATE),
        },
        other => other.to_date(cx)?.map(Ok).ok_or(CalcError::INVALID_DATE),
    }
}

/// A time argument, or the value a time-part function returns for an empty
/// argument.
fn time_arg(v: &Value, cx: &Ctx) -> Result<Result<Time, Value>, CalcError> {
    if v.is_empty() {
        return Ok(Err(v.clone()));
    }
    Ok(Ok(time_of(v, cx)?))
}

/// A value as a time the way `Hour` and `Timestamp` read it: blank text and
/// text that is a date without a time are 0:00 (`Hour("1/2/2003")` = 0,
/// though `GetAsTime("1/2/2003")` is error 501).
fn time_of(v: &Value, cx: &Ctx) -> Result<Time, CalcError> {
    let midnight = Time::from_micros(0, Default::default());
    match v.to_time(cx) {
        Ok(t) => Ok(t.unwrap_or(midnight)),
        Err(e) => match v {
            Value::Text(t)
                if matches!(Date::parse(t.as_str(), cx.locale, cx.today), Ok(Some(_))) =>
            {
                Ok(midnight)
            }
            _ => Err(e),
        },
    }
}

/// Applies `f` to the date argument.
fn of_date(args: &[Value], cx: &Ctx, f: impl FnOnce(Date) -> Value) -> Result<Value, CalcError> {
    Ok(match date_arg(&args[0], cx)? {
        Ok(d) => f(d),
        Err(v) => v,
    })
}

/// Applies `f` to the time argument.
fn of_time(args: &[Value], cx: &Ctx, f: impl FnOnce(Time) -> Value) -> Result<Value, CalcError> {
    Ok(match time_arg(&args[0], cx)? {
        Ok(t) => f(t),
        Err(v) => v,
    })
}

/// `Date(month; day; year)`: components truncated toward zero and rolled
/// over (calc-values.md 4.2). Month and day are 16-bit (`Date(1;32768;2000)`
/// is error 500), a year of 0 is always error 500 (`Date(13;1;0)`), and an
/// empty component is error 500 while text without digits counts as 0.
fn date(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let mut parts = [0i64; 3];
    for (part, v) in parts.iter_mut().zip(args) {
        if v.is_empty() {
            return Err(CalcError::INVALID_DATE);
        }
        *part = v
            .numeric_operand(cx)
            .int()
            .to_i64()
            .ok_or(CalcError::INVALID_DATE)?;
    }
    let [month, day, year] = parts;
    let short = |n: i64| i16::try_from(n).is_ok();
    if !short(month) || !short(day) || year == 0 {
        return Err(CalcError::INVALID_DATE);
    }
    Date::normalized(month, day, year)
        .map(Value::Date)
        .ok_or(CalcError::INVALID_DATE)
}

fn day(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_date(args, cx, |d| Value::number(i64::from(d.ymd().2)))
}

fn month(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_date(args, cx, |d| Value::number(i64::from(d.ymd().1)))
}

fn year(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_date(args, cx, |d| Value::number(d.ymd().0))
}

/// 1 for Sunday through 7 for Saturday, whatever the locale.
fn day_of_week(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_date(args, cx, |d| Value::number(i64::from(d.weekday())))
}

fn day_of_year(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_date(args, cx, |d| Value::number(i64::from(d.day_of_year())))
}

/// The locale's name of the weekday (calc-functions.md 3.7).
fn day_name(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_date(args, cx, |d| {
        Value::text(cx.locale.day_names[d.weekday() as usize - 1].clone())
    })
}

/// The locale's name of the month.
fn month_name(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_date(args, cx, |d| {
        Value::text(cx.locale.month_names[d.ymd().1 as usize - 1].clone())
    })
}

/// Weeks start on Sunday in every locale and week 1 holds January 1, so a
/// year has 53 or 54 partial weeks (calc-functions.md U-22).
fn week_of_year(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_date(args, cx, |d| {
        let jan1 = Date::from_day(d.day_number() - i64::from(d.day_of_year()) + 1)
            .expect("January 1 of a valid date");
        let offset = i64::from(jan1.weekday()) - 1;
        Value::number((i64::from(d.day_of_year()) - 1 + offset) / 7 + 1)
    })
}

/// The week of the fiscal year whose weeks start on `starting_day` (1 for
/// Sunday): week 1 is the first week with four or more days in the year,
/// and earlier days belong to the last week of the year before (52 or 53).
pub(crate) fn fiscal_week(d: Date, starting_day: i64) -> i64 {
    // 1 for Sunday through 7 for Saturday, as `Date::weekday` counts.
    let week_start = |day: i64| day - (day.rem_euclid(7) + 1 - starting_day).rem_euclid(7);
    let start = week_start(d.day_number());
    // The week belongs to the year holding its fourth day; that year's week
    // 1 is the week of its January 4 (proleptic outside 1..=4000).
    let year = year_of_day(start + 3);
    let first = week_start(day_number_of(year, 1, 4));
    (start - first).div_euclid(7) + 1
}

/// `WeekOfYearFiscal(date; startingDay)`: the starting day is truncated and
/// clamped to 1..=7 (`0`, `""`, text and `?` act as 1, `8` and above as 7;
/// calc-functions.md U-14).
fn week_of_year_fiscal(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let start = args[1].numeric_operand(cx).int().to_i64().unwrap_or(0);
    let start = start.clamp(1, 7);
    of_date(args, cx, |d| Value::number(fiscal_week(d, start)))
}

fn hour(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_time(args, cx, |t| Value::number(t.hms().0))
}

fn minute(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_time(args, cx, |t| Value::number(t.hms().1))
}

fn seconds(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    of_time(args, cx, |t| Value::Number(t.hms().2))
}

/// `Time(hours; minutes; seconds)` (see [`Time::from_hms`]); empty and text
/// components count as their number or 0 (`Time("";1;"")` = 0:01:00).
fn time(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let n = |i: usize| {
        let n = args[i].numeric_operand(cx);
        if n.is_empty() {
            Number::zero()
        } else {
            n
        }
    };
    Time::from_hms(&n(0), &n(1), &n(2))
        .map(Value::Time)
        .ok_or(CalcError::INVALID_TIME)
}

/// `Timestamp(date; time)`: the time added to the date, so times beyond a
/// day or negative roll the date. The whole days are added to the day of
/// the month in its 16 bits, wrapping beyond them (`Time(-2000000;0;0)` on
/// 1/1/2001 lands in 1952). An empty time gives an empty timestamp, a `?`
/// time returns the date argument unchanged, and every failure is error
/// 500. Date text that is not a date is read field by field and rolled over
/// (`Timestamp("13/45/2003";0)` = 2/14/2004).
fn timestamp(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let (date_v, time_v) = (&args[0], &args[1]);
    if time_v.is_invalid() {
        return Ok(date_v.clone());
    }
    if time_v.is_empty() || matches!(time_v.to_time(cx), Ok(None)) {
        return Ok(if date_v.is_empty() {
            date_v.clone()
        } else {
            Value::Empty(ValueType::Timestamp)
        });
    }
    let date = timestamp_date(date_v, cx).ok_or(CalcError::INVALID_DATE)?;
    let time = time_of(time_v, cx).map_err(|_| CalcError::INVALID_DATE)?;
    const DAY: i128 = 86_400 * 1_000_000;
    let (year, month, day) = date.ymd();
    let days = time.micros().div_euclid(DAY);
    let day_of_month = (i128::from(day) + days) as i16;
    let date = Date::normalized(i64::from(month), i64::from(day_of_month), year)
        .ok_or(CalcError::INVALID_DATE)?;
    let rest = Time::from_micros(time.micros().rem_euclid(DAY), Default::default());
    Timestamp::from_date_time(date, rest)
        .map(Value::Timestamp)
        .ok_or(CalcError::INVALID_DATE)
}

fn timestamp_date(v: &Value, cx: &Ctx) -> Option<Date> {
    match v {
        Value::Time(_) => None,
        Value::Text(t) => match Date::parse(t.as_str(), cx.locale, cx.today) {
            Ok(d) => d,
            Err(_) => {
                let (y, m, d) = loose_date_fields(t.as_str(), cx.locale, cx.today)?;
                Date::normalized(m, d, y)
            }
        },
        other => other.to_date(cx).ok().flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fiscal_weeks() {
        let w = |y, m, d, s| fiscal_week(Date::from_ymd(y, m, d).unwrap(), s);
        // Claris examples.
        assert_eq!(w(2008, 1, 7, 1), 2);
        assert_eq!(w(2009, 1, 1, 5), 1);
        assert_eq!(w(2009, 1, 2, 1), 53);
        // January 1 2004 is a Thursday.
        assert_eq!(
            (1..=7).map(|s| w(2004, 1, 1, s)).collect::<Vec<_>>(),
            [53, 1, 1, 1, 1, 52, 52]
        );
        assert_eq!(w(2004, 12, 31, 1), 52);
    }
}
