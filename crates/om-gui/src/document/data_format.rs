//! Field data formatting in Browse (data-format.md): a number, date, time or
//! timestamp field shows its value through the object's `KFormat` styles.
//! Only the displayed text changes; an editor opens with the value itself.
//! A value that does not parse, and a setting this implementation does not
//! interpret, show the value unchanged.

use std::borrow::Cow;
use std::sync::LazyLock;

use om_data::calc::locale::{DateOrder, Language};
use om_data::calc::{Ctx, Date, Locale, Number, Time, Timestamp, ValueType};
use om_layout::{DataFormat, DateFormat, NumberFormat, TimeFormat};

/// What a field of type `kind` shows for `value`, the field's text in the
/// file's locale (`cx`).
pub(crate) fn display<'a>(
    value: &'a str,
    kind: ValueType,
    format: Option<&DataFormat>,
    cx: &Ctx<'_>,
) -> Cow<'a, str> {
    let formatted = format
        .filter(|_| !value.is_empty())
        .and_then(|format| match kind {
            ValueType::Number => number(value, &format.number, cx),
            ValueType::Date => date(value, &format.date, cx),
            ValueType::Time => time(value, &format.time, cx),
            ValueType::Timestamp => timestamp(value, format, cx),
            ValueType::Text | ValueType::Container => None,
        });
    formatted.map_or(Cow::Borrowed(value), Cow::Owned)
}

/// data-format.md "Numeric style". The value is read with the file's
/// separators; the style's own separators only present it.
fn number(value: &str, format: &NumberFormat, cx: &Ctx<'_>) -> Option<String> {
    if format.as_entered() || format.character_style != 0 {
        return None;
    }
    let mut value = Number::parse_pure_text(
        value,
        cx.locale.decimal_separator,
        cx.locale.thousands_separator,
    )
    .filter(|value| !value.is_nan())?;
    // Boolean and general dispatch precede zero suppression.
    if format.boolean() {
        let text = if value.is_zero() {
            &format.false_string
        } else {
            &format.true_string
        };
        return Some(text.clone());
    }
    // The general format is not interpreted here.
    if format.general() {
        return None;
    }
    // `0x4000`: the system's separators, not the style's. They are the
    // file's own (its locale), as the starter solutions' currency fields
    // show them in FileMaker Pro on an en-US system (data-format.md;
    // other locales inferred).
    let (thousands, decimal) = if format.flags & 0x4000 != 0 {
        (cx.locale.thousands_separator, cx.locale.decimal_separator)
    } else {
        (format.thousands_separator, format.decimal_separator)
    };
    // Grouping with the decimal separator's own character would show
    // another number (`1.234` for 1234): keep the value.
    if format.grouping() && thousands == decimal {
        return None;
    }
    if format.hide_zero() && value.is_zero() {
        return Some(String::new());
    }
    if format.percent() {
        value = value.mul(&Number::from_i64(100), value.precision());
    }
    // A negative `decimalDigits` is kept verbatim but not interpreted.
    let fixed_places = if format.fixed_decimals() {
        Some(
            usize::try_from(format.decimal_places)
                .ok()
                .filter(|places| *places <= 400)?,
        )
    } else {
        None
    };
    // Rounding uses the calculation engine's decimal arithmetic, so 1.005
    // at two places is 1.01, without binary floating-point drift.
    if let Some(places) = fixed_places {
        value = value.round(places as i64);
    }
    let canonical = expand_scientific(&value.to_text_with('.'))?;
    let negative = canonical.starts_with('-');
    let unsigned = canonical.strip_prefix('-').unwrap_or(&canonical);
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let integer = if integer.is_empty() { "0" } else { integer };
    let places = fixed_places.unwrap_or(fraction.len());
    let separator = |separator: char| (separator != '\0').then_some(separator);
    let mut out = String::new();
    for (i, digit) in integer.chars().enumerate() {
        if format.grouping() && i > 0 && (integer.len() - i).is_multiple_of(3) {
            out.push(separator(thousands)?);
        }
        out.push(digit);
    }
    if places > 0 {
        out.push(separator(decimal)?);
        out.push_str(fraction);
        out.extend(std::iter::repeat_n(
            '0',
            places.saturating_sub(fraction.len()),
        ));
    }
    // Styles past 0 and 1 are localized resource strings: keep the value.
    let mark_negative = |out: &mut String| -> Option<()> {
        if negative {
            match format.negative_style {
                0 => out.insert(0, '-'),
                1 => out.push('-'),
                _ => return None,
            }
        }
        Some(())
    };
    if format.negative_before_symbol() {
        mark_negative(&mut out)?;
    }
    if format.percent() {
        out.push('%');
    } else if format.show_symbol() {
        if format.symbol_before() {
            out.insert_str(0, &format.symbol);
        } else {
            out.push_str(&format.symbol);
        }
    }
    if !format.negative_before_symbol() {
        mark_negative(&mut out)?;
    }
    Some(out)
}

