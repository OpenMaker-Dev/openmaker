//! JSON functions and the JSON type keywords: `JSONDeleteElement`,
//! `JSONFormatElements`, `JSONGetElement`, `JSONGetElementType`,
//! `JSONListKeys`, `JSONListValues`, `JSONMakeArray`, `JSONParse`,
//! `JSONParsedState`, `JSONSetElement`, and the undocumented
//! `JSONUniqueValues`, `JSONRemoveValues`, `JSONValueExists`,
//! `JSONValueCount`, `JSONAnyValuesExist`; keywords `JSONString`,
//! `JSONNumber`, `JSONObject`, `JSONArray`, `JSONBoolean`, `JSONNull`,
//! `JSONRaw` (spec `docs/re/calc-functions.md` 5.5 and the JSON oracle
//! answers in 9).
//!
//! The engine is jsoncpp; [`reader`] reproduces its strict reader and
//! messages, [`value`] its writers with FileMaker's number format, [`path`]
//! FileMaker's path syntax. Every function reads its JSON argument afresh:
//! the parsed-JSON cache is invisible except to `JSONParsedState`, which
//! reads the state the JSON functions leave on their text results
//! ([`Text::json_state`]): the type for a parsed value, -1 for a parse
//! error, 0 for anything else.
//!
//! Results: objects and arrays as compact JSON (formatted for
//! `JSONFormatElements`); strings as their text; numbers and booleans as
//! numbers shown as their JSON text (`100.0`, `1`); null and missing
//! elements as empty text. Line breaks in results (LF, CRLF, U+2028,
//! U+2029) become FileMaker's return, CR. Errors are text: `? ` followed by
//! the reader's or the path's message with CR line breaks, `? Incorrect
//! key, index, or path` for `JSONGetElementType` of a missing element.
//!
//! `JSONSetElement`'s list form `JSONSetElement ( json ; [ k ; v ; t ] ;
//! [ k2 ; v2 ; t2 ] )` arrives flattened (the evaluator splices `[ ... ]`
//! groups): the function sees `json, k, v, t, k2, v2, t2`, the same as the
//! plain form, and applies the triples left to right. The parser rejects
//! counts other than 1 + 3n (1201, 1202, 1211).
//!
//! Hostile input: calculations run on the server's 2 MiB threads, where a
//! stack overflow or a failed allocation aborts the process. Nothing here
//! recurses over a document's nesting (the reader, the writers and dropping
//! a value keep their stacks on the heap), and two limits, OpenMaker's
//! choice where FileMaker's behaviour is unmeasured (calc-functions.md
//! U-23), keep what a call builds in proportion to its input:
//! - no value nests deeper than the reader's 1,000 levels: a
//!   `JSONSetElement` or `JSONMakeArray` result that would is
//!   `? Exceeded stackLimit in readValue().`, the error reading it back
//!   would give;
//! - one `JSONSetElement` call adds at most a million elements to arrays
//!   ([`path::MAX_NEW_ELEMENTS`]) over all its paths; beyond that its
//!   result is `?`.
//!
//! Results are then a small multiple of the input at most, except that
//! `JSONFormatElements` indents each line by its depth as jsoncpp does
//! (up to 999 tabs, about 500 times the input).

use super::{Function, Keyword};
use crate::error::CalcError;
use crate::locale::Language;
use crate::number::Number;
use crate::text::Text;
use crate::value::{Ctx, Value};
use std::collections::HashSet;
use std::fmt::Write;

mod path;
pub(super) mod reader;
pub(super) mod value;

