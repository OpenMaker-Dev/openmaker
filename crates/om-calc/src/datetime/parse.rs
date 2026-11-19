//! Text to date, time and timestamp (spec `docs/re/calc-values.md` 4.6 and
//! the oracle answers at its end).
//!
//! Dates are two or three runs of ASCII digits joined by one kind of
//! separator (`/`, `.` or `-`) in the locale's field order; two fields are
//! month and day (day and month in day-first locales) of the current year,
//! except that two fields joined by the decimal separator read as a number
//! and are rejected (`2.1` in en-US). One- and two-digit years are windowed
//! into [current year - 69, current year + 30]; three and four digits are
//! taken literally. Out-of-range fields are rejected, not normalized.
//!
//! Times are `h`, `h:mm`, `h:mm:ss` with an optional fraction on the last
//! field, an optional leading `-`, and the locale's AM/PM markers after
//! them. A bare decimal is seconds, and so is the decimal last field of
//! two (`1:30.5` = 0:01:30.5). Minutes and seconds over 59 carry.
//!
//! Timestamps are a date, whitespace and a strict time of day: minutes and
//! seconds below 60, hours below 24, except that exactly `24:00` is
//! midnight of the next day and then the date's fields roll over instead
//! of being checked (`2/30/2003 24:00` = 3/3/2003 12:00 AM).

use super::{Date, Time, TimeStyle, Timestamp, MICROS_PER_SECOND, SECONDS_PER_DAY};
use crate::error::CalcError;
use crate::locale::{DateOrder, Locale};

fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 18 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Whether `c` can separate the fields of a date: punctuation and symbols
/// of any script (`1,5,2003`, `1;5;2003`, `1・5・2003`, `1年5年2003`, and
/// the no-break space), but not `+`, `:`, other spaces, the zero-width
/// space, digits or letters other than CJK ideographs.
fn is_date_separator(c: char) -> bool {
    match c {
        '+' | ':' | '\u{200B}' => false,
        '\u{00A0}' => true,
        c if c.is_whitespace() => false,
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' => true,
        c => !c.is_alphanumeric(),
    }
}

/// The fields of a date token and its separator.
fn date_fields(token: &str) -> Option<(Vec<&str>, char)> {
    let sep = token.chars().find(|&c| is_date_separator(c))?;
    let fields: Vec<&str> = token.split(sep).collect();
    if !(2..=3).contains(&fields.len()) || fields.iter().any(|f| digits(f).is_none()) {
        return None;
    }
    Some((fields, sep))
}

/// A one- or two-digit year in the window around `today`; three or four
/// digits literally.
fn year_of(field: &str, today: Date) -> Option<i64> {
    let value = digits(field)? as i64;
    match field.len() {
        1 | 2 => {
            let start = today.ymd().0 - 69;
            Some(start + (value - start).rem_euclid(100))
        }
        3 | 4 if value > 0 => Some(value),
        _ => None,
    }
}

/// (year, month, day) as written, before range checks.
fn date_numbers(token: &str, locale: &Locale, today: Date) -> Option<(i64, i64, i64, char)> {
    let (fields, sep) = date_fields(token)?;
    let n = |s: &str| digits(s).map(|v| v as i64);
    match fields.as_slice() {
        [a, b] => {
            if sep == locale.decimal_separator {
                return None;
            }
            let year = today.ymd().0;
            match locale.date_order {
                DateOrder::DayMonthYear => Some((year, n(b)?, n(a)?, sep)),
                _ => Some((year, n(a)?, n(b)?, sep)),
            }
        }
        [a, b, c] => match locale.date_order {
            DateOrder::MonthDayYear => Some((year_of(c, today)?, n(a)?, n(b)?, sep)),
            DateOrder::DayMonthYear => Some((year_of(c, today)?, n(b)?, n(a)?, sep)),
            DateOrder::YearMonthDay => Some((year_of(a, today)?, n(b)?, n(c)?, sep)),
        },
        _ => None,
    }
}

