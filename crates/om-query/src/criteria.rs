//! Find mode criteria: the text typed into a field of a find request,
//! parsed with FileMaker's find operators and evaluated on a record's stored
//! value (query-program.md 6.4; Claris "Finding text and characters",
//! "Finding numbers, dates, times, and timestamps", "Finding ranges of
//! information", "Finding empty or non-empty fields", "Finding duplicate
//! values").
//!
//! [`crate::find::plan`] turns a criterion into the SimpleFind finders a
//! host runs. Where the finders cannot express it (wildcards, quoted
//! phrases, `?`, relaxed kana, partial dates, fractions), they select a
//! superset and the client keeps the records [`Criterion::matches`] accepts,
//! as FileMaker Pro falls back to finding locally (architecture.md 7).
//!
//! Operators: `=` (empty, or an empty line), `==` (entirely empty), `*`
//! (not empty), `==value` (the whole value), `=word` (a whole word), plain
//! words (each the start of a word of the field), `"phrase"` (a literal
//! phrase from the start of a word; `*"phrase"` anywhere, ending at a word
//! boundary unless followed by `*`), `*` (any characters), `@` (one
//! character), `#` (one digit), `\` (the next character literally), `!`
//! (duplicates, optionally of values matching the rest), `?` (a value the
//! field's type cannot read, or a text with invalid characters), `~`
//! (relaxed kana), `<`, `<=`/`≤`, `>`, `>=`/`≥`, `a...b`/`a..b`/`a…b`, `//`
//! (today), dates with `*` or omitted parts and `{a..b}` sub-ranges, day
//! names, and times with `AM`/`PM`.
//!
//! Criteria are hostile input (a server script may find with its caller's
//! text), so reading and matching one takes no more stack than any other,
//! and time at most its length times the value's: a run of leading `!` and
//! `~` is read in a loop as the at most three operators it amounts to
//! ([`Chain`]), and wildcards match in a loop that only ever goes back to
//! the latest `*` ([`pattern_matches`]). A criterion longer than
//! [`MAX_CRITERION_CHARS`] is unreadable, OpenMaker's own limit.

use std::cmp::Ordering;

use om_collate::DateOrder;
use om_hbam::DataType;

use crate::keys::TextKeys;

/// A date as (year, month, day).
pub type Ymd = (i32, u32, u32);

/// What criteria are read with: the file's date order, today's date (`//`,
/// and dates typed without a year), and the form the field's indexes key
/// text in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindContext {
    pub order: DateOrder,
    pub today: Option<Ymd>,
    /// How the field's indexes key text (query-program.md 7.2): a text
    /// criterion's keys, and the keys it compares a value's text by, are
    /// in this form. English unless [`Self::with_text_keys`] names another.
    pub text: TextKeys,
}

impl FindContext {
    pub fn new(order: DateOrder, today: Option<Ymd>) -> Self {
        FindContext {
            order,
            today,
            text: TextKeys::Collated,
        }
    }

    /// This context for a field whose indexes key text in `text`. A form
    /// with no keys ([`TextKeys::None`]) reads criteria in English.
    pub fn with_text_keys(self, text: TextKeys) -> Self {
        let text = match text {
            TextKeys::None => TextKeys::Collated,
            form => form,
        };
        FindContext { text, ..self }
    }
}

/// Why a criterion cannot be read: FileMaker's 3 here (the value is not a
/// number, date or time its field takes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unreadable;

/// The longest criterion OpenMaker reads, in characters after trimming;
/// a longer one is [`Unreadable`]. FileMaker's limit, if it has one, is not
/// measured: this is OpenMaker's choice. It bounds what reading a
/// criterion allocates (a wildcard pattern holds a collation key per
/// character), the finders one criterion becomes (one per word), and what
/// matching costs per record (a pattern or phrase at most its length times
/// the value's).
pub const MAX_CRITERION_CHARS: usize = 10_000;

/// One field's criterion of a find request.
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    pub(crate) data_type: DataType,
    pub(crate) kind: Kind,
    /// The form its text keys are in ([`FindContext::text`]).
    pub(crate) text: TextKeys,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    /// `=`: no value, or an empty line (the value index's empty key).
    Empty,
    /// `==` alone: nothing at all in the field.
    EntirelyEmpty,
    /// `*` alone: any value.
    NotEmpty,
    /// `?`: a value the field's type cannot read.
    Invalid,
    /// `!`: a value another record holds too, matching the rest if given.
    Duplicates(Option<Box<Criterion>>),
    /// Text: every term matches (ANDed).
    Terms(Vec<Term>),
    /// `==value` on text: a line of the value is the whole criterion.
    Whole(Whole),
    /// A number field's value.
    Number(Number),
    /// `<`, `<=`, `>`, `>=`, `a...b`.
    Range {
        low: Option<(Bound, bool)>,
        high: Option<(Bound, bool)>,
    },
    /// A date, time or timestamp, possibly partial.
    Moment(Moment),
    /// `~`: kana compared without kana, voicing or size differences.
    Relaxed(Box<Criterion>),
}

/// One term of a text criterion.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Term {
    /// A word of the field starts with this word (its index key).
    Prefix(Vec<u8>),
    /// `=word`: a word of the field is this word.
    Word(Vec<u8>),
    /// A word of the field matches this pattern in full.
    Pattern(Vec<Unit>),
    /// A quoted phrase, compared without case but with accents.
    Phrase {
        text: Vec<char>,
        /// `*"…"`: anywhere, not only from a word's start.
        anywhere: bool,
        /// `"…"*` (or no leading `*`): may end inside a word.
        open_end: bool,
    },
}

/// One position of a wildcard pattern.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Unit {
    /// A character, by its collation key (case and accents ignored).
    Char(Vec<u8>),
    /// `@`: any one character.
    One,
    /// `#`: one digit.
    Digit,
    /// `*`: any characters.
    Any,
}

/// `==value` on text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Whole {
    /// The value key of the whole criterion.
    Key(Vec<u8>),
    /// With wildcards: the whole line matches.
    Pattern(Vec<Unit>),
}

/// A range bound.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Bound {
    Number(Number),
    /// Text ranges compare words (the word index, Claris "Finding ranges").
    Word(Vec<u8>),
    Date(Ymd),
    /// Seconds since midnight.
    Time(u32),
    /// A date and seconds since midnight.
    Stamp(Ymd, u32),
}

/// A date, time or timestamp criterion.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Moment {
    Date(DatePattern),
    Time(TimePattern),
    Stamp(DatePattern, TimePattern),
}

/// A component's accepted values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Any,
    /// Inclusive.
    Range(u32, u32),
}

impl Part {
    fn exact(value: u32) -> Self {
        Part::Range(value, value)
    }

    fn has(self, value: u32) -> bool {
        match self {
            Part::Any => true,
            Part::Range(low, high) => (low..=high).contains(&value),
        }
    }

    pub(crate) fn fixed(self) -> Option<u32> {
        match self {
            Part::Range(low, high) if low == high => Some(low),
            _ => None,
        }
    }

    fn low(self, default: u32) -> u32 {
        match self {
            Part::Any => default,
            Part::Range(low, _) => low,
        }
    }

