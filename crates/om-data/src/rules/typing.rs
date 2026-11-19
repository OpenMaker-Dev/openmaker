//! A user's typing into a date, time or timestamp field, read the way
//! FileMaker reads data entry for those types (schema-path.md 7.2.1, "The
//! rewrite"): the text is cleaned, parsed with the file's locale by
//! om-calc's `GetAsDate` / `GetAsTime` / `GetAsTimestamp` parsers, and a
//! date with an incomplete year (one to three digits, or none) or with a
//! time it cannot hold is written again in the file's date format with the
//! separator it was typed with. Text that does not parse is refused.
//!
//! What 7.2.1 measured is a field's type change, which validates its
//! stored values "the way data entry is validated". That Browse entry
//! stores the same text, and that it refuses what does not parse instead
//! of keeping it, is inferred (record-writes.md).

use om_calc::{Ctx, Date, Time, Timestamp};
use om_hbam::catalog::DataType;

use super::{error, four_digit_year};

/// The text a field of `data_type` stores for `text` a user typed, or the
/// error refusing it: 500 for a date or timestamp, 501 for a time. Other
/// types store the text as typed, and empty (or blank) text clears the
/// field.
pub(super) fn entered(data_type: DataType, text: &str, cx: &Ctx) -> Result<String, i32> {
    if !matches!(
        data_type,
        DataType::Date | DataType::Time | DataType::Timestamp
    ) {
        return Ok(text.to_owned());
    }
    let cleaned = clean(text);
    if cleaned.is_empty() {
        return Ok(cleaned);
    }
    let first_word = cleaned.split(' ').next().unwrap_or_default();
    let has_time = cleaned.len() > first_word.len();
    let complete = four_digit_year(first_word, cx);
    match data_type {
        DataType::Date => {
            let date = match Date::parse(&cleaned, cx.locale, cx.today) {
                Ok(Some(date)) => date,
                _ => return Err(error::DATE),
            };
            if complete && !has_time {
                Ok(cleaned)
            } else {
                Ok(date.format(cx.locale))
            }
        }
        DataType::Timestamp => {
            // A date and a time of day: a date alone, a time alone or an
            // elapsed time are not timestamps here (7.2.1 keeps them as
            // values that did not validate).
            if !has_time || !matches!(Date::parse(first_word, cx.locale, cx.today), Ok(Some(_))) {
                return Err(error::DATE);
            }
            let timestamp = match Timestamp::parse(&cleaned, cx.locale, cx.today) {
                Ok(Some(timestamp)) => timestamp,
                _ => return Err(error::DATE),
            };
            if complete {
                return Ok(cleaned);
            }
            // om-calc cannot read every timestamp it writes (a ko-KR file's
            // 오후 before the time): rather than store text the field's
            // calculations would refuse, keep the typed text, which it read.
            let rendered = timestamp.format(cx.locale);
            match Timestamp::parse(&rendered, cx.locale, cx.today) {
                Ok(Some(_)) => Ok(rendered),
                _ => Ok(cleaned),
            }
        }
        _ => match Time::parse(&cleaned, cx.locale, cx.today) {
            Ok(Some(_)) => Ok(cleaned),
            _ => Err(error::TIME),
        },
    }
}

