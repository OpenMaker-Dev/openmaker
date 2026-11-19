//! Text to number, `FixPt::Set(unistring, Locale)` (spec
//! `docs/re/calc-values.md` 2.8), and formula number literals (2.2, 7.1).

use super::{mag, Fin, Number, DEFAULT_PRECISION, GUARD, MAX_PRECISION};
use crate::error::CalcError;

/// Exponents of this size or more are out of range: a literal is error 1218,
/// text converts to `?` (`1e800`, `0.5e800`, `100e-801`; `10e799` and
/// `0.1e-799` are fine). The limit is on the written exponent.
const EXPONENT_LIMIT: u64 = 800;

/// A decimal digit, ASCII or fullwidth (`１２３` is 123); other scripts' digits
/// are not digits (`١٢٣` has none).
fn digit_value(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(c as u8 - b'0'),
        '\u{ff10}'..='\u{ff19}' => Some((c as u32 - 0xff10) as u8),
        _ => None,
    }
}

fn is_minus(c: char) -> bool {
    matches!(c, '-' | '\u{2010}' | '\u{2212}' | '\u{fe63}')
}

/// Text reads fullwidth ASCII forms as ASCII (`（１ｅ２）`, `１，５`, `－1`)
/// and the small parentheses U+FE59/U+FE5A as parentheses; other characters
/// stay as they are (superscript parentheses are not parentheses).
fn narrow(c: char) -> char {
    match c {
        '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
        '\u{fe59}' => '(',
        '\u{fe5a}' => ')',
        _ => c,
    }
}

/// Builds the number `0.digits[..point]digits[point..] * 10^exponent` with
/// `frac` digits written after the point.
fn assemble(neg: bool, digits: &[u8], frac: u64, exponent: i64) -> Number {
    let ascii: Vec<u8> = digits.iter().map(|d| d + b'0').collect();
    let coef = mag::from_digits(&ascii);
    let scale = frac as i64 - exponent;
    let precision = scale.clamp(i64::from(DEFAULT_PRECISION), i64::from(MAX_PRECISION)) as u16;
    let fin = if scale >= 0 {
        Fin::new(neg, coef, scale as u32, precision)
    } else {
        Fin::new(neg, mag::shl_digits(&coef, (-scale) as u32), 0, precision)
    };
    Number::from_fin(fin.truncated(u32::from(precision) + GUARD))
}

/// Mantissa digits, the count after the point, and the exponent of a
/// strict `digits [point digits] [e [sign] digits]` form starting at `i`.
struct Strict {
    digits: Vec<u8>,
    frac: u64,
    exponent: i64,
    end: usize,
    /// The mantissa ended with the point and no digits after it.
    bare_point: bool,
}

fn strict(
    chars: &[char],
    mut i: usize,
    decimal: char,
    thousands: Option<&[char]>,
) -> Option<Strict> {
    let mut digits = Vec::new();
    let mut frac = 0u64;
    let mut in_frac = false;
    let mut bare_point = false;
    while let Some(&c) = chars.get(i) {
        if let Some(d) = digit_value(c) {
            digits.push(d);
            if in_frac {
                frac += 1;
            }
            bare_point = false;
        } else if c == decimal && !in_frac {
            in_frac = true;
            bare_point = true;
        } else if !in_frac
            && thousands.is_some_and(|t| t.contains(&c))
            && i > 0
            && chars.get(i - 1).copied().and_then(digit_value).is_some()
            && chars.get(i + 1).copied().and_then(digit_value).is_some()
        {
            // a thousands separator between digits
        } else if let Some(t) = thousands.filter(|t| !in_frac && t.contains(&c)) {
            // Separators after a digit and before the exponent are skipped
            // (`1,e2` and, where the separator is a space, `1  e2` are 100;
            // U+202F does not count here).
            let run = chars[i..].iter().take_while(|c| t.contains(c)).count();
            let after_digit = i > 0 && chars.get(i - 1).copied().and_then(digit_value).is_some();
            if !(after_digit
                && matches!(chars.get(i + run), Some('e' | 'E'))
                && !chars[i..i + run].contains(&'\u{202f}'))
            {
                break;
            }
            i += run;
            continue;
        } else {
            break;
        }
        i += 1;
    }
    if digits.is_empty() {
        return None;
    }
    let mut exponent = 0i64;
    if matches!(chars.get(i), Some('e' | 'E')) {
        let mut j = i + 1;
        let mut neg = false;
        match chars.get(j) {
            Some('+') => j += 1,
            Some(&c) if is_minus(c) => {
                neg = true;
                j += 1;
            }
            _ => {}
        }
        let start = j;
        let mut value: u64 = 0;
        while let Some(d) = chars.get(j).copied().and_then(digit_value) {
            value = value.saturating_mul(10).saturating_add(u64::from(d));
            j += 1;
        }
        if j == start {
            return None;
        }
        if value >= EXPONENT_LIMIT {
            exponent = if neg { i64::MIN } else { i64::MAX };
        } else {
            exponent = if neg { -(value as i64) } else { value as i64 };
        }
        i = j;
    }
    Some(Strict {
        digits,
        frac,
        exponent,
        end: i,
        bare_point,
    })
}

