//! SQL values (execute-sql.md 5, 6, 8): calculation values with NULL for
//! everything empty, typed parameters, binary text comparison, the sort
//! and duplicate key, and the text of a result.

use std::cmp::Ordering;

use om_calc::{Container, Ctx, Date, Number, Text, Time, Timestamp, Value};

use crate::source::FieldType;

/// The type of an expression, fixed when the statement is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
    Container,
    /// `TRUE`, `FALSE`: numbers that print as `1`/`0` but convert to the
    /// text `True`/`False`.
    Bool,
    /// The untyped `NULL`.
    Null,
    /// A `?` whose type nothing implies.
    Param,
}

impl Ty {
    pub fn of_field(ty: FieldType) -> Ty {
        match ty {
            FieldType::Text => Ty::Text,
            FieldType::Number => Ty::Number,
            FieldType::Date => Ty::Date,
            FieldType::Time => Ty::Time,
            FieldType::Timestamp => Ty::Timestamp,
            FieldType::Container => Ty::Container,
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Ty::Number | Ty::Bool)
    }

    pub fn is_temporal(self) -> bool {
        matches!(self, Ty::Date | Ty::Time | Ty::Timestamp)
    }
}

/// A value.
#[derive(Clone, Debug)]
pub enum Val {
    Null,
    Text(String),
    Number(Number),
    Date(Date),
    Time(Time),
    Timestamp(Timestamp),
    /// A container's text form (the stored file name, 8.1).
    Container(String),
    Bool(bool),
    /// A `?` argument with its calculation type (5.7); never empty.
    Param(Value),
    /// A summary field's value: empty but not NULL (8.1).
    Summary,
}

impl Val {
    /// A field's or argument's calculation value; empty is NULL.
    pub fn from_calc(v: Value) -> Val {
        if v.is_empty() {
            return Val::Null;
        }
        match v {
            Value::Text(t) => Val::Text(t.into_string()),
            Value::Number(n) | Value::NumberText(n, _) => {
                if n.is_empty() {
                    Val::Null
                } else {
                    Val::Number(n)
                }
            }
            Value::Date(d) | Value::DateText(d, _) => Val::Date(d),
            Value::Time(t) => Val::Time(t),
            Value::Timestamp(t) => Val::Timestamp(t),
            Value::Container(c) => Val::Container(c.text.into_string()),
            Value::Empty(_) => Val::Null,
        }
    }

    pub fn is_null(&self) -> bool {
        match self {
            Val::Null => true,
            Val::Text(s) | Val::Container(s) => s.is_empty(),
            Val::Number(n) => n.is_empty(),
            _ => false,
        }
    }

    pub fn number(n: Number) -> Val {
        if n.is_empty() {
            Val::Null
        } else {
            Val::Number(n)
        }
    }

    pub fn text(s: impl Into<String>) -> Val {
        let s = s.into();
        if s.is_empty() {
            Val::Null
        } else {
            Val::Text(s)
        }
    }

    /// The calculation value, for om-calc's functions and operators.
    pub fn to_calc(&self) -> Value {
        match self {
            Val::Null | Val::Summary => Value::empty_text(),
            Val::Text(s) => Value::text(s.as_str()),
            Val::Number(n) => Value::Number(n.clone()),
            Val::Date(d) => Value::Date(*d),
            Val::Time(t) => Value::Time(*t),
            Val::Timestamp(t) => Value::Timestamp(*t),
            // Results carry a container's reference text, not its data.
            Val::Container(s) => Value::Container(Container {
                text: Text::plain(s.as_str()),
                ..Container::default()
            }),
            Val::Bool(b) => Value::boolean(*b),
            Val::Param(v) => v.clone(),
        }
    }

    /// A calculation result as a value of type `ty`: dates before
    /// 0001-01-01 and other results that are not of the type are NULL.
    pub fn from_result(v: Value, ty: Ty) -> Val {
        let val = Val::from_calc(v);
        match (ty, &val) {
            (Ty::Date, Val::Date(_))
            | (Ty::Time, Val::Time(_))
            | (Ty::Timestamp, Val::Timestamp(_)) => val,
            (Ty::Date | Ty::Time | Ty::Timestamp, _) => Val::Null,
            _ => val,
        }
    }

