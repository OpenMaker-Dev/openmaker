//! Field values between a record's stored cells and calculation values.
//!
//! A record stores each field's value as masked text, numbers, dates and
//! times included, in their text form ("2/17/2014"; file-format.md 3.6);
//! long or non-ASCII text is chunked. Reading converts the text to the
//! field's type as a calculation's declared result type does
//! (calc-values.md 6.5); writing stores the value's text in the file's
//! formats, the way FileMaker Pro uploads what a user typed.

use om_calc::{Container, Ctx, Text, Value, ValueType};
use om_hbam::{Cell, DataType, OwnedNode};

/// The calculation type of a field's data (or result) type.
pub fn value_type(data_type: DataType) -> ValueType {
    match data_type {
        DataType::Number => ValueType::Number,
        DataType::Date => ValueType::Date,
        DataType::Time => ValueType::Time,
        DataType::Timestamp => ValueType::Timestamp,
        DataType::Container => ValueType::Container,
        DataType::Text | DataType::Unknown(_) => ValueType::Text,
    }
}

/// The text of a stored cell, if it holds a single value.
pub fn cell_text(node: &OwnedNode) -> Option<String> {
    Cell::from_owned(node).text()?.ok()
}

/// The value of a field of type `ty` stored as `cell` (`None`: no value
/// stored, the empty value of the type).
pub fn decode(ty: ValueType, cell: Option<&OwnedNode>, cx: &Ctx) -> Value {
    match cell.and_then(cell_text) {
        Some(text) => from_text(ty, &text, cx),
        None => Value::empty(ty),
    }
}

/// The value of stored text `text` in a field of type `ty`.
///
/// A number field keeps text that is not the number's own form
/// (`NumberText`: "12abc" computes as 12 and shows as typed); a date, time
/// or timestamp field whose text does not convert reads as `?`.
pub fn from_text(ty: ValueType, text: &str, cx: &Ctx) -> Value {
    if text.is_empty() {
        return Value::empty(ty);
    }
    match ty {
        ValueType::Text => Value::text(text),
        ValueType::Number => {
            let number = om_calc::value::parse_number(text, cx.locale);
            let plain = Value::Number(number.clone());
            if plain.to_text(cx).as_str() == text {
                plain
            } else {
                Value::NumberText(number, Text::plain(text))
            }
        }
        // The record holds the container's reference text; its data streams
        // live in the table's library and are not read here.
        ValueType::Container => Value::Container(Container {
            text: Text::plain(text),
            ..Container::default()
        }),
        ValueType::Date => match Value::text(text).convert_declared(ty, cx) {
            Value::Date(date) => Value::DateText(date, Text::plain(text)),
            other => other,
        },
        ValueType::Time | ValueType::Timestamp => Value::text(text).convert_declared(ty, cx),
    }
}

/// The text to store for `value` in a field of type `ty`, or `None` for an
/// empty value (the cell is removed). A date, time or timestamp is stored
/// in the file's format after conversion to the field's type; what does not
/// convert is stored as given, as FileMaker keeps an entry it cannot read.
pub fn to_stored(ty: ValueType, value: &Value, cx: &Ctx) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let text = match ty {
        ValueType::Date | ValueType::Time | ValueType::Timestamp => {
            match value.convert_declared(ty, cx) {
                converted if converted.is_invalid() => value.to_text(cx),
                converted if converted.is_empty() => return None,
                converted => converted.to_text(cx),
            }
        }
        _ => value.to_text(cx),
    };
    let text = text.into_string();
    (!text.is_empty()).then_some(text)
}

