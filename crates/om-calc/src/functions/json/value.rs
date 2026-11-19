//! JSON values and how FileMaker writes them (spec `calc-functions.md`
//! 5.5 and the JSON oracle answers in 9).
//!
//! Objects keep their members sorted by the bytes of the key (jsoncpp's
//! `std::map`). Numbers are 64-bit integers or doubles as the reader made
//! them; doubles print with 15 significant digits, in fixed notation from
//! 1e-19 up to 1e15 (with `.0` when nothing follows the point, so a parsed
//! `1e2` stays `100.0`) and in exponent notation outside (`1e+15`,
//! `1e-20`). Output is compact (`{"a":1}`) or formatted the jsoncpp way
//! (tab indentation, `" : "`, short arrays of plain values on one line,
//! lines ending with CR after FileMaker's newline conversion).
//!
//! Nothing here recurses over a value's nesting: the writer, [`Json::depth`]
//! and dropping a value keep their own stack on the heap, so a deep value
//! needs no more thread stack than a flat one wherever it is written or
//! dropped (calculations run on 2 MiB server threads, and a stack overflow
//! aborts the process). `Clone` is not implemented and `Debug` and
//! `PartialEq`, which recurse, exist only in tests.

use std::collections::BTreeMap;
use std::fmt::Write;

#[cfg_attr(test, derive(Debug, PartialEq))]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Real(f64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

/// Dropping takes the children that hold children of their own out of each
/// value, one level at a time, instead of letting the compiler's drop glue
/// recurse once per level.
impl Drop for Json {
    fn drop(&mut self) {
        if !self.has_children() {
            return;
        }
        let mut pending = Vec::new();
        self.detach_children(&mut pending);
        while let Some(mut value) = pending.pop() {
            value.detach_children(&mut pending);
        }
    }
}

/// The type numbers of `JSONGetElementType`, `JSONParsedState` and the
/// type keywords (`kJSONStringValue` 1 ... `kJSONNullValue` 6).
pub mod kind {
    pub const RAW: i64 = 0;
    pub const STRING: i64 = 1;
    pub const NUMBER: i64 = 2;
    pub const OBJECT: i64 = 3;
    pub const ARRAY: i64 = 4;
    pub const BOOLEAN: i64 = 5;
    pub const NULL: i64 = 6;
}

impl Json {
    /// The JSON type number (1-6).
    pub fn kind(&self) -> i64 {
        match self {
            Json::Null => kind::NULL,
            Json::Bool(_) => kind::BOOLEAN,
            Json::Int(_) | Json::UInt(_) | Json::Real(_) => kind::NUMBER,
            Json::String(_) => kind::STRING,
            Json::Array(_) => kind::ARRAY,
            Json::Object(_) => kind::OBJECT,
        }
    }

    /// Whether the value holds other values (a non-empty object or array).
    pub fn has_children(&self) -> bool {
        match self {
            Json::Array(items) => !items.is_empty(),
            Json::Object(members) => !members.is_empty(),
            _ => false,
        }
    }

    /// How deeply values nest, counted the way the reader counts against
    /// its limit: 1 for a scalar or an empty object or array, one more for
    /// each level of objects and arrays above the deepest value.
    pub fn depth(&self) -> usize {
        let mut deepest = 1;
        let mut pending = vec![(self, 1)];
        while let Some((value, depth)) = pending.pop() {
            if value.has_children() {
                deepest = deepest.max(depth + 1);
            }
            let below = |child: &&Json| child.has_children();
            match value {
                Json::Array(items) => {
                    pending.extend(items.iter().filter(below).map(|c| (c, depth + 1)));
                }
                Json::Object(members) => {
                    pending.extend(members.values().filter(below).map(|c| (c, depth + 1)));
                }
                _ => {}
            }
        }
        deepest
    }

    /// The items of an array, or the value itself when it is not one.
    pub fn into_array(mut self) -> Result<Vec<Json>, Json> {
        match &mut self {
            Json::Array(items) => Ok(std::mem::take(items)),
            _ => Err(self),
        }
    }

