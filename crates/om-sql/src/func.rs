//! The scalar functions (execute-sql.md 5.2): their signatures, checked
//! when the statement is built, and their values, which are the
//! calculation functions' (om-calc) wherever FQL builds them as
//! calculation operators.

use om_calc::functions::{self, Implementation};
use om_calc::{CalcError, Ctx, Date, Number, Time, TimeStyle, Timestamp, Value};

use crate::error::SqlError;
use crate::value::{Ty, Val};

/// A scalar function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    Abs,
    Atan,
    Atan2,
    Ceiling,
    Cos,
    Degrees,
    Exp,
    Floor,
    Int,
    Lg,
    Ln,
    Log,
    Mod,
    Pi,
    Radians,
    Round,
    Sign,
    Sin,
    Sqrt,
    Tan,
    Max,
    Min,
    Chr,
    Left,
    Right,
    Substr,
    Length,
    Upper,
    Lower,
    Ltrim,
    Rtrim,
    Space,
    StrVal,
    NumVal,
    DateVal,
    TimeVal,
    TimestampVal,
    Day,
    Month,
    Year,
    DayOfWeek,
    Hour,
    Minute,
    Second,
    DayName,
    MonthName,
    CurrentDate,
    CurrentTime,
    CurrentTimestamp,
    User,
    Part,
}

/// What a parameter accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    Number,
    Text,
    /// Any value (`STRVAL`).
    Any,
    /// Text or a number (`NUMVAL`).
    TextOrNumber,
    /// Text, a number or a date, time or timestamp (`DATEVAL`,
    /// `TIMEVAL`, `TIMESTAMPVAL`).
    Convertible,
    /// A date or a timestamp (`DAY`, `MONTH`, `YEAR`, `DAYOFWEEK`,
    /// `DAYNAME`, `MONTHNAME`).
    DateLike,
    /// A time or a timestamp (`HOUR`, `MINUTE`, `SECOND`).
    TimeLike,
}

impl Func {
    /// The function a keyword or `FunctionTable` name stands for.
    pub fn by_name(name: &str) -> Option<Func> {
        use Func::*;
        let upper = name.to_ascii_uppercase();
        Some(match upper.as_str() {
            "ABS" => Abs,
            "ATAN" => Atan,
            "ATAN2" => Atan2,
            "CEIL" | "CEILING" => Ceiling,
            "COS" => Cos,
            "DEGREES" | "DEG" => Degrees,
            "EXP" => Exp,
            "FLOOR" => Floor,
            "INT" => Int,
            "LG" => Lg,
            "LN" => Ln,
            "LOG" => Log,
            "MOD" => Mod,
            "PI" => Pi,
            "RADIANS" => Radians,
            "ROUND" => Round,
            "SIGN" => Sign,
            "SIN" => Sin,
            "SQRT" => Sqrt,
            "TAN" => Tan,
            "MAX" => Max,
            "MIN" => Min,
            "CHR" => Chr,
            "LEFT" => Left,
            "RIGHT" => Right,
            "SUBSTR" | "SUBSTRING" => Substr,
            "LENGTH" => Length,
            "UPPER" => Upper,
            "LOWER" => Lower,
            "LTRIM" => Ltrim,
            "RTRIM" => Rtrim,
            "SPACE" => Space,
            "STRVAL" => StrVal,
            "NUMVAL" => NumVal,
            "DATEVAL" => DateVal,
            "TIMEVAL" => TimeVal,
            "TIMESTAMPVAL" => TimestampVal,
            "DAY" => Day,
            "MONTH" => Month,
            "YEAR" => Year,
            "DAYOFWEEK" => DayOfWeek,
            "HOUR" => Hour,
            "MINUTE" => Minute,
            "SECOND" => Second,
            "DAYNAME" => DayName,
            "MONTHNAME" => MonthName,
            "CURRENT_DATE" | "CURDATE" | "TODAY" | "DATE" => CurrentDate,
            "CURRENT_TIME" | "CURTIME" | "TIME" => CurrentTime,
            "CURRENT_TIMESTAMP" | "CURTIMESTAMP" | "TIMESTAMP" => CurrentTimestamp,
            "USER" | "USERNAME" | "CURRENT_USER" => User,
            "PART" => Part,
            _ => return None,
        })
    }