    fn high(self, default: u32) -> u32 {
        match self {
            Part::Any => default,
            Part::Range(_, high) => high,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DatePattern {
    pub year: Part,
    pub month: Part,
    pub day: Part,
    /// Days of the week, 0 Sunday to 6 Saturday.
    pub weekday: Part,
}

impl DatePattern {
    const ANY: DatePattern = DatePattern {
        year: Part::Any,
        month: Part::Any,
        day: Part::Any,
        weekday: Part::Any,
    };

    fn matches(&self, (year, month, day): Ymd) -> bool {
        year >= 0
            && self.year.has(year as u32)
            && self.month.has(month)
            && self.day.has(day)
            && self.weekday.has(weekday((year, month, day)))
    }

    /// The first date the pattern can name, when its year is known.
    fn earliest(&self) -> Option<Ymd> {
        let year = match self.year {
            Part::Range(low, _) => low as i32,
            Part::Any => return None,
        };
        Some((year, self.month.low(1), self.day.low(1)))
    }

    /// The last date the pattern can name, when its year is known.
    fn latest(&self) -> Option<Ymd> {
        let year = match self.year {
            Part::Range(_, high) => high as i32,
            Part::Any => return None,
        };
        let month = self.month.high(12);
        let day = self
            .day
            .high(days_in_month(year, month))
            .min(days_in_month(year, month));
        Some((year, month, day))
    }

    /// The pattern is one whole day, month or year: its dates are exactly
    /// the range from [`Self::earliest`] to [`Self::latest`].
    pub(crate) fn is_period(&self) -> bool {
        let fixed = |p: Part| p.fixed().is_some();
        self.weekday == Part::Any
            && fixed(self.year)
            && (fixed(self.month) || self.month == Part::Any && self.day == Part::Any)
            && (fixed(self.day) || self.day == Part::Any)
    }

    pub(crate) fn bounds(&self) -> Option<(Ymd, Ymd)> {
        Some((self.earliest()?, self.latest()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TimePattern {
    pub hour: Part,
    pub minute: Part,
    pub second: Part,
}

impl TimePattern {
    const ANY: TimePattern = TimePattern {
        hour: Part::Any,
        minute: Part::Any,
        second: Part::Any,
    };

    fn matches(&self, seconds: u32) -> bool {
        self.hour.has(seconds / 3600)
            && self.minute.has(seconds / 60 % 60)
            && self.second.has(seconds % 60)
    }

    pub(crate) fn earliest(&self) -> u32 {
        self.hour.low(0) * 3600 + self.minute.low(0) * 60 + self.second.low(0)
    }

    pub(crate) fn latest(&self) -> u32 {
        self.hour.high(23) * 3600 + self.minute.high(59) * 60 + self.second.high(59)
    }

    /// The pattern is a range of whole seconds from its earliest to its
    /// latest second (an hour, a minute or a second).
    pub(crate) fn is_period(&self) -> bool {
        let fixed = |p: Part| p.fixed().is_some();
        match (self.hour, self.minute, self.second) {
            (h, Part::Any, Part::Any) => fixed(h) || h == Part::Any,
            (h, m, Part::Any) => fixed(h) && fixed(m),
            (h, m, s) => fixed(h) && fixed(m) && fixed(s),
        }
    }
}

// ----- numbers ------------------------------------------------------------------

/// A decimal number as a number field holds it: the digits of its text, a
/// decimal point and a sign (as `GetAsNumber` reads text), compared by
/// value. `.50` = `.5` = `$.50`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Number {
    negative: bool,
    /// Integer digits without leading zeros.
    integer: Vec<u8>,
    /// Fraction digits without trailing zeros.
    fraction: Vec<u8>,
}

impl Number {
    pub fn parse(text: &str) -> Option<Number> {
        let mut negative = false;
        let mut integer = Vec::new();
        let mut fraction = Vec::new();
        let mut in_fraction = false;
        let mut digits = false;
        for c in text.chars() {
            match c {
                '0'..='9' => {
                    digits = true;
                    let d = c as u8 - b'0';
                    if in_fraction {
                        fraction.push(d);
                    } else {
                        integer.push(d);
                    }
                }
                '.' if !in_fraction => in_fraction = true,
                // A minus sign, ASCII or U+2212, as a stored number reads it.
                '-' | '\u{2212}' if !digits => negative = true,
                // An exponent ends the digits FileMaker reads.
                'e' | 'E' if digits => break,
                _ => {}
            }
        }
        if !digits {
            return None;
        }
        let start = integer
            .iter()
            .position(|d| *d != 0)
            .unwrap_or(integer.len());
        integer.drain(..start);
        while fraction.last() == Some(&0) {
            fraction.pop();
        }
        let zero = integer.is_empty() && fraction.is_empty();
        Some(Number {
            negative: negative && !zero,
            integer,
            fraction,
        })
    }

    /// The number's value-index key (query-program.md 7.3).
    pub(crate) fn index_key(&self) -> Option<Vec<u8>> {
        let digits = |d: &[u8]| d.iter().map(|d| char::from(b'0' + d)).collect::<String>();
        // At least one integer digit: `0` has none of its own.
        let integer = if self.integer.is_empty() {
            "0".to_string()
        } else {
            digits(&self.integer)
        };
        om_collate::number_key(&format!(
            "{}{integer}.{}",
            if self.negative { "-" } else { "" },
            digits(&self.fraction)
        ))
    }

    /// The number as a binary float (for summaries that only order groups).
    pub fn to_f64(&self) -> f64 {
        let digits = |d: &[u8]| d.iter().map(|d| char::from(b'0' + d)).collect::<String>();
        let text = format!(
            "{}{}.{}",
            if self.negative { "-" } else { "" },
            if self.integer.is_empty() {
                "0".into()
            } else {
                digits(&self.integer)
            },
            digits(&self.fraction)
        );
        text.trim_end_matches('.').parse().unwrap_or(0.0)
    }

    fn magnitude(&self, other: &Number) -> Ordering {
        self.integer
            .len()
            .cmp(&other.integer.len())
            .then_with(|| self.integer.cmp(&other.integer))
            .then_with(|| self.fraction.cmp(&other.fraction))
    }
}

impl Ord for Number {
    fn cmp(&self, other: &Number) -> Ordering {
        match (self.negative, other.negative) {
            (false, false) => self.magnitude(other),
            (true, true) => other.magnitude(self),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Number) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ----- dates and times ------------------------------------------------------

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

pub(crate) fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1/1/1970 (proleptic Gregorian).
fn days_from_civil((year, month, day): Ymd) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date `seconds` after 1/1/1970 00:00 (proleptic Gregorian): today,
/// from the clock and the local offset.
pub fn civil_date(seconds: i64) -> Ymd {
    let days = seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

/// 0 Sunday to 6 Saturday.
fn weekday(date: Ymd) -> u32 {
    // 1/1/1970 was a Thursday.
    (days_from_civil(date) + 4).rem_euclid(7) as u32
}

const DAY_NAMES: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];

/// A day name, full or abbreviated to three letters or more (`Fri`,
/// `Thurs`); English names work in every system format (Claris).
fn day_name(text: &str) -> Option<u32> {
    let lower = text.trim().to_lowercase();
    if lower.len() < 3 {
        return None;
    }
    DAY_NAMES
        .iter()
        .position(|name| name.starts_with(&lower))
        .map(|i| i as u32)
}

/// A stored date in `order` (four-digit year), or year/month/day.
pub(crate) fn read_date(text: &str, order: DateOrder) -> Option<Ymd> {
    let parts: Vec<&str> = text.trim().split(['/', '.', '-']).collect();
    let [a, b, c] = parts.as_slice() else {
        return None;
    };
    let (year, month, day) = if a.len() == 4 {
        (a, b, c)
    } else {
        match order {
            DateOrder::MonthDayYear => (c, a, b),
            DateOrder::DayMonthYear => (c, b, a),
            DateOrder::YearMonthDay => (a, b, c),
        }
    };
    let (year, month, day): (i32, u32, u32) = (
        year.trim().parse().ok()?,
        month.trim().parse().ok()?,
        day.trim().parse().ok()?,
    );
    ((1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day))
        .then_some((year, month, day))
}

/// A stored time: `h:mm[:ss[.fff]]`, 24-hour or with `AM`/`PM`; seconds
/// since midnight (fractions dropped).
pub(crate) fn read_time(text: &str) -> Option<u32> {
    let text = text.trim();
    let upper = text.to_ascii_uppercase();
    let (clock, meridiem) = if let Some(clock) = upper.strip_suffix("AM") {
        (clock.trim_end(), Some(false))
    } else if let Some(clock) = upper.strip_suffix("PM") {
        (clock.trim_end(), Some(true))
    } else {
        (upper.as_str(), None)
    };
    let mut parts = clock.split(':');
    let hour: u32 = parts.next()?.trim().parse().ok()?;
    let minute: u32 = parts.next().map_or(Some(0), |m| m.trim().parse().ok())?;
    let second: f64 = parts.next().map_or(Some(0.0), |s| s.trim().parse().ok())?;
    if parts.next().is_some() || minute > 59 || !(0.0..60.0).contains(&second) {
        return None;
    }
    let hour = match meridiem {
        Some(pm) if (1..=12).contains(&hour) => hour % 12 + if pm { 12 } else { 0 },
        Some(_) => return None,
        None if hour <= 23 => hour,
        None => return None,
    };
    Some(hour * 3600 + minute * 60 + second as u32)
}

/// A stored timestamp: a date, a space and a time.
pub(crate) fn read_stamp(text: &str, order: DateOrder) -> Option<(Ymd, u32)> {
    let (date, time) = text.trim().split_once(' ')?;
    Some((read_date(date, order)?, read_time(time)?))
}

/// A year as typed: two digits fall within the 100 years from 70 years
/// before today (FileMaker's window, inferred), otherwise as written.
fn full_year(text: &str, ctx: &FindContext) -> Option<u32> {
    let year: u32 = text.parse().ok()?;
    if text.len() > 2 {
        return (1..=4000).contains(&year).then_some(year);
    }
    let current = ctx.today.map_or(2026, |(y, _, _)| y.max(100) as u32);
    let century = current / 100 * 100;
    let mut full = century + year;
    if full > current + 30 {
        full -= 100;
    }
    Some(full)
}

/// Splits `text` at depth 0 of `{}` on any of `separators`.
fn split_outside_braces<'a>(text: &'a str, separators: &[char]) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            c if depth == 0 && separators.contains(&c) => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// A component: a number, `*` or nothing (any), or `{a..b}` (a sub-range).
fn part(text: &str, read: impl Fn(&str) -> Option<u32>) -> Option<Part> {
    let text = text.trim();
    if text.is_empty() || text == "*" {
        return Some(Part::Any);
    }
    if let Some(inner) = text.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
        let (low, high) = split_range(inner)?;
        let (low, high) = (read(low.trim())?, read(high.trim())?);
        return (low <= high).then_some(Part::Range(low, high));
    }
    read(text).map(Part::exact)
}

fn number_part(text: &str) -> Option<u32> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// Year parts read two-digit years in FileMaker's window.
fn year_part(text: &str, ctx: &FindContext) -> Option<Part> {
    part(text, |t| {
        number_part(t)?;
        full_year(t, ctx)
    })
}

/// A date pattern: `//` (today), a day name or range of day names, or
/// two or three components in the file's order (`5/12/*`, `6/2019`,
/// `{1..3}/2020`, `5/12` in the current year), or a year.
fn date_pattern(text: &str, ctx: &FindContext) -> Option<DatePattern> {
    let text = text.trim().trim_start_matches('=');
    if text == "//" {
        let (year, month, day) = ctx.today?;
        return Some(DatePattern {
            year: Part::exact(year as u32),
            month: Part::exact(month),
            day: Part::exact(day),
            weekday: Part::Any,
        });
    }
    if let Some(day) = day_name(text) {
        return Some(DatePattern {
            weekday: Part::exact(day),
            ..DatePattern::ANY
        });
    }
    if let Some((first, last)) = split_range(text) {
        let (first, last) = (day_name(first)?, day_name(last)?);
        return Some(DatePattern {
            weekday: if first <= last {
                Part::Range(first, last)
            } else {
                return None;
            },
            ..DatePattern::ANY
        });
    }
    let parts = split_outside_braces(text, &['/', '-', '.']);
    let month = |t: &str| part(t, |n| number_part(n).filter(|m| (1..=12).contains(m)));
    let day = |t: &str| part(t, |n| number_part(n).filter(|d| (1..=31).contains(d)));
    let current = || {
        ctx.today
            .map_or(Part::Any, |(year, _, _)| Part::exact(year as u32))
    };
    let pattern = match parts.as_slice() {
        [year] => {
            let digits = number_part(year.trim())?;
            if year.trim().len() < 3 && digits <= 31 {
                return None;
            }
            DatePattern {
                year: year_part(year, ctx)?,
                ..DatePattern::ANY
            }
        }
        [a, b] => {
            let b_trim = b.trim();
            let is_year = b_trim.len() >= 3 && number_part(b_trim).is_some();
            match ctx.order {
                DateOrder::YearMonthDay if a.trim().len() >= 3 => DatePattern {
                    year: year_part(a, ctx)?,
                    month: month(b)?,
                    ..DatePattern::ANY
                },
                _ if is_year => DatePattern {
                    year: year_part(b, ctx)?,
                    month: month(a)?,
                    ..DatePattern::ANY
                },
                DateOrder::DayMonthYear => DatePattern {
                    year: current(),
                    month: month(b)?,
                    day: day(a)?,
                    weekday: Part::Any,
                },
                _ => DatePattern {
                    year: current(),
                    month: month(a)?,
                    day: day(b)?,
                    weekday: Part::Any,
                },
            }
        }
        [a, b, c] => {
            let (year, m, d) = if a.trim().len() == 4 {
                (a, b, c)
            } else {
                match ctx.order {
                    DateOrder::MonthDayYear => (c, a, b),
                    DateOrder::DayMonthYear => (c, b, a),
                    DateOrder::YearMonthDay => (a, b, c),
                }
            };
            DatePattern {
                year: year_part(year, ctx)?,
                month: month(m)?,
                day: day(d)?,
                weekday: Part::Any,
            }
        }
        _ => return None,
    };
    Some(pattern)
}

/// A time pattern: `h[:mm[:ss]]` with `*` and `{a..b}` components and an
/// optional `AM`/`PM`, or `AM`/`PM` alone.
fn time_pattern(text: &str) -> Option<TimePattern> {
    let upper = text.trim().to_ascii_uppercase();
    let (clock, meridiem) = if let Some(clock) = upper.strip_suffix("AM") {
        (clock.trim_end(), Some(false))
    } else if let Some(clock) = upper.strip_suffix("PM") {
        (clock.trim_end(), Some(true))
    } else {
        (upper.as_str(), None)
    };
    if clock.is_empty() {
        let pm = meridiem?;
        return Some(TimePattern {
            hour: if pm {
                Part::Range(12, 23)
            } else {
                Part::Range(0, 11)
            },
            ..TimePattern::ANY
        });
    }
    let parts = split_outside_braces(clock, &[':']);
    if parts.len() > 3 {
        return None;
    }
    let hour_limit = if meridiem.is_some() { 12 } else { 23 };
    let mut hour = part(parts[0], |h| {
        number_part(h).filter(|h| *h <= hour_limit && (meridiem.is_none() || *h >= 1))
    })?;
    if let Some(pm) = meridiem {
        let convert = |h: u32| h % 12 + if pm { 12 } else { 0 };
        hour = match hour {
            Part::Any => {
                if pm {
                    Part::Range(12, 23)
                } else {
                    Part::Range(0, 11)
                }
            }
            Part::Range(low, high) => {
                let (low, high) = (convert(low), convert(high));
                if low > high {
                    return None;
                }
                Part::Range(low, high)
            }
        };
    }
    let sixty = |t: &str| {
        part(t, |n| {
            let whole = n.split('.').next()?;
            number_part(whole).filter(|v| *v <= 59)
        })
    };
    Some(TimePattern {
        hour,
        minute: parts.get(1).map_or(Some(Part::Any), |m| sixty(m))?,
        second: parts.get(2).map_or(Some(Part::Any), |s| sixty(s))?,
    })
}

/// A timestamp pattern: a date part (`//`, a date, a day name with an
/// optional year: `=Mon 2020`) and an optional time part (`7 PM`,
/// `*:30 PM`, `12:59:09 PM`).
fn stamp_pattern(text: &str, ctx: &FindContext) -> Option<(DatePattern, TimePattern)> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let (first, rest) = tokens.split_first()?;
    let mut date = date_pattern(first, ctx)?;
    let mut rest = rest;
    // A day name may be followed by a year.
    if date.weekday != Part::Any && date.year == Part::Any {
        if let Some((year, tail)) = rest.split_first() {
            if year.len() == 4 && number_part(year).is_some() {
                date.year = year_part(year, ctx)?;
                rest = tail;
            }
        }
    }
    let time = if rest.is_empty() {
        TimePattern::ANY
    } else {
        time_pattern(&rest.join(" "))?
    };
    Some((date, time))
}

/// The position of a range operator `...`, `..` or `…` outside quotes and
/// braces: (left, right).
pub(crate) fn split_range(text: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            '{' if !quoted => depth += 1,
            '}' if !quoted => depth = depth.saturating_sub(1),
            '…' if !quoted && depth == 0 => return Some((&text[..i], &text[i + c.len_utf8()..])),
            '.' if !quoted && depth == 0 && text[i..].starts_with("..") => {
                let width = if text[i..].starts_with("...") { 3 } else { 2 };
                return Some((&text[..i], &text[i + width..]));
            }
            _ => {}
        }
    }
    None
}

// ----- text -------------------------------------------------------------------------

/// A character's key in `form`, as the index compares it: its primary
/// weights (case and accents ignored), or in Unicode raw its UTF-16 code
/// units; empty for an ignorable character.
fn char_key(form: TextKeys, c: char) -> Vec<u8> {
    let mut buffer = [0u8; 4];
    prefix_key(form, c.encode_utf8(&mut buffer))[1..].to_vec()
}

/// The whole key of `text` (a word, a line, a bound) in `form`.
pub(crate) fn whole_key(form: TextKeys, text: &str) -> Vec<u8> {
    form.whole_key(text)
        .unwrap_or_else(|| om_collate::text_key(text))
}

/// The key a word or value that starts with `text` starts with, in `form`:
/// its whole key but in the padded form (System, Unicode standard), where
/// only the primary weights come first and `00 00` and the secondary bytes
/// follow them (query-program.md 7.2).
pub(crate) fn prefix_key(form: TextKeys, text: &str) -> Vec<u8> {
    match form {
        // Primary weights alone, so a prefix's key starts the word's.
        TextKeys::Raw | TextKeys::Japanese => whole_key(form, text),
        _ => om_collate::text_key(text),
    }
}

/// Characters with their collation keys.
type KeyedChars = Vec<(char, Vec<u8>)>;

/// The non-ignorable characters of `text` with their keys in `form`.
fn units(form: TextKeys, text: &str) -> KeyedChars {
    text.chars()
        .map(|c| (c, char_key(form, c)))
        .filter(|(_, key)| !key.is_empty())
        .collect()
}

/// Whether `unit` can take the character `c` (with its collation key).
fn unit_takes(unit: &Unit, (c, key): &(char, Vec<u8>)) -> bool {
    match unit {
        Unit::Char(expected) => key == expected,
        Unit::One | Unit::Any => true,
        Unit::Digit => c.is_numeric(),
    }
}

/// Whether `pattern` matches all of `word`.
///
/// Every unit but `*` takes exactly one character, so the text between
/// two `*` is best matched as far left as it fits: a later `*` then has
/// the most room. On a mismatch only the latest `*` takes one more
/// character and the units after it are tried again; an earlier `*`
/// taking more could only push them further right. That is the usual
/// iterative wildcard match: no recursion, and at most the pattern's
/// length times the word's steps, however many `*` there are.
fn pattern_matches(pattern: &[Unit], word: &[(char, Vec<u8>)]) -> bool {
    let (mut p, mut w) = (0, 0);
    // The latest `*` seen and the word position its match ends at.
    let mut star: Option<(usize, usize)> = None;
    while w < word.len() {
        match pattern.get(p) {
            Some(Unit::Any) => {
                star = Some((p, w));
                p += 1;
            }
            Some(unit) if unit_takes(unit, &word[w]) => {
                p += 1;
                w += 1;
            }
            _ => match star {
                Some((at, end)) => {
                    star = Some((at, end + 1));
                    p = at + 1;
                    w = end + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|unit| *unit == Unit::Any)
}

/// The words of a value as find compares them: the word index's words
/// (query-program.md 7.1).
fn words(value: &str) -> Vec<&str> {
    om_collate::words(value)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// A value as phrases read it: its characters as [`phrase_chars`] gives
/// them, and for each place between them (one more than the characters)
/// whether it lies inside a word of the word index ([`words`]), so that a
/// phrase starts and ends where the index's words do (`t` is inside
/// `don't`, `テ` starts a word of `日本語のテキスト`).
struct PhraseView {
    chars: Vec<char>,
    inside: Vec<bool>,
}

fn phrase_view(form: TextKeys, value: &str) -> PhraseView {
    // Each word's byte span in `value`.
    let spans: Vec<(usize, usize)> = words(value)
        .iter()
        .map(|w| {
            let start = w.as_ptr() as usize - value.as_ptr() as usize;
            (start, start + w.len())
        })
        .collect();
    let mut chars = Vec::new();
    let mut inside = Vec::new();
    // The spans are in order: one cursor walks them with the characters.
    let mut span = 0;
    for (offset, c) in value.char_indices() {
        while spans.get(span).is_some_and(|&(_, end)| end <= offset) {
            span += 1;
        }
        let first = chars.len();
        chars.extend(phrase_chars(form, std::iter::once(c)));
        let in_word = spans
            .get(span)
            .is_some_and(|&(start, end)| start < offset && offset < end);
        // Between the characters one character lowers to, inside it.
        inside.extend((first..chars.len()).map(|i| in_word || i > first));
    }
    inside.push(false);
    PhraseView { chars, inside }
}

/// Whether the phrase `text` is in `value`, starting a word unless
/// `anywhere`, and ending one when `anywhere` and not `open_end`.
fn phrase_matches(text: &[char], anywhere: bool, open_end: bool, value: &PhraseView) -> bool {
    let chars = &value.chars;
    if text.is_empty() || text.len() > chars.len() {
        return text.is_empty();
    }
    let starts_word = is_word_char(text[0]);
    let ends_word = is_word_char(text[text.len() - 1]);
    (0..=chars.len() - text.len()).any(|start| {
        if chars[start..start + text.len()] != *text {
            return false;
        }
        if !anywhere && starts_word && value.inside[start] {
            return false;
        }
        let end = start + text.len();
        if anywhere && !open_end && ends_word && value.inside[end] {
            return false;
        }
        true
    })
}

/// One lexed term: its characters (escaped ones literal), and its quoted
/// groups.
#[derive(Default)]
struct RawTerm {
    /// (character, literal): escaped or quoted characters are literal.
    chars: Vec<(char, bool)>,
    quoted: bool,
}

/// Splits a text criterion into terms at whitespace outside quotes;
/// `\` makes the next character literal.
fn lex_terms(text: &str) -> Vec<RawTerm> {
    let mut terms = Vec::new();
    let mut current = RawTerm::default();
    let mut in_quotes = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.chars.push((next, true));
                }
            }
            '"' => {
                in_quotes = !in_quotes;
                current.quoted = true;
                // Quote marks themselves are kept as markers.
                current.chars.push(('"', false));
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.chars.is_empty() {
                    terms.push(std::mem::take(&mut current));
                }
            }
            c => current.chars.push((c, in_quotes)),
        }
    }
    if !current.chars.is_empty() {
        terms.push(current);
    }
    terms
}

/// The characters of a whole-value criterion, whitespace kept: quoted and
/// escaped characters are literal, the quote marks dropped.
fn literal_chars(text: &str) -> Vec<(char, bool)> {
    let mut out = Vec::new();
    let mut quoted = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next().map(|next| (next, true))),
            '"' => quoted = !quoted,
            c => out.push((c, quoted)),
        }
    }
    out
}