    /// Moves this value's children into `pending` when one of them holds
    /// children of its own (the others are dropped here). Without such a
    /// child the value is left whole: dropping it goes one level down.
    fn detach_children(&mut self, pending: &mut Vec<Json>) {
        match self {
            Json::Array(items) if items.iter().any(Json::has_children) => {
                pending.extend(std::mem::take(items).into_iter().filter(Json::has_children));
            }
            Json::Object(members) if members.values().any(Json::has_children) => {
                pending.extend(
                    std::mem::take(members)
                        .into_values()
                        .filter(Json::has_children),
                );
            }
            _ => {}
        }
    }

    /// The number as FileMaker writes it.
    pub fn number_text(&self) -> Option<String> {
        match self {
            Json::Int(i) => Some(i.to_string()),
            Json::UInt(u) => Some(u.to_string()),
            Json::Real(f) => Some(real(*f)),
            _ => None,
        }
    }

    /// jsoncpp's `asString`, which the undocumented value functions match
    /// on: `null` is empty, booleans `true`/`false`, doubles with 17
    /// significant digits (`1.0`, `0.10000000000000001`). Objects and arrays
    /// have none (the engine aborts on them; these functions skip them).
    pub fn as_string(&self) -> Option<String> {
        Some(match self {
            Json::Null => String::new(),
            Json::Bool(b) => b.to_string(),
            Json::Int(i) => i.to_string(),
            Json::UInt(u) => u.to_string(),
            Json::Real(f) => {
                let mut s = significant(*f, 17, -4..17);
                if !s.contains(['.', 'e']) {
                    s.push_str(".0");
                }
                s
            }
            Json::String(s) => s.clone(),
            Json::Array(_) | Json::Object(_) => return None,
        })
    }
}

/// A double as FileMaker writes it in JSON.
pub fn real(f: f64) -> String {
    if !f.is_finite() {
        return "null".to_string();
    }
    let mut s = significant(f, 15, -19..15);
    if !s.contains(['.', 'e']) {
        s.push_str(".0");
    }
    s
}

/// `f` with `digits` significant digits, trailing zeros removed, in fixed
/// notation for decimal exponents in `fixed` and in C's `%g` exponent
/// notation outside (`1e+15`, `4.94065645841247e-324`).
fn significant(f: f64, digits: usize, fixed: std::ops::Range<i32>) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    // `d.dddde±x`, correctly rounded.
    let sci = format!("{:.*e}", digits - 1, f.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    let mantissa: String = mantissa.chars().filter(|&c| c != '.').collect();
    let mantissa = mantissa.trim_end_matches('0');
    let mantissa = if mantissa.is_empty() { "0" } else { mantissa };
    let mut out = String::new();
    if f < 0.0 {
        out.push('-');
    }
    if fixed.contains(&exponent) {
        if exponent < 0 {
            out.push_str("0.");
            out.push_str(&"0".repeat((-exponent - 1) as usize));
            out.push_str(mantissa);
        } else {
            let point = exponent as usize + 1;
            if mantissa.len() <= point {
                out.push_str(mantissa);
                out.push_str(&"0".repeat(point - mantissa.len()));
            } else {
                out.push_str(&mantissa[..point]);
                out.push('.');
                out.push_str(&mantissa[point..]);
            }
        }
    } else {
        out.push_str(&mantissa[..1]);
        if mantissa.len() > 1 {
            out.push('.');
            out.push_str(&mantissa[1..]);
        }
        let _ = write!(
            out,
            "e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.abs()
        );
    }
    out
}

/// A JSON string literal: `"` `\` and the control characters escaped
/// (`\b \f \n \r \t`, others `\u00XX`), everything else as it is.
pub fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Compact JSON: `{"a":[1,2]}`.
pub fn compact(v: &Json) -> String {
    Writer::new("").write(v).unwrap_or_default()
}

#[cfg(test)]
fn formatted(v: &Json) -> String {
    formatted_within(v, usize::MAX).unwrap_or_default()
}