    /// Whether `name` may be called as `IDENTIFIER(...)`: the 20 entries of
    /// FQL's `FunctionTable`.
    pub fn in_function_table(name: &str) -> bool {
        const TABLE: &[&str] = &[
            "ABS", "ATAN", "ATAN2", "CEIL", "CEILING", "COS", "DEGREES", "DEG", "EXP", "FLOOR",
            "LG", "LN", "LOG", "MOD", "PI", "RADIANS", "SIGN", "SIN", "SQRT", "TAN",
        ];
        TABLE.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    /// The parameters: required ones, then optional ones.
    pub fn params(self) -> (&'static [Param], &'static [Param]) {
        use Func::*;
        use Param as P;
        match self {
            Abs | Atan | Ceiling | Cos | Degrees | Exp | Floor | Int | Lg | Ln | Log | Radians
            | Sign | Sin | Sqrt | Tan => (&[P::Number], &[]),
            Atan2 | Mod | Round | Max | Min => (&[P::Number, P::Number], &[]),
            Pi | CurrentDate | CurrentTime | CurrentTimestamp | User => (&[], &[]),
            Chr | Space => (&[P::Number], &[]),
            Left | Right => (&[P::Text, P::Number], &[]),
            Substr => (&[P::Text, P::Number], &[P::Number]),
            Length | Upper | Lower | Ltrim | Rtrim => (&[P::Text], &[]),
            NumVal => (&[P::TextOrNumber], &[]),
            StrVal => (&[P::Any], &[]),
            DateVal | TimeVal | TimestampVal => (&[P::Convertible], &[]),
            Day | Month | Year | DayOfWeek | DayName | MonthName => (&[P::DateLike], &[]),
            Hour | Minute | Second => (&[P::TimeLike], &[]),
            Part => (&[P::Text, P::Text], &[]),
        }
    }

    pub fn result(self) -> Ty {
        use Func::*;
        match self {
            Chr | Left | Right | Substr | Upper | Lower | Ltrim | Rtrim | Space | StrVal
            | DayName | MonthName | User => Ty::Text,
            DateVal | CurrentDate => Ty::Date,
            TimeVal | CurrentTime => Ty::Time,
            TimestampVal | CurrentTimestamp => Ty::Timestamp,
            _ => Ty::Number,
        }
    }
}

/// Whether an argument of type `ty` fits `param` (5.2: the untyped `NULL`
/// fails the numeric and date parameters, `?` fits any).
pub fn accepts(param: Param, ty: Ty) -> bool {
    match (param, ty) {
        (_, Ty::Param) => true,
        (Param::Any, _) => true,
        (Param::Text, Ty::Text | Ty::Null) => true,
        (Param::Number, Ty::Number | Ty::Bool) => true,
        (Param::TextOrNumber, Ty::Text | Ty::Number | Ty::Null) => true,
        (Param::Convertible, t) => !matches!(t, Ty::Container | Ty::Bool),
        (Param::DateLike, Ty::Date | Ty::Timestamp) => true,
        (Param::TimeLike, Ty::Time | Ty::Timestamp) => true,
        _ => false,
    }
}

fn library(name: &str, args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    match functions::lookup(name).map(|f| f.implementation) {
        Some(Implementation::Pure(f)) => f(args, cx),
        Some(Implementation::Aggregate(f)) => {
            let lists: Vec<Vec<Value>> = args.iter().map(|a| vec![a.clone()]).collect();
            f(&lists, cx)
        }
        None => Err(CalcError::NOT_SUPPORTED),
    }
}

/// The parameter's value as a calculation value of the type the function
/// takes (5.7: a `?` converts like the calculation converts).
fn arg_value(v: &Val, param: Param, cx: &Ctx) -> Value {
    let calc = v.to_calc();
    match (param, v) {
        (Param::Number, Val::Param(_)) => Value::Number(calc.to_number(cx)),
        (Param::Text, Val::Param(_)) => Value::Text(calc.to_text(cx)),
        _ => calc,
    }
}

/// Evaluates a function on its arguments; `now` and `user` serve the clock
/// and user functions. A NULL argument gives NULL, except for `STRVAL`.
pub fn call(f: Func, args: &[Val], cx: &Ctx, now: Timestamp, user: &str) -> Result<Val, SqlError> {
    use Func::*;
    let (required, optional) = f.params();
    let params: Vec<Param> = required.iter().chain(optional).copied().collect();
    if f != StrVal && args.iter().any(Val::is_null) {
        return Ok(Val::Null);
    }
    let a: Vec<Value> = args
        .iter()
        .zip(&params)
        .map(|(v, p)| arg_value(v, *p, cx))
        .collect();
    let number = |v: Result<Value, CalcError>| -> Result<Val, SqlError> { Ok(Val::from_calc(v?)) };
    let text_result = |v: Result<Value, CalcError>| -> Result<Val, SqlError> {
        let v = v?;
        Ok(Val::text(v.to_text(cx).into_string()))
    };
    match f {
        Abs => number(library("Abs", &a, cx)),
        Atan => number(library("Atan", &a, cx)),
        Ceiling => number(library("Ceiling", &a, cx)),
        Cos => number(library("Cos", &a, cx)),
        Degrees => number(library("Degrees", &a, cx)),
        Exp => number(library("Exp", &a, cx)),
        Floor => number(library("Floor", &a, cx)),
        Int => number(library("Int", &a, cx)),
        Lg => number(library("Lg", &a, cx)),
        Ln => number(library("Ln", &a, cx)),
        Log => number(library("Log", &a, cx)),
        Mod => number(library("Mod", &a, cx)),
        Pi => number(library("Pi", &a, cx)),
        Radians => number(library("Radians", &a, cx)),
        Sign => number(library("Sign", &a, cx)),
        Sin => number(library("Sin", &a, cx)),
        Sqrt => number(library("Sqrt", &a, cx)),
        Tan => number(library("Tan", &a, cx)),
        Round => {
            let digits = Value::Number(a[1].to_number(cx).int());
            number(library("Round", &[a[0].clone(), digits], cx))
        }
        Atan2 => {
            let y = to_f64(&a[0].to_number(cx));
            let x = to_f64(&a[1].to_number(cx));
            Ok(Val::number(Number::from_f64(y.atan2(x), cx.precision)))
        }
        Max | Min => {
            let (x, y) = (a[0].to_number(cx), a[1].to_number(cx));
            let pick_first = match x.compare(&y) {
                Some(std::cmp::Ordering::Less) => f == Min,
                Some(std::cmp::Ordering::Greater) => f == Max,
                _ => true,
            };
            Ok(Val::number(if pick_first { x } else { y }))
        }
        Chr => {
            let n = a[0].to_number(cx).int().as_long_long();
            let code = (n.unsigned_abs() % 256) as u32;
            Ok(Val::text(char::from_u32(code).unwrap_or('\0').to_string()))
        }
        Left | Right => {
            if a[1].to_number(cx).is_negative() {
                return Err(SqlError::Engine(14));
            }
            text_result(library(if f == Left { "Left" } else { "Right" }, &a, cx))
        }
        Substr => {
            if a.get(2).is_some_and(|n| n.to_number(cx).is_negative()) {
                return Err(SqlError::Engine(14));
            }
            let mut args = a.clone();
            if args.len() == 2 {
                // To the end.
                let len = Value::text(a[0].to_text(cx).as_str());
                args.push(library("Length", &[len], cx)?);
            }
            text_result(library("Middle", &args, cx))
        }
        Length => number(library("Length", &a, cx)),
        Upper => text_result(library("Upper", &a, cx)),
        Lower => text_result(library("Lower", &a, cx)),
        Ltrim => Ok(Val::text(a[0].to_text(cx).as_str().trim_start_matches(' '))),
        Rtrim => Ok(Val::text(a[0].to_text(cx).as_str().trim_end_matches(' '))),
        Space => {
            let n = a[0].to_number(cx).int().as_long_long().max(0) as usize;
            Ok(Val::text(" ".repeat(n)))
        }
        StrVal => Ok(Val::text(args[0].strval(cx))),
        NumVal => match &args[0] {
            Val::Number(n) => Ok(Val::Number(n.clone())),
            _ => numval(&a[0].to_text(cx).into_string(), cx),
        },
        DateVal => dateval(&args[0]),
        TimeVal => timeval(&args[0]),
        TimestampVal => timestampval(&args[0]),
        Day => number(library("Day", &a, cx)),
        Month => number(library("Month", &a, cx)),
        Year => number(library("Year", &a, cx)),
        DayOfWeek => number(library("DayOfWeek", &a, cx)),
        Hour => number(library("Hour", &a, cx)),
        Minute => number(library("Minute", &a, cx)),
        Second => number(library("Seconds", &a, cx)),
        DayName => text_result(library("DayName", &a, cx)),
        MonthName => text_result(library("MonthName", &a, cx)),
        CurrentDate => Ok(Val::Date(now.date())),
        CurrentTime => Ok(Val::Time(whole_seconds(now.time_of_day()))),
        CurrentTimestamp => {
            let t = whole_seconds(now.time_of_day());
            Ok(Timestamp::from_date_time(now.date(), t).map_or(Val::Null, Val::Timestamp))
        }
        User => Ok(Val::text(user)),
        Part => {
            let _ = (a[0].to_text(cx), a[1].to_text(cx));
            Ok(Val::Number(Number::zero()))
        }
    }
}

fn whole_seconds(t: Time) -> Time {
    let micros = t.micros() - t.micros() % 1_000_000;
    Time::from_micros(micros, TimeStyle::Elapsed)
}

pub fn to_f64(n: &Number) -> f64 {
    n.to_text_with('.').parse().unwrap_or(f64::NAN)
}

/// `NUMVAL` and text-to-number `CAST`: text in the file's locale; error 502
/// for text that is not a number (5.2).
pub fn numval(text: &str, cx: &Ctx) -> Result<Val, SqlError> {
    if text.is_empty() {
        return Ok(Val::Null);
    }
    let n = om_calc::value::parse_number(text, cx.locale);
    if n.is_empty() || n.is_nan() || !is_numeric_text(text, cx) {
        return Err(SqlError::Engine(502));
    }
    Ok(Val::Number(n))
}

/// Text that is a number: an optional sign, digits with the locale's
/// thousands separators, an optional decimal part and exponent, with spaces
/// around.
fn is_numeric_text(text: &str, cx: &Ctx) -> bool {
    let t = text.trim_matches(' ');
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    let mut digits = 0;
    let mut seen_point = false;
    let mut chars = t.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            digits += 1;
        } else if c == cx.locale.decimal_separator && !seen_point {
            seen_point = true;
        } else if c == cx.locale.thousands_separator && !seen_point && digits > 0 {
        } else if c == 'e' || c == 'E' {
            chars.next();
            if matches!(chars.peek(), Some('+' | '-')) {
                chars.next();
            }
            return digits > 0 && chars.clone().count() > 0 && chars.all(|c| c.is_ascii_digit());
        } else {
            return false;
        }
        chars.next();
    }
    digits > 0
}

