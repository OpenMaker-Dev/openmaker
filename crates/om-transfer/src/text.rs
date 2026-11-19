//! The text of exported values (oracle x01, x03).
//!
//! A stored field exports the text its record holds: a number field keeps
//! what was entered (`007`, `2.50`, `abc`), a date field typed as `2/29/2024`
//! stays so. Computed values (unstored calculations, summaries) export in
//! the storage forms: numbers as FileMaker writes them (`-.002`), dates
//! `MM/DD/YYYY`, times `HH:MM:SS`, timestamps `MM/DD/YYYY hh:mm:ss AM`, with
//! fractional seconds after the seconds. Comma-separated text writes a
//! number field's number instead of its text (`007` is `7`, `abc` is `0`).

use om_calc::{Date, Locale, Number, Time, Timestamp, Value};

use crate::FieldType;

/// One value (a repetition) of an exported field.
#[derive(Clone, Debug)]
pub enum ExportValue {
    /// A stored field's text as the record holds it.
    Stored(String),
    /// A value computed for the export.
    Computed(Value),
}

impl ExportValue {
    pub fn empty() -> ExportValue {
        ExportValue::Stored(String::new())
    }

    pub fn is_empty(&self) -> bool {
        match self {
            ExportValue::Stored(s) => s.is_empty(),
            ExportValue::Computed(v) => v.is_empty(),
        }
    }

    /// The text tab-separated, merge, HTML and XML files carry.
    pub fn text(&self) -> String {
        match self {
            ExportValue::Stored(s) => s.clone(),
            ExportValue::Computed(v) => computed_text(v),
        }
    }

    /// A number field's value as comma-separated text writes it: the
    /// number, `""` when empty.
    pub fn number_text(&self) -> String {
        match self.number() {
            Some(n) => number_text(&n),
            None => String::new(),
        }
    }

    /// The number a value holds: `None` when empty; non-numeric text is 0.
    pub fn number(&self) -> Option<Number> {
        match self {
            ExportValue::Stored(s) if s.is_empty() => None,
            ExportValue::Stored(s) => Some(parse_number(s)),
            ExportValue::Computed(Value::Number(n))
            | ExportValue::Computed(Value::NumberText(n, _)) => (!n.is_empty()).then(|| n.clone()),
            ExportValue::Computed(v) if v.is_empty() => None,
            ExportValue::Computed(v) => Some(parse_number(&computed_text(v))),
        }
    }
}

/// Text to number as a number field reads it (`1e3` is 1000, `abc` 0).
pub fn parse_number(text: &str) -> Number {
    let n = Number::parse_text(text, '.', ',');
    if n.is_empty() || n.is_nan() {
        Number::zero()
    } else {
        n
    }
}

/// FileMaker's text of a number (`-.002`, `24691357802469135780.25`).
pub fn number_text(n: &Number) -> String {
    n.to_text_with('.')
}

/// The text of a computed value in the storage forms (module docs).
pub fn computed_text(v: &Value) -> String {
    match v {
        Value::Text(t) | Value::NumberText(_, t) => t.as_str().to_owned(),
        Value::Number(n) => number_text(n),
        Value::Date(d) | Value::DateText(d, _) => date_text(*d),
        Value::Time(t) => time_text(*t),
        Value::Timestamp(t) => timestamp_text(*t),
        Value::Container(c) => c.text.as_str().to_owned(),
        Value::Empty(_) => String::new(),
    }
}

/// `MM/DD/YYYY`, four-digit years (`01/01/0001`).
pub fn date_text(d: Date) -> String {
    let (y, m, day) = d.ymd();
    format!("{m:02}/{day:02}/{y:04}")
}

fn seconds_text(seconds: &Number) -> String {
    let text = number_text(seconds);
    // "5.25" -> "05.25", ".5" -> "00.5"
    let int_len = text.find('.').unwrap_or(text.len());
    match int_len {
        0 => format!("00{text}"),
        1 => format!("0{text}"),
        _ => text,
    }
}