use path::Seg;
use value::{kind, Json};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("JSONAnyValuesExist", json_any_values_exist),
    Function::pure("JSONDeleteElement", json_delete_element),
    Function::pure("JSONFormatElements", json_format_elements),
    Function::pure("JSONGetElement", json_get_element),
    Function::pure("JSONGetElementType", json_get_element_type),
    Function::pure("JSONListKeys", json_list_keys),
    Function::pure("JSONListValues", json_list_values),
    Function::pure("JSONMakeArray", json_make_array),
    Function::pure("JSONParse", json_parse),
    Function::pure("JSONParsedState", json_parsed_state),
    Function::pure("JSONRemoveValues", json_remove_values),
    Function::pure("JSONSetElement", json_set_element),
    Function::pure("JSONUniqueValues", json_unique_values),
    Function::pure("JSONValueCount", json_value_count),
    Function::pure("JSONValueExists", json_value_exists),
];

pub(super) const KEYWORDS: &[Keyword] = &[
    ("JSONString", kind::STRING),
    ("JSONNumber", kind::NUMBER),
    ("JSONObject", kind::OBJECT),
    ("JSONArray", kind::ARRAY),
    ("JSONBoolean", kind::BOOLEAN),
    ("JSONNull", kind::NULL),
    ("JSONRaw", kind::RAW),
];

type Result<T> = std::result::Result<T, CalcError>;

/// A failure that the function returns as its text.
type Failed = String;

// ----- results ------------------------------------------------------------

/// FileMaker text from JSON output: LF, CRLF, U+2028 and U+2029 become CR
/// (`Unicode::ReplaceAllNewLines`); NEL stays.
fn returns(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push('\r');
            }
            '\n' | '\u{2028}' | '\u{2029}' => out.push('\r'),
            other => out.push(other),
        }
    }
    out
}

/// Text marked as parsed JSON with `state`.
fn parsed(text: String, state: i64) -> Value {
    Value::Text(Text::plain(text).with_json_state(state as i8))
}

/// `? ` and a reader or path message with CR line breaks, the trailing
/// break removed (`DBData::HandleJsonParseError`).
fn failure(message: &str) -> String {
    let mut text = format!("? {}", returns(message));
    if text.ends_with('\r') {
        text.pop();
    }
    text
}

/// A failure as an unparsed result (`JSONGetElement`, `JSONListKeys`...).
fn failed(message: &str) -> Value {
    Value::text(failure(message))
}

/// A failure as a parsed-but-invalid result (`JSONSetElement`,
/// `JSONParse`, `JSONFormatElements`...): state -1.
fn failed_parsed(message: &str) -> Value {
    parsed(failure(message), -1)
}

/// A JSON value as FileMaker text (`DBData::SetTextFromJsonValue`):
/// strings as they are, numbers in JSON form with the file's decimal
/// separator (`100,0` in German files), booleans 1 and 0, null empty,
/// objects and arrays compact.
fn text_of(v: &Json, cx: &Ctx) -> String {
    returns(&match v {
        Json::Null => String::new(),
        Json::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        Json::String(s) => s.clone(),
        Json::Array(_) | Json::Object(_) => value::compact(v),
        number => local_number(number, cx),
    })
}

/// A JSON number's text with the file's decimal separator.
fn local_number(v: &Json, cx: &Ctx) -> String {
    let text = v.number_text().unwrap_or_default();
    text.replace('.', cx.locale.decimal_separator.encode_utf8(&mut [0; 4]))
}

/// A JSON value as a parsed result: its text marked with its type.
fn parsed_value(v: &Json, cx: &Ctx) -> Value {
    parsed(text_of(v, cx), v.kind())
}

/// An element as `JSONGetElement` returns it: objects and arrays parsed,
/// numbers and booleans as numbers shown as their JSON text, strings as
/// text, null as empty text.
fn element(v: &Json, cx: &Ctx) -> Value {
    match v {
        Json::Null => Value::empty_text(),
        Json::String(_) => Value::text(text_of(v, cx)),
        Json::Array(_) | Json::Object(_) => parsed_value(v, cx),
        Json::Bool(b) => Value::NumberText(Number::from_bool(*b), Text::plain(text_of(v, cx))),
        number => {
            let json = number.number_text().unwrap_or_default();
            Value::NumberText(
                Number::parse_text(&json, '.', '\0'),
                Text::plain(local_number(number, cx)),
            )
        }
    }
}