/// ISO date text: `Y-M-D` or `Y/M/D`, one- or two-digit month and day, a
/// four-digit year 1-4000, spaces around (5.4).
pub fn parse_iso_date(text: &str) -> Option<Date> {
    let t = text.trim_matches(' ');
    let sep = if t.contains('-') { '-' } else { '/' };
    let parts: Vec<&str> = t.split(sep).collect();
    let [y, m, d] = parts[..] else {
        return None;
    };
    let all_digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if !(y.len() == 4 && all_digits(y) && (1..=2).contains(&m.len()) && all_digits(m))
        || !((1..=2).contains(&d.len()) && all_digits(d))
    {
        return None;
    }
    let (y, m, d): (i64, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=4000).contains(&y) {
        return None;
    }
    Date::from_ymd(y, m, d).map(|date| date.with_separator(Some(sep)))
}

/// ISO time text `H:M[:S[.f]]` with an optional `AM`/`PM`: parts carry,
/// hours may exceed 23 or be negative, fractions keep six digits.
pub fn parse_iso_time(text: &str) -> Option<Time> {
    let mut t = text.trim_matches(' ').to_string();
    let mut meridiem = None;
    for (suffix, pm) in [("AM", false), ("PM", true)] {
        if t.len() >= 2 && t[t.len() - 2..].eq_ignore_ascii_case(suffix) {
            t.truncate(t.len() - 2);
            t = t.trim_end_matches(' ').to_string();
            meridiem = Some(pm);
        }
    }
    let negative = t.starts_with('-');
    let body = t.trim_start_matches('-');
    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let int = |s: &str| -> Option<i128> {
        (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).then(|| s.parse().ok())?
    };
    let mut hours = int(parts[0])?;
    let minutes = int(parts[1])?;
    let mut micros: i128 = 0;
    if let Some(sec) = parts.get(2) {
        let (whole, frac) = match sec.split_once('.') {
            Some((w, f)) => (w, f),
            None => (*sec, ""),
        };
        micros += int(whole)? * 1_000_000;
        if !frac.is_empty() {
            if !frac.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let six: String = frac.chars().chain(std::iter::repeat('0')).take(6).collect();
            micros += six.parse::<i128>().ok()?;
        }
    }
    if let Some(pm) = meridiem {
        if hours == 12 {
            hours = 0;
        }
        if pm {
            hours += 12;
        }
    }
    let total = (hours * 3600 + minutes * 60) * 1_000_000 + micros;
    Some(Time::from_micros(
        if negative { -total } else { total },
        TimeStyle::Elapsed,
    ))
}

