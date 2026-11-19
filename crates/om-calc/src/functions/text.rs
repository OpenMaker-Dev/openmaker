//! Text functions: `Char`, `Code`, `Exact`, `Filter`, `FilterValues`,
//! `GetAsCSS`, `GetAsDate`, `GetAsNumber`, `GetAsSVG`, `GetAsText`,
//! `GetAsTime`, `GetAsTimestamp`, `GetAsURLEncoded`, `GetValue`, `Left`,
//! `LeftValues`, `LeftWords`, `Length`, `Lower`, `Middle`, `MiddleValues`,
//! `MiddleWords`, `PatternCount`, `Position`, `Proper`, `Quote`, `Replace`,
//! `Right`, `RightValues`, `RightWords`, `SerialIncrement`, `SortValues`,
//! `Substitute`, `Trim`, `TrimAll`, `UniqueValues`, `Upper`, `ValueCount`,
//! `WordCount` (spec `docs/re/calc-functions.md` 3, 4 and the text oracle
//! answers in 9).
//!
//! Characters are legacy grapheme clusters ([`graphemes`]); searches match
//! code points and report the grapheme position of the match; the character
//! rules (case, words, collation, CSS) live in [`crate::text`]. Styles of
//! kept characters are kept, inserted text takes the style where it goes.
//!
//! `Substitute`'s list form `Substitute ( text ; [ a ; b ] ; [ c ; d ] )`
//! arrives flattened: the evaluator passes each `[ search ; replace ]` list
//! as two arguments in order, so the function sees `text, a, b, c, d`, the
//! same as the plain form's `text, search, replace`. The parser rejects
//! every other shape (1201, 1202, 1211).

use super::{arg, Function, Keyword};
use crate::datetime::{Date, Time, Timestamp};
use crate::error::CalcError;
use crate::number::{Number, DEFAULT_PRECISION};
use crate::text::case::{fold, fold_str, lower, proper, upper};
use crate::text::markup;
use crate::text::sorting::Collation;
use crate::text::words::word_ranges;
use crate::text::{
    grapheme_count, graphemes, lone_surrogate, utf16_units, CharStyle, Rebuild, Text, RETURN,
};
use crate::value::{parse_number, Ctx, Value, ValueType, Work};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};
use std::fmt::Write;
use std::ops::Range;

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("Char", char_),
    Function::pure("Code", code),
    Function::pure("Exact", exact),
    Function::pure("Filter", filter),
    Function::pure("FilterValues", filter_values),
    Function::pure("GetAsCSS", get_as_css),
    Function::pure("GetAsDate", get_as_date),
    Function::pure("GetAsNumber", get_as_number),
    Function::pure("GetAsSVG", get_as_svg),
    Function::pure("GetAsText", get_as_text),
    Function::pure("GetAsTime", get_as_time),
    Function::pure("GetAsTimestamp", get_as_timestamp),
    Function::pure("GetAsURLEncoded", get_as_url_encoded),
    Function::pure("GetValue", get_value),
    Function::pure("Left", left),
    Function::pure("LeftValues", left_values),
    Function::pure("LeftWords", left_words),
    Function::pure("Length", length),
    Function::pure("Lower", lower_),
    Function::pure("Middle", middle),
    Function::pure("MiddleValues", middle_values),
    Function::pure("MiddleWords", middle_words),
    Function::pure("PatternCount", pattern_count),
    Function::pure("Position", position),
    Function::pure("Proper", proper_),
    Function::pure("Quote", quote),
    Function::pure("Replace", replace),
    Function::pure("Right", right),
    Function::pure("RightValues", right_values),
    Function::pure("RightWords", right_words),
    Function::pure("SerialIncrement", serial_increment),
    Function::pure("SortValues", sort_values),
    Function::pure("Substitute", substitute),
    Function::pure("Trim", trim),
    Function::pure("TrimAll", trim_all),
    Function::pure("UniqueValues", unique_values),
    Function::pure("Upper", upper_),
    Function::pure("ValueCount", value_count),
    Function::pure("WordCount", word_count),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

/// 503, what `Code` reports for more than 80 UTF-16 units.
const TOO_LONG_FOR_CODE: CalcError = match CalcError::from_code(503) {
    Some(e) => e,
    None => unreachable!(),
};

/// The most UTF-16 units `Code` converts (`kMaxTextSizeForCode`).
const MAX_CODE_UNITS: usize = 80;

type Result<T> = std::result::Result<T, CalcError>;

fn text(v: &Value, cx: &Ctx) -> Text {
    v.to_text(cx)
}

fn text_value(t: Text) -> Result<Value> {
    Ok(Value::Text(t))
}

/// An integer argument as the text functions receive it
/// (`FixPt::AsLongLong`, calc-functions.md 3.1 and the oracle answers):
/// the low three base-10^9 words of the integer part, wrapping in 64 bits,
/// and the value's sign. Empty, `?` and text without digits are 0.
/// Each function then reads the low 32 bits its own way.
#[derive(Clone, Copy, Debug)]
struct IntArg {
    negative: bool,
    low: u64,
}

impl IntArg {
    fn of(v: &Value, cx: &Ctx) -> IntArg {
        let n = v.to_number(cx);
        let Some(fin) = n.fin() else {
            return IntArg {
                negative: false,
                low: 0,
            };
        };
        let integer = Number::from_fin(fin.abs().trunc_int());
        let words = integer
            .modulo(&pow10(27), DEFAULT_PRECISION)
            .to_scaled_i128(0)
            .unwrap_or(0);
        IntArg {
            negative: n.is_negative(),
            // The low 64 bits of a value below 10^27.
            low: words as u64,
        }
    }

    fn low32(self) -> u32 {
        self.low as u32
    }

    /// A count read unsigned: negative is 0, 2^32 wraps to 0, 2^31 is
    /// "all" (`Left`, `Right`, `Middle`, `*Words` but `MiddleWords`,
    /// `*Values`).
    fn count(self) -> u32 {
        if self.negative {
            0
        } else {
            self.low32()
        }
    }

    /// A count read signed: negative and 2^31 and above are 0 (`Replace`,
    /// `MiddleWords`).
    fn signed_count(self) -> u32 {
        if self.negative {
            0
        } else {
            u32::try_from(self.low32() as i32).unwrap_or(0)
        }
    }