fn is_wildcard(c: char, literal: bool) -> bool {
    !literal && matches!(c, '*' | '@' | '#')
}

fn pattern_of(form: TextKeys, chars: &[(char, bool)]) -> Vec<Unit> {
    chars
        .iter()
        .filter_map(|&(c, literal)| match c {
            '*' if !literal => Some(Unit::Any),
            '@' if !literal => Some(Unit::One),
            '#' if !literal => Some(Unit::Digit),
            c => {
                let key = char_key(form, c);
                (!key.is_empty()).then_some(Unit::Char(key))
            }
        })
        .collect()
}

/// A value's characters as a phrase compares them: in lower case, but in
/// Unicode raw, whose keys keep case, as written.
fn phrase_chars(form: TextKeys, text: impl Iterator<Item = char>) -> Vec<char> {
    match form {
        TextKeys::Raw => text.collect(),
        _ => text.flat_map(char::to_lowercase).collect(),
    }
}

fn text_terms(form: TextKeys, text: &str) -> Vec<Term> {
    let mut terms = Vec::new();
    for raw in lex_terms(text) {
        let mut chars = raw.chars.as_slice();
        let whole_word = matches!(chars.first(), Some(('=', false)));
        if whole_word {
            chars = &chars[1..];
        }
        if raw.quoted {
            let first_quote = chars.iter().position(|&(c, l)| c == '"' && !l);
            let last_quote = chars.iter().rposition(|&(c, l)| c == '"' && !l);
            let anywhere =
                matches!(chars.first(), Some(('*', false))) && first_quote.is_some_and(|q| q > 0);
            let trailing_star = matches!(chars.last(), Some(('*', false)))
                && last_quote.is_some_and(|q| q + 1 < chars.len());
            let inner: Vec<char> = phrase_chars(
                form,
                chars
                    .iter()
                    .enumerate()
                    .filter(|&(i, &(c, literal))| {
                        !(c == '"' && !literal)
                            && !(anywhere && i == 0)
                            && !(trailing_star && i + 1 == chars.len())
                    })
                    .map(|(_, &(c, _))| c),
            );
            terms.push(Term::Phrase {
                text: inner,
                anywhere,
                open_end: !anywhere || trailing_star,
            });
        } else if chars.iter().any(|&(c, l)| is_wildcard(c, l)) {
            terms.push(Term::Pattern(pattern_of(form, chars)));
        } else {
            let plain: String = chars.iter().map(|&(c, _)| c).collect();
            for word in words(&plain) {
                terms.push(if whole_word {
                    Term::Word(whole_key(form, word))
                } else {
                    Term::Prefix(prefix_key(form, word))
                });
            }
        }
    }
    terms
}

