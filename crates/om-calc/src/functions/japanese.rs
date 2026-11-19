//! Japanese functions: `DayNameJ`, `Hiragana`, `KanaHankaku`, `KanaZenkaku`,
//! `KanjiNumeral`, `Katakana`, `MonthNameJ`, `NumToJText`, `RomanHankaku`,
//! `RomanZenkaku`, `YearName`, and the undocumented `LunarDate` and
//! `SolarDate`, which the engine parses but answers with error 1223.
//! `Furigana` needs the platform and is not here.
//!
//! None depends on the file's locale except through text-to-date and
//! number-to-text conversion. The character conversions keep each
//! character's style (two halfwidth characters that combine take the
//! first's) and return their argument unchanged, type and all, when
//! nothing changes; a number keeps its value under the new text
//! (`KanjiNumeral(123) + 1` = 124).

use super::datetime::date_arg;
use super::{Function, Keyword};
use crate::datetime::{day_number_of, loose_date_fields, year_of_day, Date};
use crate::value::parse_number;
use crate::{CalcError, Ctx, Number, Text, Value};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("DayNameJ", day_name_j),
    Function::pure("Hiragana", hiragana),
    Function::pure("KanaHankaku", kana_hankaku),
    Function::pure("KanaZenkaku", kana_zenkaku),
    Function::pure("KanjiNumeral", kanji_numeral),
    Function::pure("Katakana", katakana),
    Function::pure("MonthNameJ", month_name_j),
    Function::pure("NumToJText", num_to_j_text),
    Function::pure("RomanHankaku", roman_hankaku),
    Function::pure("RomanZenkaku", roman_zenkaku),
    Function::pure("YearName", year_name),
    Function::pure("LunarDate", not_implemented),
    Function::pure("SolarDate", not_implemented),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

/// 1223, "function referred to is not yet implemented".
const NOT_IMPLEMENTED: CalcError = match CalcError::from_code(1223) {
    Some(e) => e,
    None => unreachable!(),
};

fn not_implemented(_: &[Value], _: &Ctx) -> Result<Value, CalcError> {
    Err(NOT_IMPLEMENTED)
}

// ---------------------------------------------------------------------------
// Character conversions.

/// The halfwidth katakana U+FF66..=U+FF9D as fullwidth katakana.
const HALFWIDTH_KANA: [char; 56] = [
    'ヲ', 'ァ', 'ィ', 'ゥ', 'ェ', 'ォ', 'ャ', 'ュ', 'ョ', 'ッ', 'ー', 'ア', 'イ', 'ウ', 'エ', 'オ',
    'カ', 'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ', 'タ', 'チ', 'ツ', 'テ', 'ト', 'ナ',
    'ニ', 'ヌ', 'ネ', 'ノ', 'ハ', 'ヒ', 'フ', 'ヘ', 'ホ', 'マ', 'ミ', 'ム', 'メ', 'モ', 'ヤ', 'ユ',
    'ヨ', 'ラ', 'リ', 'ル', 'レ', 'ロ', 'ワ', 'ン',
];
const HALFWIDTH_VOICED: char = 'ﾞ';
const HALFWIDTH_SEMI_VOICED: char = 'ﾟ';

/// The fullwidth katakana of a halfwidth one.
fn widen_kana(c: char) -> Option<char> {
    let i = (c as u32).checked_sub(0xFF66)? as usize;
    HALFWIDTH_KANA.get(i).copied()
}

/// A fullwidth katakana with a voiced or semi-voiced mark, if it has one.
fn voiced(base: char, mark: char) -> Option<char> {
    let shift = |n: u32| char::from_u32(base as u32 + n);
    match (base, mark) {
        ('ウ', HALFWIDTH_VOICED) => Some('ヴ'),
        ('ワ', HALFWIDTH_VOICED) => Some('ヷ'),
        ('ヲ', HALFWIDTH_VOICED) => Some('ヺ'),
        (
            'カ' | 'キ' | 'ク' | 'ケ' | 'コ' | 'サ' | 'シ' | 'ス' | 'セ' | 'ソ' | 'タ' | 'チ'
            | 'ツ' | 'テ' | 'ト',
            HALFWIDTH_VOICED,
        ) => shift(1),
        ('ハ' | 'ヒ' | 'フ' | 'ヘ' | 'ホ', HALFWIDTH_VOICED) => shift(1),
        ('ハ' | 'ヒ' | 'フ' | 'ヘ' | 'ホ', HALFWIDTH_SEMI_VOICED) => shift(2),
        _ => None,
    }
}