    /// A 1-based start read signed: 0, negative and 2^31 and above are 1
    /// (`Middle`, `Replace`, `MiddleWords`).
    fn signed_start(self) -> u32 {
        self.signed_count().max(1)
    }

    /// A 1-based start read unsigned: 0 and negative are 1, 2^31 is far past
    /// the end (`Position`, `MiddleValues`).
    fn start(self) -> u32 {
        self.count().max(1)
    }

    /// The low 32 bits of the two's complement value as a signed number
    /// (`Position` occurrence, `GetValue`, `TrimAll` trim type): -1 and
    /// 4294967295 are both -1.
    fn wrapping(self) -> i32 {
        let low = self.low32();
        (if self.negative {
            low.wrapping_neg()
        } else {
            low
        }) as i32
    }
}

fn pow10(k: usize) -> Number {
    Number::parse_text(&format!("1{}", "0".repeat(k)), '.', ',')
}

/// The decimal digits of a non-negative integer, however long.
fn integer_digits(n: &Number) -> String {
    if let Some(v) = n.to_scaled_i128(0) {
        return v.unsigned_abs().to_string();
    }
    let billion = Number::from_i64(1_000_000_000);
    let mut words = Vec::new();
    let mut rest = n.clone();
    while !rest.is_zero() {
        let word = rest.modulo(&billion, DEFAULT_PRECISION).to_scaled_i128(0);
        words.push(word.unwrap_or(0));
        rest = match rest.div_int(&billion, DEFAULT_PRECISION) {
            Ok(q) => q,
            Err(_) => break,
        };
    }
    let mut out = String::new();
    for (i, word) in words.iter().rev().enumerate() {
        if i == 0 {
            let _ = write!(out, "{word}");
        } else {
            let _ = write!(out, "{word:09}");
        }
    }
    out
}

/// The slice of `t` from the start of `ranges[first]` to the end of
/// `ranges[last]` (inclusive, 0-based).
fn span(t: &Text, ranges: &[Range<usize>], first: usize, last: usize) -> Text {
    t.slice(ranges[first].start..ranges[last].end)
}

/// The 0-based inclusive range of `count` items from 1-based `start` among
/// `len`, or `None` when it is empty.
fn window(start: u32, count: u32, len: usize) -> Option<(usize, usize)> {
    let first = start as usize - 1;
    if count == 0 || first >= len {
        return None;
    }
    let last = (first as u64 + u64::from(count) - 1).min(len as u64 - 1) as usize;
    Some((first, last))
}

// ---- Characters -----------------------------------------------------------

fn left(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let g = graphemes(t.as_str());
    let k = (IntArg::of(&args[1], cx).count() as usize).min(g.len());
    text_value(if k == 0 {
        Text::new()
    } else {
        span(&t, &g, 0, k - 1)
    })
}

fn right(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let g = graphemes(t.as_str());
    let k = (IntArg::of(&args[1], cx).count() as usize).min(g.len());
    text_value(if k == 0 {
        Text::new()
    } else {
        span(&t, &g, g.len() - k, g.len() - 1)
    })
}

fn middle(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let g = graphemes(t.as_str());
    let start = IntArg::of(&args[1], cx).signed_start();
    let count = IntArg::of(&args[2], cx).count();
    text_value(match window(start, count, g.len()) {
        Some((first, last)) => span(&t, &g, first, last),
        None => Text::new(),
    })
}

fn replace(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let g = graphemes(t.as_str());
    let start = IntArg::of(&args[1], cx).signed_start() as usize;
    let count = IntArg::of(&args[2], cx).signed_count() as usize;
    let first = (start - 1).min(g.len());
    let last = first.saturating_add(count).min(g.len());
    let from = g.get(first).map_or(t.as_str().len(), |r| r.start);
    let to = if last > first { g[last - 1].end } else { from };
    let replacement = text(&args[3], cx);
    text_value(
        t.splice_all_within(
            std::slice::from_ref(&(from..to)),
            &replacement,
            cx.text_limit,
        )
        .ok_or(CalcError::OUT_OF_MEMORY)?,
    )
}

/// Characters of text; of container data, the byte size of its main stream
/// (calc-functions.md 3.2).
fn length(args: &[Value], cx: &Ctx) -> Result<Value> {
    if let Value::Container(container) = &args[0] {
        if !container.streams.is_empty() {
            return Ok(Value::number(container.size() as i64));
        }
    }
    Ok(Value::number(
        grapheme_count(text(&args[0], cx).as_str()) as i64
    ))
}

fn exact(args: &[Value], cx: &Ctx) -> Result<Value> {
    let a = text(&args[0], cx);
    let b = text(&args[1], cx);
    Ok(Value::boolean(a.as_str() == b.as_str()))
}

fn filter(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let keep = text(&args[1], cx);
    let keep: HashSet<&str> = graphemes(keep.as_str())
        .into_iter()
        .map(|r| &keep.as_str()[r])
        .collect();
    let mut out = Rebuild::new(&t);
    for r in graphemes(t.as_str()) {
        if keep.contains(&t.as_str()[r.clone()]) {
            out.keep(r);
        }
    }
    text_value(out.finish())
}

fn upper_(args: &[Value], cx: &Ctx) -> Result<Value> {
    text_value(upper(&text(&args[0], cx)))
}

fn lower_(args: &[Value], cx: &Ctx) -> Result<Value> {
    text_value(lower(&text(&args[0], cx)))
}

fn proper_(args: &[Value], cx: &Ctx) -> Result<Value> {
    text_value(proper(&text(&args[0], cx), cx.locale))
}

/// The characters `Trim` removes: U+0020, U+00A0, U+1680, U+180E,
/// U+2000-U+200B, U+205F, U+3000 (measured; tab, CR and LF stay).
fn is_trimmed(c: char) -> bool {
    matches!(
        c,
        ' ' | '\u{a0}' | '\u{1680}' | '\u{180e}' | '\u{2000}'
            ..='\u{200b}' | '\u{205f}' | '\u{3000}'
    )
}

fn trim(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let s = t.as_str();
    let start = s.len() - s.trim_start_matches(is_trimmed).len();
    let end = s.trim_end_matches(is_trimmed).len();
    text_value(if start < end {
        t.slice(start..end)
    } else {
        Text::new()
    })
}