    /// The text of a value in a result (8.2).
    pub fn output(&self, cx: &Ctx) -> String {
        match self {
            Val::Null | Val::Summary => String::new(),
            Val::Text(s) | Val::Container(s) => s.clone(),
            Val::Number(n) => n.to_text_with('.'),
            Val::Date(d) => iso_date(*d),
            Val::Time(t) => iso_time(t.micros()),
            Val::Timestamp(t) => iso_timestamp(*t),
            Val::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            Val::Param(v) => v.to_text(cx).into_string(),
        }
    }

    /// The text `STRVAL` and `CAST(... AS VARCHAR)` give: the output text,
    /// except booleans, which are `True`/`False`.
    pub fn strval(&self, cx: &Ctx) -> String {
        match self {
            Val::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            other => other.output(cx),
        }
    }
}

/// `YYYY-MM-DD`.
pub fn iso_date(d: Date) -> String {
    let (y, m, day) = d.ymd();
    format!("{y:04}-{m:02}-{day:02}")
}

/// `HH:MM:SS[.ffffff]` of elapsed microseconds: at least two hour digits,
/// a leading `-` for negative times, fraction digits without trailing
/// zeros.
pub fn iso_time(micros: i128) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let m = micros.unsigned_abs();
    let secs = m / 1_000_000;
    let frac = m % 1_000_000;
    let (h, mi, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let mut out = format!("{sign}{h:02}:{mi:02}:{s:02}");
    if frac != 0 {
        let f = format!("{frac:06}");
        out.push('.');
        out.push_str(f.trim_end_matches('0'));
    }
    out
}

/// `YYYY-MM-DD HH:MM:SS[.ffffff]`.
pub fn iso_timestamp(t: Timestamp) -> String {
    let time = t.time_of_day();
    format!("{} {}", iso_date(t.date()), iso_time(time.micros()))
}

/// Text in UTF-16 code unit order (6.1).
pub fn text_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Two non-NULL values of comparable types in SQL's order (6.1): text as
/// UTF-16, numbers, dates, times and timestamps by value. `None` when they
/// cannot be compared.
pub fn compare(a: &Val, b: &Val) -> Option<Ordering> {
    match (a, b) {
        (Val::Text(x), Val::Text(y)) => Some(text_cmp(x, y)),
        (Val::Number(_) | Val::Bool(_), Val::Number(_) | Val::Bool(_)) => {
            number_of(a)?.compare(&number_of(b)?)
        }
        (Val::Date(x), Val::Date(y)) => Some(x.day_number().cmp(&y.day_number())),
        (Val::Time(x), Val::Time(y)) => Some(x.micros().cmp(&y.micros())),
        (Val::Timestamp(x), Val::Timestamp(y)) => Some(x.micros().cmp(&y.micros())),
        _ => None,
    }
}

fn number_of(v: &Val) -> Option<Number> {
    match v {
        Val::Number(n) => Some(n.clone()),
        Val::Bool(b) => Some(Number::from_bool(*b)),
        _ => None,
    }
}

/// The sort and duplicate key of a value (6.2).
#[derive(Clone, Debug)]
pub enum Key {
    /// NULL and text whose key is empty: first ascending.
    Empty,
    Text(Vec<u16>),
    Number(Number),
    Micros(i128),
}

impl Key {
    pub fn of(v: &Val) -> Key {
        match v {
            Val::Null | Val::Summary | Val::Container(_) => Key::Empty,
            Val::Text(s) => text_key(s),
            Val::Number(n) => Key::Number(n.clone()),
            Val::Bool(b) => Key::Number(Number::from_bool(*b)),
            Val::Date(d) => Key::Micros(i128::from(d.day_number())),
            Val::Time(t) => Key::Micros(t.micros()),
            Val::Timestamp(t) => Key::Micros(t.micros()),
            // A `?` sorts as its calculation value.
            Val::Param(p) => Key::of(&Val::from_calc(p.clone())),
        }
    }

