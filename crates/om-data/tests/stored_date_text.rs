//! A stored date retains its entered text and styling through field reads.
//! Native Pro preserves those for GetAsText and text-result calculations;
//! GetAsDate and arithmetic instead produce a normalized, unstyled date.
//! All fields and values in these regressions are synthetic.

use om_data::calc::eval::{evaluate_text, Context, Variables};
use om_data::calc::{ops, CharStyle, Ctx, Date, Locale, Text, Time, Value, ValueType};
use om_data::formula::{self, Formula, Key};
use om_data::host::{
    stored_text, stored_value, CalcSchema, FieldDef, FileCalc, FileHost, MemorySource,
    RecordValues, SessionInfo,
};

const TABLE: u64 = 0x8001;
const OCCURRENCE: u64 = 0xd00001;

fn today() -> Date {
    Date::from_ymd(2026, 1, 2).unwrap()
}

fn locale() -> Locale {
    let mut locale = Locale::en_us();
    locale.pad_month = true;
    locale.pad_day = true;
    locale
}

fn red(text: &str) -> Text {
    Text::styled(
        text,
        CharStyle {
            color: Some(0xdd0000),
            ..Default::default()
        },
    )
}

fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Stored dates", locale());
    schema.add_table(TABLE, "Records");
    schema.add_occurrence(OCCURRENCE, "Records", TABLE);
    schema.add_field(TABLE, FieldDef::data(1, "Due", ValueType::Date));
    for (id, name, ty, text) in [
        (2, "Date copy", ValueType::Date, "Due"),
        (
            3,
            "Text copy",
            ValueType::Text,
            "GetAsText ( GetField ( \"Date copy\" ) )",
        ),
    ] {
        let mut context = formula::Context::new(schema.names());
        context.occurrence = Some(OCCURRENCE);
        let formula = Formula {
            context: Some(Key::from_id(OCCURRENCE)),
            formatted: None,
            polish: formula::compile(text, &context).unwrap().expr,
        };
        schema.add_field(TABLE, FieldDef::calculation(id, name, ty, formula, false));
    }
    schema
}

#[test]
fn field_and_nested_calculation_reads_keep_entered_text_and_color() {
    let schema = schema();
    let cx = Ctx::new(&schema.locale, today());
    let mut row = RecordValues::default();
    row.set(1, 1, stored_text(red("1/2/2026"), ValueType::Date, &cx));
    let mut source = MemorySource::new();
    source.put_record(TABLE, 1, row);
    let info = SessionInfo::default();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&schema, &source, today()),
        &info,
        &mut locals,
        &mut globals,
    );
    for expression in ["Due", "Date copy", "Text copy", "GetAsText ( Due )"] {
        let result = evaluate_text(expression, &mut host, &Context::record(OCCURRENCE, 1));
        assert_eq!(result.code(), 0, "{expression}");
        let text = result.value.to_text(&cx);
        assert_eq!(text.as_str(), "1/2/2026", "{expression}");
        assert_eq!(text.style_at(0).color, Some(0xdd0000), "{expression}");
    }
    for expression in ["GetAsCSS ( Due )", "GetAsCSS ( Text copy )"] {
        let result = evaluate_text(expression, &mut host, &Context::record(OCCURRENCE, 1));
        assert_eq!(result.code(), 0);
        assert_eq!(
            result.value.to_text(&cx).as_str(),
            "<span style=\"color: #DD0000;\" >1/2/2026</span>"
        );
    }
    for expression in ["GetAsDate ( Due )", "Due + 0"] {
        let result = evaluate_text(expression, &mut host, &Context::record(OCCURRENCE, 1));
        assert_eq!(result.code(), 0);
        assert_eq!(result.value.value_type(), ValueType::Date);
        let text = result.value.to_text(&cx);
        assert_eq!(text.as_str(), "01/02/2026");
        assert!(text.styled_runs().is_empty());
    }
}

#[test]
fn stored_dates_still_compute_and_compare_as_dates_not_their_display_text() {
    let locale = locale();
    let cx = Ctx::new(&locale, today());
    let value = stored_text(red("1/2/2026"), ValueType::Date, &cx);
    assert_eq!(value.value_type(), ValueType::Date);
    assert!(value.is_true(&cx));
    assert_eq!(value.to_number(&cx).to_i64(), Some(today().day_number()));
    let midnight = Value::Timestamp(today().to_timestamp());
    assert!(ops::compare_op(&value, ops::CompareOp::Eq, &midnight, &cx).is_true(&cx));
    let hour = Value::Time(Time::from_micros(3_600_000_000, Default::default()));
    let sum = ops::add(&value, &hour, &cx).unwrap();
    assert_eq!(sum.value_type(), ValueType::Timestamp);
    assert_eq!(
        sum.to_timestamp(&cx).unwrap().unwrap().date().ymd(),
        today().ymd()
    );
    assert_eq!(
        sum.to_timestamp(&cx)
            .unwrap()
            .unwrap()
            .time_of_day()
            .micros(),
        3_600_000_000
    );
}

#[test]
fn empty_and_invalid_stored_dates_keep_the_existing_behavior() {
    let locale = locale();
    let cx = Ctx::new(&locale, today());
    let empty = stored_value("", ValueType::Date, &cx);
    assert!(empty.is_empty());
    assert_eq!(empty.value_type(), ValueType::Date);
    let invalid = stored_text(red("not a date"), ValueType::Date, &cx);
    assert_eq!(invalid.value_type(), ValueType::Text);
    assert_eq!(invalid.to_text(&cx).as_str(), "not a date");
    assert_eq!(invalid.to_text(&cx).style_at(0).color, Some(0xdd0000));
}