fn strict_date(token: &str, locale: &Locale, today: Date) -> Option<Date> {
    let (y, m, d, sep) = date_numbers(token, locale, today)?;
    let date = Date::from_ymd(y, u32::try_from(m).ok()?, u32::try_from(d).ok()?)?;
    Some(date.with_separator(Some(sep)))
}

/// Strips a trailing AM/PM marker (any case, spaces before it allowed);
/// returns the rest and whether the marker was PM.
fn strip_marker<'a>(s: &'a str, locale: &Locale) -> (&'a str, Option<bool>) {
    let Some((am, pm)) = locale.am_pm() else {
        return (s, None);
    };
    for (marker, is_pm) in [(am, false), (pm, true)] {
        if s.len() >= marker.len() {
            let split = s.len() - marker.len();
            if s.is_char_boundary(split) && s[split..].eq_ignore_ascii_case(marker) {
                return (s[..split].trim_end(), Some(is_pm));
            }
        }
    }
    (s, None)
}

/// Seconds with an optional fraction: `15`, `15.25`, `1.`, `.5`; as
/// (whole seconds, microseconds).
fn seconds_field(s: &str, decimal: char) -> Option<(u64, i128)> {
    match s.split_once(decimal) {
        None => Some((digits(s)?, 0)),
        Some((whole, frac)) => {
            if whole.is_empty() && frac.is_empty() {
                return None;
            }
            let whole = if whole.is_empty() { 0 } else { digits(whole)? };
            if !frac.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let six: String = frac.chars().chain(std::iter::repeat('0')).take(6).collect();
            Some((whole, six.parse::<i128>().ok()?))
        }
    }
}

/// Applies an AM/PM marker to an hour of 0..=12.
fn marked_hour(hour: u64, pm: Option<bool>) -> Option<u64> {
    match pm {
        None => Some(hour),
        Some(_) if hour > 12 => None,
        Some(true) if hour < 12 => Some(hour + 12),
        Some(true) => Some(12),
        Some(false) if hour == 12 => Some(0),
        Some(false) => Some(hour),
    }
}

fn micros(hours: u64, minutes: u64, seconds: u64, frac: i128) -> i128 {
    ((i128::from(hours) * 60 + i128::from(minutes)) * 60 + i128::from(seconds)) * MICROS_PER_SECOND
        + frac
}

/// An elapsed time or a time of day with a marker.
fn elapsed_time(s: &str, locale: &Locale) -> Option<Time> {
    let (body, pm) = strip_marker(s, locale);
    let (negative, body) = match body.strip_prefix('-') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, body),
    };
    if negative && pm.is_some() {
        return None;
    }
    let decimal = locale.decimal_separator;
    let fields: Vec<&str> = body.split(locale.time_separator).collect();
    let (value, style) = match fields.as_slice() {
        [single] if single.contains(decimal) => {
            if pm.is_some() {
                return None;
            }
            let (s, f) = seconds_field(single, decimal)?;
            (micros(0, 0, s, f), TimeStyle::Elapsed)
        }
        [hours] => {
            let h = marked_hour(digits(hours)?, pm)?;
            let style = if pm.is_some() {
                TimeStyle::HourOnly
            } else {
                TimeStyle::Elapsed
            };
            (micros(h, 0, 0, 0), style)
        }
        [minutes, seconds] if seconds.contains(decimal) => {
            if pm.is_some() {
                return None;
            }
            let (s, f) = seconds_field(seconds, decimal)?;
            (micros(0, digits(minutes)?, s, f), TimeStyle::Elapsed)
        }
        [hours, minutes] => (
            micros(marked_hour(digits(hours)?, pm)?, digits(minutes)?, 0, 0),
            if pm.is_some() {
                TimeStyle::TimeOfDay
            } else {
                TimeStyle::Elapsed
            },
        ),
        [hours, minutes, seconds] => {
            let (s, f) = seconds_field(seconds, decimal)?;
            (
                micros(marked_hour(digits(hours)?, pm)?, digits(minutes)?, s, f),
                if pm.is_some() {
                    TimeStyle::TimeOfDay
                } else {
                    TimeStyle::Elapsed
                },
            )
        }
        _ => return None,
    };
    Some(Time::from_micros(
        if negative { -value } else { value },
        style,
    ))
}