// ----- arguments ----------------------------------------------------------

/// The JSON argument of most functions: its text, read strictly. A value
/// a JSON function already found invalid keeps its error (oracle:
/// `JSONFormatElements ( JSONParse ( "[3" ) )` reports the missing `]`).
fn document(v: &Value, cx: &Ctx) -> std::result::Result<Json, Failed> {
    read(v, v.to_text(cx).as_str())
}

fn read(v: &Value, text: &str) -> std::result::Result<Json, Failed> {
    if let Value::Text(t) = v {
        if t.json_state() == -1 {
            return Err(unfailure(t.as_str()));
        }
    }
    reader::parse(text, reader::STRICT)
}

/// The message [`failure`] made `text` from.
fn unfailure(text: &str) -> Failed {
    let body = text.strip_prefix("? ").unwrap_or(text);
    format!("{}\n", body.replace('\r', "\n"))
}

/// The JSON argument of `JSONParse`, `JSONFormatElements` and the value
/// arrays of `JSONRemoveValues` and `JSONAnyValuesExist`: only text counts;
/// a number, date or time reads as empty (oracle: `JSONParse ( 5 )` reports
/// an empty document, `JSONGetElement ( 5 ; "" )` a bad root).
fn text_document(v: &Value, cx: &Ctx) -> String {
    match v {
        Value::Text(_) | Value::Container(_) => v.to_text(cx).into_string(),
        _ => String::new(),
    }
}

/// `keyOrIndexOrPath` for a document: a whole number (a number value, or
/// text that is exactly a number such as ` 1`, `1.0`, `+1`, `1e0`) indexes
/// a root array, modulo 2^32; anything else is a path.
fn path_for(arg: &Value, root: &Json, cx: &Ctx) -> std::result::Result<Vec<Seg>, Failed> {
    if let Json::Array(_) = root {
        if let Some(index) = whole_number(arg, cx) {
            return Ok(vec![Seg::Index(index as u32)]);
        }
    }
    path::parse(arg.to_text(cx).as_str())
}

fn whole_number(arg: &Value, cx: &Ctx) -> Option<i64> {
    let n = match arg {
        Value::Text(t) => {
            let s = t.as_str().trim_matches(|c: char| c.is_ascii_whitespace());
            let decimal = cx.locale.decimal_separator;
            if !is_number_text(s, decimal) {
                return None;
            }
            Number::parse_text(s, decimal, '\0')
        }
        Value::Container(_) | Value::Empty(_) => return None,
        other => other.to_number(cx),
    };
    if n.is_empty() || n.is_nan() || n.compare(&n.int()) != Some(std::cmp::Ordering::Equal) {
        return None;
    }
    Some(n.as_long_long())
}