/// `JSONFormatElements`' layout, with LF line ends, or `None` once it
/// passes `limit` bytes: each line is indented by its depth, so the layout
/// can be hundreds of times the compact text.
pub fn formatted_within(v: &Json, limit: usize) -> Option<String> {
    Writer::new("\t").with_limit(limit).write(v)
}

/// The text of a value without children: a scalar, `[]` or `{}`.
fn leaf(v: &Json) -> String {
    match v {
        Json::Null => "null".to_string(),
        Json::Bool(b) => b.to_string(),
        Json::String(s) => quoted(s),
        Json::Array(_) => "[]".to_string(),
        Json::Object(_) => "{}".to_string(),
        number => number.number_text().unwrap_or_default(),
    }
}

/// jsoncpp's `BuiltStyledStreamWriter` without comments: with an empty
/// indentation it writes compact JSON. jsoncpp recurses into each object
/// and array; this writer keeps the open ones on [`Open`] instead.
struct Writer {
    out: String,
    indentation: &'static str,
    indent: String,
    indented: bool,
    /// The one-line forms of a short array's elements.
    child_values: Vec<String>,
    /// The output's bytes past which writing stops.
    limit: usize,
}

/// An object or array being written, with the children still to write and
/// whether none has been written yet.
enum Open<'a> {
    Object(std::collections::btree_map::Iter<'a, String, Json>, bool),
    Array(std::slice::Iter<'a, Json>, bool),
}

/// Arrays whose one-line form would reach this many bytes are split.
const RIGHT_MARGIN: usize = 74;

impl Writer {
    fn new(indentation: &'static str) -> Writer {
        Writer {
            out: String::new(),
            indentation,
            indent: String::new(),
            indented: true,
            child_values: Vec::new(),
            limit: usize::MAX,
        }
    }

    fn with_limit(self, limit: usize) -> Writer {
        Writer { limit, ..self }
    }

    /// The text, or `None` once it passes the limit: checked after each
    /// child, so it grows at most one value and one indentation past it.
    fn write(mut self, root: &Json) -> Option<String> {
        let mut open = Vec::new();
        self.begin(root, &mut open);
        while let Some(top) = open.last_mut() {
            if self.out.len() > self.limit {
                return None;
            }
            let (name, child, first) = match top {
                Open::Object(members, first) => match members.next() {
                    Some((name, child)) => (Some(name), child, std::mem::replace(first, false)),
                    None => {
                        open.pop();
                        self.close("}");
                        continue;
                    }
                },
                Open::Array(items, first) => match items.next() {
                    Some(child) => (None, child, std::mem::replace(first, false)),
                    None => {
                        open.pop();
                        self.close("]");
                        continue;
                    }
                },
            };
            if !first {
                self.out.push(',');
            }
            match name {
                Some(name) => {
                    self.write_with_indent(&quoted(name));
                    self.out.push_str(self.colon());
                    self.begin(child, &mut open);
                }
                None => {
                    if !self.indented {
                        self.write_indent();
                    }
                    self.indented = true;
                    let depth = open.len();
                    self.begin(child, &mut open);
                    // A child that opened an object or array clears this
                    // when it closes.
                    if open.len() == depth {
                        self.indented = false;
                    }
                }
            }
        }
        (self.out.len() <= self.limit).then_some(self.out)
    }

    fn colon(&self) -> &'static str {
        if self.indentation.is_empty() {
            ":"
        } else {
            " : "
        }
    }

    fn write_indent(&mut self) {
        if !self.indentation.is_empty() {
            self.out.push('\n');
            self.out.push_str(&self.indent);
        }
    }

    fn write_with_indent(&mut self, s: &str) {
        if !self.indented {
            self.write_indent();
        }
        self.out.push_str(s);
        self.indented = false;
    }

    /// Writes a value without children whole, or opens an object or array:
    /// writes what comes before its first child and pushes it on `open`.
    fn begin<'a>(&mut self, v: &'a Json, open: &mut Vec<Open<'a>>) {
        match v {
            Json::Object(members) if !members.is_empty() => {
                self.write_with_indent("{");
                self.indent.push_str(self.indentation);
                open.push(Open::Object(members.iter(), true));
            }
            Json::Array(items) if !items.is_empty() => {
                if !self.is_multiline(items) {
                    self.write_one_line();
                    return;
                }
                self.write_with_indent("[");
                self.indent.push_str(self.indentation);
                let rendered = std::mem::take(&mut self.child_values);
                if rendered.is_empty() {
                    open.push(Open::Array(items.iter(), true));
                    return;
                }
                for (i, child) in rendered.iter().enumerate() {
                    if i > 0 {
                        self.out.push(',');
                    }
                    self.write_with_indent(child);
                }
                self.close("]");
            }
            _ => self.out.push_str(&leaf(v)),
        }
    }

    /// Ends an object or array opened on several lines.
    fn close(&mut self, bracket: &str) {
        self.indent
            .truncate(self.indent.len() - self.indentation.len());
        self.write_with_indent(bracket);
    }

    /// `[ 1, 2 ]` (`[1,2]` compact) from the rendered elements.
    fn write_one_line(&mut self) {
        let spaced = !self.indentation.is_empty();
        self.out.push('[');
        if spaced {
            self.out.push(' ');
        }
        let rendered = std::mem::take(&mut self.child_values);
        for (i, child) in rendered.iter().enumerate() {
            if i > 0 {
                self.out.push_str(if spaced { ", " } else { "," });
            }
            self.out.push_str(child);
        }
        if spaced {
            self.out.push(' ');
        }
        self.out.push(']');
    }

    /// Whether an array goes on several lines: 25 or more elements, a
    /// non-empty object or array among them, or a one-line form of
    /// [`RIGHT_MARGIN`] bytes or more. A one-line candidate leaves its
    /// rendered elements in `child_values`.
    fn is_multiline(&mut self, items: &[Json]) -> bool {
        self.child_values.clear();
        if items.len() * 3 >= RIGHT_MARGIN || items.iter().any(Json::has_children) {
            return true;
        }
        self.child_values = items.iter().map(leaf).collect();
        let length =
            4 + (items.len() - 1) * 2 + self.child_values.iter().map(String::len).sum::<usize>();
        length >= RIGHT_MARGIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reals() {
        for (f, s) in [
            (1.0 / 3.0, "0.333333333333333"),
            (1e20, "1e+20"),
            (1e15, "1e+15"),
            (1e14, "100000000000000.0"),
            (100.0, "100.0"),
            (1e-7, "0.0000001"),
            (1e-19, "0.0000000000000000001"),
            (1e-20, "1e-20"),
            (-1.5e-3, "-0.0015"),
            (5e-324, "4.94065645841247e-324"),
            (2.2250738585072014e-308, "2.2250738585072e-308"),
            (0.30000000000000004, "0.3"),
            (0.9999999999999999, "1.0"),
            (-0.0, "-0.0"),
            (1.234_567_890_123_457e-5, "0.0000123456789012346"),
        ] {
            assert_eq!(real(f), s, "{f:e}");
        }
        assert_eq!(Json::Real(0.1).as_string().unwrap(), "0.10000000000000001");
        assert_eq!(Json::Real(1.0).as_string().unwrap(), "1.0");
    }

    #[test]
    fn layouts() {
        let doc = super::super::reader::parse(
            "{\"a\":[1,{\"c\":null}],\"b\":{},\"d\":[1,2]}",
            super::super::reader::STRICT,
        )
        .unwrap();
        assert_eq!(
            compact(&doc),
            "{\"a\":[1,{\"c\":null}],\"b\":{},\"d\":[1,2]}"
        );
        assert_eq!(
            formatted(&doc),
            "{\n\t\"a\" : \n\t[\n\t\t1,\n\t\t{\n\t\t\t\"c\" : null\n\t\t}\n\t],\n\t\"b\" : {},\n\t\"d\" : [ 1, 2 ]\n}"
        );
        let long = Json::Array((0..30).map(Json::Int).collect());
        assert!(formatted(&long).starts_with("[\n\t0,\n\t1,"));
        assert_eq!(quoted("q\"\\\u{1}\u{7f}é"), "\"q\\\"\\\\\\u0001\u{7f}é\"");
    }
}