/// FileMaker's cleaning of typed numbers, dates and times
/// (`TextBuffer::NumberDateTimeCleaner`, schema-path.md 7.2.1 step 1):
/// full-width ASCII forms become ASCII, the ideographic comma and full stop
/// (and their half-width forms) a comma and a full stop, the em dash and the
/// prolonged sound mark a hyphen, other white space a space; leading and
/// trailing white space is removed.
pub(super) fn clean(text: &str) -> String {
    let mapped: String = text
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
    mapped.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_calc::Locale;

    /// schema-path.md 7.2.1's table of a type change's rendered text (FileMaker
    /// Server 26.0.3, 2026-11-11), as data entry stores it. Where that table
    /// keeps a value only because the rewrite writes nothing of the same
    /// length (`01-02-03`, full-width digits), entry stores the rendered or
    /// cleaned text; where it keeps a value that did not parse, entry
    /// refuses it.
    #[test]
    fn typed_dates_and_times_follow_the_type_change_rewrite() {
        let today = Date::from_ymd(2026, 11, 11).unwrap();
        let (en, de, ko) = (Locale::en_us(), Locale::de_de(), Locale::ko_kr());
        let (en, de, ko) = (
            Ctx::new(&en, today),
            Ctx::new(&de, today),
            Ctx::new(&ko, today),
        );
        let ok = |text: &str| -> Result<String, i32> { Ok(text.to_owned()) };
        for (cx, data_type, typed, stored) in [
            (&en, DataType::Date, "1/2/2003", ok("1/2/2003")),
            (&en, DataType::Date, "01/02/2003", ok("01/02/2003")),
            (&en, DataType::Date, "1/2/03", ok("1/2/2003")),
            (&en, DataType::Date, "1/2/3", ok("1/2/2003")),
            (&en, DataType::Date, "1-2-03", ok("1-2-2003")),
            (&en, DataType::Date, "01-02-03", ok("1-2-2003")),
            (&en, DataType::Date, "1.2.03", ok("1.2.2003")),
            (&en, DataType::Date, "1/2/203", ok("1/2/0203")),
            (&en, DataType::Date, "1/2", ok("1/2/2026")),
            (&en, DataType::Date, "1/2/2003 13:00", ok("1/2/2003")),
            (&en, DataType::Date, "01/02/2003 1:30 PM", ok("1/2/2003")),
            (&en, DataType::Date, "  1/2/03  ", ok("1/2/2003")),
            (&en, DataType::Date, "１/２/０３", ok("1/2/2003")),
            (&en, DataType::Date, "１/２/２００３", ok("1/2/2003")),
            (&en, DataType::Date, "abc", Err(error::DATE)),
            (&en, DataType::Date, "13:30", Err(error::DATE)),
            (&en, DataType::Date, "12", Err(error::DATE)),
            (&en, DataType::Date, "2/30/2003", Err(error::DATE)),
            (&en, DataType::Date, "  ", ok("")),
            (
                &en,
                DataType::Timestamp,
                "1/2/2003 1:30 PM",
                ok("1/2/2003 1:30 PM"),
            ),
            (
                &en,
                DataType::Timestamp,
                "1/2/03 1:30 PM",
                ok("1/2/2003 1:30 PM"),
            ),
            // Not from 7.2.1: om-calc writes a ko-KR timestamp it cannot
            // read back, so the typed text is kept.
            (&ko, DataType::Timestamp, "26-1-2 13:30", ok("26-1-2 13:30")),
            (
                &ko,
                DataType::Timestamp,
                "2026-1-2 13:30",
                ok("2026-1-2 13:30"),
            ),
            (
                &en,
                DataType::Timestamp,
                "1/2/03 13:00",
                ok("1/2/2003 1:00 PM"),
            ),
            (
                &en,
                DataType::Timestamp,
                "1-2-03 13:30",
                ok("1-2-2003 1:30 PM"),
            ),
            (
                &en,
                DataType::Timestamp,
                "1/2 13:30",
                ok("1/2/2026 1:30 PM"),
            ),
            (&en, DataType::Timestamp, "1/2/2003", Err(error::DATE)),
            (&en, DataType::Timestamp, "13:30", Err(error::DATE)),
            (&en, DataType::Timestamp, "1/2/2003 25:00", Err(error::DATE)),
            (&en, DataType::Time, "10:30", ok("10:30")),
            (&en, DataType::Time, " 25:00 ", ok("25:00")),
            (&en, DataType::Time, "1:30 PM", ok("1:30 PM")),
            (&en, DataType::Time, "abc", Err(error::TIME)),
            (&en, DataType::Number, " 007 ", ok(" 007 ")),
            (&de, DataType::Date, "2.1.2003", ok("2.1.2003")),
            (&de, DataType::Date, "02.01.2003", ok("02.01.2003")),
            (&de, DataType::Date, "2.1.03", ok("02.01.2003")),
            (&de, DataType::Date, "2-1-03", ok("02-01-2003")),
            (&de, DataType::Date, "2.1.2003 13:00", ok("02.01.2003")),
            (
                &de,
                DataType::Timestamp,
                "2.1.03 13:30",
                ok("02.01.2003 13:30"),
            ),
        ] {
            assert_eq!(
                entered(data_type, typed, cx),
                stored,
                "{data_type:?} {typed:?}"
            );
        }
    }
}