/// The halfwidth form of a fullwidth katakana, as one or two characters.
fn narrow_kana(c: char) -> Option<String> {
    let single = |c: char| {
        HALFWIDTH_KANA
            .iter()
            .position(|&k| k == c)
            .and_then(|i| char::from_u32(0xFF66 + i as u32))
    };
    if let Some(h) = single(c) {
        return Some(h.to_string());
    }
    match c {
        '・' => return Some('･'.to_string()),
        'ヴ' => return Some("ｳﾞ".to_string()),
        'ヷ' => return Some("ﾜﾞ".to_string()),
        'ヺ' => return Some("ｦﾞ".to_string()),
        _ => {}
    }
    // A voiced or semi-voiced katakana: its base is one or two code points
    // lower.
    for (back, mark) in [(1, HALFWIDTH_VOICED), (2, HALFWIDTH_SEMI_VOICED)] {
        let base = char::from_u32((c as u32).checked_sub(back)?)?;
        if voiced(base, mark) == Some(c) {
            return Some(format!("{}{mark}", single(base)?));
        }
    }
    None
}

/// Hiragana and katakana sit 0x60 apart from ぁ/ァ through ゖ/ヶ.
fn is_hiragana_letter(c: char) -> bool {
    ('ぁ'..='ゖ').contains(&c)
}

fn is_katakana_letter(c: char) -> bool {
    ('ァ'..='ヶ').contains(&c)
}

fn offset(c: char, by: i32) -> char {
    char::from_u32((c as i32 + by) as u32).unwrap_or(c)
}

/// Converts `text` character by character: `step` gets a character and the
/// next one and returns the replacement and whether it used the next one.
fn convert_chars(text: &Text, step: impl Fn(char, Option<char>) -> (String, bool)) -> Text {
    let s = text.as_str();
    let mut out = Text::new();
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let next = chars.peek().map(|&(_, n)| n);
        let (replacement, used_next) = step(c, next);
        if used_next {
            chars.next();
        }
        out.push_styled(&replacement, &text.style_at(i));
    }
    out
}

/// One character replaced by `map`, or kept.
fn per_char(map: impl Fn(char) -> Option<char>) -> impl Fn(char, Option<char>) -> (String, bool) {
    move |c, _| (map(c).unwrap_or(c).to_string(), false)
}

/// The value of a character conversion: the argument itself when nothing
/// changed, a number under its new text, or text.
fn converted(v: &Value, cx: &Ctx, f: impl FnOnce(&Text) -> Text) -> Value {
    let text = v.to_text(cx);
    let out = f(&text);
    if out == text {
        return v.clone();
    }
    match v {
        Value::Number(n) | Value::NumberText(n, _) => Value::NumberText(n.clone(), out),
        _ => Value::Text(out),
    }
}

/// Katakana (fullwidth and halfwidth) to hiragana. Halfwidth katakana with
/// a voiced mark combine (`ｶﾞ` = が), except that `ｳﾞ`, `ﾜﾞ` and `ｦﾞ` give
/// the katakana ヴ, ヷ and ヺ; `ｰ` becomes ー. ヷヸヹヺ, ヽヾ, ・ and
/// halfwidth punctuation stay.
fn hiragana(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(converted(&args[0], cx, |t| {
        convert_chars(t, |c, next| {
            if is_katakana_letter(c) {
                return (offset(c, -0x60).to_string(), false);
            }
            let Some(wide) = widen_kana(c) else {
                return (c.to_string(), false);
            };
            if let Some(v) = next.and_then(|m| voiced(wide, m)) {
                let v = if matches!(v, 'ヴ' | 'ヷ' | 'ヺ') {
                    v
                } else {
                    offset(v, -0x60)
                };
                return (v.to_string(), true);
            }
            let wide = if is_katakana_letter(wide) {
                offset(wide, -0x60)
            } else {
                wide
            };
            (wide.to_string(), false)
        })
    }))
}