/// ISO timestamp text `Y-M-D H:M[:S[.f]]`.
pub fn parse_iso_timestamp(text: &str) -> Option<Timestamp> {
    let t = text.trim_matches(' ');
    let (date, time) = t.split_once(' ')?;
    let date = parse_iso_date(date)?;
    let time = parse_iso_time(time)?;
    let micros = i128::from(date.day_number() - 1) * 86_400_000_000 + time.micros();
    Timestamp::from_micros(micros, TimeStyle::Elapsed).map(|ts| ts.with_separator(date.separator()))
}

fn dateval(v: &Val) -> Result<Val, SqlError> {
    match v {
        Val::Date(d) => Ok(Val::Date(*d)),
        Val::Timestamp(t) => Ok(Val::Date(t.date())),
        Val::Number(n) => Date::from_number(n)
            .map(Val::Date)
            .ok_or(SqlError::Engine(500)),
        Val::Text(s) => parse_iso_date(s)
            .or_else(|| parse_iso_timestamp(s).map(|t| t.date()))
            .map(Val::Date)
            .ok_or(SqlError::Engine(500)),
        Val::Param(p) => match Val::from_calc(p.clone()) {
            Val::Param(_) => Ok(Val::Null),
            other => dateval(&other),
        },
        _ => Err(SqlError::Engine(500)),
    }
}

