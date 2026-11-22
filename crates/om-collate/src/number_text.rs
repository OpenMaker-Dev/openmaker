//! Text to number as FileMaker reads it, `FixPt::Set(unistring, Locale)`
//! (spec `docs/re/calc-values.md` 2.8, and the value-core oracle answers in
//! its section 13): the reading behind `GetAsNumber`, arithmetic on text, and
//! a number field's index keys (`DBFixPtKey::InternalSet<FixPt>`,
//! `docs/re/query-program.md` 7.3). [`read_number`] gives the digits read;
//! the calculation engine builds its numbers from them and
//! [`crate::number_key_in`] its keys.

/// Exponents of this size or more are out of range: a literal is error 1218,
/// text converts to `?` (`1e800`, `0.5e800`, `100e-801`; `10e799` and
/// `0.1e-799` are fine). The limit is on the written exponent.
pub const EXPONENT_LIMIT: u64 = 800;

/// A number read from text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadNumber {
    /// No digits: the empty number.
    Empty,
    /// `?`: exactly `?`, or a pure exponent of [`EXPONENT_LIMIT`] or more.
    Invalid,
    /// `0.digits[..point]digits[point..] * 10^exponent`, negative when
    /// `negative`, with `frac` of the mantissa's `digits` (each 0 to 9)
    /// written after the point.
    Value {
        negative: bool,
        digits: Vec<u8>,
        frac: u64,
        exponent: i64,
    },
}

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

fn value(negative: bool, s: Strict) -> ReadNumber {
    ReadNumber::Value {
        negative,
        digits: s.digits,
        frac: s.frac,
        exponent: s.exponent,
    }
}

/// A number literal of formula text (`1.5`, `.5`, `1e3`, `1,5` in a locale
/// whose decimal separator is `,`), all digits kept; `None` for anything
/// else, `5.` and an exponent of [`EXPONENT_LIMIT`] or more in magnitude
/// included.
pub fn read_number_literal(token: &str, decimal: char) -> Option<ReadNumber> {
    let chars: Vec<char> = token.chars().collect();
    let s = strict(&chars, 0, decimal, None)?;
    if s.end != chars.len() || s.bare_point || s.exponent.unsigned_abs() >= EXPONENT_LIMIT {
        return None;
    }
    Some(value(false, s))
}

/// Text to number (spec 2.8). Fullwidth forms read as ASCII. A text that
/// is, after leading whitespace, an optional sign, any `(` and spaces,
/// digits with thousands separators between them, an optional decimal part,
/// an optional exponent, then any `)` and spaces converts exactly ("pure";
/// trailing spaces are allowed unless the thousands separator is a space);
/// it is negative for a `-` sign or when it has both a `(` and a `)`
/// (`(1e2)` is -100, `(1e2` is 100). Anything else is read "impurely" (see
/// [`impure`]). No digits gives [`ReadNumber::Empty`]; exactly `?` gives
/// [`ReadNumber::Invalid`] (`" ? "` and `"??"` are empty), and so does a
/// pure exponent of 800 or more.
pub fn read_number(text: &str, decimal: char, thousands: char) -> ReadNumber {
    if text == "?" {
        return ReadNumber::Invalid;
    }
    read_pure_number(text, decimal, thousands)
        .unwrap_or_else(|| impure(&text.chars().map(narrow).collect::<Vec<_>>(), decimal))
}

/// The number a text is when it is one in the "pure" form of
/// [`read_number`] (`1.5`, `-3`, `1e3`, `(12)`, `1,000`), `None` when it is
/// not (`12abc`, `abc`, `$5`).
pub fn read_pure_number(text: &str, decimal: char, thousands: char) -> Option<ReadNumber> {
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

fn pure(
    chars: &[char],
    decimal: char,
    thousands: &[char],
    space_thousands: bool,
) -> Option<ReadNumber> {
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
        return Some(ReadNumber::Invalid);
    }
    Some(value(neg || (opened && closed), s))
}

/// The impure reading: every digit in order, the first decimal separator
/// placing the point. Signs and parentheses count only before the first
/// digit and the decimal separator: the first `+` or `-` is the sign, a `(`
/// opens and a `)` cancels the opening; between the separator and the
/// first digit a `(` or `)` cancels it too and signs are ignored
/// (`.(22)` and `.-22` are .22, `(.)22` is .22). After the first digit the
/// first parenthesis decides: `)` closes an open one (negative), `(` ends
/// the matter (`(1(2))` is 12, `(1)(2)` is -12).
fn impure(chars: &[char], decimal: char) -> ReadNumber {
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
        return ReadNumber::Empty;
    }
    let frac = point.map_or(0, |p| digits.len() - p) as u64;
    ReadNumber::Value {
        negative: first_sign == Some(true) || (open && closed),
        digits,
        frac,
        exponent: 0,
    }
}