/// Hiragana to fullwidth katakana, and ゝゞ to ヽヾ; halfwidth katakana
/// stay.
fn katakana(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(converted(&args[0], cx, |t| {
        convert_chars(
            t,
            per_char(|c| match c {
                c if is_hiragana_letter(c) => Some(offset(c, 0x60)),
                'ゝ' => Some('ヽ'),
                'ゞ' => Some('ヾ'),
                _ => None,
            }),
        )
    }))
}

/// Fullwidth katakana to halfwidth, voiced ones as two characters (ガ =
/// ｶﾞ), ・ to ･ and ー to ｰ. Katakana without a halfwidth form (ヮヰヱヵヶ
/// ヸヹ), 。「」、 and ゛゜ stay.
fn kana_hankaku(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(converted(&args[0], cx, |t| {
        convert_chars(t, |c, _| {
            (narrow_kana(c).unwrap_or_else(|| c.to_string()), false)
        })
    }))
}

/// Halfwidth katakana to fullwidth, combining voiced marks where a voiced
/// katakana exists (`ｶﾞ` = ガ, `ﾜﾞ` = ヷ; `ｱﾞ` = アﾞ), and ･ to ・;
/// halfwidth ｡｢｣､ and lone marks stay.
fn kana_zenkaku(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(converted(&args[0], cx, |t| {
        convert_chars(t, |c, next| {
            if c == '･' {
                return ('・'.to_string(), false);
            }
            let Some(wide) = widen_kana(c) else {
                return (c.to_string(), false);
            };
            match next.and_then(|m| voiced(wide, m)) {
                Some(v) => (v.to_string(), true),
                None => (wide.to_string(), false),
            }
        })
    }))
}

/// Fullwidth ASCII to ASCII, the ideographic space to a space, the
/// fullwidth signs ￠￡￢￣￤￥￦ to ¢£¬¯¦¥₩, and 〝〞 to “”.
fn roman_hankaku(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(converted(&args[0], cx, |t| {
        convert_chars(
            t,
            per_char(|c| match c {
                '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0),
                '\u{3000}' => Some(' '),
                '￠' => Some('¢'),
                '￡' => Some('£'),
                '￢' => Some('¬'),
                '￣' => Some('¯'),
                '￤' => Some('¦'),
                '￥' => Some('¥'),
                '￦' => Some('₩'),
                '〝' => Some('“'),
                '〞' => Some('”'),
                _ => None,
            }),
        )
    }))
}

/// ASCII to fullwidth, a space to the ideographic space, and “” to 〝〞
/// (¢£¬¯¦¥₩ stay).
fn roman_zenkaku(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(converted(&args[0], cx, |t| {
        convert_chars(
            t,
            per_char(|c| match c {
                '!'..='~' => char::from_u32(c as u32 + 0xFEE0),
                ' ' => Some('\u{3000}'),
                '“' => Some('〝'),
                '”' => Some('〞'),
                _ => None,
            }),
        )
    }))
}

