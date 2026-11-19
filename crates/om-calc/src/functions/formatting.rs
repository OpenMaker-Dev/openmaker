//! Text formatting functions and the style keywords they take: `RGB`,
//! `TextColor`, `TextColorRemove`, `TextFont`, `TextFontRemove`,
//! `TextFormatRemove`, `TextSize`, `TextSizeRemove`, `TextStyleAdd`,
//! `TextStyleRemove`; keywords `Plain`, `Bold`, `Italic`, ... and the script
//! keywords of `TextFont` (`Roman`, `Greek`, ...).
//!
//! Each function returns its first argument's text (numbers, dates and
//! times as their text) with every character's style changed, unstyled
//! characters included. Numeric arguments are truncated, negatives count as
//! 0 and larger values wrap: colors modulo 2^24, sizes and style sets
//! modulo 2^16 (`TextColor(t; 16777217)` is `#000001`, `TextSize(t;
//! 65548)` is 12). A size of 0 leaves the size alone. `TextFont`'s script
//! argument has no effect that can be observed, not even on
//! `TextFontRemove`, and is ignored. `Extend` the style keyword shares its
//! name with the repeating-field function, which the evaluator owns.

use super::{arg, Function, Keyword};
use crate::text::style_ops::{self, same_font};
use crate::{CalcError, CharStyle, Ctx, Number, Value};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("RGB", rgb),
    Function::pure("TextColor", text_color),
    Function::pure("TextColorRemove", text_color_remove),
    Function::pure("TextFont", text_font),
    Function::pure("TextFontRemove", text_font_remove),
    Function::pure("TextFormatRemove", text_format_remove),
    Function::pure("TextSize", text_size),
    Function::pure("TextSizeRemove", text_size_remove),
    Function::pure("TextStyleAdd", text_style_add),
    Function::pure("TextStyleRemove", text_style_remove),
];

pub(super) const KEYWORDS: &[Keyword] = &[
    ("Plain", 0),
    ("Bold", style_ops::BOLD as i64),
    ("Italic", style_ops::ITALIC as i64),
    ("Underline", style_ops::UNDERLINE as i64),
    ("Condense", style_ops::CONDENSE as i64),
    ("Extend", style_ops::EXTEND as i64),
    ("Strikethrough", style_ops::STRIKETHROUGH as i64),
    ("SmallCaps", style_ops::SMALL_CAPS as i64),
    ("Superscript", style_ops::SUPERSCRIPT as i64),
    ("Subscript", style_ops::SUBSCRIPT as i64),
    ("Uppercase", style_ops::UPPERCASE as i64),
    ("Lowercase", style_ops::LOWERCASE as i64),
    ("Titlecase", style_ops::TITLECASE as i64),
    ("WordUnderline", style_ops::WORD_UNDERLINE as i64),
    ("DoubleUnderline", style_ops::DOUBLE_UNDERLINE as i64),
    ("AllStyles", style_ops::ALL_STYLES as i64),
    ("HighlightYellow", style_ops::HIGHLIGHT_YELLOW as i64),
    // Font scripts (`TextFont`'s third argument).
    ("Roman", 0),
    ("Greek", 1),
    ("Cyrillic", 2),
    ("CentralEurope", 3),
    ("ShiftJIS", 4),
    ("TraditionalChinese", 5),
    ("SimplifiedChinese", 6),
    ("OEM", 7),
    ("Symbol", 8),
    ("Other", 10),
];

/// A numeric argument truncated toward zero, negatives as 0, modulo
/// 2^`bits`; empty, text without digits and `?` are 0.
fn wrapped(v: &Value, bits: u32, cx: &Ctx) -> u32 {
    let n = v.numeric_operand(cx).int();
    if n.is_nan() || n.is_empty() || n.is_negative() {
        return 0;
    }
    let modulus = Number::from_i64(1 << bits);
    let rem = n.modulo(&modulus, crate::number::DEFAULT_PRECISION);
    rem.to_i64().unwrap_or(0) as u32
}

/// The first argument's text with `f` applied to every character's style,
/// neighbouring runs that end up equal merged (the setting functions, and
/// the removing ones called without a value to remove).
fn restyle(args: &[Value], cx: &Ctx, f: impl FnMut(&CharStyle) -> CharStyle) -> Value {
    Value::Text(args[0].to_text(cx).map_styles(f))
}