/// An optional sign, digits with an optional fraction after the file's
/// decimal separator (`1,0` indexes in a German file, `1.0` does not), an
/// optional exponent.
fn is_number_text(s: &str, decimal: char) -> bool {
    let c: Vec<char> = s.chars().collect();
    let mut i = usize::from(matches!(c.first(), Some('+' | '-')));
    let digits = |i: &mut usize| {
        let start = *i;
        while c.get(*i).is_some_and(char::is_ascii_digit) {
            *i += 1;
        }
        *i - start
    };
    let mut count = digits(&mut i);
    if c.get(i) == Some(&decimal) {
        i += 1;
        count += digits(&mut i);
    }
    if count == 0 {
        return false;
    }
    if matches!(c.get(i), Some('e' | 'E')) {
        i += 1;
        if matches!(c.get(i), Some('+' | '-')) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == c.len()
}

/// The element a path names (`None` when it names nothing), or the
/// failure text.
fn lookup(json: &Value, key: &Value, cx: &Ctx) -> std::result::Result<Option<Json>, Failed> {
    let root = document(json, cx)?;
    let segs = path_for(key, &root, cx)?;
    Ok(path::take(root, &segs))
}

/// A value as `JSONSetElement` and `JSONMakeArray` store it with a type.
fn typed(v: &Value, ty: i64, cx: &Ctx) -> Json {
    let text = || v.to_text(cx).into_string();
    let lenient = || reader::parse(&text(), reader::LENIENT);
    match ty {
        kind::STRING => Json::String(text()),
        kind::NUMBER => number(&v.to_number(cx)),
        // The text `true`, else the value's truth (`"a1"` and `"?"` are
        // true, `"TRUE"` and `"abc"` false).
        kind::BOOLEAN => Json::Bool(text() == "true" || v.is_true(cx)),
        kind::NULL => Json::Null,
        kind::OBJECT => match lenient() {
            Ok(o @ Json::Object(_)) => o,
            _ => Json::Object(Default::default()),
        },
        kind::ARRAY => match lenient() {
            Ok(a @ Json::Array(_)) => a,
            _ => Json::Array(Vec::new()),
        },
        // JSONRaw and any other number: JSON text, else the text itself.
        _ => lenient().unwrap_or_else(|_| Json::String(text())),
    }
}

/// A FileMaker number as JSON: whole numbers within ±(2^63 - 1) as
/// integers, others as doubles (null beyond the double range); empty and
/// `?` are 0.
fn number(n: &Number) -> Json {
    if n.is_empty() || n.is_nan() {
        return Json::Int(0);
    }
    if n.compare(&n.int()) == Some(std::cmp::Ordering::Equal) {
        if let Some(i) = n.to_i64().filter(|&i| i != i64::MIN) {
            return Json::Int(i);
        }
    }
    match n.to_f64() {
        Some(f) if f.is_finite() => Json::Real(f),
        _ => Json::Null,
    }
}

fn type_arg(v: &Value, cx: &Ctx) -> i64 {
    v.to_number(cx).as_long_long()
}

// ----- the documented functions -------------------------------------------

fn json_get_element(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(match lookup(&args[0], &args[1], cx) {
        Ok(Some(v)) => element(&v, cx),
        Ok(None) => Value::empty_text(),
        Err(e) => failed(&e),
    })
}

fn json_get_element_type(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(match lookup(&args[0], &args[1], cx) {
        Ok(Some(v)) => Value::number(v.kind()),
        Ok(None) => Value::text("? Incorrect key, index, or path"),
        Err(e) => failed(&e),
    })
}

/// Lines with CR between them, written straight into one text.
fn lines<T>(items: impl Iterator<Item = T>, mut write: impl FnMut(&mut String, T)) -> String {
    let mut out = String::new();
    for (i, item) in items.enumerate() {
        if i > 0 {
            out.push('\r');
        }
        write(&mut out, item);
    }
    out
}

/// `JSONListKeys`: an object's keys in order or an array's indexes, one
/// per line; nothing for anything else.
fn json_list_keys(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(match &lookup(&args[0], &args[1], cx) {
        Ok(Some(Json::Object(m))) => Value::text(lines(m.keys(), |out, k| out.push_str(k))),
        Ok(Some(Json::Array(v))) => Value::text(lines(0..v.len(), |out, i| {
            let _ = write!(out, "{i}");
        })),
        Ok(_) => Value::empty_text(),
        Err(e) => failed(e),
    })
}

/// `JSONListValues`: the values one per line, nulls left out, strings as
/// they are (line breaks in them unchanged), objects and arrays compact.
fn json_list_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    let line = |out: &mut String, v: &Json| match v {
        Json::String(s) => out.push_str(s),
        Json::Bool(b) => out.push(if *b { '1' } else { '0' }),
        Json::Array(_) | Json::Object(_) => out.push_str(&value::compact(v)),
        number => out.push_str(&local_number(number, cx)),
    };
    let present = |v: &&Json| !matches!(v, Json::Null);
    Ok(match &lookup(&args[0], &args[1], cx) {
        Ok(Some(Json::Object(m))) => Value::text(lines(m.values().filter(present), line)),
        Ok(Some(Json::Array(v))) => Value::text(lines(v.iter().filter(present), line)),
        Ok(_) => Value::empty_text(),
        Err(e) => failed(e),
    })
}