impl Number {
    /// A number literal of formula text (`1.5`, `.5`, `1e3`, `1,5` in a
    /// locale whose decimal separator is `,`): p = max(16, fractional digits
    /// written), all digits kept. `5.` and an exponent of 800 or more in
    /// magnitude are error 1218.
    pub fn from_literal(token: &str, decimal: char) -> Result<Number, CalcError> {
        let chars: Vec<char> = token.chars().collect();
        let s = strict(&chars, 0, decimal, None).ok_or(CalcError::CANNOT_EVALUATE_NUMBER)?;
        if s.end != chars.len() || s.bare_point || s.exponent.unsigned_abs() >= EXPONENT_LIMIT {
            return Err(CalcError::CANNOT_EVALUATE_NUMBER);
        }
        Ok(assemble(false, &s.digits, s.frac, s.exponent))
    }

    /// Text to number (spec 2.8, and the value-core oracle answers in
    /// calc-values.md 13). Fullwidth forms read as ASCII. A text that is,
    /// after leading whitespace, an optional sign, any `(` and spaces,
    /// digits with thousands separators between them, an optional decimal
    /// part, an optional exponent, then any `)` and spaces converts exactly
    /// ("pure"; trailing spaces are allowed unless the thousands separator
    /// is a space); it is negative for a `-` sign or when it has both a `(`
    /// and a `)` (`(1e2)` is -100, `(1e2` is 100). Anything else is read
    /// "impurely" (see [`impure`]). No digits gives the empty number;
    /// exactly `?` gives `?` (`" ? "` and `"??"` are empty), and so does a
    /// pure exponent of 800 or more.
    pub fn parse_text(text: &str, decimal: char, thousands: char) -> Number {
        if text == "?" {
            return Number::nan();
        }
        Self::parse_pure_text(text, decimal, thousands)
            .unwrap_or_else(|| impure(&text.chars().map(narrow).collect::<Vec<_>>(), decimal))
    }

    /// The number a text is when it is one in the "pure" form of
    /// [`Number::parse_text`] (`1.5`, `-3`, `1e3`, `(12)`, `1,000`), `None`
    /// when it is not (`12abc`, `abc`, `$5`): what a field validated as
    /// "Strict data type: Numeric only" accepts (script-runtime.md 13.21).
    pub fn parse_pure_text(text: &str, decimal: char, thousands: char) -> Option<Number> {
        let spaces = ['\u{20}', '\u{a0}', '\u{202f}'];
        let space_thousands = spaces.contains(&thousands);
        let thousand_chars: &[char] = if space_thousands {
            &spaces
        } else {
            std::slice::from_ref(&thousands)
        };
        let chars: Vec<char> = text.chars().map(narrow).collect();
        pure(&chars, decimal, thousand_chars, space_thousands)
    }
}