fn timeval(v: &Val) -> Result<Val, SqlError> {
    match v {
        Val::Time(t) => Ok(Val::Time(*t)),
        Val::Timestamp(t) => Ok(Val::Time(t.time_of_day().with_style(TimeStyle::Elapsed))),
        Val::Number(n) => Time::from_seconds(n)
            .map(Val::Time)
            .ok_or(SqlError::Engine(501)),
        Val::Text(s) => parse_iso_time(s)
            .map(Val::Time)
            .ok_or(SqlError::Engine(501)),
        Val::Param(p) => match Val::from_calc(p.clone()) {
            Val::Param(_) => Ok(Val::Null),
            other => timeval(&other),
        },
        _ => Err(SqlError::Engine(501)),
    }
}

fn timestampval(v: &Val) -> Result<Val, SqlError> {
    match v {
        Val::Timestamp(t) => Ok(Val::Timestamp(*t)),
        Val::Date(d) => Ok(Val::Timestamp(d.to_timestamp())),
        Val::Number(n) => Timestamp::from_seconds(n)
            .map(Val::Timestamp)
            .ok_or(SqlError::Engine(501)),
        Val::Text(s) => parse_iso_timestamp(s)
            .or_else(|| parse_iso_date(s).map(Date::to_timestamp))
            .map(Val::Timestamp)
            .ok_or(SqlError::Engine(501)),
        Val::Param(p) => match Val::from_calc(p.clone()) {
            Val::Param(_) => Ok(Val::Null),
            other => timestampval(&other),
        },
        _ => Err(SqlError::Engine(501)),
    }
}

/// `TRIM`: repeated occurrences of the first character of `chars` (spaces
/// by default) removed from either end.
pub fn trim(text: &str, chars: Option<&str>, leading: bool, trailing: bool) -> Val {
    let c = chars.and_then(|s| s.chars().next()).unwrap_or(' ');
    let mut s = text;
    if leading {
        s = s.trim_start_matches(c);
    }
    if trailing {
        s = s.trim_end_matches(c);
    }
    Val::text(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{iso_time, iso_timestamp};

    #[test]
    fn iso_literals() {
        assert!(parse_iso_date("2014-02-17").is_some());
        assert!(parse_iso_date(" 2014/2/7 ").is_some());
        for bad in [
            "02/17/2014",
            "20140217",
            "14-02-17",
            "2014-02-30",
            "4001-01-01",
        ] {
            assert!(parse_iso_date(bad).is_none(), "{bad}");
        }
        let time = |s| iso_time(parse_iso_time(s).unwrap().micros());
        assert_eq!(time("13:61:00"), "14:01:00");
        assert_eq!(time("25:00:00"), "25:00:00");
        assert_eq!(time("-1:00:00"), "-01:00:00");
        assert_eq!(time("1:45:10 PM"), "13:45:10");
        assert_eq!(time("13:45:10.1234567"), "13:45:10.123456");
        assert_eq!(
            iso_timestamp(parse_iso_timestamp("2014-02-17 24:00:00").unwrap()),
            "2014-02-18 00:00:00"
        );
        assert!(parse_iso_timestamp("2014-02-17").is_none());
    }
}