/// `JSONSetElement`: each `key ; value ; type` triple in turn. Empty JSON
/// text starts from nothing (the first key makes an object or an array).
/// The triples share one budget of elements added to arrays.
fn json_set_element(args: &[Value], cx: &Ctx) -> Result<Value> {
    let mut root = if args[0].is_empty() {
        Json::Null
    } else {
        match document(&args[0], cx) {
            Ok(root) => root,
            Err(e) => return Ok(failed_parsed(&e)),
        }
    };
    let mut added = 0;
    for triple in args[1..].chunks_exact(3) {
        let segs = match path_for(&triple[0], &root, cx) {
            Ok(segs) => segs,
            Err(e) => return Ok(failed_parsed(&e)),
        };
        let value = typed(&triple[1], type_arg(&triple[2], cx), cx);
        if let Err(e) = path::set(&mut root, &segs, value, &mut added) {
            return Ok(match e.message() {
                Some(message) => failed_parsed(message),
                None => Value::invalid(),
            });
        }
    }
    Ok(parsed_value(&root, cx))
}

/// `JSONDeleteElement`: an empty path returns the JSON argument as it was
/// (unparsed); a path that names nothing changes nothing.
fn json_delete_element(args: &[Value], cx: &Ctx) -> Result<Value> {
    let mut root = match document(&args[0], cx) {
        Ok(root) => root,
        Err(e) => return Ok(failed_parsed(&e)),
    };
    if matches!(&args[1], Value::Text(t) if t.is_empty()) {
        return Ok(Value::Text(args[0].to_text(cx)));
    }
    match path_for(&args[1], &root, cx) {
        Ok(segs) => {
            path::delete(&mut root, &segs);
            Ok(parsed_value(&root, cx))
        }
        Err(e) => Ok(failed_parsed(&e)),
    }
}

fn json_format_elements(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(match read(&args[0], &text_document(&args[0], cx)) {
        Ok(root) => match value::formatted_within(&root, cx.text_limit) {
            Some(text) => parsed(returns(&text), root.kind()),
            None => return Err(CalcError::OUT_OF_MEMORY),
        },
        Err(e) => failed_parsed(&e),
    })
}

/// `JSONParse`: the text unchanged, marked parsed, or the parse error.
fn json_parse(args: &[Value], cx: &Ctx) -> Result<Value> {
    let text = text_document(&args[0], cx);
    Ok(match read(&args[0], &text) {
        Ok(root) => parsed(args[0].to_text(cx).into_string(), root.kind()),
        Err(e) => failed_parsed(&e),
    })
}

fn json_parsed_state(args: &[Value], _cx: &Ctx) -> Result<Value> {
    Ok(Value::number(match &args[0] {
        Value::Text(t) => i64::from(t.json_state()),
        _ => 0,
    }))
}

/// `JSONMakeArray`: with an empty separator the list's values (a trailing
/// return adds nothing), else the pieces between separators (case
/// sensitive; a trailing separator adds an empty piece), each stored with
/// the type. An array too deep to read back (a piece that reads as JSON
/// 1,000 levels deep) fails as reading it would.
fn json_make_array(args: &[Value], cx: &Ctx) -> Result<Value> {
    let list = args[0].to_text(cx);
    // Empty input is `?` with error -1.
    if list.is_empty() {
        return Err(CalcError::UNKNOWN);
    }
    let separator = args[1].to_text(cx).into_string();
    let pieces: Box<dyn Iterator<Item = &str>> = if separator.is_empty() {
        Box::new(list.value_ranges().into_iter().map(|r| &list.as_str()[r]))
    } else {
        Box::new(list.as_str().split(separator.as_str()))
    };
    let ty = type_arg(&args[2], cx);
    let array = Json::Array(pieces.map(|p| typed(&Value::text(p), ty, cx)).collect());
    if array.depth() > reader::STACK_LIMIT {
        return Ok(failed_parsed(reader::STACK_LIMIT_MESSAGE));
    }
    Ok(parsed_value(&array, cx))
}