/// `TrimAll ( text ; trimSpaces ; trimType )`: spaces are U+0020 and, with
/// trimSpaces, U+3000 (which then becomes U+0020); without it U+3000 is
/// kept wherever it is, and a run holding one keeps only those. Leading and
/// trailing runs go; an inner run becomes one space or none by the
/// characters around it (roman below U+2F00): two non-roman, none; two
/// roman, one (trimType 3: none); mixed, none for 0 and 3, one for 1 and 2;
/// trimType 1 also puts a space between touching roman and non-roman
/// characters. Other trimTypes act as 0. The text is edited in place: the
/// space kept stays in the style run of the run's first space, an inserted
/// one joins the run of the character before it, and when the space kept
/// starts or ends its style run the runs around it merge if their styles
/// are equal (calc-values.md 13, value-core answers).
fn trim_all(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let full_width = IntArg::of(&args[1], cx).low != 0;
    let trim_type = match IntArg::of(&args[2], cx).wrapping() {
        k @ 0..=3 => k,
        _ => 0,
    };
    let is_space = |c: char| c == ' ' || c == '\u{3000}';
    let roman = |c: char| u32::from(c) < 0x2F00;
    let s = t.as_str();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let run_edges: Vec<Range<usize>> = t.runs().into_iter().map(|(r, _)| r).collect();
    let mut out = Rebuild::new(&t);
    let mut prev: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let (offset, c) = chars[i];
        if !is_space(c) {
            if let Some(p) = prev {
                if trim_type == 1 && roman(p) != roman(c) {
                    out.insert(" ", chars[i - 1].0);
                }
            }
            out.keep(offset..offset + c.len_utf8());
            prev = Some(c);
            i += 1;
            continue;
        }
        let run_start = i;
        while i < chars.len() && is_space(chars[i].1) {
            i += 1;
        }
        let run = &chars[run_start..i];
        let next = chars.get(i).map(|&(_, c)| c);
        let protected: Vec<usize> = run
            .iter()
            .filter(|&&(_, c)| c == '\u{3000}' && !full_width)
            .map(|&(o, _)| o)
            .collect();
        if !protected.is_empty() {
            for o in protected {
                out.keep(o..o + '\u{3000}'.len_utf8());
            }
        } else if let (Some(p), Some(n)) = (prev, next) {
            let one = match (roman(p), roman(n)) {
                (false, false) => false,
                (true, true) => trim_type != 3,
                _ => matches!(trim_type, 1 | 2),
            };
            if one {
                let first = run[0].0;
                let edge = first + run[0].1.len_utf8();
                if run_edges.iter().any(|r| r.start == first || r.end == edge) {
                    out.join();
                }
                out.insert(" ", first);
            }
        }
        // A run, kept or not, already decided the spacing here: no space is
        // inserted before the next character.
        prev = None;
    }
    text_value(out.finish())
}

/// `Quote`: the text between quotation marks as a formula literal: `"`
/// and `¶` escaped with a backslash, a backslash escaped only where it would
/// start an escape (before `"`, `\`, `¶`, a return or the end), returns
/// written as `¶`, U+E004 as a return. The text is edited in place: the
/// marks join the runs of the first and last characters, an escape that of
/// the character before it.
fn quote(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let s = t.as_str();
    let mut out = Rebuild::new(&t);
    let mut last = 0;
    out.insert("\"", 0);
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let escape = match c {
            '"' | '¶' => true,
            '\\' => chars
                .peek()
                .map_or(true, |&(_, next)| matches!(next, '"' | '\\' | '¶' | RETURN)),
            _ => false,
        };
        if escape {
            out.insert("\\", last);
        }
        match c {
            RETURN => out.insert("¶", i),
            '\u{e004}' => out.insert("\r", i),
            _ => out.keep(i..i + c.len_utf8()),
        }
        last = i;
    }
    out.insert("\"", last);
    text_value(out.finish())
}

fn char_(args: &[Value], cx: &Ctx) -> Result<Value> {
    let n = args[0].to_number(cx);
    if n.is_nan() {
        return Ok(Value::invalid());
    }
    let Some(fin) = n.fin() else {
        return text_value(Text::new());
    };
    if n.is_negative() {
        return text_value(Text::new());
    }
    // Five-digit groups from the right, each a UTF-16 unit (taken modulo
    // 2^16); groups that come to 0 add nothing (`Char ( 65536 )` is empty).
    let digits = integer_digits(&Number::from_fin(fin.trunc_int()));
    let mut units = Vec::new();
    let mut end = digits.len();
    while end > 0 {
        let start = end.saturating_sub(5);
        let group: u32 = digits[start..end].parse().unwrap_or(0);
        if group as u16 != 0 {
            units.push(group as u16);
        }
        end = start;
    }
    // A unit left without its pair is a lone surrogate (its stand-in).
    let s: String = char::decode_utf16(units)
        .map(|c| {
            c.unwrap_or_else(|e| {
                lone_surrogate(e.unpaired_surrogate()).unwrap_or(char::REPLACEMENT_CHARACTER)
            })
        })
        .collect();
    text_value(Text::plain(s))
}

fn code(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let units: Vec<u16> = utf16_units(t.as_str()).collect();
    if units.len() > MAX_CODE_UNITS {
        return Err(TOO_LONG_FOR_CODE);
    }
    if units.is_empty() {
        return Ok(Value::empty_text());
    }
    let mut digits = String::new();
    for (i, unit) in units.iter().rev().enumerate() {
        if i == 0 {
            let _ = write!(digits, "{unit}");
        } else {
            let _ = write!(digits, "{unit:05}");
        }
    }
    Ok(Value::Number(Number::parse_text(&digits, '.', ',')))
}

// ---- Searching ------------------------------------------------------------

/// The folded characters of `text` with their byte offsets.
fn folded(text: &str) -> Vec<(usize, char)> {
    text.char_indices().map(|(i, c)| (i, fold(c))).collect()
}