/// The removing functions: given a value to remove they change the runs
/// that have it in place and keep every run boundary (calc-values.md 13,
/// value-core answers); without one they remove it from every run and
/// merge equal neighbours.
fn remove(args: &[Value], cx: &Ctx, f: impl FnMut(&CharStyle) -> CharStyle) -> Value {
    if args.len() > 1 {
        Value::Text(args[0].to_text(cx).restyle(f))
    } else {
        restyle(args, cx, f)
    }
}

/// `RGB(red; green; blue)` = `red * 65536 + green * 256 + blue`, computed
/// as numbers with no clamping (`RGB(256;0;0)` = 16777216, `RGB(-1;0;0)` =
/// -65536, `RGB(0;0;1.5)` = 1.5); empty and text without digits are 0.
fn rgb(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let p = cx.precision;
    let n = |v: &Value| {
        let n = v.numeric_operand(cx);
        if n.is_empty() {
            Number::zero()
        } else {
            n
        }
    };
    let total = n(&args[0])
        .mul(&Number::from_i64(65536), p)
        .add(&n(&args[1]).mul(&Number::from_i64(256), p), p)
        .add(&n(&args[2]), p);
    Ok(Value::Number(total))
}

fn text_color(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let color = wrapped(&args[1], 24, cx);
    Ok(restyle(args, cx, |s| CharStyle {
        color: Some(color),
        ..s.clone()
    }))
}

/// Removes every color, or only `color` (`""` is black).
fn text_color_remove(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let only = arg(args, 1).map(|v| wrapped(v, 24, cx));
    Ok(remove(args, cx, |s| {
        let mut s = s.clone();
        if only.is_none() || s.color == only {
            s.color = None;
        }
        s
    }))
}

/// Sets the font to the name's characters (`""` is a font name too).
fn text_font(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let name = args[1].to_text(cx).into_string();
    Ok(restyle(args, cx, |s| CharStyle {
        font: Some(name.clone()),
        ..s.clone()
    }))
}

/// Removes every font, or the fonts with the given name ignoring case and
/// width.
fn text_font_remove(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let only = arg(args, 1).map(|v| v.to_text(cx).into_string());
    Ok(remove(args, cx, |s| {
        let mut s = s.clone();
        let matches = match (&only, &s.font) {
            (None, _) => true,
            (Some(name), Some(font)) => same_font(name, font),
            (Some(_), None) => false,
        };
        if matches {
            s.font = None;
        }
        s
    }))
}

fn text_format_remove(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(Value::Text(args[0].to_text(cx).without_styles()))
}

fn text_size(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let size = wrapped(&args[1], 16, cx) as u16;
    if size == 0 {
        return Ok(Value::Text(args[0].to_text(cx)));
    }
    Ok(restyle(args, cx, |s| CharStyle {
        size: Some(size),
        ..s.clone()
    }))
}

/// Removes every size, or only `size` (0 removes none).
fn text_size_remove(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let only = arg(args, 1).map(|v| wrapped(v, 16, cx) as u16);
    Ok(remove(args, cx, |s| {
        let mut s = s.clone();
        if only.is_none() || s.size == only {
            s.size = None;
        }
        s
    }))
}

/// See [`CharStyle::with_styles_added`]; `Plain`, and any value that wraps
/// to 0, sets every style to normal.
fn text_style_add(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let styles = wrapped(&args[1], 16, cx) as u16;
    Ok(restyle(args, cx, |s| s.with_styles_added(styles)))
}

/// See [`CharStyle::with_styles_removed`].
fn text_style_remove(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let styles = wrapped(&args[1], 16, cx) as u16;
    if styles == 0 {
        return Ok(Value::Text(args[0].to_text(cx)));
    }
    Ok(remove(args, cx, |s| s.with_styles_removed(styles)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Date, Locale};

    #[test]
    fn wrapping() {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
        let w = |s: &str, bits| {
            let n = Number::parse_text(s, '.', ',');
            wrapped(&Value::Number(n), bits, &cx)
        };
        assert_eq!(w("16777217", 24), 1);
        assert_eq!(w("-1", 24), 0);
        assert_eq!(w("255.9", 24), 255);
        assert_eq!(w("100000000000000000000", 24), 0x100000);
        assert_eq!(w("65548", 16), 12);
        assert_eq!(w("4294967552", 16), 256);
        assert_eq!(wrapped(&Value::text("abc"), 16, &cx), 0);
        assert_eq!(wrapped(&Value::invalid(), 16, &cx), 0);
    }
}