// ----- the undocumented value functions -----------------------------------

/// What two values match by (`IsMatchWithLocale`): their jsoncpp string
/// forms (`1` matches `"1"`, `1.0` does not match `1`, null matches empty
/// text) equal at the collation's secondary strength, so case, width and
/// kana type do not count but accents do (`a` = `A` = `Ａ`, `あ` = `ア`,
/// `é` = `É` but not `e`). In a Japanese file they must be identical.
/// Each value's key is computed once, so comparing an array with many
/// values is a set lookup per element rather than a collation per pair.
#[derive(PartialEq, Eq, Hash)]
enum MatchKey {
    Identical(String),
    Weights((Vec<u16>, Vec<u16>)),
}

fn match_key(s: String, cx: &Ctx) -> MatchKey {
    if cx.locale.language == Language::Japanese {
        MatchKey::Identical(s)
    } else {
        MatchKey::Weights(secondary(&s))
    }
}

/// A value's key; objects and arrays match nothing (the engine aborts on
/// them).
fn key_of(v: &Json, cx: &Ctx) -> Option<MatchKey> {
    v.as_string().map(|s| match_key(s, cx))
}

/// The primary and secondary DUCET weights of a text, ignorables left out.
fn secondary(s: &str) -> (Vec<u16>, Vec<u16>) {
    let elements = om_collate::collation_elements(s);
    let weights =
        |f: fn(&om_collate::Element) -> u16| elements.iter().map(f).filter(|&w| w != 0).collect();
    (weights(|e| e.primary), weights(|e| e.secondary))
}

/// The array a path names for the value functions: `Ok(Ok(items))` for an
/// array, `Ok(Err(other))` for anything else (`None` when the path names
/// nothing), or the failure text.
type Target = std::result::Result<Vec<Json>, Option<Json>>;

fn target(json: &Value, key: &Value, cx: &Ctx) -> std::result::Result<Target, Failed> {
    Ok(match lookup(json, key, cx)? {
        Some(found) => found.into_array().map_err(Some),
        None => Err(None),
    })
}

fn value_count(args: &[Value], cx: &Ctx) -> std::result::Result<usize, Failed> {
    let wanted = match_key(args[2].to_text(cx).into_string(), cx);
    Ok(match target(&args[0], &args[1], cx)? {
        Ok(items) => items
            .iter()
            .filter(|v| key_of(v, cx).is_some_and(|k| k == wanted))
            .count(),
        Err(_) => 0,
    })
}

/// `JSONValueExists`: 1 when the array holds a value matching `value`.
fn json_value_exists(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(match value_count(args, cx) {
        Ok(n) => Value::boolean(n > 0),
        Err(e) => failed(&e),
    })
}

/// `JSONValueCount`: how many of the array's values match `value`.
fn json_value_count(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(match value_count(args, cx) {
        Ok(n) => Value::number(n as i64),
        Err(e) => failed(&e),
    })
}

/// `JSONUniqueValues`: the array without values matching an earlier one;
/// a non-array element comes back as it is, nothing as null. More than two
/// arguments is `?` with error -1.
fn json_unique_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    if args.len() > 2 {
        return Err(CalcError::UNKNOWN);
    }
    Ok(match target(&args[0], &args[1], cx) {
        Ok(Ok(items)) => {
            let mut kept: Vec<Json> = Vec::new();
            let mut seen = HashSet::new();
            for item in items {
                if key_of(&item, cx).map_or(true, |k| seen.insert(k)) {
                    kept.push(item);
                }
            }
            parsed_value(&Json::Array(kept), cx)
        }
        Ok(Err(other)) => parsed_value(&other.unwrap_or(Json::Null), cx),
        Err(e) => failed_parsed(&e),
    })
}