/// The stored cell of `text` under field key `segment`, written the way
/// FileMaker writes text values (chunked when long or not ASCII).
pub fn cell(segment: &[u8], text: &str) -> OwnedNode {
    om_scripts::text_node(segment, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_calc::{Date, Locale};

    fn cx(locale: &Locale) -> Ctx<'_> {
        Ctx::new(locale, Date::from_ymd(2026, 10, 28).unwrap())
    }

    fn text_of(value: &Value, cx: &Ctx) -> String {
        value.to_text(cx).into_string()
    }

    #[test]
    fn stored_text_reads_as_the_fields_type() {
        let locale = Locale::en_us();
        let cx = cx(&locale);
        let stored = |ty, text: &str| decode(ty, Some(&cell(&[1], text)), &cx);

        let name = stored(ValueType::Text, "Ångström 東京");
        assert_eq!(name.value_type(), ValueType::Text);
        assert_eq!(text_of(&name, &cx), "Ångström 東京");

        // Numbers compute; text that is not the number's own form is kept.
        let count = stored(ValueType::Number, "42");
        assert!(matches!(count, Value::Number(_)));
        assert_eq!(count.to_number(&cx).to_i64(), Some(42));
        let typed = stored(ValueType::Number, "12abc");
        assert!(matches!(typed, Value::NumberText(..)));
        assert_eq!(typed.to_number(&cx).to_i64(), Some(12));
        assert_eq!(text_of(&typed, &cx), "12abc");

        // Dates, times and timestamps in the file's formats.
        let due = stored(ValueType::Date, "2/17/2014");
        assert_eq!(
            due.to_date(&cx).unwrap(),
            Some(Date::from_ymd(2014, 2, 17).unwrap())
        );
        let padded = stored(ValueType::Date, "02/17/2014");
        assert_eq!(padded.to_date(&cx).unwrap(), due.to_date(&cx).unwrap());
        assert_eq!(text_of(&padded, &cx), "02/17/2014");
        let at = stored(ValueType::Time, "13:30:05");
        assert_eq!(at.value_type(), ValueType::Time);
        assert_eq!(text_of(&at, &cx), "13:30:05");
        let stamp = stored(ValueType::Timestamp, "10/28/2026 3:12:42 PM");
        assert_eq!(stamp.value_type(), ValueType::Timestamp);
        assert!(stored(ValueType::Date, "not a date").is_invalid());

        let photo = stored(ValueType::Container, "8001\u{e000}");
        assert_eq!(photo.value_type(), ValueType::Container);

        // No cell, or an empty one, is the type's empty value.
        for ty in [ValueType::Text, ValueType::Number, ValueType::Date] {
            let empty = decode(ty, None, &cx);
            assert!(empty.is_empty());
            assert_eq!(empty.value_type(), ty);
            assert!(stored(ty, "").is_empty());
        }
    }

    #[test]
    fn values_store_as_their_text() {
        let locale = Locale::en_us();
        let cx = cx(&locale);
        assert_eq!(
            to_stored(ValueType::Text, &Value::text("Done"), &cx),
            Some("Done".into())
        );
        assert_eq!(
            to_stored(ValueType::Number, &Value::number(7), &cx),
            Some("7".into())
        );
        // A date field stores dates in the file's format, also from text.
        let date = Value::Date(Date::from_ymd(2026, 10, 28).unwrap());
        assert_eq!(
            to_stored(ValueType::Date, &date, &cx),
            Some("10/28/2026".into())
        );
        assert_eq!(
            to_stored(ValueType::Date, &Value::text("10/28/2026"), &cx),
            Some("10/28/2026".into())
        );
        // What does not convert is kept; empty values remove the cell.
        assert_eq!(
            to_stored(ValueType::Date, &Value::text("soon"), &cx),
            Some("soon".into())
        );
        assert_eq!(to_stored(ValueType::Text, &Value::empty_text(), &cx), None);
        assert_eq!(
            to_stored(ValueType::Date, &Value::empty(ValueType::Date), &cx),
            None
        );

        // A stored value reads back as the value it came from.
        for (ty, value) in [
            (ValueType::Number, Value::number(-3)),
            (ValueType::Date, date.clone()),
            (ValueType::Text, Value::text("Grüße\rline 2")),
        ] {
            let text = to_stored(ty, &value, &cx).unwrap();
            let back = decode(ty, Some(&cell(&[2], &text)), &cx);
            assert_eq!(text_of(&back, &cx), text_of(&value, &cx));
            assert_eq!(back.value_type(), ty);
        }
    }

    #[test]
    fn non_ascii_text_is_chunked_as_filemaker_writes_it() {
        let ascii = cell(&[1], "Plain");
        assert!(ascii.children.is_empty() && ascii.value.is_some());
        let unicode = cell(&[1], "東京");
        assert_eq!(
            unicode.child(&[0]).unwrap().value.as_deref().unwrap().len(),
            8
        );
        assert_eq!(cell_text(&unicode).as_deref(), Some("東京"));
    }
}
