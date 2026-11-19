//! Synthetic stored character styles survive calculation text operations.

use om_data::calc::{CharStyle, Ctx, Date, Locale, Text, Value, ValueType};
use om_data::host::styled_value;
use om_layout::{CharacterStyle, Color, StyledRun, StyledText};

fn styled(text: &str, range: std::ops::Range<usize>, style: CharacterStyle) -> StyledText {
    StyledText {
        text: text.into(),
        character_runs: vec![StyledRun { range, style }],
        ..Default::default()
    }
}

#[test]
fn stored_runs_keep_utf8_ranges_gaps_and_partial_face_overrides() {
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, Date::from_ymd(2026, 1, 2).unwrap());
    let value = styled_value(
        &styled(
            "aβz",
            1..3,
            CharacterStyle {
                font_family: Some("Helvetica".into()),
                font_size: Some(9.0),
                color: Some(Color {
                    red: 1.0,
                    alpha: 1.0,
                    ..Default::default()
                }),
                faces: Some(0),
                face_mask: Some(256),
                ..Default::default()
            },
        ),
        ValueType::Text,
        &cx,
    );
    let text = value.to_text(&cx);
    assert_eq!(text.as_str(), "aβz");
    assert_eq!(
        text.styled_runs(),
        vec![(
            1..3,
            CharStyle {
                font: Some("Helvetica".into()),
                size: Some(9),
                color: Some(0xff0000),
                flags: 256 << 16,
            }
        )]
    );
    assert_eq!(text.slice(1..3).styled_runs()[0].0, 0..2);
    assert!(text.slice(0..1).styled_runs().is_empty());
    assert!(text.slice(3..4).styled_runs().is_empty());
}

#[test]
fn stored_dates_and_numbers_keep_styles_but_explicit_conversions_clear_them() {
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, Date::from_ymd(2026, 1, 2).unwrap());
    for (source, ty) in [("4/4/2017", ValueType::Date), ("012", ValueType::Number)] {
        let value = styled_value(
            &styled(
                source,
                0..source.len(),
                CharacterStyle {
                    color: Some(Color {
                        red: 1.0,
                        alpha: 1.0,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ),
            ty,
            &cx,
        );
        assert_eq!(value.value_type(), ty);
        assert_eq!(value.to_text(&cx).as_str(), source);
        assert_eq!(value.to_text(&cx).styled_runs()[0].1.color, Some(0xff0000));
        let converted = if ty == ValueType::Date {
            Value::Date(value.to_date(&cx).unwrap().unwrap())
        } else {
            Value::Number(value.to_number(&cx))
        };
        assert!(converted.to_text(&cx).styled_runs().is_empty());
    }
}

#[test]
fn invalid_or_overlapping_public_run_ranges_do_not_change_the_text() {
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, Date::from_ymd(2026, 1, 2).unwrap());
    let mut text = styled(
        "aβz",
        1..3,
        CharacterStyle {
            faces: Some(256),
            ..Default::default()
        },
    );
    for range in [2..3, 1..4, 6..8] {
        text.character_runs.push(StyledRun {
            range,
            style: CharacterStyle::default(),
        });
    }
    let result = styled_value(&text, ValueType::Text, &cx).to_text(&cx);
    assert_eq!(result.as_str(), "aβz");
    assert_eq!(
        result.styled_runs(),
        vec![(
            1..3,
            CharStyle {
                flags: 256,
                ..Default::default()
            }
        )]
    );
    // Non-text/date/number types keep their existing plain conversion.
    let time = styled_value(
        &styled(
            "8:15",
            0..4,
            CharacterStyle {
                faces: Some(256),
                ..Default::default()
            },
        ),
        ValueType::Time,
        &cx,
    );
    assert!(time.to_text(&cx).styled_runs().is_empty());
    assert_eq!(Text::plain("aβz").as_str(), result.as_str());
}