/// A strict time of day for a timestamp: microseconds since midnight, and
/// whether it is exactly `24:00`.
fn clock(s: &str, locale: &Locale) -> Option<(i128, bool)> {
    let (body, pm) = strip_marker(s, locale);
    let decimal = locale.decimal_separator;
    let fields: Vec<&str> = body.split(locale.time_separator).collect();
    let (h, m, s, f) = match fields.as_slice() {
        [h, m] => (digits(h)?, digits(m)?, 0, 0),
        [h, m, s] => {
            let (s, f) = seconds_field(s, decimal)?;
            (digits(h)?, digits(m)?, s, f)
        }
        _ => return None,
    };
    if m > 59 || s > 59 {
        return None;
    }
    let h = marked_hour(h, pm)?;
    if h == 24 && m == 0 && s == 0 && f == 0 {
        return Some((0, true));
    }
    (h < 24).then(|| (micros(h, m, s, f), false))
}

/// The first word and the rest; the no-break space separates date fields,
/// not words.
fn split_first_word(s: &str) -> (&str, &str) {
    match s.find(|c: char| c.is_whitespace() && c != '\u{00A0}') {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

/// The timestamp of a date token and a clock text.
fn date_and_clock(token: &str, rest: &str, locale: &Locale, today: Date) -> Option<Timestamp> {
    let (time, next_day) = clock(rest, locale)?;
    let date = if next_day {
        // `24:00` rolls the date's fields over instead of checking them.
        let (y, m, d, sep) = date_numbers(token, locale, today)?;
        Date::normalized(m, d.checked_add(1)?, y)?.with_separator(Some(sep))
    } else {
        strict_date(token, locale, today)?
    };
    let midnight = date.to_timestamp().micros();
    Some(Timestamp::from_micros(midnight + time, TimeStyle::TimeOfDay)?.with_separator(date.sep))
}

/// The (year, month, day) fields of the first word of `text` in the
/// locale's order, before any range check: what `Timestamp` and `YearName`
/// fall back to when the text is not a date (`Timestamp("2/30/2003";0)` =
/// 3/2/2003, `Timestamp("1/2/2003 abc";0)` = 1/2/2003). Month and day wrap
/// to 16 bits like `DateTime`'s fields (`"1/99999/2003"` is day -31073).
pub(crate) fn loose_date_fields(
    text: &str,
    locale: &Locale,
    today: Date,
) -> Option<(i64, i64, i64)> {
    let (token, _) = split_first_word(text.trim());
    let (y, m, d, _) = date_numbers(token, locale, today)?;
    Some((y, i64::from(m as i16), i64::from(d as i16)))
}

impl Date {
    /// `GetAsDate` of text (spec 4.6): `Ok(None)` for blank text, error 500
    /// when the text is not a date. A date followed by a valid time converts
    /// to the date. `today` supplies a missing year and the two-digit-year
    /// window.
    pub fn parse(text: &str, locale: &Locale, today: Date) -> Result<Option<Date>, CalcError> {
        let s = text.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let (token, rest) = split_first_word(s);
        let date = if rest.is_empty() {
            strict_date(token, locale, today)
        } else {
            date_and_clock(token, rest, locale, today).map(Timestamp::date)
        };
        date.map(Some).ok_or(CalcError::INVALID_DATE)
    }
}

impl Time {
    /// `GetAsTime` of text: `Ok(None)` for blank text, error 501 when the
    /// text is not a time. A timestamp's text gives its time of day.
    pub fn parse(text: &str, locale: &Locale, today: Date) -> Result<Option<Time>, CalcError> {
        let s = text.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let (token, rest) = split_first_word(s);
        if !rest.is_empty() && date_fields(token).is_some() {
            return date_and_clock(token, rest, locale, today)
                .map(|ts| Some(ts.time_of_day()))
                .ok_or(CalcError::INVALID_TIME);
        }
        elapsed_time(s, locale)
            .map(Some)
            .ok_or(CalcError::INVALID_TIME)
    }
}

impl Timestamp {
    /// `GetAsTimestamp` of text: a date and a time, a date alone (midnight)
    /// or a time alone (on 1/1/0001); `Ok(None)` for blank text, error 500
    /// otherwise.
    pub fn parse(text: &str, locale: &Locale, today: Date) -> Result<Option<Timestamp>, CalcError> {
        let s = text.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let (token, rest) = split_first_word(s);
        let ts = if date_fields(token).is_some() {
            if rest.is_empty() {
                strict_date(token, locale, today).map(Date::to_timestamp)
            } else {
                date_and_clock(token, rest, locale, today)
            }
        } else {
            clock(s, locale).and_then(|(time, next_day)| {
                let time = if next_day {
                    SECONDS_PER_DAY * MICROS_PER_SECOND
                } else {
                    time
                };
                Timestamp::from_micros(time, TimeStyle::TimeOfDay)
            })
        };
        ts.map(Some).ok_or(CalcError::INVALID_DATE)
    }
}

/// A stored value cleaned as FileMaker cleans it before reading it as a
/// date or time (`TextBuffer::NumberDateTimeCleaner`, schema-path.md
/// 7.2.1): full-width ASCII forms made ASCII, ideographic commas and full
/// stops and the long dashes made `,`, `.` and `-`, other white space a
/// space, and leading and trailing white space removed.
fn cleaned_for_type_change(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3001}' | '\u{FF64}' => ',',
            '\u{3002}' | '\u{FF61}' => '.',
            '\u{2014}' | '\u{30FC}' => '-',
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .collect();
    cleaned.trim().to_owned()
}

/// What a field's stored text becomes when the field's type changes to date
/// (`0x400`) or, with `timestamp`, to timestamp (`0x800`; schema-path.md
/// 7.2.1): the text to store, or `None` to keep what is stored. The text is
/// cleaned and read in the file's `locale`; `today` gives a missing year
/// and the two-digit-year window.
/// - **To date:** a complete date (a four-digit year) without a time keeps
///   the cleaned text; a date with a time, or an incomplete one (no year, or
///   one to three digits), is rendered again (`GetAsText ( GetAsDate (
///   text ) )`: the locale's order and padding, the separator it was typed
///   with).
/// - **To timestamp:** a date with a time of day keeps the cleaned text when
///   the date is complete and is rendered again (`GetAsText (
///   GetAsTimestamp ( text ) )`) when it is not; a date alone, or a time
///   alone, keeps the cleaned text.
///
/// Anything that does not read as such (no date, an invalid one, an hour
/// alone after a date) stays as stored, and the new text is written only
/// when its length in characters differs from the stored text's, as
/// FileMaker writes it.
pub fn type_change_text(
    stored: &str,
    timestamp: bool,
    locale: &Locale,
    today: Date,
) -> Option<String> {
    let cleaned = cleaned_for_type_change(stored);
    let (token, rest) = split_first_word(&cleaned);
    let new = match date_fields(token) {
        Some((fields, _)) => {
            let year = match (fields.as_slice(), locale.date_order) {
                ([year, _, _], DateOrder::YearMonthDay) => Some(*year),
                ([_, _, year], _) => Some(*year),
                _ => None,
            };
            let complete = year.is_some_and(|year| year.len() >= 4);
            match (timestamp, rest.is_empty()) {
                (false, true) => {
                    let date = strict_date(token, locale, today)?;
                    if complete {
                        cleaned.clone()
                    } else {
                        date.format(locale)
                    }
                }
                (false, false) => date_and_clock(token, rest, locale, today)?
                    .date()
                    .format(locale),
                (true, true) => {
                    strict_date(token, locale, today)?;
                    cleaned.clone()
                }
                (true, false) => {
                    let stamp = date_and_clock(token, rest, locale, today)?;
                    if complete {
                        cleaned.clone()
                    } else {
                        stamp.format(locale)
                    }
                }
            }
        }
        // A time alone stays a time as a timestamp's text; as a date it is
        // no date, which stays as stored.
        None if timestamp => {
            Timestamp::parse(&cleaned, locale, today).ok()??;
            cleaned.clone()
        }
        None => return None,
    };
    (new.chars().count() != stored.chars().count()).then_some(new)
}

#[cfg(test)]
mod tests {
    use crate::datetime::{Date, Time, Timestamp};
    use crate::locale::Locale;

    /// FileMaker Server 26.0.3's type-change rewrite (schema-path.md 7.2.1,
    /// its `tools/fms-calc-harness` table, current year 2026): each stored
    /// text and what it becomes as a date and as a timestamp, `=` kept.
    #[test]
    fn a_type_change_rewrites_stored_text_as_filemaker_server_does() {
        let today = Date::from_ymd(2026, 11, 11).unwrap();
        let rows: [(&Locale, &str, &str, &str); 28] = [
            (&Locale::en_us(), "1/2/2003", "=", "="),
            (&Locale::en_us(), "01/02/2003", "=", "="),
            (&Locale::en_us(), "1/2/03", "1/2/2003", "="),
            (&Locale::en_us(), "1/2/3", "1/2/2003", "="),
            (&Locale::en_us(), "1-2-03", "1-2-2003", "="),
            (&Locale::en_us(), "01-02-03", "=", "="),
            (&Locale::en_us(), "1.2.03", "1.2.2003", "="),
            (&Locale::en_us(), "1/2/203", "1/2/0203", "="),
            (&Locale::en_us(), "1/2", "1/2/2026", "="),
            (&Locale::en_us(), "1-2", "1-2-2026", "="),
            (&Locale::en_us(), "1/2/2003 13:00", "1/2/2003", "="),
            (&Locale::en_us(), "01/02/2003 1:30 PM", "1/2/2003", "="),
            (
                &Locale::en_us(),
                "1/2/03 13:00",
                "1/2/2003",
                "1/2/2003 1:00 PM",
            ),
            (&Locale::en_us(), "  1/2/03  ", "1/2/2003", "1/2/03"),
            (&Locale::en_us(), "１/２/０３", "1/2/2003", "="),
            (&Locale::en_us(), "１/２/２００３", "=", "="),
            (
                &Locale::en_us(),
                "1/2/03 1:30 PM",
                "1/2/2003",
                "1/2/2003 1:30 PM",
            ),
            (
                &Locale::en_us(),
                "1-2-03 13:30",
                "1-2-2003",
                "1-2-2003 1:30 PM",
            ),
            (
                &Locale::en_us(),
                "1/2 13:30",
                "1/2/2026",
                "1/2/2026 1:30 PM",
            ),
            (&Locale::en_us(), "13:30", "=", "="),
            (&Locale::en_us(), "abc", "=", "="),
            (&Locale::en_us(), "12", "=", "="),
            (&Locale::en_us(), "2/30/2003", "=", "="),
            (&Locale::en_us(), "1/2/2003 13", "=", "="),
            (&Locale::de_de(), "2.1.03", "02.01.2003", "="),
            (&Locale::de_de(), "2-1-03", "02-01-2003", "="),
            (
                &Locale::de_de(),
                "2.1.03 13:30",
                "02.01.2003",
                "02.01.2003 13:30",
            ),
            (&Locale::de_de(), "2.1.", "=", "="),
        ];
        for (locale, stored, as_date, as_timestamp) in rows {
            for (timestamp, want) in [(false, as_date), (true, as_timestamp)] {
                let want = (want != "=").then(|| want.to_owned());
                assert_eq!(
                    super::type_change_text(stored, timestamp, locale, today),
                    want,
                    "{stored:?} to {}",
                    if timestamp { "timestamp" } else { "date" }
                );
            }
        }
        let de = Locale::de_de();
        for stored in ["2.1.2003", "02.01.2003"] {
            assert_eq!(super::type_change_text(stored, false, &de, today), None);
            assert_eq!(super::type_change_text(stored, true, &de, today), None);
        }
        assert_eq!(
            super::type_change_text("2.1.2003 13:00", false, &de, today).as_deref(),
            Some("02.01.2003")
        );
    }

    fn today() -> Date {
        Date::from_ymd(2026, 10, 28).unwrap()
    }

    fn date(text: &str, locale: &Locale) -> String {
        match Date::parse(text, locale, today()) {
            Ok(Some(d)) => d.format(locale),
            Ok(None) => String::new(),
            Err(e) => format!("?{}", e.code()),
        }
    }

    fn time(text: &str, locale: &Locale) -> String {
        match Time::parse(text, locale, today()) {
            Ok(Some(t)) => t.format(locale),
            Ok(None) => String::new(),
            Err(e) => format!("?{}", e.code()),
        }
    }

    fn timestamp(text: &str, locale: &Locale) -> String {
        match Timestamp::parse(text, locale, today()) {
            Ok(Some(t)) => t.format(locale),
            Ok(None) => String::new(),
            Err(e) => format!("?{}", e.code()),
        }
    }

    #[test]
    fn dates_en_us() {
        let en = Locale::en_us();
        for (text, want) in [
            ("1/2/2003", "1/2/2003"),
            ("01/02/2003", "1/2/2003"),
            ("1/2/03", "1/2/2003"),
            ("1/2/56", "1/2/2056"),
            ("1/2/57", "1/2/1957"),
            ("1/2/0", "1/2/2000"),
            ("1/2/203", "1/2/0203"),
            ("12/31/1", "12/31/2001"),
            ("1/2", "1/2/2026"),
            ("2.1", "?500"),
            ("1-2-2003", "1-2-2003"),
            ("2003-01-02", "?500"),
            ("1 2 2003", "?500"),
            ("Jan 2, 2003", "?500"),
            ("2/30/2003", "?500"),
            ("1/2/2003 13:00", "1/2/2003"),
            ("1/2/2003 abc", "?500"),
            ("1/2-2003", "?500"),
            ("1/2/02003", "?500"),
            ("1/2/0000", "?500"),
            ("x1/2/2003y", "?500"),
            (" 1/2/2003\t", "1/2/2003"),
            ("", ""),
            ("12", "?500"),
        ] {
            assert_eq!(date(text, &en), want, "{text:?}");
        }
    }

    #[test]
    fn date_separators() {
        let en = Locale::en_us();
        for (text, want) in [
            ("1,5,2003", "1,5,2003"),
            ("1;5", "1;5;2026"),
            ("1年5年2003", "1年5年2003"),
            ("1\u{a0}5\u{a0}2003", "1\u{a0}5\u{a0}2003"),
            ("1+5+2003", "?500"),
            ("1:5:2003", "?500"),
            ("1x5x2003", "?500"),
            ("1あ5あ2003", "?500"),
            ("1\u{3000}5\u{3000}2003", "?500"),
        ] {
            assert_eq!(date(text, &en), want, "{text:?}");
        }
        assert_eq!(date("2,1", &Locale::de_de()), "?500");
    }

    #[test]
    fn dates_other_locales() {
        assert_eq!(date("2.1.2003", &Locale::de_de()), "02.01.2003");
        assert_eq!(date("1/2/2003", &Locale::de_de()), "01/02/2003");
        assert_eq!(date("2.1", &Locale::de_de()), "02.01.2026");
        assert_eq!(date("2.1.03", &Locale::sv_se()), "2002.01.03");
        assert_eq!(date("2003-1-2", &Locale::sv_se()), "2003-01-02");
        assert_eq!(date("1/2/03", &Locale::ja_jp()), "2001/02/03");
        assert_eq!(date("2.1.2003", &Locale::ja_jp()), "?500");
        assert_eq!(date("2.1", &Locale::ja_jp()), "?500");
    }

    #[test]
    fn times() {
        let en = Locale::en_us();
        for (text, want) in [
            ("1:30", "1:30:00"),
            ("1:30:15.25", "1:30:15.25"),
            ("1:30 pm", "1:30 PM"),
            ("1:30PM", "1:30 PM"),
            ("12:30 AM", "12:30 AM"),
            ("0:00 PM", "12:00 PM"),
            ("13:30 PM", "?501"),
            ("1 PM", "1 PM"),
            ("1:75", "2:15:00"),
            ("-1:30", "-1:30:00"),
            ("- 1:30", "-1:30:00"),
            ("+1:30", "?501"),
            ("90", "90:00:00"),
            ("1.5", "0:00:01.5"),
            ("-1.5", "-0:00:01.5"),
            ("1:30.5", "0:01:30.5"),
            ("1:30.", "0:01:30"),
            (".5", "0:00:00.5"),
            ("1:30.25 PM", "?501"),
            ("1:2:3:4", "?501"),
            ("1 : 30", "?501"),
            (":30", "?501"),
            ("x1:30y", "?501"),
            ("1:30 A.M.", "?501"),
            ("01:02:03.1234567890", "1:02:03.123456"),
            ("1/2/2003 13:30", "1:30 PM"),
            ("", ""),
        ] {
            assert_eq!(time(text, &en), want, "{text:?}");
        }
        assert_eq!(time("1:05 PM", &Locale::de_de()), "?501");
        assert_eq!(time("1,5", &Locale::de_de()), "00:00:01,5");
        assert_eq!(time("1:05午後", &Locale::ja_jp()), "13:05");
        assert_eq!(time("13:05 午後", &Locale::ja_jp()), "?501");
    }

    #[test]
    fn timestamps() {
        let en = Locale::en_us();
        for (text, want) in [
            ("1/2/2003 1:30 PM", "1/2/2003 1:30 PM"),
            ("1/2/2003 13:30:15", "1/2/2003 1:30:15 PM"),
            ("1/2/2003", "1/2/2003 12:00 AM"),
            ("1:30", "1/1/0001 1:30 AM"),
            ("2.1.2003 13:05", "2.1.2003 1:05 PM"),
            ("1/2/2003 24:00", "1/3/2003 12:00 AM"),
            ("2/30/2003 24:00", "3/3/2003 12:00 AM"),
            ("1/2/2003 24:30", "?500"),
            ("1/2/2003 25:00", "?500"),
            ("1/2/2003 1:75", "?500"),
            ("1/2/2003 1:30:60", "?500"),
            ("1/2/2003 13", "?500"),
            ("1/2/2003 1 PM", "?500"),
            ("1/2/2003T13:30", "?500"),
            ("2003-01-02 13:30:15", "?500"),
            ("1/2 1:00", "1/2/2026 1:00 AM"),
        ] {
            assert_eq!(timestamp(text, &en), want, "{text:?}");
        }
        assert_eq!(
            timestamp("1/2/2003 24:00", &Locale::sv_se()),
            "2006/07/28 00:00"
        );
        assert_eq!(
            timestamp("2/30/2003 24:00", &Locale::fr_fr()),
            "03/06/2005 00:00"
        );
    }
}