/// A word key starts with `prefix` (a word index prefix range,
/// `[key, key ff fe)`).
fn starts_with_key(word: &[u8], prefix: &[u8]) -> bool {
    word.starts_with(prefix)
}

/// A value as the terms of a text criterion read it: its words, each
/// word's key and characters computed the first time a term needs them and
/// kept for the next term, so that a criterion of many terms reads the
/// value once rather than once per term.
struct ValueWords<'a> {
    form: TextKeys,
    value: &'a str,
    words: Vec<&'a str>,
    keys: Vec<Option<Vec<u8>>>,
    units: Vec<Option<KeyedChars>>,
    /// The value as phrases compare it ([`phrase_view`]).
    lowered: Option<PhraseView>,
}

impl<'a> ValueWords<'a> {
    fn new(form: TextKeys, value: &'a str) -> Self {
        let words = words(value);
        let count = words.len();
        ValueWords {
            form,
            value,
            words,
            keys: vec![None; count],
            units: vec![None; count],
            lowered: None,
        }
    }

    /// Whether the key of a word of the value passes `test`.
    fn any_key(&mut self, test: impl Fn(&[u8]) -> bool) -> bool {
        let form = self.form;
        (0..self.words.len())
            .any(|i| test(self.keys[i].get_or_insert_with(|| whole_key(form, self.words[i]))))
    }