/// Whether `needle` stands at `i` of `hay`; the characters compared count
/// as the search's work, which grows with both lengths.
fn matches_at(hay: &[(usize, char)], needle: &[char], i: usize, work: &mut Work) -> Result<bool> {
    let mut compared = 0;
    let mut found = true;
    for (&(_, c), &n) in hay[i..i + needle.len()].iter().zip(needle) {
        compared += 1;
        if c != n {
            found = false;
            break;
        }
    }
    work.add(compared)?;
    Ok(found)
}

/// `Position`: the 1-based grapheme position of the occurrence-th match of
/// `search` (case folded, matched by code point, not overlapping), scanning
/// forward from grapheme `start`, or backward over matches starting at or
/// before it for a negative occurrence; 0 when there is none. A match inside
/// a character counts the characters before it (`Position ( "a" & ¶ & LF ;
/// LF ; 1 ; 1 )` is 3).
fn find_position(
    text: &str,
    search: &str,
    start: u32,
    occurrence: i32,
    work: &mut Work,
) -> Result<i64> {
    let hay = folded(text);
    let needle: Vec<char> = search.chars().map(fold).collect();
    if needle.is_empty() || needle.len() > hay.len() || occurrence == 0 {
        return Ok(0);
    }
    let g = graphemes(text);
    let from = match g.get(start as usize - 1) {
        Some(r) => hay.partition_point(|&(offset, _)| offset < r.start),
        None => hay.len(),
    };
    let last_start = hay.len() - needle.len();
    let mut seen = 0;
    let found = if occurrence > 0 {
        let mut i = from;
        loop {
            if i > last_start {
                break None;
            }
            if matches_at(&hay, &needle, i, work)? {
                seen += 1;
                if seen == occurrence.unsigned_abs() {
                    break Some(i);
                }
                i += needle.len();
            } else {
                i += 1;
            }
        }
    } else {
        let mut i = from.min(last_start) as isize;
        loop {
            if i < 0 {
                break None;
            }
            if matches_at(&hay, &needle, i as usize, work)? {
                seen += 1;
                if seen == occurrence.unsigned_abs() {
                    break Some(i as usize);
                }
                i -= needle.len() as isize;
            } else {
                i -= 1;
            }
        }
    };
    Ok(found.map_or(0, |i| grapheme_count(&text[..hay[i].0]) as i64 + 1))
}

fn position(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let s = text(&args[1], cx);
    let start = IntArg::of(&args[2], cx).start();
    let occurrence = IntArg::of(&args[3], cx).wrapping();
    Ok(Value::number(find_position(
        t.as_str(),
        s.as_str(),
        start,
        occurrence,
        &mut Work::new(cx),
    )?))
}

fn pattern_count(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let s = text(&args[1], cx);
    let hay = folded(t.as_str());
    let needle: Vec<char> = s.as_str().chars().map(fold).collect();
    let mut count = 0;
    let mut work = Work::new(cx);
    if !needle.is_empty() {
        let mut i = 0;
        while i + needle.len() <= hay.len() {
            if matches_at(&hay, &needle, i, &mut work)? {
                count += 1;
                i += needle.len();
            } else {
                i += 1;
            }
        }
    }
    Ok(Value::number(count))
}

/// Every occurrence of `search` (case-sensitive, by code point, left to
/// right, not overlapping, not rescanned) replaced in place
/// ([`Text::splice_all`]); each replacement takes the style of the first
/// character it replaces. Error 7 before the text is built when it would
/// be past the text limit: each occurrence may grow it by the whole
/// replacement.
fn substitute_one(t: &Text, search: &str, replacement: &Text, cx: &Ctx) -> Result<Text> {
    let found: Vec<Range<usize>> = if search.is_empty() {
        Vec::new()
    } else {
        t.as_str()
            .match_indices(search)
            .map(|(i, m)| i..i + m.len())
            .collect()
    };
    if found.is_empty() {
        return Ok(t.clone());
    }
    t.splice_all_within(&found, replacement, cx.text_limit)
        .ok_or(CalcError::OUT_OF_MEMORY)
}

/// Each pair is a pass over the text, so the work grows with the text
/// times the pairs, which `Evaluate` text may give by the thousand.
fn substitute(args: &[Value], cx: &Ctx) -> Result<Value> {
    let mut t = text(&args[0], cx);
    let mut work = Work::new(cx);
    for pair in args[1..].chunks_exact(2) {
        t = substitute_one(&t, text(&pair[0], cx).as_str(), &text(&pair[1], cx), cx)?;
        work.add(t.as_str().len().max(1))?;
    }
    text_value(t)
}

// ---- Conversions ----------------------------------------------------------

fn get_as_text(args: &[Value], cx: &Ctx) -> Result<Value> {
    text_value(text(&args[0], cx))
}

fn get_as_number(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(match &args[0] {
        Value::Text(t) if t.is_empty() => args[0].clone(),
        Value::Number(_) => args[0].clone(),
        other => Value::Number(other.to_number(cx)),
    })
}

/// `GetAsDate`, `GetAsTime`, `GetAsTimestamp`: empty text comes back
/// unchanged, blank text gives the typed empty value.
fn get_as<T>(
    v: &Value,
    ty: ValueType,
    convert: impl FnOnce(&Value) -> Result<Option<T>>,
    wrap: impl FnOnce(T) -> Value,
) -> Result<Value> {
    if matches!(v, Value::Text(t) if t.is_empty()) {
        return Ok(v.clone());
    }
    Ok(match convert(v)? {
        Some(x) => wrap(x),
        None => Value::empty(ty),
    })
}

fn get_as_date(args: &[Value], cx: &Ctx) -> Result<Value> {
    get_as(&args[0], ValueType::Date, |v| v.to_date(cx), Value::Date)
}

fn get_as_time(args: &[Value], cx: &Ctx) -> Result<Value> {
    get_as(&args[0], ValueType::Time, |v| v.to_time(cx), Value::Time)
}

fn get_as_timestamp(args: &[Value], cx: &Ctx) -> Result<Value> {
    get_as(
        &args[0],
        ValueType::Timestamp,
        |v| v.to_timestamp(cx),
        Value::Timestamp,
    )
}