/// data-format.md "Date style".
fn date(value: &str, format: &DateFormat, cx: &Ctx<'_>) -> Option<String> {
    if format.format() == 0 {
        return None;
    }
    format_date(Date::parse(value, cx.locale, cx.today).ok()??, format, cx)
}

fn format_date(date: Date, format: &DateFormat, cx: &Ctx<'_>) -> Option<String> {
    if format.character_style() != 0 {
        return None;
    }
    match format.format() {
        1 => {
            let mut out = format.separators[0].clone();
            for (element, separator) in format.elements.iter().zip(&format.separators[1..]) {
                out.push_str(&date_element(date, *element, format, cx)?);
                out.push_str(separator);
            }
            Some(out)
        }
        format_code @ (2 | 7) => {
            let year = if format_code == 2 { 7 } else { 8 };
            let elements = match cx.locale.date_order {
                DateOrder::MonthDayYear => [4, 1, year],
                DateOrder::DayMonthYear => [1, 4, year],
                DateOrder::YearMonthDay => [year, 4, 1],
            };
            let elements = elements
                .into_iter()
                .map(|element| date_element(date, element, format, cx))
                .collect::<Option<Vec<_>>>()?;
            Some(elements.join(&format.separator.to_string()))
        }
        code @ 3..=6 => english_named_date(date, cx.locale, code),
        9 => {
            static SYSTEM: LazyLock<Option<Locale>> =
                LazyLock::new(om_data::new_file::configured_system_locale);
            english_named_date(date, SYSTEM.as_ref()?, 4)
        }
        _ => None,
    }
}

/// The measured month-first English presets. The caller supplies the
/// selected locale because fixed and system presets can differ.
fn english_named_date(date: Date, locale: &Locale, code: u8) -> Option<String> {
    if locale.language != Language::English || locale.date_order != DateOrder::MonthDayYear {
        return None;
    }
    let (year, month, day) = date.ymd();
    let months = if matches!(code, 3 | 5) {
        &locale.month_abbreviations
    } else {
        &locale.month_names
    };
    let month = &months[month as usize - 1];
    let date_text = format!("{month} {day}, {year:04}");
    let weekdays = match code {
        5 => &locale.day_abbreviations,
        6 => &locale.day_names,
        _ => return Some(date_text),
    };
    let weekday = &weekdays[date.weekday() as usize - 1];
    Some(format!("{weekday}, {date_text}"))
}

/// One `DateElement` (file-metadata.md 2.3) with the style's padding.
fn date_element(date: Date, element: u8, format: &DateFormat, cx: &Ctx<'_>) -> Option<String> {
    let (year, month, day) = date.ymd();
    let month = month as usize - 1;
    let weekday = date.weekday() as usize - 1;
    let locale = cx.locale;
    Some(match element {
        0 => String::new(),
        1 => padded(u64::from(day), format.day_style())?,
        2 => locale.day_abbreviations[weekday].clone(),
        3 => locale.day_names[weekday].clone(),
        4 => padded(month as u64 + 1, format.month_style())?,
        5 => locale.month_abbreviations[month].clone(),
        6 => locale.month_names[month].clone(),
        7 => {
            let year = year.to_string();
            year[year.len().saturating_sub(2)..].to_owned()
        }
        8 => format!("{year:04}"),
        9 => locale.quarter_abbreviations[month / 3].clone(),
        10 => locale.quarter_names[month / 3].clone(),
        _ => return None,
    })
}