const KANJI_DIGITS: [char; 10] = ['〇', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
const DAIJI_DIGITS: [char; 10] = ['〇', '壱', '弐', '参', '四', '伍', '六', '七', '八', '九'];

/// ASCII and fullwidth digits to kanji digits, one for one (`2024` = 二〇二四).
fn kanji_numeral(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(converted(&args[0], cx, |t| {
        convert_chars(
            t,
            per_char(|c| match c {
                '0'..='9' => Some(KANJI_DIGITS[c as usize - '0' as usize]),
                '０'..='９' => Some(KANJI_DIGITS[c as usize - '０' as usize]),
                _ => None,
            }),
        )
    }))
}

// ---------------------------------------------------------------------------
// Dates.

const DAY_NAMES_J: [&str; 7] = [
    "日曜日",
    "月曜日",
    "火曜日",
    "水曜日",
    "木曜日",
    "金曜日",
    "土曜日",
];

/// The Japanese weekday name, in any locale; the argument as `DayName`
/// takes it.
fn day_name_j(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(match date_arg(&args[0], cx)? {
        Ok(d) => Value::text(DAY_NAMES_J[d.weekday() as usize - 1]),
        Err(v) => v,
    })
}

/// The month as `6月`, ASCII digits.
fn month_name_j(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(match date_arg(&args[0], cx)? {
        Ok(d) => Value::text(format!("{}月", d.ymd().1)),
        Err(v) => v,
    })
}

/// A Japanese era.
struct Era {
    /// The first day (year, month, day).
    start: (i64, u32, u32),
    name: &'static str,
    abbreviation: &'static str,
    initial: &'static str,
}

const fn era(
    start: (i64, u32, u32),
    name: &'static str,
    abbreviation: &'static str,
    initial: &'static str,
) -> Era {
    Era {
        start,
        name,
        abbreviation,
        initial,
    }
}

/// The eras, newest first.
const ERAS: [Era; 5] = [
    era((2019, 5, 1), "令和", "令", "R"),
    era((1989, 1, 8), "平成", "平", "H"),
    era((1926, 12, 25), "昭和", "昭", "S"),
    era((1912, 7, 30), "大正", "大", "T"),
    era((1868, 9, 8), "明治", "明", "M"),
];

/// The day number `YearName` reads its argument as; `None` when it is not
/// a date, which names the year 0. Numbers of 1 or more are day numbers
/// with no upper limit; text that is not a date is read field by field,
/// with an out-of-range month taken as January and days rolled over
/// (`"4/31/2019"` is May 1 2019).
fn year_name_day(v: &Value, cx: &Ctx) -> Option<i64> {
    match v {
        Value::Date(d) | Value::DateText(d, _) => Some(d.day_number()),
        Value::Timestamp(t) => Some(t.date().day_number()),
        Value::Number(n) | Value::NumberText(n, _) => n.int().to_i64().filter(|&d| d >= 1),
        Value::Text(t) => match Date::parse(t.as_str(), cx.locale, cx.today) {
            Ok(d) => d.map(Date::day_number),
            Err(_) => {
                let (y, m, d) = loose_date_fields(t.as_str(), cx.locale, cx.today)?;
                let m = if (1..=12).contains(&m) { m } else { 1 };
                Date::normalized(m, d, y).map(Date::day_number)
            }
        },
        Value::Container(c) => year_name_day(&Value::Text(c.text.clone()), cx),
        Value::Time(_) | Value::Empty(_) => None,
    }
}

/// `YearName(date; format)`: the Japanese era and year, format 0 `平成20`,
/// 1 `(平)20`, 2 `H20`; the first year of an era is 元 in formats 0 and 1.
/// Dates before Meiji, and arguments that are not dates, give the Western
/// year: `西暦1868`, `(西)1868`, `1868 CE` (`YearName("abc";0)` = 西暦0).
/// Any other format is 0.
fn year_name(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let v = &args[0];
    if v.is_empty() {
        return Ok(v.clone());
    }
    if let Value::Time(_) = v {
        return Ok(Value::Empty(crate::ValueType::Time));
    }
    let format = args[1]
        .numeric_operand(cx)
        .int()
        .to_i64()
        .filter(|f| (1..=2).contains(f))
        .unwrap_or(0);
    let day = year_name_day(v, cx);
    let year = day.map_or(0, year_of_day);
    let era = day.and_then(|day| {
        ERAS.iter().find(|era| {
            let (y, m, d) = era.start;
            day >= day_number_of(y, m, d)
        })
    });
    let text = match era {
        Some(era) => {
            let n = year - era.start.0 + 1;
            let shown = if n == 1 {
                "元".to_string()
            } else {
                n.to_string()
            };
            match format {
                1 => format!("({}){shown}", era.abbreviation),
                2 => format!("{}{n}", era.initial),
                _ => format!("{}{shown}", era.name),
            }
        }
        None => match format {
            1 => format!("(西){year}"),
            2 => format!("{year} CE"),
            _ => format!("西暦{year}"),
        },
    };
    Ok(Value::text(text))
}

// ---------------------------------------------------------------------------
// NumToJText.

const MYRIADS: [&str; 13] = [
    "", "万", "億", "兆", "京", "垓", "し", "穰", "溝", "澗", "正", "載", "極",
];
const MYRIADS_DAIJI: [&str; 12] = [
    "", "萬", "億", "兆", "京", "垓", "し", "穣", "溝", "澗", "正", "載",
];
const TENS: [&str; 4] = ["", "十", "百", "千"];
const TENS_DAIJI: [&str; 4] = ["", "拾", "陌", "阡"];

/// A digit, minus sign, decimal point or comma in character type `kind`
/// (0 ASCII, 1 fullwidth, 2 kanji, 3 traditional kanji); kanji types keep
/// the ASCII signs.
fn j_char(c: char, kind: i64) -> char {
    match (c, kind) {
        ('0'..='9', 1) => char::from_u32(c as u32 + 0xFEE0).unwrap_or(c),
        ('0'..='9', 2) => KANJI_DIGITS[c as usize - '0' as usize],
        ('0'..='9', 3) => DAIJI_DIGITS[c as usize - '0' as usize],
        ('!'..='~', 1) => char::from_u32(c as u32 + 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

fn j_text(s: &str, kind: i64) -> String {
    s.chars().map(|c| j_char(c, kind)).collect()
}

/// `n`'s text without an exponent: the display digits (rounded to the
/// display precision) written out in full (`1.5E+30` as 31 digits).
fn plain_digits(n: &Number, decimal: char) -> String {
    let text = n.to_text_with(decimal);
    let Some((mantissa, exponent)) = text.split_once('E') else {
        return text;
    };
    let exponent: i64 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let (int, frac) = mantissa.split_once(decimal).unwrap_or((mantissa, ""));
    let digits: String = format!("{int}{frac}").trim_end_matches('0').to_string();
    let point = int.len() as i64 + exponent;
    let body = if point <= 0 {
        format!(
            "{decimal}{}{digits}",
            "0".repeat(point.unsigned_abs() as usize)
        )
    } else if point as usize >= digits.len() {
        format!("{digits}{}", "0".repeat(point as usize - digits.len()))
    } else {
        let (i, f) = digits.split_at(point as usize);
        format!("{i}{decimal}{f}")
    };
    format!("{sign}{body}")
}

/// Separator 1: a comma every three characters left of the first `.`
/// (FileMaker groups the display text, so in a comma-decimal locale the
/// decimals join in: `1234567,5` is `123,456,7,5`).
fn thousands(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", text),
    };
    let (int, frac) = match rest.find('.') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let chars: Vec<char> = int.chars().collect();
    let mut out = String::from(sign);
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out.push_str(frac);
    out
}

/// Separator 2: four-digit groups with 万, 億, ... after each non-zero one,
/// leading zeros dropped; groups past 極 have no unit.
fn myriad_groups(int: &str, kind: i64) -> String {
    let digits: Vec<char> = int.chars().collect();
    let groups = digits.len().div_ceil(4);
    let mut out = String::new();
    for q in (0..groups).rev() {
        let end = digits.len() - q * 4;
        let start = end.saturating_sub(4);
        let group: String = digits[start..end].iter().collect();
        let group = group.trim_start_matches('0');
        if group.is_empty() {
            continue;
        }
        out.push_str(&j_text(group, kind));
        out.push_str(MYRIADS.get(q).copied().unwrap_or(""));
    }
    out
}

/// Separator 3: every digit with its place (千, 百, 十; 1 before them is
/// left out except in traditional kanji) and 万, 億, ... after each non-zero
/// group. From the 極 group up the places are gone: digits other than 0
/// are written as they are, and 極 follows only a non-zero digit in its
/// place (`1234 * 10^48` = 一二三四極, `10^49` = 一).
fn myriad_places(int: &str, kind: i64) -> String {
    let digits: Vec<u32> = int.chars().filter_map(|c| c.to_digit(10)).collect();
    let (tens, myriads): (&[&str; 4], &[&str]) = if kind == 3 {
        (&TENS_DAIJI, &MYRIADS_DAIJI)
    } else {
        (&TENS, &MYRIADS[..12])
    };
    let digit = |d: u32| j_char(char::from_digit(d, 10).unwrap_or('0'), kind);
    let mut out = String::new();
    let mut group_nonzero = false;
    for (i, &d) in digits.iter().enumerate() {
        let p = digits.len() - 1 - i;
        let (q, r) = (p / 4, p % 4);
        if q >= 12 {
            if d != 0 {
                out.push(digit(d));
                if p == 48 {
                    out.push('極');
                }
            }
            continue;
        }
        if d != 0 {
            group_nonzero = true;
            if !(d == 1 && r > 0 && kind != 3) {
                out.push(digit(d));
            }
            out.push_str(tens[r]);
        }
        if r == 0 {
            if group_nonzero {
                out.push_str(myriads[q]);
            }
            group_nonzero = false;
        }
    }
    out
}

/// `NumToJText(number; separator; characterType)`. Separator 0 none, 1
/// commas, 2 myriad units (1億2345万6789), 3 places and units
/// (1億2千3百4十5万...); character type 0 ASCII digits, 1 fullwidth, 2
/// kanji, 3 traditional kanji (壱弐参四伍...拾陌阡萬). Out-of-range or
/// non-numeric separators and types are 0. Separators 0 and 1 write the
/// number's display digits in full and return text; 2 and 3 write the
/// integer part in units, then the fraction as the display text of the
/// fractional part (`1.5E-30` gives `01.5E-30`), and return a number shown
/// as that text. Text without digits comes back unchanged.
fn num_to_j_text(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let v = &args[0];
    let small = |v: &Value| {
        v.numeric_operand(cx)
            .int()
            .to_i64()
            .filter(|n| (0..=3).contains(n))
            .unwrap_or(0)
    };
    let (separator, kind) = (small(&args[1]), small(&args[2]));
    if v.is_empty() {
        return Ok(Value::empty_text());
    }
    let n = match v {
        Value::Text(t) => {
            let s = t.as_str();
            if s.trim().is_empty() {
                Number::empty()
            } else if !s.chars().any(|c| c.is_ascii_digit()) {
                return Ok(Value::Text(t.without_styles()));
            } else {
                parse_number(s, cx.locale)
            }
        }
        other => other.to_number(cx),
    };
    let decimal = cx.locale.decimal_separator;
    if separator < 2 {
        let text = plain_digits(&n, decimal);
        let text = if separator == 1 {
            thousands(&text)
        } else {
            text
        };
        return Ok(Value::text(j_text(&text, kind)));
    }
    let n = if n.is_nan() || n.is_empty() {
        Number::zero()
    } else {
        n
    };
    let abs = n.abs();
    let int = abs.int();
    let int_text = plain_digits(&int, decimal);
    let mut out = String::new();
    if n.is_negative() {
        out.push(j_char('-', kind));
    }
    if int.is_zero() {
        out.push(j_char('0', kind));
    } else if separator == 2 {
        out.push_str(&myriad_groups(&int_text, kind));
    } else {
        out.push_str(&myriad_places(&int_text, kind));
    }
    let frac = abs.sub(&int, cx.precision);
    if !frac.is_zero() {
        out.push_str(&j_text(&frac.to_text_with(decimal), kind));
    }
    Ok(Value::NumberText(n, Text::plain(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kana_tables() {
        assert_eq!(narrow_kana('ガ').as_deref(), Some("ｶﾞ"));
        assert_eq!(narrow_kana('ポ').as_deref(), Some("ﾎﾟ"));
        assert_eq!(narrow_kana('ヴ').as_deref(), Some("ｳﾞ"));
        assert_eq!(narrow_kana('ン').as_deref(), Some("ﾝ"));
        assert_eq!(narrow_kana('ヶ'), None);
        assert_eq!(narrow_kana('ッ').as_deref(), Some("ｯ"));
        assert_eq!(widen_kana('ｦ'), Some('ヲ'));
        assert_eq!(widen_kana('ﾝ'), Some('ン'));
        assert_eq!(voiced('ツ', HALFWIDTH_VOICED), Some('ヅ'));
        assert_eq!(voiced('ッ', HALFWIDTH_VOICED), None);
        assert_eq!(voiced('ア', HALFWIDTH_VOICED), None);
    }

    #[test]
    fn numerals() {
        assert_eq!(myriad_groups("123456789", 0), "1億2345万6789");
        assert_eq!(myriad_groups("110000000", 2), "一億一〇〇〇万");
        assert_eq!(
            myriad_places("123456789", 3),
            "壱億弐阡参陌四拾伍萬六阡七陌八拾九"
        );
        assert_eq!(myriad_places("1010101010", 0), "十億千十万千十");
        assert_eq!(myriad_places("10000", 3), "壱萬");
        assert_eq!(thousands("-12345"), "-12,345");
        assert_eq!(thousands("1234567,5"), "123,456,7,5");
        assert_eq!(
            plain_digits(&Number::parse_text("1.5e30", '.', ','), '.').len(),
            31
        );
        assert_eq!(
            plain_digits(&Number::parse_text("1.5e-30", '.', ','), '.'),
            ".0000000000000000000000000000015"
        );
    }
}