/// UTF-8, every byte but ASCII letters, digits and `:-_.!~*'()` as `%XX`
/// with upper-case hex (the Claris example shows lower case); styles go.
fn get_as_url_encoded(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let kept = |b: u8| b.is_ascii_alphanumeric() || b":-_.!~*'()".contains(&b);
    let escaped = t.as_str().bytes().filter(|&b| !kept(b)).count();
    cx.check_size(t.as_str().len() + 2 * escaped)?;
    let mut out = String::new();
    for b in t.as_str().bytes() {
        if kept(b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    text_value(Text::plain(out))
}

fn get_as_css(args: &[Value], cx: &Ctx) -> Result<Value> {
    let css = markup::css_within(&text(&args[0], cx), cx.text_limit);
    text_value(Text::plain(css.ok_or(CalcError::OUT_OF_MEMORY)?))
}

fn get_as_svg(args: &[Value], cx: &Ctx) -> Result<Value> {
    let svg = markup::svg_within(&text(&args[0], cx), cx.text_limit);
    text_value(Text::plain(svg.ok_or(CalcError::OUT_OF_MEMORY)?))
}

// ---- Value lists ----------------------------------------------------------

/// `slice` followed by a return in the style of its last character.
fn with_return(mut slice: Text) -> Text {
    let style = match slice.as_str().len() {
        0 => CharStyle::default(),
        n => slice.style_at(n - 1),
    };
    slice.push_styled("\r", &style);
    slice
}

fn value_count(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(Value::number(text(&args[0], cx).value_ranges().len() as i64))
}

fn get_value(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let values = t.value_ranges();
    let index = IntArg::of(&args[1], cx).wrapping();
    text_value(match usize::try_from(index) {
        Ok(i) if i >= 1 && i <= values.len() => t.slice(values[i - 1].clone()),
        _ => Text::new(),
    })
}

/// `LeftValues`, `MiddleValues`, `RightValues`: the text from the first
/// value's start to the last value's end (inner separators as they are),
/// then a return.
fn values_window(t: &Text, start: u32, count: u32) -> Text {
    let values = t.value_ranges();
    match window(start, count, values.len()) {
        Some((first, last)) => {
            with_return(t.slice(values[first].start.min(values[last].end)..values[last].end))
        }
        None => Text::new(),
    }
}

fn left_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    text_value(values_window(&t, 1, IntArg::of(&args[1], cx).count()))
}

fn right_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let len = t.value_ranges().len();
    let count = IntArg::of(&args[1], cx).count();
    let start = len.saturating_sub(count as usize) + 1;
    text_value(values_window(&t, start as u32, count))
}

fn middle_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let start = IntArg::of(&args[1], cx).start();
    let count = IntArg::of(&args[2], cx).count();
    text_value(values_window(&t, start, count))
}

/// `FilterValues`: the values of `text` that equal one of `filterValues`
/// with case folded (accents, width and `ß` still count), each followed by
/// its own separator, or by a return when it has none; the others are
/// deleted in place.
fn filter_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let wanted = text(&args[1], cx);
    let wanted: HashSet<String> = wanted
        .value_ranges()
        .into_iter()
        .map(|r| fold_str(&wanted.as_str()[r]))
        .collect();
    let s = t.as_str();
    let mut out = Rebuild::new(&t);
    for r in t.value_ranges() {
        if !wanted.contains(&fold_str(&s[r.clone()])) {
            continue;
        }
        match s[r.end..].chars().next() {
            Some(sep) => out.keep(r.start..r.end + sep.len_utf8()),
            None => {
                let last = s[..r.end].char_indices().last().map_or(r.start, |(i, _)| i);
                out.keep(r);
                out.insert("\r", last);
            }
        }
    }
    text_value(out.finish())
}

/// What `SortValues` and `UniqueValues` compare a value as (datatype 1-5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum DataType {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
}

/// A value read as a number, date, time or timestamp: blank values first,
/// values that do not convert last (and equal to each other).
enum Typed {
    Empty,
    Number(Number),
    Moment(i128),
    Invalid,
}

impl Typed {
    fn of(value: &str, ty: DataType, cx: &Ctx) -> Typed {
        let moment = |r: Result<Option<i128>>| match r {
            Ok(None) => Typed::Empty,
            Ok(Some(m)) => Typed::Moment(m),
            Err(_) => Typed::Invalid,
        };
        match ty {
            DataType::Number => {
                let n = parse_number(value, cx.locale);
                if n.is_empty() && value.trim().is_empty() {
                    Typed::Empty
                } else if n.is_empty() || n.is_nan() {
                    Typed::Invalid
                } else {
                    Typed::Number(n)
                }
            }
            DataType::Date => moment(
                Date::parse(value, cx.locale, cx.today)
                    .map(|d| d.map(|d| i128::from(d.day_number()))),
            ),
            DataType::Time => {
                moment(Time::parse(value, cx.locale, cx.today).map(|t| t.map(Time::micros)))
            }
            DataType::Timestamp => moment(
                Timestamp::parse(value, cx.locale, cx.today).map(|t| t.map(Timestamp::micros)),
            ),
            DataType::Text => Typed::Invalid,
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Typed::Empty => 0,
            Typed::Number(_) => 1,
            Typed::Moment(_) => 2,
            Typed::Invalid => 3,
        }
    }