    fn matches(&mut self, term: &Term) -> bool {
        match term {
            Term::Prefix(prefix) => self.any_key(|key| starts_with_key(key, prefix)),
            Term::Word(word) => self.any_key(|key| key == word.as_slice()),
            // A pattern of wildcards alone matches any word.
            Term::Pattern(pattern) => (0..self.words.len()).any(|i| {
                let form = self.form;
                pattern_matches(
                    pattern,
                    self.units[i].get_or_insert_with(|| units(form, self.words[i])),
                )
            }),
            Term::Phrase {
                text,
                anywhere,
                open_end,
            } => {
                let (form, value) = (self.form, self.value);
                let lowered = self.lowered.get_or_insert_with(|| phrase_view(form, value));
                phrase_matches(text, *anywhere, *open_end, lowered)
            }
        }
    }
}

/// The lines of a value: the value index keys each line (query-program.md
/// 7.1).
fn lines(value: &str) -> impl Iterator<Item = &str> {
    value.split(['\r', '\n'])
}

/// Kana compared by `~`: hiragana and katakana, voiced, semi-voiced and
/// unvoiced, small and regular kana, and the iteration marks are the same
/// (Claris "Finding text and characters").
pub(crate) fn relax(text: &str) -> String {
    const VOICED: &str = "がぎぐげござじずぜぞだぢづでどばびぶべぼぱぴぷぺぽゔゞ";
    const PLAIN: &str = "かきくけこさしすせそたちつてとはひふへほはひふへほうゝ";
    const SMALL: &str = "ぁぃぅぇぉっゃゅょゎゕゖ";
    const REGULAR: &str = "あいうえおつやゆよわかけ";
    text.chars()
        .map(|c| {
            // Katakana to hiragana.
            let c = match c {
                '\u{30a1}'..='\u{30f6}' | '\u{30fd}'..='\u{30fe}' => {
                    char::from_u32(c as u32 - 0x60).unwrap_or(c)
                }
                c => c,
            };
            if let Some(i) = VOICED.chars().position(|v| v == c) {
                return PLAIN.chars().nth(i).unwrap_or(c);
            }
            if let Some(i) = SMALL.chars().position(|s| s == c) {
                return REGULAR.chars().nth(i).unwrap_or(c);
            }
            c
        })
        .collect()
}