/// `HH:MM:SS`, hours past 24 and negative times as they are (`26:01:00`,
/// `-01:00:00`).
pub fn time_text(t: Time) -> String {
    let negative = t.micros() < 0;
    let (h, m, s) = Time::from_micros(t.micros().abs(), t.style()).hms();
    format!(
        "{}{:02}:{:02}:{}",
        if negative { "-" } else { "" },
        h,
        m,
        seconds_text(&s)
    )
}

/// `MM/DD/YYYY hh:mm:ss AM` (`12/31/1999 11:59:59.5 PM`).
pub fn timestamp_text(t: Timestamp) -> String {
    let date = date_text(t.date());
    let (h, m, s) = t.time_of_day().hms();
    let (hour, half) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{date} {hour:02}:{m:02}:{} {half}", seconds_text(&s))
}

/// Parse a stored date (`M/D/YYYY` in the file's order); `None` for text
/// that is not a date.
pub fn parse_date(text: &str, locale: &Locale) -> Option<Date> {
    let cx = om_calc::Ctx::new(locale, Date::from_ymd(2000, 1, 1)?);
    Value::text(text).to_date(&cx).ok().flatten()
}

/// Parse a stored time; `None` for text that is not a time.
pub fn parse_time(text: &str, locale: &Locale) -> Option<Time> {
    let cx = om_calc::Ctx::new(locale, Date::from_ymd(2000, 1, 1)?);
    Value::text(text).to_time(&cx).ok().flatten()
}

/// Parse a stored timestamp; `None` for text that is not one.
pub fn parse_timestamp(text: &str, locale: &Locale) -> Option<Timestamp> {
    let cx = om_calc::Ctx::new(locale, Date::from_ymd(2000, 1, 1)?);
    Value::text(text).to_timestamp(&cx).ok().flatten()
}

/// The value a typed export cell holds (Excel): a number, the day serial
/// of a date or timestamp from 1900 on, the day fraction of a time that is
/// not negative; otherwise text.
#[derive(Clone, Debug, PartialEq)]
pub enum Typed {
    Number(String),
    Text(String),
}

/// The day number of Excel's day 0, 1899-12-30.
fn excel_day0() -> i64 {
    Date::from_ymd(1899, 12, 30).map_or(693_594, Date::day_number)
}

/// The day fraction FileMaker multiplies seconds by: 1/86400 to 15
/// significant digits (its Excel times agree with this to the 15th digit).
fn day_fraction(seconds: f64) -> f64 {
    seconds * 0.000_011_574_074_074_074_1
}

/// A double in FileMaker's number text (no leading zero).
fn double_text(v: f64) -> String {
    let s = format!("{v}");
    let s = s.strip_suffix(".0").unwrap_or(&s).to_owned();
    if let Some(rest) = s.strip_prefix("0.") {
        format!(".{rest}")
    } else if let Some(rest) = s.strip_prefix("-0.") {
        format!("-.{rest}")
    } else {
        s
    }
}

impl ExportValue {
    /// The Excel cell of a value of a field of type `ty`.
    pub fn typed(&self, ty: FieldType, locale: &Locale) -> Option<Typed> {
        if self.is_empty() {
            return None;
        }
        let text = self.text();
        Some(match ty {
            FieldType::Number => Typed::Number(self.number_text()),
            FieldType::Date => match self.date(locale) {
                Some(d) if d.ymd().0 >= 1900 => {
                    Typed::Number((d.day_number() - excel_day0()).to_string())
                }
                _ => Typed::Text(text),
            },
            FieldType::Time => match self.time(locale) {
                Some(t) if t.micros() >= 0 => {
                    Typed::Number(double_text(day_fraction(t.micros() as f64 / 1e6)))
                }
                _ => Typed::Text(text),
            },
            FieldType::Timestamp => match self.timestamp(locale) {
                Some(ts) if ts.date().ymd().0 >= 1900 => {
                    let days = ts.date().day_number() - excel_day0();
                    let secs = ts.time_of_day().micros() as f64 / 1e6;
                    Typed::Number(double_text(days as f64 + day_fraction(secs)))
                }
                _ => Typed::Text(text),
            },
            FieldType::Text | FieldType::Container => Typed::Text(text),
        })
    }