/// data-format.md "Time style".
fn time(value: &str, format: &TimeFormat, cx: &Ctx<'_>) -> Option<String> {
    if format.as_entered() {
        return None;
    }
    format_time(Time::parse(value, cx.locale, cx.today).ok()??, format)
}

fn format_time(time: Time, format: &TimeFormat) -> Option<String> {
    // Word separators, platform system formats and other numeral styles
    // need resources this implementation does not have: never substitute
    // colon formatting for them.
    if format.as_entered()
        || format.system_format()
        || !format.use_separator()
        || format.character_style != 0
        || !(format.show_hours() || format.show_minutes() || format.show_seconds())
    {
        return None;
    }
    // The style says nothing about negative (elapsed) times or about the
    // digits of fractional seconds: keep the value's own text for those.
    let micros = u128::try_from(time.micros()).ok()?;
    if format.show_seconds() && !micros.is_multiple_of(1_000_000) {
        return None;
    }
    let whole = micros / 1_000_000;
    let mut hours = whole / 3600;
    let am = hours % 24 < 12;
    if !format.clock_24h() {
        hours = match hours % 12 {
            0 => 12,
            hour => hour,
        };
    }
    // Without hours, minutes are elapsed; without both, seconds are.
    let minutes = if format.show_hours() {
        whole / 60 % 60
    } else {
        whole / 60
    };
    let seconds = if format.show_hours() || format.show_minutes() {
        whole % 60
    } else {
        whole
    };
    let mut elements = Vec::new();
    if format.show_hours() {
        elements.push(padded(hours as u64, format.hour_style)?);
    }
    if format.show_minutes() {
        elements.push(padded(minutes as u64, format.minute_second_style)?);
    }
    if format.show_seconds() {
        elements.push(padded(seconds as u64, format.minute_second_style)?);
    }
    let mut out = elements.join(&format.separator.to_string());
    let marker = if format.clock_24h() {
        &format.marker_24h
    } else if am {
        &format.am
    } else {
        &format.pm
    };
    if format.marker_before() {
        out.insert_str(0, marker);
    } else {
        out.push_str(marker);
    }
    Some(out)
}

/// data-format.md "Timestamps": the date style shows the date and the time
/// style the time, one space apart. A part left as entered keeps its text,
/// and is left out when the value has none.
fn timestamp(value: &str, format: &DataFormat, cx: &Ctx<'_>) -> Option<String> {
    let stamp = Timestamp::parse(value, cx.locale, cx.today).ok()??;
    let format_date_part = format.date.format() != 0;
    let format_time_part = !format.time.as_entered();
    if !format_date_part && !format_time_part {
        return None;
    }
    let (date_text, time_text) = timestamp_texts(value, cx);
    let date = if format_date_part {
        Some(format_date(stamp.date(), &format.date, cx)?)
    } else {
        date_text.map(str::to_owned)
    };
    let time = if format_time_part {
        Some(format_time(stamp.time_of_day(), &format.time)?)
    } else {
        time_text.map(str::to_owned)
    };
    Some(date.into_iter().chain(time).collect::<Vec<_>>().join(" "))
}

/// The date and time texts of a value that parses as a timestamp, read as
/// calc-values.md 4.6 reads them: a date alone (midnight), a time alone (on
/// 1/1/0001), or a date word, whitespace and the time. A no-break space can
/// separate a date's fields, so it does not end the date.
fn timestamp_texts<'a>(value: &'a str, cx: &Ctx<'_>) -> (Option<&'a str>, Option<&'a str>) {
    let value = value.trim();
    // A timestamp's text has a date exactly when it also reads as a date.
    if Date::parse(value, cx.locale, cx.today).is_err() {
        return (None, Some(value));
    }
    match value.split_once(|c: char| c.is_whitespace() && c != '\u{00A0}') {
        Some((date, time)) => (Some(date), Some(time.trim_start())),
        None => (Some(value), None),
    }
}