/// A character `?` finds in text: a control character other than tab and
/// line breaks, the replacement character or a noncharacter (inferred: the
/// characters FileMaker "displays as blanks").
fn invalid_char(c: char) -> bool {
    (c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
        || c == '\u{fffd}'
        || matches!(c as u32, 0xfdd0..=0xfdef)
        || (c as u32) & 0xfffe == 0xfffe
}

// ----- criteria ---------------------------------------------------------------------

/// The `!` and `~` a criterion starts with, by what they mean.
///
/// Each applies to the rest of the criterion, which may start with another
/// one, but a run of them comes down to at most three: `~` relaxes the
/// value, and relaxing it again changes nothing ([`relax`] maps every
/// character it changes to one it leaves alone), and `!` asks whether
/// another record holds the value, the same question however often it is
/// asked of the same value. So a run is a `!` on the value as stored, a
/// `~`, and a `!` on the relaxed value, each there or not, and reads as
/// those whatever its length; `!!x` finds what `!x` finds.
#[derive(Default)]
struct Chain {
    /// A `!` before any `~`.
    duplicates: bool,
    /// A `~` on a text field (elsewhere `~` is ignored).
    relaxed: bool,
    /// A `!` after a `~`.
    relaxed_duplicates: bool,
}

impl Chain {
    fn add_duplicates(&mut self) {
        if self.relaxed {
            self.relaxed_duplicates = true;
        } else {
            self.duplicates = true;
        }
    }

    /// The criterion this run applies to `operand`; `None` for the run
    /// alone (it ended with a lone `!`).
    fn apply(self, operand: Option<Kind>, data_type: DataType, text: TextKeys) -> Option<Kind> {
        let boxed = move |kind| {
            Box::new(Criterion {
                data_type,
                kind,
                text,
            })
        };
        let mut kind = operand;
        if self.relaxed_duplicates {
            kind = Some(Kind::Duplicates(kind.map(boxed)));
        }
        if self.relaxed {
            kind = kind.map(|kind| Kind::Relaxed(boxed(kind)));
        }
        if self.duplicates {
            kind = Some(Kind::Duplicates(kind.map(boxed)));
        }
        kind
    }
}

impl Criterion {
    /// The criterion typed into a field of `data_type`; `None` when the text
    /// is empty (no criterion). Text the field's type cannot read, and text
    /// longer than [`MAX_CRITERION_CHARS`], is [`Unreadable`].
    pub fn parse(
        text: &str,
        data_type: DataType,
        ctx: &FindContext,
    ) -> Result<Option<Criterion>, Unreadable> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        if text.chars().nth(MAX_CRITERION_CHARS).is_some() {
            return Err(Unreadable);
        }
        let kind = Self::kind(text, data_type, ctx)?;
        Ok(Some(Criterion {
            data_type,
            kind,
            text: ctx.text,
        }))
    }

    /// The kind of `text`: its leading `!` and `~`, read in a loop
    /// ([`Chain`]), around the rest.
    fn kind(text: &str, data_type: DataType, ctx: &FindContext) -> Result<Kind, Unreadable> {
        let mut chain = Chain::default();
        let mut rest = text.trim();
        let operand = loop {
            match rest {
                "=" => break Some(Kind::Empty),
                "==" => break Some(Kind::EntirelyEmpty),
                "*" => break Some(Kind::NotEmpty),
                "?" => break Some(Kind::Invalid),
                "!" => {
                    chain.add_duplicates();
                    break None;
                }
                _ => {}
            }
            if data_type == DataType::Container {
                return Err(Unreadable);
            }
            if let Some(after) = rest.strip_prefix('!') {
                chain.add_duplicates();
                rest = after.trim();
            } else if let Some(after) = rest.strip_prefix('~') {
                chain.relaxed |= data_type == DataType::Text;
                rest = after.trim();
            } else if chain.relaxed {
                // `relax` changes only kana, never an operator or a space,
                // so relaxing the operand alone reads as relaxing the text
                // after the first `~`.
                break Some(Self::operand(&relax(rest), data_type, ctx)?);
            } else {
                break Some(Self::operand(rest, data_type, ctx)?);
            }
        };
        chain.apply(operand, data_type, ctx.text).ok_or(Unreadable)
    }

    /// The kind of a criterion without a leading `!` or `~`.
    fn operand(text: &str, data_type: DataType, ctx: &FindContext) -> Result<Kind, Unreadable> {
        for (operator, inclusive, upper) in [
            (">=", true, false),
            ("≥", true, false),
            ("<=", true, true),
            ("≤", true, true),
            (">", false, false),
            ("<", false, true),
        ] {
            if let Some(value) = text.strip_prefix(operator) {
                let (low, high) = Self::bounds(value, data_type, ctx)?;
                return Ok(if upper {
                    // `<x`: before x's first value; `<=x`: up to its last.
                    let bound = if inclusive { high } else { low };
                    Kind::Range {
                        low: None,
                        high: Some((bound, inclusive)),
                    }
                } else {
                    let bound = if inclusive { low } else { high };
                    Kind::Range {
                        low: Some((bound, inclusive)),
                        high: None,
                    }
                });
            }
        }
        let date_like = matches!(
            data_type,
            DataType::Date | DataType::Time | DataType::Timestamp
        );
        if let Some((left, right)) = split_range(text) {
            let weekdays = date_like && day_name(left).is_some() && day_name(right).is_some();
            if !weekdays {
                let (low, _) = Self::bounds(left, data_type, ctx)?;
                let (_, high) = Self::bounds(right, data_type, ctx)?;
                return Ok(Kind::Range {
                    low: Some((low, true)),
                    high: Some((high, true)),
                });
            }
        }
        match data_type {
            DataType::Text | DataType::Unknown(_) => {
                if let Some(rest) = text.strip_prefix("==") {
                    let chars = literal_chars(rest.trim());
                    let wild = chars.iter().any(|&(c, l)| is_wildcard(c, l));
                    return Ok(Kind::Whole(if wild {
                        Whole::Pattern(pattern_of(ctx.text, &chars))
                    } else {
                        let plain: String = chars.iter().map(|&(c, _)| c).collect();
                        Whole::Key(whole_key(ctx.text, plain.trim()))
                    }));
                }
                Ok(Kind::Terms(text_terms(ctx.text, text)))
            }
            DataType::Number => {
                let value = text
                    .strip_prefix("==")
                    .or(text.strip_prefix('='))
                    .unwrap_or(text);
                if value.contains(['*', '@', '#']) {
                    // A number's characters, whatever the form of text.
                    return Ok(Kind::Whole(Whole::Pattern(pattern_of(
                        TextKeys::Collated,
                        &value.chars().map(|c| (c, false)).collect::<Vec<_>>(),
                    ))));
                }
                Number::parse(value).map(Kind::Number).ok_or(Unreadable)
            }
            DataType::Date => date_pattern(text, ctx)
                .map(|p| Kind::Moment(Moment::Date(p)))
                .ok_or(Unreadable),
            DataType::Time => time_pattern(text.trim_start_matches('='))
                .map(|p| Kind::Moment(Moment::Time(p)))
                .ok_or(Unreadable),
            DataType::Timestamp => stamp_pattern(text, ctx)
                .map(|(d, t)| Kind::Moment(Moment::Stamp(d, t)))
                .ok_or(Unreadable),
            DataType::Container => Err(Unreadable),
        }
    }

    /// A range operand's first and last value (a partial date covers its
    /// days, an hour its seconds).
    fn bounds(
        text: &str,
        data_type: DataType,
        ctx: &FindContext,
    ) -> Result<(Bound, Bound), Unreadable> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Unreadable);
        }
        match data_type {
            DataType::Number => {
                let n = Number::parse(text).ok_or(Unreadable)?;
                Ok((Bound::Number(n.clone()), Bound::Number(n)))
            }
            DataType::Date => {
                let (low, high) = date_pattern(text, ctx)
                    .and_then(|p| p.bounds())
                    .ok_or(Unreadable)?;
                Ok((Bound::Date(low), Bound::Date(high)))
            }
            DataType::Time => {
                let p = time_pattern(text).ok_or(Unreadable)?;
                Ok((Bound::Time(p.earliest()), Bound::Time(p.latest())))
            }
            DataType::Timestamp => {
                let (date, time) = stamp_pattern(text, ctx).ok_or(Unreadable)?;
                let (low, high) = date.bounds().ok_or(Unreadable)?;
                Ok((
                    Bound::Stamp(low, time.earliest()),
                    Bound::Stamp(high, time.latest()),
                ))
            }
            DataType::Container => Err(Unreadable),
            DataType::Text | DataType::Unknown(_) => {
                let key = whole_key(ctx.text, text.trim_matches('"'));
                Ok((Bound::Word(key.clone()), Bound::Word(key)))
            }
        }
    }

    /// The field type the criterion was read for.
    pub fn data_type(&self) -> DataType {
        self.data_type
    }

    /// Whether the criterion asks for duplicates (`!`), so that matching
    /// needs to know which values other records hold.
    pub fn finds_duplicates(&self) -> bool {
        match &self.kind {
            Kind::Duplicates(_) => true,
            Kind::Relaxed(inner) => inner.finds_duplicates(),
            _ => false,
        }
    }

    /// Whether a record whose stored value of the field is `value` (its
    /// text, repetition 1) matches. `shared` tells whether another record
    /// holds the same value (`!`).
    pub fn matches(&self, value: &str, order: DateOrder, shared: &dyn Fn(&str) -> bool) -> bool {
        let empty = value.trim().is_empty();
        match &self.kind {
            Kind::Empty => empty || lines(value).any(|l| l.trim().is_empty()),
            Kind::EntirelyEmpty => value.is_empty(),
            Kind::NotEmpty => lines(value).any(|l| !l.trim().is_empty()),
            Kind::Invalid => {
                !empty
                    && match self.data_type {
                        DataType::Number => Number::parse(value).is_none(),
                        DataType::Date => read_date(value, order).is_none(),
                        DataType::Time => read_time(value).is_none(),
                        DataType::Timestamp => read_stamp(value, order).is_none(),
                        _ => value.chars().any(invalid_char),
                    }
            }
            Kind::Duplicates(rest) => {
                !empty
                    && shared(value)
                    && rest
                        .as_ref()
                        .map_or(true, |rest| rest.matches(value, order, shared))
            }
            Kind::Terms(terms) => {
                !terms.is_empty() && {
                    let mut words = ValueWords::new(self.text, value);
                    terms.iter().all(|term| words.matches(term))
                }
            }
            Kind::Whole(Whole::Key(key)) => {
                lines(value).any(|l| whole_key(self.text, l.trim()) == *key)
            }
            Kind::Whole(Whole::Pattern(pattern)) => {
                lines(value).any(|l| pattern_matches(pattern, &units(self.text, l.trim())))
            }
            Kind::Number(n) => Number::parse(value).is_some_and(|v| v == *n),
            Kind::Range { low, high } => self.in_range(value, order, low.as_ref(), high.as_ref()),
            Kind::Moment(moment) => match moment {
                Moment::Date(p) => read_date(value, order).is_some_and(|d| p.matches(d)),
                Moment::Time(p) => read_time(value).is_some_and(|t| p.matches(t)),
                Moment::Stamp(d, t) => read_stamp(value, order)
                    .is_some_and(|(date, time)| d.matches(date) && t.matches(time)),
            },
            Kind::Relaxed(inner) => inner.matches(&relax(value), order, shared),
        }
    }

    fn in_range(
        &self,
        value: &str,
        order: DateOrder,
        low: Option<&(Bound, bool)>,
        high: Option<&(Bound, bool)>,
    ) -> bool {
        let within = |ordering_low: Option<Ordering>, ordering_high: Option<Ordering>| {
            let above = match (low, ordering_low) {
                (None, _) => true,
                (Some((_, inclusive)), Some(o)) => {
                    o == Ordering::Greater || *inclusive && o == Ordering::Equal
                }
                (Some(_), None) => false,
            };
            let below = match (high, ordering_high) {
                (None, _) => true,
                (Some((_, inclusive)), Some(o)) => {
                    o == Ordering::Less || *inclusive && o == Ordering::Equal
                }
                (Some(_), None) => false,
            };
            above && below
        };
        let compare = |bound: Option<&(Bound, bool)>, value: &Bound| -> Option<Ordering> {
            let (bound, _) = bound?;
            match (value, bound) {
                (Bound::Number(v), Bound::Number(b)) => Some(v.cmp(b)),
                (Bound::Word(v), Bound::Word(b)) => Some(v.cmp(b)),
                (Bound::Date(v), Bound::Date(b)) => Some(v.cmp(b)),
                (Bound::Time(v), Bound::Time(b)) => Some(v.cmp(b)),
                (Bound::Stamp(vd, vt), Bound::Stamp(bd, bt)) => Some((vd, vt).cmp(&(bd, bt))),
                _ => None,
            }
        };
        let test = |v: Bound| within(compare(low, &v), compare(high, &v));
        if value.trim().is_empty() {
            return false;
        }
        match self.data_type {
            DataType::Number => Number::parse(value).is_some_and(|n| test(Bound::Number(n))),
            DataType::Date => read_date(value, order).is_some_and(|d| test(Bound::Date(d))),
            DataType::Time => read_time(value).is_some_and(|t| test(Bound::Time(t))),
            DataType::Timestamp => {
                read_stamp(value, order).is_some_and(|(d, t)| test(Bound::Stamp(d, t)))
            }
            DataType::Container => false,
            DataType::Text | DataType::Unknown(_) => words(value)
                .iter()
                .any(|w| test(Bound::Word(whole_key(self.text, w)))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: Ymd = (2019, 4, 4);

    fn ctx() -> FindContext {
        FindContext::new(DateOrder::MonthDayYear, Some(TODAY))
    }

    fn finds(data_type: DataType, criterion: &str, value: &str) -> bool {
        Criterion::parse(criterion, data_type, &ctx())
            .unwrap_or_else(|_| panic!("{criterion:?} parses"))
            .unwrap()
            .matches(value, DateOrder::MonthDayYear, &|_| false)
    }

    fn text(criterion: &str, value: &str) -> bool {
        finds(DataType::Text, criterion, value)
    }

    #[test]
    fn text_operators_follow_the_claris_examples() {
        // Plain words start words of the field, in any order.
        for value in [
            "Chris Smith",
            "Smith Chris",
            "Chris Smithson",
            "Smith Christenson",
        ] {
            assert!(text("Chris Smith", value), "{value}");
        }
        assert!(!text("Chris Smith", "Chris Jones"));
        // Quoted phrases from a word's start, punctuation included.
        assert!(text(
            "\"Marten and Jones Interiors\"",
            "Marten and Jones Interiors"
        ));
        assert!(!text(
            "\"Marten and Jones Interiors\"",
            "Jones and Marten Interiors"
        ));
        assert!(text("\", Ltd.\"", "Acme, Ltd."));
        assert!(!text("\", Ltd.\"", "Acme Ltd."));
        assert!(text("\"Spring\"", "Springville"));
        assert!(!text("\"Spring\"", "ColdSpring Harbor"));
        assert!(!text("\"Spring\"", "HotSpring"));
        // @ is one character, # one digit, * any characters.
        assert!(text("Gr@y", "Gray") && text("Gr@y", "Grey"));
        assert!(text("@on", "Don") && text("@on", "Ron") && !text("@on", "Bron"));
        assert!(text("#", "3") && !text("#", "30"));
        assert!(text("##", "30") && !text("##", "3") && !text("##", "300"));
        assert!(text("#3", "53") && text("#3", "43") && !text("#3", "3"));
        assert!(text("Jo*n", "Jon") && text("Jo*n", "John"));
        assert!(text("J*r", "Jr.") && text("J*r", "Junior"));
        assert!(text("*phan*", "Phan") && text("*phan*", "Stephanie"));
        for value in ["Sophie", "Steve", "Sven"] {
            assert!(text("S*", value));
        }
        // Literal operators, punctuation and spaces in quotes; \ escapes.
        assert!(text("\"@\"", "joey@abc.net") && !text("\"@\"", "joey"));
        assert!(text("\",\"", "a, b") && !text("\",\"", "a b"));
        assert!(text("\"   \"", "a   b") && !text("\"   \"", "a b"));
        assert!(text("\\\"Joey\\\"", "\"Joey\""));
        assert!(text("joey\\@abc.net", "joey@abc.net"));
        // Quotes keep accents; plain words do not.
        assert!(text("\"òpera\"", "òpera") && !text("\"òpera\"", "opera"));
        assert!(text("òpera", "opera") && text("òpera", "òpera"));
        // A leading * finds a phrase anywhere, ending at a word boundary
        // unless a * follows.
        assert!(text("*\"son & Phil\"*", "Johnson & Phillips"));
        assert!(text("*\"son & Phil\"*", "Paulson & Philson"));
        assert!(text("*\"son & Phillips\"", "Johnson & Phillips"));
        assert!(!text("*\"son & Phillips\"", "Paulson & Philson"));
        assert!(!text("*\"Phil\"", "Phillips") && text("*\"Phil\"*", "Phillips"));
        // Phrases start and end where the word index's words do (7.1).
        assert!(!text("\"t\"", "don't") && text("\"don't\"", "don't"));
        assert!(text(
            "\"\u{30c6}\u{30ad}\u{30b9}\u{30c8}\"",
            "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8}"
        ));
        // == the whole field, = whole words.
        assert!(text("==John", "John") && !text("==John", "John Smith"));
        assert!(text("==John Smith", "John Smith"));
        assert!(!text("==John Smith", "Smith, John") && !text("==John Smith", "John Smithers"));
        for value in ["Market", "Market Services", "Ongoing Market Research"] {
            assert!(text("=Market", value), "{value}");
        }
        assert!(!text("=Market", "Marketing") && !text("=Market", "Supermarket"));
        assert!(text("=Chris =Smith", "Smith Chris"));
        assert!(!text("=Chris =Smith", "Chris") && !text("=Chris =Smith", "Christopher Smithson"));
        // Case is ignored.
        assert!(text("chris", "CHRIS"));
    }

    #[test]
    fn empty_duplicates_invalid_and_relaxed() {
        assert!(text("=", "") && text("=", "a\r") && !text("=", "a"));
        assert!(text("==", "") && !text("==", "\r") && !text("==", "a"));
        assert!(text("*", "a") && !text("*", ""));
        let dup = Criterion::parse("!", DataType::Text, &ctx())
            .unwrap()
            .unwrap();
        assert!(dup.matches("ABC Company", DateOrder::MonthDayYear, &|_| true));
        assert!(!dup.matches("ABC Company", DateOrder::MonthDayYear, &|_| false));
        assert!(!dup.matches("", DateOrder::MonthDayYear, &|_| true));
        let only = Criterion::parse("!ABC", DataType::Text, &ctx())
            .unwrap()
            .unwrap();
        assert!(only.matches("ABC Company", DateOrder::MonthDayYear, &|_| true));
        assert!(!only.matches("DEF Ltd.", DateOrder::MonthDayYear, &|_| true));
        // ? finds what the type cannot read.
        assert!(finds(DataType::Number, "?", "twelve"));
        assert!(!finds(DataType::Number, "?", "12") && !finds(DataType::Number, "?", "twelve30"));
        assert!(
            finds(DataType::Date, "?", "Next Tuesday") && finds(DataType::Date, "?", "2/33/2019")
        );
        assert!(finds(DataType::Time, "?", "midnight") && !finds(DataType::Time, "?", "12:00:00"));
        assert!(text("?", "a\u{1}b") && !text("?", "ab"));
        // ~ ignores kana form, voicing and size.
        assert!(text("~は", "バス") && text("~は", "パン") && text("~は", "はし"));
        assert_eq!(relax("ガッコウ"), "かつこう");
    }

    #[test]
    fn numbers_compare_by_value() {
        let number = |c, v| finds(DataType::Number, c, v);
        assert!(number(".50", ".5") && number(".50", ".50") && number(".50", "$.50"));
        assert!(!number(".50", "5"));
        assert!(number("1", "1") && number("0", "0") && !number("1", "10"));
        assert!(number("<40", "39.9") && !number("<40", "40"));
        assert!(number("<=40", "40") && number(">=100", "100") && !number(">100", "100"));
        assert!(number("-5...5", "-2") && number("-5...5", "5") && !number("-5...5", "-6"));
        assert!(number("#", "3") && !number("#", "30"));
        assert!(Criterion::parse("abc", DataType::Number, &ctx()).is_err());
    }

    #[test]
    fn dates_times_and_timestamps() {
        let date = |c, v| finds(DataType::Date, c, v);
        assert!(date("3/3/2019", "3/3/2019") && !date("3/3/2019", "3/4/2019"));
        assert!(date("//", "4/4/2019") && !date("//", "4/5/2019"));
        // 4/2/2019 was a Tuesday.
        assert!(date("Tuesday", "4/2/2019") && !date("Tuesday", "4/3/2019"));
        assert!(date("=Thu", "4/4/2019") && date("Mon..Fri", "4/5/2019"));
        assert!(!date("Mon..Fri", "4/6/2019"));
        assert!(
            date("5/12/*", "5/12/1999") && date("5/12", "5/12/2019") && !date("5/12", "5/12/2018")
        );
        assert!(date("6/2019", "6/30/2019") && !date("6/2019", "7/1/2019"));
        assert!(date("2019", "12/31/2019") && !date("2019", "1/1/2020"));
        assert!(date("7/{1...15}/2019", "7/15/2019") && !date("7/{1...15}/2019", "7/16/2019"));
        assert!(date("{1..3}/{10..16}/2020", "2/14/2020"));
        assert!(date("*/{10..15}/2019", "8/12/2019"));
        assert!(date("3/*/2019", "3/31/2019"));
        assert!(date("7/2019...10/2020", "10/31/2020") && !date("7/2019...10/2020", "6/30/2019"));
        assert!(date("<9/7/2019", "9/6/2019") && !date("<9/7/2019", "9/7/2019"));
        assert!(date(">=9/7/2019", "9/7/2019"));
        assert!(date("<=6/2019", "6/30/2019") && !date(">6/2019", "6/30/2019"));
        assert!(date("1/1/2019..6/6/2020", "1/1/2020"));
        let time = |c, v| finds(DataType::Time, c, v);
        assert!(time("12:53:09", "12:53:09") && !time("12:53:09", "12:53:10"));
        assert!(time("*:15", "7:15:30") && !time("*:15", "7:16:00"));
        assert!(time("3 PM", "15:59:59") && !time("3 PM", "16:00:00"));
        assert!(time("AM", "11:59:59") && !time("AM", "12:00:00"));
        assert!(time("8 AM...8 PM", "20:59:59") && !time("8 AM...8 PM", "21:00:00"));
        assert!(time("{4..6}:30 PM", "17:30:00") && !time("{4..6}:30 PM", "17:31:00"));
        assert!(time("12:{30..45}", "12:40:00") && time("≥8:00", "8:00:00"));
        assert!(time("≤05:00:00", "5:00:00") && time("12:30...17:30", "5:30:00 PM"));
        let stamp = |c, v| finds(DataType::Timestamp, c, v);
        assert!(stamp("3/3/2019 12:59:09 PM", "3/3/2019 12:59:09 PM"));
        assert!(
            stamp("1/1/* 7 PM", "1/1/2000 7:30:00 PM")
                && !stamp("1/1/* 7 PM", "1/1/2000 8:00:00 PM")
        );
        assert!(stamp("// 3 PM", "4/4/2019 15:10:00"));
        assert!(stamp("5/2019 7 PM", "5/31/2019 19:00:00"));
        // 6/1/2020 was a Monday.
        assert!(stamp("=Mon 2020", "6/1/2020 1:00:00") && !stamp("=Mon 2020", "6/1/2021 1:00:00"));
        assert!(stamp("11/{10..16}/2020 {3..5} PM", "11/12/2020 17:59:00"));
        assert!(stamp("2019 *:30 PM", "4/4/2019 13:30:00"));
        assert!(Criterion::parse("someday", DataType::Date, &ctx()).is_err());
    }

    #[test]
    fn text_ranges_compare_words() {
        assert!(text("<M", "Smith Adams") && !text("<M", "Smith"));
        assert!(text("A...M", "Lee") && !text("A...M", "Zed"));
        assert!(text(">=M", "Maria"));
    }

    #[test]
    fn calendar_helpers() {
        assert_eq!(weekday((1970, 1, 1)), 4);
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(
            civil_date(days_from_civil((2024, 2, 29)) * 86_400 + 5),
            (2024, 2, 29)
        );
        assert_eq!(weekday((2019, 4, 4)), 4);
        assert_eq!(days_in_month(2020, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(
            read_date("2026-10-28", DateOrder::MonthDayYear),
            Some((2026, 10, 28))
        );
        assert_eq!(read_time("9:26:57 PM"), Some(77217));
        assert_eq!(full_year("19", &ctx()), Some(2019));
        assert_eq!(full_year("70", &ctx()), Some(1970));
    }

    /// Runs `f` on a thread with a stack far smaller than the 2 MiB a
    /// server script's thread has, so that reading or matching that
    /// recursed with a criterion's length would overflow it (aborting the
    /// test binary) at lengths a test reaches quickly.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn(f)
            .expect("a thread")
            .join()
            .expect("no panic")
    }

    #[test]
    fn hostile_criteria_take_bounded_stack_and_time() {
        on_small_stack(|| {
            let parse = |text: &str, data_type| Criterion::parse(text, data_type, &ctx());
            let long = MAX_CRITERION_CHARS - 2;
            // A run of `!` and `~` of any length reads as the operators it
            // amounts to.
            for (data_type, criterion, same_as) in [
                (DataType::Text, format!("{}x", "!".repeat(long)), "!x"),
                (DataType::Text, format!("{}は", "~".repeat(long)), "~は"),
                (
                    DataType::Text,
                    format!("{}x", "!~".repeat(long / 2)),
                    "!~!x",
                ),
                (DataType::Text, format!("{}!", "~ ".repeat(long / 2)), "~!"),
                (
                    DataType::Number,
                    format!("{}5", "~!".repeat(long / 2)),
                    "!5",
                ),
            ] {
                assert_eq!(parse(&criterion, data_type), parse(same_as, data_type));
            }
            let deep = parse(&format!("{}x", "!".repeat(long)), DataType::Text)
                .unwrap()
                .unwrap();
            assert!(deep.matches("x", DateOrder::MonthDayYear, &|_| true));
            assert!(!deep.matches("x", DateOrder::MonthDayYear, &|_| false));
            // Many `*`, before and between characters.
            let stars = format!("{}h", "*".repeat(long));
            assert!(text(&stars, "h") && text(&stars, "xh") && !text(&stars, "hx"));
            let a60 = "a".repeat(60);
            let between = format!("=={}*b", "*a".repeat(20));
            assert!(!text(&between, &a60) && text(&between, &format!("{a60}b")));
            // Long values, and a long pattern that nearly matches at every
            // position (the matcher's worst case, its length times the
            // value's).
            let h = "h".repeat(20_000);
            assert!(text("*h", &h) && !text("*h", &format!("{h}x")) && text("==h*", &h));
            let near = format!("==*{}b", "a".repeat(1_000));
            assert!(!text(&near, &"a".repeat(4_000)));
            assert!(text(&near, &format!("{}b", "a".repeat(4_000))));
            // Longer criteria are unreadable; characters count, not bytes.
            let at_limit = "é".repeat(MAX_CRITERION_CHARS);
            assert!(parse(&format!("  {at_limit}\r"), DataType::Text).is_ok());
            assert!(parse(&format!("{at_limit}é"), DataType::Text).is_err());
        });
    }

    #[test]
    fn criteria_of_operators_and_fragments_never_panic() {
        let texts =
            "!|~|!~|~!|! ~ !|=|==|===|*|**|?|@|#|\\|\"|\"\"|*\"|\"*|*\"\"*|=\"|<|<=|≤|>|≥|<=…|\
            ...|..|…|a…|…b|é…é|{|}|{..}|{1..}|1/{2..|{3..1}/2020|//|/|//2|1//|AM| PM|12:|:|::|\
            1:2:3:4|13 PM|é|ガ|~ガ|!é|\u{fffd}|\u{1}|0e|-|.|-.|1e5|Mon..|..Fri|=Mon 2020 3|\
            a\rb|==*é|=é=|\"é|é\\";
        let values = ["", "é", "ガ", "1", "a\rb", "3/3/2019", "7:15 PM", "x\u{1}"];
        on_small_stack(move || {
            for data_type in [
                DataType::Text,
                DataType::Number,
                DataType::Date,
                DataType::Time,
                DataType::Timestamp,
                DataType::Container,
                DataType::Unknown(9),
            ] {
                for text in texts.split('|') {
                    let Ok(Some(criterion)) = Criterion::parse(text, data_type, &ctx()) else {
                        continue;
                    };
                    crate::find::plan(1, 1, &criterion);
                    for value in values.iter().chain([&text]) {
                        criterion.matches(value, DateOrder::MonthDayYear, &|_| true);
                    }
                }
            }
        });
    }

    /// The recursive matcher [`pattern_matches`] replaced, kept as the
    /// reference for its semantics: it recursed once per unit and was
    /// exponential in the number of `*`.
    fn reference_matches(pattern: &[Unit], word: &[(char, Vec<u8>)]) -> bool {
        match pattern.split_first() {
            None => word.is_empty(),
            Some((Unit::Any, rest)) => {
                (0..=word.len()).any(|skip| reference_matches(rest, &word[skip..]))
            }
            Some((unit, rest)) => {
                let Some(((c, key), tail)) = word.split_first() else {
                    return false;
                };
                let ok = match unit {
                    Unit::Char(expected) => key == expected,
                    Unit::One => true,
                    Unit::Digit => c.is_numeric(),
                    Unit::Any => unreachable!(),
                };
                ok && reference_matches(rest, tail)
            }
        }
    }

    #[test]
    fn wildcard_matching_agrees_with_the_recursive_reference() {
        let pattern = |text: &str| {
            pattern_of(
                TextKeys::Collated,
                &text.chars().map(|c| (c, false)).collect::<Vec<_>>(),
            )
        };
        // The patterns and words of the Claris examples above, and others.
        let mut patterns: Vec<String> = [
            "Gr@y", "@on", "#", "##", "#3", "Jo*n", "J*r", "*phan*", "S*", "", "*", "**", "*a",
            "a*", "*a*b", "a*b*", "@*", "*@", "#*#", "*ab*ab", "É*",
        ]
        .map(String::from)
        .to_vec();
        let mut words: Vec<String> = "Gray Grey Don Ron Bron 3 30 300 53 43 Jon John Jr Junior \
            Phan Stephanie Sophie Steve Sven abab aabab e ée"
            .split(' ')
            .map(String::from)
            .collect();
        // Every pattern of up to four of `a`, `1`, `@`, `#` and `*`
        // against every word of up to four of `a`, `b` and `1`.
        let all = |alphabet: &[char], up_to: u32| -> Vec<String> {
            (0..=up_to)
                .flat_map(|len| {
                    let n = alphabet.len();
                    (0..n.pow(len)).map(move |mut i| {
                        (0..len)
                            .map(|_| {
                                let c = alphabet[i % n];
                                i /= n;
                                c
                            })
                            .collect()
                    })
                })
                .collect()
        };
        patterns.extend(all(&['a', '1', '@', '#', '*'], 4));
        words.extend(all(&['a', 'b', '1'], 4));
        let words: Vec<_> = words
            .iter()
            .map(|w| (w, units(TextKeys::Collated, w)))
            .collect();
        for p in &patterns {
            let units_of_p = pattern(p);
            for (w, units_of_w) in &words {
                assert_eq!(
                    pattern_matches(&units_of_p, units_of_w),
                    reference_matches(&units_of_p, units_of_w),
                    "{p:?} on {w:?}"
                );
            }
        }
    }
}