/// The values of a `jsonValuesArray` argument: an array's elements, none
/// for an object; anything else is the reader's failure.
fn value_array(v: &Value, cx: &Ctx) -> std::result::Result<Vec<Json>, Failed> {
    Ok(read(v, &text_document(v, cx))?
        .into_array()
        .unwrap_or_default())
}

/// The keys of the values `jsonValuesArray` lists.
fn keys(values: &[Json], cx: &Ctx) -> HashSet<MatchKey> {
    values.iter().filter_map(|v| key_of(v, cx)).collect()
}

fn any_of(item: &Json, keys: &HashSet<MatchKey>, cx: &Ctx) -> bool {
    key_of(item, cx).is_some_and(|k| keys.contains(&k))
}

/// `JSONRemoveValues`: the array without the values matching any of
/// `jsonValuesArrayToRemove`.
fn json_remove_values(args: &[Value], cx: &Ctx) -> Result<Value> {
    if args.len() > 3 {
        return Err(CalcError::UNKNOWN);
    }
    let found = target(&args[0], &args[1], cx).and_then(|t| Ok((t, value_array(&args[2], cx)?)));
    Ok(match found {
        Ok((Ok(items), remove)) => {
            let remove = keys(&remove, cx);
            parsed_value(
                &Json::Array(
                    items
                        .into_iter()
                        .filter(|v| !any_of(v, &remove, cx))
                        .collect(),
                ),
                cx,
            )
        }
        Ok((Err(other), _)) => parsed_value(&other.unwrap_or(Json::Null), cx),
        Err(e) => failed_parsed(&e),
    })
}

/// `JSONAnyValuesExist`: 1 when the array holds a value matching any of
/// `jsonValuesArrayToFind`.
fn json_any_values_exist(args: &[Value], cx: &Ctx) -> Result<Value> {
    let found = target(&args[0], &args[1], cx).and_then(|t| Ok((t, value_array(&args[2], cx)?)));
    Ok(match found {
        Ok((Ok(items), find)) => {
            let find = keys(&find, cx);
            Value::boolean(items.iter().any(|v| any_of(v, &find, cx)))
        }
        Ok((Err(_), _)) => Value::boolean(false),
        Err(e) => failed(&e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::Date;
    use crate::locale::Locale;

    #[test]
    fn whole_number_arguments() {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
        for (text, index) in [
            (" 1", Some(1)),
            ("1.0", Some(1)),
            ("+1", Some(1)),
            ("1e0", Some(1)),
            ("-0", Some(0)),
        ] {
            assert_eq!(whole_number(&Value::text(text), &cx), index, "{text:?}");
        }
        for text in ["1,0", "$1", "x1", "1a", "0x1", "1.5", "", "."] {
            assert_eq!(whole_number(&Value::text(text), &cx), None, "{text:?}");
        }
        assert_eq!(whole_number(&Value::number(4), &cx), Some(4));
    }

    #[test]
    fn numbers_as_json() {
        let n = |s: &str| number(&Number::parse_text(s, '.', '\0'));
        assert_eq!(n("5"), Json::Int(5));
        assert_eq!(n("9223372036854775807"), Json::Int(i64::MAX));
        assert!(matches!(n("-9223372036854775808"), Json::Real(_)));
        assert_eq!(n(".5"), Json::Real(0.5));
        assert_eq!(Number::zero().to_f64(), Some(0.0));
        assert_eq!(Number::parse_text("-1.25", '.', '\0').to_f64(), Some(-1.25));
        assert_eq!(n("1").number_text().unwrap(), "1");
        assert_eq!(
            value::real(match n("123456789012345.678") {
                Json::Real(f) => f,
                _ => 0.0,
            }),
            "123456789012346.0"
        );
    }
}