fn pure(
    chars: &[char],
    decimal: char,
    thousands: &[char],
    space_thousands: bool,
) -> Option<Number> {
    let mut start = chars.iter().take_while(|c| c.is_whitespace()).count();
    let mut end = chars.len();
    if !space_thousands {
        while end > start && chars[end - 1] == ' ' {
            end -= 1;
        }
    }
    let mut neg = false;
    match chars.get(start) {
        Some('+') => start += 1,
        Some(&c) if is_minus(c) => {
            neg = true;
            start += 1;
        }
        _ => {}
    }
    // Spaces may pad the parentheses unless the space is the thousands
    // separator (`( 1e3 )` is -1000 in en-US, impure in fr-FR).
    let mut opened = false;
    while let Some(&c) = chars.get(start) {
        match c {
            '(' => opened = true,
            ' ' if !space_thousands => {}
            _ => break,
        }
        start += 1;
    }
    let s = strict(&chars[..end], start, decimal, Some(thousands))?;
    let mut closed = false;
    for &c in &chars[s.end..end] {
        match c {
            ')' => closed = true,
            ' ' if !space_thousands => {}
            _ => return None,
        }
    }
    if s.exponent.unsigned_abs() >= EXPONENT_LIMIT {
        return Some(Number::nan());
    }
    Some(assemble(
        neg || (opened && closed),
        &s.digits,
        s.frac,
        s.exponent,
    ))
}

/// The impure reading: every digit in order, the first decimal separator
/// placing the point. Signs and parentheses count only before the first
/// digit and the decimal separator: the first `+` or `-` is the sign, a `(`
/// opens and a `)` cancels the opening; between the separator and the
/// first digit a `(` or `)` cancels it too and signs are ignored
/// (`.(22)` and `.-22` are .22, `(.)22` is .22). After the first digit the
/// first parenthesis decides: `)` closes an open one (negative), `(` ends
/// the matter (`(1(2))` is 12, `(1)(2)` is -12).
fn impure(chars: &[char], decimal: char) -> Number {
    let mut digits = Vec::new();
    let mut point: Option<usize> = None;
    let mut first_sign: Option<bool> = None;
    let mut open = false;
    let mut closed = false;
    let mut decided = false;
    for &c in chars {
        if let Some(d) = digit_value(c) {
            digits.push(d);
        } else if c == decimal && point.is_none() {
            point = Some(digits.len());
        } else if !digits.is_empty() {
            if !decided && matches!(c, '(' | ')') {
                closed = c == ')';
                decided = true;
            }
        } else if point.is_some() {
            if matches!(c, '(' | ')') {
                open = false;
            }
        } else if first_sign.is_none() && (is_minus(c) || c == '+') {
            first_sign = Some(is_minus(c));
        } else if c == '(' {
            open = true;
        } else if c == ')' {
            open = false;
        }
    }
    if digits.is_empty() {
        return Number::empty();
    }
    let frac = point.map_or(0, |p| digits.len() - p) as u64;
    let neg = first_sign == Some(true) || (open && closed);
    assemble(neg, &digits, frac, 0)
}

#[cfg(test)]
mod tests {
    use crate::error::CalcError;
    use crate::number::Number;

    fn en(s: &str) -> String {
        Number::parse_text(s, '.', ',').to_text_with('.')
    }

    fn de(s: &str) -> String {
        Number::parse_text(s, ',', '.').to_text_with(',')
    }

    fn sv(s: &str) -> String {
        Number::parse_text(s, ',', ' ').to_text_with(',')
    }

    #[test]
    fn spec_table_en_us() {
        for (text, number) in [
            ("abc12d3.4", "123.4"),
            ("1.2.3", "1.23"),
            ("1-2", "12"),
            ("a-1", "-1"),
            ("--12", "-12"),
            ("12-", "12"),
            ("(12)", "-12"),
            ("$1,234.56", "1234.56"),
            ("1,5", "15"),
            ("1.234,56", "1.23456"),
            ("1.5e2x", "1.52"),
            ("1e3e4", "134"),
            ("1/2/2003", "122003"),
            ("12:30", "1230"),
            ("5.", "5"),
            (".5", ".5"),
            ("１２３", "123"),
            ("١٢٣", ""),
            ("¼", ""),
            ("abc", ""),
            (" ", ""),
            ("-", ""),
            (".", ""),
            ("?", "?"),
            ("1e800", "?"),
            ("0.000000000000000000001", "1.0E-21"),
            ("1e3", "1000"),
            ("1e-3", ".001"),
            ("1,5e3", "15000"),
            (" 12 ", "12"),
            ("+-1", "1"),
            ("-+1", "-1"),
            ("(1)2", "-12"),
            ("1(2)", "12"),
            ("(a)1", "1"),
            ("((1))", "-1"),
            ("−5", "-5"),
            ("1e3 ", "1000"),
            ("1e3\t", "13"),
            ("\t1e3", "1000"),
            ("1e+", "1"),
            ("5.e1", "50"),
            (".5e1", "5"),
            ("1,234.5e2", "123450"),
        ] {
            assert_eq!(en(text), number, "{text:?}");
        }
    }