    fn cmp(&self, other: &Typed) -> Ordering {
        match (self, other) {
            (Typed::Number(a), Typed::Number(b)) => a.compare(b).unwrap_or(Ordering::Equal),
            (Typed::Moment(a), Typed::Moment(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

// Typed values of a given datatype use the same equality for sorting and
// deduplication. Keep a total order across variants as well.
impl PartialEq for Typed {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Typed {}

impl PartialOrd for Typed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(Ord::cmp(self, other))
    }
}

impl Ord for Typed {
    fn cmp(&self, other: &Self) -> Ordering {
        Typed::cmp(self, other)
    }
}

/// The arguments `SortValues` and `UniqueValues` share: the values, the
/// datatype (negative: descending; anything but ±1-5 is text ascending)
/// and the collation (`None` for an unknown locale name).
struct ListArgs {
    text: Text,
    values: Vec<Range<usize>>,
    ty: DataType,
    descending: bool,
    collation: Option<Collation>,
}

impl ListArgs {
    fn of(args: &[Value], cx: &Ctx) -> ListArgs {
        let text = text(&args[0], cx);
        let values = text.value_ranges();
        let code = arg(args, 1).map_or(1, |v| IntArg::of(v, cx).wrapping());
        let (ty, descending) = match code.unsigned_abs() {
            1 => (DataType::Text, code < 0),
            2 => (DataType::Number, code < 0),
            3 => (DataType::Date, code < 0),
            4 => (DataType::Time, code < 0),
            5 => (DataType::Timestamp, code < 0),
            _ => (DataType::Text, false),
        };
        let collation = match arg(args, 2).map(|v| v.to_text(cx)) {
            Some(name) if !name.is_empty() => Collation::named(name.as_str()),
            _ => Some(Collation::of(cx.locale.language)),
        };
        ListArgs {
            text,
            values,
            ty,
            descending,
            collation,
        }
    }
}

fn sort_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    let a = ListArgs::of(args, cx);
    let Some(collation) = a.collation else {
        return Ok(Value::invalid());
    };
    let s = a.text.as_str();
    // Stable sorts: values that compare equal keep their order, also when
    // descending (`b¶a¶B¶A` descending is `b¶B¶a¶A`).
    let mut order: Vec<usize> = (0..a.values.len()).collect();
    let direct = |o: Ordering| if a.descending { o.reverse() } else { o };
    if a.ty == DataType::Text {
        let keys: Vec<_> = a
            .values
            .iter()
            .map(|r| collation.sort_key(&s[r.clone()]))
            .collect();
        order.sort_by(|&x, &y| direct(keys[x].cmp(&keys[y])));
    } else {
        let keys: Vec<Typed> = a
            .values
            .iter()
            .map(|r| Typed::of(&s[r.clone()], a.ty, cx))
            .collect();
        order.sort_by(|&x, &y| direct(keys[x].cmp(&keys[y])));
    }
    let mut out = Text::new();
    for i in order {
        out.push(&with_return(a.text.slice(a.values[i].clone())));
    }
    text_value(out)
}

fn unique_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    let a = ListArgs::of(args, cx);
    let Some(collation) = a.collation else {
        return Ok(Value::invalid());
    };
    let s = a.text.as_str();
    let mut out = Text::new();
    let mut seen_text: HashSet<Vec<u32>> = HashSet::new();
    let mut seen_typed: BTreeSet<Typed> = BTreeSet::new();
    for r in &a.values {
        let value = &s[r.clone()];
        let new = if a.ty == DataType::Text {
            seen_text.insert(collation.index_key(value))
        } else {
            seen_typed.insert(Typed::of(value, a.ty, cx))
        };
        if new {
            out.push(&with_return(a.text.slice(r.clone())));
        }
    }
    text_value(out)
}

// ---- Words ----------------------------------------------------------------

fn word_count(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    Ok(Value::number(
        word_ranges(t.as_str(), cx.locale).len() as i64
    ))
}

/// The text from the first word's start to the last word's end.
fn words_window(t: &Text, start: u32, count: u32, cx: &Ctx) -> Text {
    let words = word_ranges(t.as_str(), cx.locale);
    match window(start, count, words.len()) {
        Some((first, last)) => span(t, &words, first, last),
        None => Text::new(),
    }
}

fn left_words(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    text_value(words_window(&t, 1, IntArg::of(&args[1], cx).count(), cx))
}

fn right_words(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let len = word_ranges(t.as_str(), cx.locale).len();
    let count = IntArg::of(&args[1], cx).count();
    let start = len.saturating_sub(count as usize) + 1;
    text_value(words_window(&t, start as u32, count, cx))
}

fn middle_words(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let start = IntArg::of(&args[1], cx).signed_start();
    let count = IntArg::of(&args[2], cx).signed_count();
    text_value(words_window(&t, start, count, cx))
}

// ---- SerialIncrement --------------------------------------------------------

/// The decimal digits of Unicode 5.2 (measured: newer digit sets such as
/// U+0DE6 and U+A9F0 are not digits to `SerialIncrement`).
fn is_serial_digit(c: char) -> bool {
    matches!(
        c,
        '0'..='9'
            | '\u{660}'..='\u{669}'
            | '\u{6f0}'..='\u{6f9}'
            | '\u{7c0}'..='\u{7c9}'
            | '\u{966}'..='\u{96f}'
            | '\u{9e6}'..='\u{9ef}'
            | '\u{a66}'..='\u{a6f}'
            | '\u{ae6}'..='\u{aef}'
            | '\u{b66}'..='\u{b6f}'
            | '\u{be6}'..='\u{bef}'
            | '\u{c66}'..='\u{c6f}'
            | '\u{ce6}'..='\u{cef}'
            | '\u{d66}'..='\u{d6f}'
            | '\u{e50}'..='\u{e59}'
            | '\u{ed0}'..='\u{ed9}'
            | '\u{f20}'..='\u{f29}'
            | '\u{1040}'..='\u{1049}'
            | '\u{1090}'..='\u{1099}'
            | '\u{17e0}'..='\u{17e9}'
            | '\u{1810}'..='\u{1819}'
            | '\u{1946}'..='\u{194f}'
            | '\u{19d0}'..='\u{19da}'
            | '\u{1a80}'..='\u{1a89}'
            | '\u{1a90}'..='\u{1a99}'
            | '\u{1b50}'..='\u{1b59}'
            | '\u{1bb0}'..='\u{1bb9}'
            | '\u{1c40}'..='\u{1c49}'
            | '\u{1c50}'..='\u{1c59}'
            | '\u{a620}'..='\u{a629}'
            | '\u{a8d0}'..='\u{a8d9}'
            | '\u{a900}'..='\u{a909}'
            | '\u{a9d0}'..='\u{a9d9}'
            | '\u{aa50}'..='\u{aa59}'
            | '\u{abf0}'..='\u{abf9}'
            | '\u{ff10}'..='\u{ff19}'
    )
}

/// `SerialIncrement ( text ; incrementBy )`: the last run of digits plus the
/// increment truncated toward zero, written at least as wide as the run
/// with leading zeros, a sign included (`abc000` - 1 = `abc0-1`), and in
/// full-width digits when the run had any; without digits the increment is
/// appended (`abc` + 1 = `abc1`). Digits read as the number parser reads
/// them (other scripts' digits count as 0). Styles go.
fn serial_increment(args: &[Value], cx: &Ctx) -> Result<Value> {
    let t = text(&args[0], cx);
    let s = t.as_str();
    let by = args[1].to_number(cx);
    let by = match by.fin() {
        Some(f) => Number::from_fin(f.trunc_int()),
        None => by,
    };
    let number_text = |n: &Number| -> String {
        match n.fin() {
            Some(_) if n.is_negative() => format!("-{}", integer_digits(&n.abs())),
            Some(_) => integer_digits(n),
            None => n.to_text_with(cx.locale.decimal_separator),
        }
    };
    let Some(last) = s.char_indices().rev().find(|&(_, c)| is_serial_digit(c)) else {
        return text_value(Text::plain(format!("{s}{}", number_text(&by))));
    };
    let end = last.0 + last.1.len_utf8();
    let start = s[..end]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_serial_digit(c))
        .last()
        .map_or(last.0, |(i, _)| i);
    let run = &s[start..end];
    let width = run.chars().count();
    let wide = run.chars().any(|c| ('\u{ff10}'..='\u{ff19}').contains(&c));
    let value = parse_number(run, cx.locale);
    let value = if value.is_empty() {
        Number::zero()
    } else {
        value
    };
    let sum = number_text(&value.add(&by, DEFAULT_PRECISION));
    let mut digits = "0".repeat(width.saturating_sub(sum.chars().count()));
    digits.push_str(&sum);
    if wide {
        digits = digits
            .chars()
            .map(|c| match c {
                '0'..='9' => char::from_u32(u32::from(c) - 0x30 + 0xFF10).unwrap_or(c),
                '-' => '－',
                other => other,
            })
            .collect();
    }
    text_value(Text::plain(format!("{}{digits}{}", &s[..start], &s[end..])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Locale;

    fn cx(locale: &Locale) -> Ctx<'_> {
        Ctx::new(locale, Date::from_ymd(2026, 10, 28).unwrap())
    }

    fn int(n: &str) -> IntArg {
        let en = Locale::en_us();
        IntArg::of(&Value::Number(Number::parse_text(n, '.', ',')), &cx(&en))
    }

    #[test]
    fn integer_arguments_wrap_like_filemaker() {
        assert_eq!(int("-1").count(), 0);
        assert_eq!(int("4294967296").count(), 0);
        assert_eq!(int("4294967297").count(), 1);
        assert_eq!(int("2147483648").count(), 2147483648);
        assert_eq!(int("2147483648").signed_count(), 0);
        assert_eq!(int("2147483649").signed_start(), 1);
        assert_eq!(int("18446744069414584323").count(), 3);
        assert_eq!(int("1000000000000000000000000000002").count(), 2);
        assert_eq!(int("79228162514264337593543950338").count(), 1744830466);
        assert_eq!(int("-4294967295").wrapping(), 1);
        assert_eq!(int("4294967295").wrapping(), -1);
        assert_eq!(int("-4294967294").signed_start(), 1);
        assert_eq!(int("2.9").count(), 2);
    }

    fn find(text: &str, search: &str, start: u32, occurrence: i32) -> i64 {
        find_position(text, search, start, occurrence, &mut Work::default()).unwrap()
    }

    #[test]
    fn positions() {
        assert_eq!(find("Mississippi", "iss", 1, 2), 5);
        assert_eq!(find("aaaa", "aa", 4, -2), 1);
        assert_eq!(find("abcb", "b", 10, -1), 4);
        assert_eq!(find("abcb", "b", 1, -1), 0);
        assert_eq!(find("e\u{301}\u{301}", "\u{301}", 1, 1), 2);
        assert_eq!(find("a\r\nb\n", "\n", 1, 2), 4);
        assert_eq!(find("😀b😀b", "b", 3, -1), 2);
        assert_eq!(find("İ", "i", 1, 1), 1);
    }

    fn styled(s: &str, flags: u32) -> Value {
        Value::Text(Text::styled(
            s,
            CharStyle {
                flags,
                ..CharStyle::default()
            },
        ))
    }

    fn cat(parts: &[Value]) -> Value {
        let mut t = Text::new();
        for p in parts {
            if let Value::Text(p) = p {
                t.push(p);
            }
        }
        Value::Text(t)
    }

    /// `GetAsCSS ( name ( args ) )` in an en-US file.
    fn css(name: &str, args: &[Value]) -> String {
        let en = Locale::en_us();
        let f = FUNCTIONS.iter().find(|f| f.name == name).unwrap();
        let super::super::Implementation::Pure(f) = f.implementation else {
            unreachable!()
        };
        match f(args, &cx(&en)).unwrap() {
            Value::Text(t) => markup::css(&t),
            other => panic!("{name}: {other:?}"),
        }
    }

    const BOLD: u32 = 256;
    const ITALIC: u32 = 512;
    const PLAIN: u32 = 0xFFFF << 16;

    #[test]
    fn styles_follow_the_text_they_go_into() {
        let t = |s: &str| Value::text(s);
        let n = |k: i64| Value::number(k);
        assert_eq!(
            css(
                "Replace",
                &[
                    cat(&[styled("ab", BOLD), t("c")]),
                    n(2),
                    n(1),
                    styled("X", ITALIC)
                ]
            ),
            "<span style=\"font-weight: bold;\" >a</span>\
             <span style=\"font-weight: bold;font-style:italic;\" >X</span>\
             <span style=\"\" >c</span>"
        );
        assert_eq!(
            css(
                "Replace",
                &[
                    cat(&[t("a"), styled("b", BOLD), t("c")]),
                    n(2),
                    n(0),
                    t("X")
                ]
            ),
            "<span style=\"\" >a</span><span style=\"font-weight: bold;\" >Xb</span>\
             <span style=\"\" >c</span>"
        );
        assert_eq!(
            css(
                "Substitute",
                &[styled("abc", BOLD), t("b"), styled("X", PLAIN)]
            ),
            "<span style=\"font-weight: bold;\" >a</span><span style=\"font-size: 12px;\
             background-color: normal;font-weight: normal;font-style:normal;\
             text-decoration:none;font-variant:normal;letter-spacing: normal;\
             text-transform: none;\" >X</span><span style=\"font-weight: bold;\" >c</span>"
        );
        assert_eq!(
            css(
                "Substitute",
                &[cat(&[t("a"), styled("b", BOLD), t("c")]), t("bc"), t("X")]
            ),
            "<span style=\"\" >a</span><span style=\"font-weight: bold;\" >X</span>"
        );
        assert_eq!(
            css("Quote", &[cat(&[t("a"), styled("\"", BOLD), t("b")])]),
            "<span style=\"\" >&quot;a\\</span><span style=\"font-weight: bold;\" >&quot;</span>\
             <span style=\"\" >b&quot;</span>"
        );
        assert_eq!(
            css("TrimAll", &[cat(&[styled("a", BOLD), t("ア")]), n(1), n(1)]),
            "<span style=\"font-weight: bold;\" >a </span><span style=\"\" >&#12450;</span>"
        );
        assert_eq!(
            css(
                "TrimAll",
                &[cat(&[t("a "), styled(" b", BOLD)]), n(1), n(1)]
            ),
            "<span style=\"\" >a </span><span style=\"font-weight: bold;\" >b</span>"
        );
        assert_eq!(
            css("Proper", &[cat(&[t("ab "), styled("cd", BOLD)])]),
            "<span style=\"\" >Ab </span><span style=\"font-weight: bold;\" >Cd</span>"
        );
    }

    #[test]
    fn appended_returns_take_the_last_style() {
        let t = |s: &str| Value::text(s);
        let n = |k: i64| Value::number(k);
        assert_eq!(
            css("LeftValues", &[cat(&[styled("ab", BOLD), t("\rcd")]), n(1)]),
            "<span style=\"font-weight: bold;\" >ab<br/></span>"
        );
        assert_eq!(
            css(
                "MiddleValues",
                &[cat(&[styled("ab", BOLD), t("\rcd\ref")]), n(1), n(2)]
            ),
            "<span style=\"font-weight: bold;\" >ab</span><span style=\"\" ><br/>cd<br/></span>"
        );
        // FilterValues keeps a value's own separator.
        assert_eq!(
            css(
                "FilterValues",
                &[cat(&[styled("a", BOLD), t("\rb\ra")]), t("a")]
            ),
            "<span style=\"font-weight: bold;\" >a</span><span style=\"\" ><br/>a<br/></span>"
        );
        assert_eq!(
            css("SortValues", &[cat(&[styled("b", BOLD), t("\ra")])]),
            "<span style=\"\" >a<br/></span><span style=\"font-weight: bold;\" >b<br/></span>"
        );
        assert_eq!(
            css("UniqueValues", &[cat(&[styled("a", BOLD), t("\ra\rb")])]),
            "<span style=\"font-weight: bold;\" >a<br/></span><span style=\"\" >b<br/></span>"
        );
    }

    #[test]
    fn some_functions_drop_styles() {
        assert_eq!(
            css("SerialIncrement", &[styled("ab9", BOLD), Value::number(1)]),
            "<span style=\"\" >ab10</span>"
        );
        assert_eq!(
            css("GetAsURLEncoded", &[styled("a b", BOLD)]),
            "<span style=\"\" >a%20b</span>"
        );
    }

    #[test]
    fn digits_of_long_integers() {
        let n = Number::parse_text(&format!("65{}", "0".repeat(400)), '.', ',');
        let d = integer_digits(&n);
        assert_eq!(d.len(), 402);
        assert!(d.starts_with("650"));
    }

    #[test]
    fn unique_typed_values_keep_first_spelling_and_input_order() {
        let en = Locale::en_us();
        let cx = cx(&en);
        for (kind, input, expected) in [
            (2, "34\r600\r18\r600\r18.0", "34\r600\r18\r"),
            (2, "1\r01\r1.0\rabc\r\rx", "1\rabc\r\r"),
            (2, "-0\r0\r10\r1e1\rten", "-0\r10\rten\r"),
            (2, "9999999999999999999999999999999\r9999999999999999999999999999998\r9999999999999999999999999999999", "9999999999999999999999999999999\r9999999999999999999999999999998\r"),
            (3, "1/2/2003\r01/02/2003\r1-2-2003\rabc\rxyz", "1/2/2003\rabc\r"),
            (4, "1:00\r1:00:00\r60:00\rabc", "1:00\r60:00\rabc\r"),
            (4, "1:00 PM\r13:00", "1:00 PM\r"),
            (5, "1/2/2003 1:00\r1/2/2003 1:00:00\rabc", "1/2/2003 1:00\rabc\r"),
        ] {
            for kind in [kind, -kind] {
                let actual = unique_values(&[Value::text(input), Value::number(kind)], &cx)
                    .unwrap().to_text(&cx);
                assert_eq!(actual.as_str(), expected, "datatype {kind}: {input:?}");
            }
        }
        for kind in 2..=5 {
            for (input, expected) in [("", ""), ("\r\r", "\r")] {
                assert_eq!(
                    unique_values(&[Value::text(input), Value::number(kind)], &cx)
                        .unwrap()
                        .to_text(&cx)
                        .as_str(),
                    expected
                );
            }
        }
    }

    #[test]
    fn unique_typed_values_keep_first_styles_and_separator_styles() {
        let en = Locale::en_us();
        let cx = cx(&en);
        let input = cat(&[
            styled("03", BOLD),
            Value::text("\r"),
            styled("3.0", ITALIC),
            Value::text("\r"),
            styled("2", ITALIC),
            Value::text("\r"),
            styled("02", BOLD),
        ]);
        let actual = unique_values(&[input, Value::number(2)], &cx)
            .unwrap()
            .to_text(&cx);
        let expected = cat(&[styled("03\r", BOLD), styled("2\r", ITALIC)]).to_text(&cx);
        assert_eq!(actual.as_str(), expected.as_str());
        assert_eq!(actual.runs(), expected.runs());
    }

    #[test]
    fn unique_typed_numbers_follow_decimal_locale() {
        let de = Locale::de_de();
        let cx = cx(&de);
        let actual = unique_values(
            &[Value::text("2,5\r2,50\r1,5\r1,50"), Value::number(2)],
            &cx,
        )
        .unwrap()
        .to_text(&cx);
        assert_eq!(actual.as_str(), "2,5\r1,5\r");
    }
}