/// Padding styles shared by dates and times: 0 none, 1 zeros, 2 spaces, to
/// two digits.
fn padded(number: u64, style: u8) -> Option<String> {
    Some(match style {
        0 => number.to_string(),
        1 => format!("{number:0>2}"),
        2 => format!("{number:>2}"),
        _ => return None,
    })
}

/// The plain digits of a number's text, which uses a scientific form for
/// some large and small values (calc-values.md 2.7).
fn expand_scientific(text: &str) -> Option<String> {
    let Some((mantissa, exponent)) = text.split_once('E') else {
        return Some(text.to_owned());
    };
    let exponent: i32 = exponent.parse().ok()?;
    if exponent.unsigned_abs() > 800 {
        return None;
    }
    let sign = if mantissa.starts_with('-') { "-" } else { "" };
    let mantissa = mantissa.trim_start_matches('-');
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32 + exponent;
    let digits = mantissa.replace('.', "");
    let body = if point <= 0 {
        format!("0.{}{}", "0".repeat(point.unsigned_abs() as usize), digits)
    } else if point as usize >= digits.len() {
        format!("{}{}", digits, "0".repeat(point as usize - digits.len()))
    } else {
        let (left, right) = digits.split_at(point as usize);
        format!("{left}.{right}")
    };
    Some(format!("{sign}{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::calc::Locale;

    fn us() -> Locale {
        Locale::en_us()
    }
    fn cx(locale: &Locale) -> Ctx<'_> {
        Ctx::new(locale, Date::from_ymd(2026, 1, 1).unwrap())
    }
    fn numeric(flags: u16) -> NumberFormat {
        NumberFormat {
            flags,
            symbol: "\" X".into(),
            thousands_separator: ',',
            decimal_separator: '.',
            decimal_places: 3,
            ..Default::default()
        }
    }
    fn us_number(value: &str, format: &NumberFormat) -> Option<String> {
        number(value, format, &cx(&us()))
    }

    #[test]
    fn grouping_fixed_decimals_and_suffix_symbols_change_only_the_display() {
        assert_eq!(us_number("725675", &numeric(0x2802)).unwrap(), "725,675");
        assert_eq!(us_number("8", &numeric(0x200c)).unwrap(), "8.000\" X");
        assert_eq!(
            us_number("-1.2345", &numeric(0x200c)).unwrap(),
            "-1.235\" X"
        );
        assert_eq!(
            us_number("1000000000", &numeric(0x2802)).unwrap(),
            "1,000,000,000"
        );
        assert_eq!(us_number("1.5E-10", &numeric(0)).unwrap(), "0.00000000015");
        // As entered, and values that are not numbers, keep their text.
        assert!(us_number("250000", &numeric(0x0801)).is_none());
        assert!(us_number("?", &numeric(0x2802)).is_none());
        assert!(us_number("12 apples", &numeric(0x2802)).is_none());
        let us = us();
        let cx = cx(&us);
        let format = DataFormat {
            font_height: 11,
            number: numeric(0x2802),
            date: DateFormat::default(),
            time: TimeFormat::default(),
            raw: Vec::new(),
        };
        assert_eq!(
            display("725675", ValueType::Number, Some(&format), &cx),
            "725,675"
        );
        assert!(matches!(
            display("725675", ValueType::Text, Some(&format), &cx),
            Cow::Borrowed("725675")
        ));
        assert!(matches!(
            display("725675", ValueType::Number, None, &cx),
            Cow::Borrowed("725675")
        ));
        assert!(matches!(
            display("", ValueType::Number, Some(&format), &cx),
            Cow::Borrowed("")
        ));
    }

    #[test]
    fn currency_rounds_in_decimal_and_places_the_negative_marker_by_flag() {
        let mut currency = numeric(0x080c);
        currency.symbol = "$".into();
        currency.decimal_places = 2;
        assert_eq!(us_number("1.005", &currency).unwrap(), "$1.01");
        assert_eq!(us_number("-1.005", &currency).unwrap(), "-$1.01");
        currency.flags |= 0x400;
        assert_eq!(us_number("-1.005", &currency).unwrap(), "$-1.01");
        // Unsupported settings keep the value rather than guess.
        currency.negative_style = 2;
        assert!(us_number("-1.005", &currency).is_none());
        currency.decimal_places = -1;
        assert!(us_number("1.005", &currency).is_none());
    }

    #[test]
    fn values_use_the_file_locale_and_the_style_its_own_separators() {
        let mut currency = numeric(0x080e);
        currency.symbol = "$".into();
        currency.decimal_places = 2;
        currency.thousands_separator = '.';
        currency.decimal_separator = ',';
        assert_eq!(us_number("12345.5", &currency).unwrap(), "$12.345,50");
        // A German file stores and calculates 12345,5 with a comma decimal
        // and a period between thousands.
        let german = Locale::de_de();
        let cx = cx(&german);
        currency.thousands_separator = ',';
        currency.decimal_separator = '.';
        assert_eq!(number("12.345,5", &currency, &cx).unwrap(), "$12,345.50");
        assert_eq!(number("12345,5", &currency, &cx).unwrap(), "$12,345.50");
        // The period is the German file's grouping, never its decimal.
        assert_eq!(number("1.5", &currency, &cx).unwrap(), "$15.00");
        // With the system's separators (`0x4000`), the file's own present
        // it, whatever the style stores: the starter solutions' currency
        // style (`0x4a0c`) shows $12.50 on an en-US system.
        let mut system = numeric(0x4a0c);
        system.symbol = "$".into();
        system.decimal_places = 2;
        system.thousands_separator = '.';
        system.decimal_separator = ',';
        assert_eq!(us_number("12.5", &system).unwrap(), "$12.50");
        system.flags |= 0x0002;
        assert_eq!(us_number("1234.5", &system).unwrap(), "$1,234.50");
        assert_eq!(number("1234,5", &system, &cx).unwrap(), "$1.234,50");
        // Grouping that would use the decimal separator keeps the value.
        let mut same = Locale::en_us();
        same.thousands_separator = '.';
        let same_cx = Ctx::new(&same, Date::from_ymd(2026, 1, 1).unwrap());
        assert!(number("1234", &system, &same_cx).is_none());
        system.thousands_separator = '.';
        system.decimal_separator = '.';
        system.flags &= !0x4000;
        assert!(us_number("1234", &system).is_none());
    }

    #[test]
    fn blank_zero_boolean_and_percent_follow_independent_flags() {
        assert_eq!(us_number("0", &numeric(0x2002)).as_deref(), Some(""));
        assert_eq!(us_number("0", &numeric(2)).as_deref(), Some("0"));
        let boolean = NumberFormat {
            flags: 0x2120,
            true_string: "Yes".into(),
            false_string: "No".into(),
            ..numeric(0)
        };
        assert_eq!(us_number("0", &boolean).as_deref(), Some("No"));
        assert_eq!(us_number("-2", &boolean).as_deref(), Some("Yes"));
        let percent = NumberFormat {
            flags: 0x081c,
            decimal_places: 1,
            ..numeric(0)
        };
        assert_eq!(us_number("0.125", &percent).as_deref(), Some("12.5%"));
        let trailing = NumberFormat {
            negative_style: 1,
            ..numeric(2)
        };
        assert_eq!(us_number("-1234", &trailing).as_deref(), Some("1,234-"));
    }

    #[test]
    fn numeric_dates_use_the_file_order_and_as_entered_ignores_its_template() {
        let us = us();
        let cx = cx(&us);
        let mut format = DateFormat {
            flags: 2 << 24,
            separator: '/',
            ..Default::default()
        };
        assert_eq!(date("4/11/2017", &format, &cx).as_deref(), Some("4/11/17"));
        format.flags = 7 << 24 | 1 << 8 | 1;
        assert_eq!(
            date("4/11/2017", &format, &cx).as_deref(),
            Some("04/11/2017")
        );
        format.flags = 7 << 24 | 2 << 8 | 2;
        assert_eq!(
            date("4/1/2017", &format, &cx).as_deref(),
            Some(" 4/ 1/2017")
        );
        format.flags = 0;
        format.elements = [4, 1, 7, 0];
        assert!(date("4/11/2017", &format, &cx).is_none());
        let german = Locale::de_de();
        let cx = Ctx::new(&german, cx.today);
        format.flags = 2 << 24;
        format.separator = '.';
        assert_eq!(date("11.4.2017", &format, &cx).as_deref(), Some("11.4.17"));
        assert!(date("not a date", &format, &cx).is_none());
    }

    #[test]
    fn system_long_date_and_hour_minute_presets_show_measured_forms() {
        let date = Date::from_ymd(2026, 11, 26).unwrap();
        assert_eq!(
            english_named_date(date, &Locale::en_us(), 4).as_deref(),
            Some("November 26, 2026")
        );
        assert!(english_named_date(date, &Locale::de_de(), 4).is_none());

        let locale = Locale::en_us();
        let style = TimeFormat {
            flags: 0x0083,
            separator: ':',
            minute_second_style: 1,
            am: " AM".into(),
            pm: " PM".into(),
            ..Default::default()
        };
        assert_eq!(
            time("4:31:08 AM", &style, &cx(&locale)).as_deref(),
            Some("4:31 AM")
        );
        assert_eq!(
            time("3:04:05 PM", &style, &cx(&locale)).as_deref(),
            Some("3:04 PM")
        );
    }

    #[test]
    fn fixed_named_date_presets_use_the_measured_month_and_weekday() {
        let locale = Locale::en_us();
        for (code, expected) in [
            (3, "Dec 25, 2024"),
            (4, "December 25, 2024"),
            (5, "Wed, Dec 25, 2024"),
            (6, "Wednesday, December 25, 2024"),
        ] {
            let format = DateFormat {
                flags: code << 24,
                ..Default::default()
            };
            assert_eq!(
                date("12/25/2024", &format, &cx(&locale)).as_deref(),
                Some(expected)
            );
            let german = Locale::de_de();
            assert!(date("25.12.2024", &format, &cx(&german)).is_none());
        }
    }

    #[test]
    fn custom_dates_keep_localized_names_and_all_five_literal_separators() {
        let us = us();
        let format = DateFormat {
            flags: 1 << 24,
            elements: [3, 6, 1, 8],
            separators: ["[".into(), ", ".into(), " ".into(), ", ".into(), "]".into()],
            ..Default::default()
        };
        assert_eq!(
            date("4/11/2017", &format, &cx(&us)).as_deref(),
            Some("[Tuesday, April 11, 2017]")
        );
        let quarter = DateFormat {
            elements: [9, 8, 0, 0],
            separators: Default::default(),
            ..format
        };
        let expected = format!("{}2017", us.quarter_abbreviations[3]);
        assert_eq!(
            date("11/30/2017", &quarter, &cx(&us)).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn time_styles_choose_components_clock_padding_and_markers() {
        let us = us();
        let cx = cx(&us);
        let mut format = TimeFormat {
            flags: 0x83,
            minute_second_style: 1,
            separator: ':',
            am: " AM".into(),
            pm: " PM".into(),
            ..Default::default()
        };
        assert_eq!(time("21:03:04", &format, &cx).as_deref(), Some("9:03 PM"));
        assert_eq!(time("0:15:00", &format, &cx).as_deref(), Some("12:15 AM"));
        format.flags = 0xc3;
        assert_eq!(time("9:03:04", &format, &cx).as_deref(), Some(" AM9:03"));
        format.flags = 0x97;
        format.hour_style = 1;
        format.marker_24h = " h".into();
        assert_eq!(time("9:03:04", &format, &cx).as_deref(), Some("09:03:04 h"));
        // Without hours, minutes count the elapsed time.
        format.flags = 0x96;
        format.marker_24h.clear();
        assert_eq!(time("1:03:04", &format, &cx).as_deref(), Some("63:04"));
        // As entered, word separators, negative times and fractional seconds
        // keep the value's text.
        format.flags = 0x8f;
        assert!(time("9:03:04", &format, &cx).is_none());
        format.flags = 0x17;
        assert!(time("9:03:04", &format, &cx).is_none());
        format.flags = 0x97;
        assert!(time("-0:00:01", &format, &cx).is_none());
        assert!(time("9:03:04.5", &format, &cx).is_none());
        format.flags = 0x93;
        assert_eq!(time("9:03:04.5", &format, &cx).as_deref(), Some("09:03"));
    }

    #[test]
    fn timestamps_format_each_part_and_keep_an_as_entered_part() {
        let us = us();
        let cx = cx(&us);
        let mut format = DataFormat {
            font_height: 11,
            number: NumberFormat::default(),
            date: DateFormat {
                flags: 7 << 24 | 1 << 8 | 1,
                separator: '-',
                ..Default::default()
            },
            time: TimeFormat {
                flags: 0x93,
                hour_style: 1,
                minute_second_style: 1,
                separator: ':',
                ..Default::default()
            },
            raw: Vec::new(),
        };
        let stamp = "4/11/2017 9:03:04 PM";
        assert_eq!(
            display(stamp, ValueType::Timestamp, Some(&format), &cx),
            "04-11-2017 21:03"
        );
        format.time.flags = 0x08;
        assert_eq!(
            display(stamp, ValueType::Timestamp, Some(&format), &cx),
            "04-11-2017 9:03:04 PM"
        );
        format.date.flags = 0;
        assert_eq!(
            display(stamp, ValueType::Timestamp, Some(&format), &cx),
            stamp
        );
        format.time.flags = 0x93;
        assert_eq!(
            display(stamp, ValueType::Timestamp, Some(&format), &cx),
            "4/11/2017 21:03"
        );
    }

    #[test]
    fn date_only_and_time_only_timestamps_format_the_parts_they_have() {
        let us = us();
        let cx = cx(&us);
        let mut format = DataFormat {
            font_height: 11,
            number: NumberFormat::default(),
            date: DateFormat {
                flags: 7 << 24 | 1 << 8 | 1,
                separator: '-',
                ..Default::default()
            },
            time: TimeFormat {
                flags: 0x93,
                hour_style: 1,
                minute_second_style: 1,
                separator: ':',
                ..Default::default()
            },
            raw: Vec::new(),
        };
        let show = |value: &str, format: &DataFormat| {
            display(value, ValueType::Timestamp, Some(format), &cx).into_owned()
        };
        // A date alone is midnight; a time alone falls on 1/1/0001 and its
        // marker belongs to the time.
        assert_eq!(show("4/11/2017", &format), "04-11-2017 00:00");
        assert_eq!(show("9:03:04 PM", &format), "01-01-0001 21:03");
        // An as-entered part the value does not have is left out.
        format.time.flags = 0x08;
        assert_eq!(show("4/11/2017", &format), "04-11-2017");
        assert_eq!(show("9:03:04 PM", &format), "01-01-0001 9:03:04 PM");
        format.date.flags = 0;
        format.time.flags = 0x93;
        assert_eq!(show("9:03:04 PM", &format), "21:03");
        assert_eq!(show("4/11/2017", &format), "4/11/2017 00:00");
        // A no-break space between a date's fields does not end the date.
        assert_eq!(
            show("4\u{a0}11\u{a0}2017 9:03 PM", &format),
            "4\u{a0}11\u{a0}2017 21:03"
        );
    }
}