    fn date(&self, locale: &Locale) -> Option<Date> {
        match self {
            ExportValue::Computed(Value::Date(d) | Value::DateText(d, _)) => Some(*d),
            _ => parse_date(&self.text(), locale),
        }
    }

    fn time(&self, locale: &Locale) -> Option<Time> {
        match self {
            ExportValue::Computed(Value::Time(t)) => Some(*t),
            _ => parse_time(&self.text(), locale),
        }
    }

    fn timestamp(&self, locale: &Locale) -> Option<Timestamp> {
        match self {
            ExportValue::Computed(Value::Timestamp(t)) => Some(*t),
            _ => parse_timestamp(&self.text(), locale),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_date_presentation_keeps_typed_export_storage_forms() {
        let date = Date::from_ymd(2026, 1, 2).unwrap();
        let stored = ExportValue::Computed(Value::DateText(date, om_calc::Text::plain("2.1.26")));
        let plain = ExportValue::Computed(Value::Date(date));
        assert_eq!(stored.text(), plain.text());
        assert_eq!(stored.date(&Locale::en_us()), Some(date));
        assert_eq!(stored.date(&Locale::de_de()), Some(date));
    }

    #[test]
    fn storage_forms() {
        let d = Date::from_ymd(1815, 12, 10).unwrap();
        assert_eq!(date_text(d), "12/10/1815");
        assert_eq!(date_text(Date::from_ymd(1, 1, 1).unwrap()), "01/01/0001");
        let t = |s: &str| Time::from_seconds(&Number::parse_text(s, '.', ',')).unwrap();
        assert_eq!(time_text(t("47109")), "13:05:09");
        assert_eq!(time_text(t("93660")), "26:01:00");
        assert_eq!(time_text(t("-3600")), "-01:00:00");
        assert_eq!(time_text(t("0")), "00:00:00");
        let ts =
            Timestamp::from_date_time(Date::from_ymd(1999, 12, 31).unwrap(), t("86399.5")).unwrap();
        assert_eq!(timestamp_text(ts), "12/31/1999 11:59:59.5 PM");
        let ts =
            Timestamp::from_date_time(Date::from_ymd(2020, 1, 2).unwrap(), t("54245")).unwrap();
        assert_eq!(timestamp_text(ts), "01/02/2020 03:04:05 PM");
        let ts = Timestamp::from_date_time(Date::from_ymd(2020, 1, 2).unwrap(), t("300")).unwrap();
        assert_eq!(timestamp_text(ts), "01/02/2020 12:05:00 AM");
    }

    #[test]
    fn numbers_in_comma_separated_text() {
        let n = |s: &str| ExportValue::Stored(s.into()).number_text();
        assert_eq!(n("007"), "7");
        assert_eq!(n("-0.001"), "-.001");
        assert_eq!(n("2.50"), "2.5");
        assert_eq!(n("abc"), "0");
        assert_eq!(n("1e3"), "1000");
        assert_eq!(n(""), "");
        assert_eq!(n("12345678901234567890.125"), "12345678901234567890.125");
    }

    #[test]
    fn excel_cells() {
        let en = Locale::en_us();
        let cell = |s: &str, ty| ExportValue::Stored(s.into()).typed(ty, &en);
        assert_eq!(
            cell("2/29/2024", FieldType::Date),
            Some(Typed::Number("45351".into()))
        );
        assert_eq!(
            cell("12/10/1815", FieldType::Date),
            Some(Typed::Text("12/10/1815".into()))
        );
        assert_eq!(
            cell("-01:00:00", FieldType::Time),
            Some(Typed::Text("-01:00:00".into()))
        );
        assert_eq!(
            cell("00:00:00", FieldType::Time),
            Some(Typed::Number("0".into()))
        );
        assert_eq!(
            cell("abc", FieldType::Number),
            Some(Typed::Number("0".into()))
        );
        assert_eq!(cell("", FieldType::Text), None);
        let Some(Typed::Number(t)) = cell("13:05:09", FieldType::Time) else {
            panic!()
        };
        // FileMaker: .545243055555556805353836
        assert!(t.starts_with(".54524305555555"), "{t}");
    }
}