    pub fn order(&self, other: &Key) -> Ordering {
        match (self, other) {
            (Key::Empty, Key::Empty) => Ordering::Equal,
            (Key::Empty, _) => Ordering::Less,
            (_, Key::Empty) => Ordering::Greater,
            (Key::Text(a), Key::Text(b)) => a.cmp(b),
            (Key::Number(a), Key::Number(b)) => a.compare(b).unwrap_or(Ordering::Equal),
            (Key::Micros(a), Key::Micros(b)) => a.cmp(b),
            (a, b) => a.rank().cmp(&b.rank()),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Key::Empty => 0,
            Key::Number(_) => 1,
            Key::Micros(_) => 2,
            Key::Text(_) => 3,
        }
    }
}

/// Text's key: leading U+0020 and U+3000 dropped, empty when the rest
/// starts a new line, else its first 100 UTF-16 code units.
fn text_key(s: &str) -> Key {
    let rest = s.trim_start_matches([' ', '\u{3000}']);
    if rest.is_empty() || rest.starts_with(['\r', '\n', '\u{2028}']) {
        return Key::Empty;
    }
    Key::Text(rest.encode_utf16().take(100).collect())
}

/// Rows' keys compared column by column.
pub fn keys_cmp(a: &[Key], b: &[Key]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        match x.order(y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

/// `LIKE` (6.1): `%` any run, `_` one character, case-sensitive, nothing
/// else special.
pub fn like(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    // Iterative wildcard matching with backtracking to the last `%`.
    let (mut ti, mut pi) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '_' || (p[pi] != '%' && p[pi] == t[ti])) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '%' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '%' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_order(a: &str, b: &str) -> Ordering {
        Key::of(&Val::text(a)).order(&Key::of(&Val::text(b)))
    }

    #[test]
    fn text_compares_as_utf16() {
        assert_eq!(text_cmp("B", "a"), Ordering::Less);
        assert_eq!(text_cmp("Z", "_"), Ordering::Less);
        assert_eq!(text_cmp("z", "ä"), Ordering::Less);
        assert_eq!(text_cmp("10", "9"), Ordering::Less);
        // A supplementary character is a surrogate pair, below U+E000.
        assert_eq!(text_cmp("😀", "\u{ff5e}"), Ordering::Less);
    }

    #[test]
    fn sort_keys_drop_leading_spaces_and_stop_at_100_units() {
        assert_eq!(key_order(" b", "b"), Ordering::Equal);
        assert_eq!(key_order("\u{3000}b", "b"), Ordering::Equal);
        assert_eq!(key_order("\tb", "b"), Ordering::Less);
        assert_eq!(key_order("\rb", " "), Ordering::Equal);
        assert_eq!(key_order("b\t", "b\n"), Ordering::Less);
        let long = "x".repeat(100);
        assert_eq!(
            key_order(&format!("{long}a"), &format!("{long}b")),
            Ordering::Equal
        );
    }

    #[test]
    fn like_patterns() {
        assert!(like("Checklist", "Check%"));
        assert!(like("Checklist", "%list"));
        assert!(like("abc", "a_c"));
        assert!(!like("abc", "A%"));
        assert!(like("a[b]", "a[b]"));
        assert!(like("", "%"));
        assert!(!like("ab", "a"));
        assert!(like("aXbXc", "%b%c"));
    }

    #[test]
    fn result_text() {
        assert_eq!(iso_time(0), "00:00:00");
        assert_eq!(iso_time(3_723_000_000), "01:02:03");
        assert_eq!(iso_time(360_000_000_000), "100:00:00");
        assert_eq!(iso_time(-3_600_000_000), "-01:00:00");
        assert_eq!(iso_time(-60_000_000), "-00:01:00");
        assert_eq!(iso_time(49_510_500_000), "13:45:10.5");
        assert_eq!(iso_date(Date::from_ymd(1, 1, 1).unwrap()), "0001-01-01");
    }

    #[test]
    fn stored_date_presentation_does_not_change_sql_type_or_key() {
        let date = Date::from_ymd(2026, 1, 2).unwrap();
        let stored = Val::from_calc(Value::DateText(date, om_calc::Text::plain("2.1.26")));
        let plain = Val::Date(date);
        assert!(matches!(stored, Val::Date(d) if d == date));
        assert_eq!(Key::of(&stored).order(&Key::of(&plain)), Ordering::Equal);
    }
}