    #[test]
    fn parentheses_and_the_decimal_separator() {
        for (text, number) in [
            (".(22)", ".22"),
            ("(.22)", "-.22"),
            ("(.)22", ".22"),
            (".-22", ".22"),
            ("-.22", "-.22"),
            ("(.(22))", ".22"),
            ("(1(2))", "12"),
            ("(1)(2)", "-12"),
            ("((1)2)", "-12"),
            ("(a)1)", "1"),
            ("((a)1)", "1"),
            ("(a.)1)", ".1"),
            ("(.1)2)", "-.12"),
            ("1(.2)", "1.2"),
            (",(22)", "-22"),
            (",-22", "-22"),
            ("(1e2)", "-100"),
            ("((1e2))", "-100"),
            ("( 1e2 )", "-100"),
            ("(1e2", "100"),
            ("1e2)", "100"),
            ("-(1e3)", "-1000"),
            ("(-1e3)", "-13"),
            ("(+1e2)", "-12"),
            ("(1e2)x", "-12"),
            ("(\t1e2)", "-12"),
            ("\t(1e2)", "-100"),
            ("(1e800)", "?"),
            ("（1e2）", "-100"),
            ("1．5", "1.5"),
            ("．(22)", ".22"),
            ("－12", "-12"),
            ("‐1", "-1"),
            ("﹙1﹚", "-1"),
            ("⁽1⁾", "1"),
            ("1ｅ2", "100"),
        ] {
            assert_eq!(en(text), number, "{text:?}");
        }
        assert_eq!(de(",(22)"), ",22");
        assert_eq!(de(".(22)"), "-22");
        assert_eq!(de("１，５"), "1,5");
        assert_eq!(de("(.5e1)"), "-51");
    }

    #[test]
    fn comma_locales() {
        assert_eq!(de("1,5"), "1,5");
        assert_eq!(de("1.5"), "15");
        assert_eq!(de("1.234,56"), "1234,56");
        assert_eq!(de("1,234.56"), "1,23456");
        assert_eq!(de("1,5e3"), "1500");
        assert_eq!(sv("1e3 "), "13");
        assert_eq!(sv("1 000,5"), "1000,5");
        assert_eq!(sv("1\u{a0}000,5"), "1000,5");
        assert_eq!(sv(",5"), ",5");
        assert_eq!(sv("1.5e-20"), "1520");
    }

    #[test]
    fn precision_follows_the_written_digits() {
        let n = Number::parse_text("0.1234567890123456789", '.', ',');
        assert_eq!(n.precision(), 19);
        let n = Number::parse_text("1.0000000000000000000e2", '.', ',');
        assert_eq!(n.precision(), 17);
        assert_eq!(Number::parse_text("1e-20", '.', ',').precision(), 20);
    }

    #[test]
    fn literals() {
        let lit = |s: &str| Number::from_literal(s, '.').map(|n| n.to_text_with('.'));
        assert_eq!(lit("1.50").unwrap(), "1.5");
        assert_eq!(lit("0001").unwrap(), "1");
        assert_eq!(lit(".5").unwrap(), ".5");
        assert_eq!(lit("1e-400").unwrap(), "1.0E-400");
        assert_eq!(lit("10e799").unwrap(), "1.0E+800");
        assert_eq!(lit("1e0799").unwrap(), "1.0E+799");
        assert_eq!(lit("5.").unwrap_err(), CalcError::CANNOT_EVALUATE_NUMBER);
        assert_eq!(lit("1e800").unwrap_err(), CalcError::CANNOT_EVALUATE_NUMBER);
        assert_eq!(
            lit("0.5e800").unwrap_err(),
            CalcError::CANNOT_EVALUATE_NUMBER
        );
        assert_eq!(
            lit("100e-801").unwrap_err(),
            CalcError::CANNOT_EVALUATE_NUMBER
        );
        assert_eq!(lit("0.1e-799").unwrap(), "1.0E-800");
        assert_eq!(
            Number::from_literal("1,5", ',').unwrap().to_text_with(','),
            "1,5"
        );
    }
}
