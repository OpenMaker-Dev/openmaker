//! The JSON reader: jsoncpp's `OurReader` as FileMaker uses it (spec
//! `calc-functions.md` 5.5 and the JSON oracle answers in 9), reproduced
//! from black-box runs: the same tokens, the same error messages at the
//! same line and column (bytes of the UTF-8 text, lines ending at CR, LF or
//! CRLF), and the same recovery, which skips to the next `]` or `}` after
//! an error so that later errors (`Extra non-whitespace after JSON value.`)
//! are still reported.
//!
//! The JSON arguments of the functions are read in strict mode ([`STRICT`]):
//! no comments before a value, no trailing commas, an object or array at
//! the root, nothing after it, no duplicate keys. Values given as
//! `JSONObject`, `JSONArray` or `JSONRaw` are read with the defaults
//! ([`LENIENT`]): comments, trailing commas, any root, duplicate keys (the
//! last one wins), anything after the value ignored.
//!
//! Values nest at most [`STACK_LIMIT`] deep, jsoncpp's limit. jsoncpp
//! recurses once per level; this reader keeps the open objects and arrays
//! on a heap stack instead, so reading a deep document takes no more thread
//! stack than a flat one, from any depth of the calculation that calls it.

use super::value::Json;
use std::collections::BTreeMap;

/// How deep values may nest (jsoncpp's `stackLimit`), counted as
/// [`Json::depth`] counts. The JSON functions build nothing deeper either
/// (`path::set`, `JSONMakeArray`), so that every value can be read back.
pub const STACK_LIMIT: usize = 1000;

/// The reader's message for nesting beyond [`STACK_LIMIT`]; jsoncpp throws
/// it past every error collected before.
pub const STACK_LIMIT_MESSAGE: &str = "Exceeded stackLimit in readValue().\n";

#[derive(Clone, Copy)]
pub struct Features {
    comments: bool,
    trailing_commas: bool,
    strict_root: bool,
    fail_if_extra: bool,
    reject_dup_keys: bool,
}

pub const STRICT: Features = Features {
    comments: false,
    trailing_commas: false,
    strict_root: true,
    fail_if_extra: true,
    reject_dup_keys: true,
};

pub const LENIENT: Features = Features {
    comments: true,
    trailing_commas: true,
    strict_root: false,
    fail_if_extra: false,
    reject_dup_keys: false,
};

/// Reads `text`; the error is the reader's message, lines separated by LF
/// and ending with one (`* Line 1, Column 1\n  Syntax error: ...\n`).
pub fn parse(text: &str, features: Features) -> Result<Json, String> {
    let mut doc = text.as_bytes();
    // A UTF-8 byte order mark is skipped; positions count after it.
    if doc.starts_with(b"\xEF\xBB\xBF") {
        doc = &doc[3..];
    }
    let mut reader = Reader {
        doc,
        pos: 0,
        errors: Vec::new(),
        features,
    };
    let Ok((root, ok)) = reader.read_value() else {
        return Err(STACK_LIMIT_MESSAGE.to_string());
    };
    let token = reader.skip_comment_tokens();
    if features.fail_if_extra && token.kind != Kind::EndOfStream {
        reader.error("Extra non-whitespace after JSON value.", token.start, None);
        return Err(reader.messages());
    }
    if features.strict_root && !matches!(root, Json::Array(_) | Json::Object(_)) {
        reader.error(
            "A valid JSON document must be either an array or an object value.",
            0,
            None,
        );
        return Err(reader.messages());
    }
    if ok {
        Ok(root)
    } else {
        Err(reader.messages())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    EndOfStream,
    ObjectBegin,
    ObjectEnd,
    ArrayBegin,
    ArrayEnd,
    String,
    Number,
    True,
    False,
    Null,
    ArraySeparator,
    MemberSeparator,
    Comment,
    Error,
}

#[derive(Clone, Copy, Debug)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}

struct ReadError {
    at: usize,
    message: String,
    extra: Option<usize>,
}

/// Nesting beyond [`STACK_LIMIT`].
struct StackLimit;

/// An object or array being read: what it holds so far, and for an object
/// the name of the member read last.
enum Open {
    Object(BTreeMap<String, Json>, String),
    Array(Vec<Json>),
}

impl Open {
    fn into_json(self) -> Json {
        match self {
            Open::Object(members, _) => Json::Object(members),
            Open::Array(items) => Json::Array(items),
        }
    }
}

/// Where reading an object or array stopped: at a child value to read, or
/// at its end (whether it was read without error).
enum Step {
    Child,
    Done(bool),
}

struct Reader<'a> {
    doc: &'a [u8],
    pos: usize,
    errors: Vec<ReadError>,
    features: Features,
}

impl Reader<'_> {
    fn error(&mut self, message: impl Into<String>, at: usize, extra: Option<usize>) {
        self.errors.push(ReadError {
            at,
            message: message.into(),
            extra,
        });
    }

    /// `* Line L, Column C` / `  message` / `See Line L, Column C for
    /// detail.` for each error.
    fn messages(&self) -> String {
        let mut out = String::new();
        for e in &self.errors {
            out.push_str(&format!("* {}\n  {}\n", self.location(e.at), e.message));
            if let Some(extra) = e.extra {
                out.push_str(&format!("See {} for detail.\n", self.location(extra)));
            }
        }
        out
    }

    fn location(&self, at: usize) -> String {
        let (mut line, mut line_start, mut i) = (1, 0, 0);
        while i < at && i < self.doc.len() {
            let c = self.doc[i];
            i += 1;
            if c == b'\r' {
                if self.doc.get(i) == Some(&b'\n') {
                    i += 1;
                }
                line_start = i;
                line += 1;
            } else if c == b'\n' {
                line_start = i;
                line += 1;
            }
        }
        format!("Line {line}, Column {}", at.saturating_sub(line_start) + 1)
    }

    /// The next byte; 0 at the end (and for a NUL byte, which therefore
    /// ends the document as jsoncpp's reader does).
    fn next_byte(&mut self) -> u8 {
        match self.doc.get(self.pos) {
            Some(&c) => {
                self.pos += 1;
                c
            }
            None => 0,
        }
    }

    fn skip_spaces(&mut self) {
        while matches!(self.doc.get(self.pos), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    /// The next token and whether it is well formed.
    fn read_token(&mut self) -> (bool, Token) {
        self.skip_spaces();
        let start = self.pos;
        let c = self.next_byte();
        let (kind, ok) = match c {
            b'{' => (Kind::ObjectBegin, true),
            b'}' => (Kind::ObjectEnd, true),
            b'[' => (Kind::ArrayBegin, true),
            b']' => (Kind::ArrayEnd, true),
            b'"' => (Kind::String, self.read_string()),
            b'/' => (Kind::Comment, self.read_comment()),
            b'0'..=b'9' => {
                self.read_number(false);
                (Kind::Number, true)
            }
            // `-Infinity` and `+Infinity` are not allowed.
            b'-' | b'+' => (Kind::Number, self.read_number(true)),
            b't' => (Kind::True, self.matches(b"rue")),
            b'f' => (Kind::False, self.matches(b"alse")),
            b'n' => (Kind::Null, self.matches(b"ull")),
            b',' => (Kind::ArraySeparator, true),
            b':' => (Kind::MemberSeparator, true),
            0 => (Kind::EndOfStream, true),
            _ => (Kind::Error, false),
        };
        let kind = if ok { kind } else { Kind::Error };
        (
            ok,
            Token {
                kind,
                start,
                end: self.pos,
            },
        )
    }

    fn matches(&mut self, pattern: &[u8]) -> bool {
        if self.doc[self.pos..].starts_with(pattern) {
            self.pos += pattern.len();
            true
        } else {
            false
        }
    }

    fn read_string(&mut self) -> bool {
        let mut c = 0;
        while self.pos < self.doc.len() {
            c = self.next_byte();
            if c == b'\\' {
                self.next_byte();
            } else if c == b'"' {
                break;
            }
        }
        c == b'"'
    }

    fn read_comment(&mut self) -> bool {
        match self.next_byte() {
            b'*' => {
                while self.pos + 1 < self.doc.len() {
                    let c = self.next_byte();
                    if c == b'*' && self.doc[self.pos] == b'/' {
                        break;
                    }
                }
                self.next_byte() == b'/'
            }
            b'/' => {
                while self.pos < self.doc.len() {
                    let c = self.next_byte();
                    if c == b'\n' {
                        break;
                    }
                    if c == b'\r' {
                        if self.doc.get(self.pos) == Some(&b'\n') {
                            self.pos += 1;
                        }
                        break;
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// The rest of a number token after its first character: digits, then
    /// `.` and digits, then `e`, a sign and digits, each part optional.
    fn read_number(&mut self, check_inf: bool) -> bool {
        if check_inf && self.doc.get(self.pos) == Some(&b'I') {
            self.pos = self.doc.len();
            return false;
        }
        let digits = |r: &mut Self| {
            while matches!(r.doc.get(r.pos), Some(b'0'..=b'9')) {
                r.pos += 1;
            }
        };
        digits(self);
        if self.doc.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            digits(self);
        }
        if matches!(self.doc.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.doc.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            digits(self);
        }
        true
    }

    fn skip_comment_tokens(&mut self) -> Token {
        let (_, mut token) = self.read_token();
        if self.features.comments {
            while token.kind == Kind::Comment {
                token = self.read_token().1;
            }
        }
        token
    }

    /// Skips tokens up to and including `until` (or the end); the error
    /// already recorded stands.
    fn recover(&mut self, until: Kind) -> bool {
        loop {
            let (_, token) = self.read_token();
            if token.kind == until || token.kind == Kind::EndOfStream {
                return false;
            }
        }
    }

    /// A value and whether it was read without error; a failed value is
    /// whatever was built before the error. jsoncpp's `readValue`,
    /// `readObject` and `readArray` call each other for every child value;
    /// here a loop does, with the open objects and arrays on `open`.
    fn read_value(&mut self) -> Result<(Json, bool), StackLimit> {
        let mut open: Vec<Open> = Vec::new();
        loop {
            // A value one level below the open objects and arrays: a
            // scalar whole, or the start of an object or array.
            if open.len() >= STACK_LIMIT {
                return Err(StackLimit);
            }
            let token = self.skip_comment_tokens();
            let mut step = match token.kind {
                Kind::ObjectBegin | Kind::ArrayBegin => {
                    let mut started = if token.kind == Kind::ObjectBegin {
                        Open::Object(BTreeMap::new(), String::new())
                    } else {
                        Open::Array(Vec::new())
                    };
                    let step = self.next_child(&mut started);
                    open.push(started);
                    step
                }
                _ => {
                    let (value, ok) = self.read_scalar(token);
                    match open.last_mut() {
                        Some(parent) => self.add_child(parent, value, ok),
                        None => return Ok((value, ok)),
                    }
                }
            };
            // Each object or array that ended is a child of the one before.
            while let Step::Done(ok) = step {
                let value = open
                    .pop()
                    .expect("a step belongs to an open value")
                    .into_json();
                step = match open.last_mut() {
                    Some(parent) => self.add_child(parent, value, ok),
                    None => return Ok((value, ok)),
                };
            }
        }
    }

    /// A value that is not an object or array, from its token.
    fn read_scalar(&mut self, token: Token) -> (Json, bool) {
        match token.kind {
            Kind::Number => match self.decode_number(token) {
                Some(n) => (n, true),
                None => (Json::Null, false),
            },
            Kind::String => match self.decode_string(token) {
                Some(s) => (Json::String(s), true),
                None => (Json::Null, false),
            },
            Kind::True => (Json::Bool(true), true),
            Kind::False => (Json::Bool(false), true),
            Kind::Null => (Json::Null, true),
            _ => {
                self.error(
                    "Syntax error: value, object or array expected.",
                    token.start,
                    None,
                );
                (Json::Null, false)
            }
        }
    }

    /// Reads on in an object or array up to its next child value or its
    /// end: an object's member name and `:`, or an array's `]`.
    fn next_child(&mut self, open: &mut Open) -> Step {
        match open {
            Open::Object(members, name) => self.next_member(members, name),
            Open::Array(items) => {
                self.skip_spaces();
                if self.doc.get(self.pos) == Some(&b']')
                    && (items.is_empty() || self.features.trailing_commas)
                {
                    self.read_token();
                    return Step::Done(true);
                }
                Step::Child
            }
        }
    }

    fn next_member(&mut self, members: &BTreeMap<String, Json>, name: &mut String) -> Step {
        let (ok, mut token) = self.read_token();
        if ok {
            let mut ok = true;
            while token.kind == Kind::Comment && ok {
                (ok, token) = self.read_token();
            }
            // `{}`, or a trailing comma (jsoncpp also lets one through
            // after an empty key: `{"":1,}`).
            if ok
                && token.kind == Kind::ObjectEnd
                && (name.is_empty() || self.features.trailing_commas)
            {
                return Step::Done(true);
            }
            if ok && token.kind == Kind::String {
                match self.decode_string(token) {
                    Some(s) => *name = s,
                    None => return Step::Done(self.recover(Kind::ObjectEnd)),
                }
                if self.features.reject_dup_keys && members.contains_key(name.as_str()) {
                    self.error(format!("Duplicate key: '{name}'"), token.start, None);
                    return Step::Done(self.recover(Kind::ObjectEnd));
                }
                let (ok, colon) = self.read_token();
                if !ok || colon.kind != Kind::MemberSeparator {
                    self.error("Missing ':' after object member name", colon.start, None);
                    return Step::Done(self.recover(Kind::ObjectEnd));
                }
                return Step::Child;
            }
        }
        self.error("Missing '}' or object member name", token.start, None);
        Step::Done(self.recover(Kind::ObjectEnd))
    }

    /// Adds a child value just read to its object or array and reads on to
    /// the next child or the end.
    fn add_child(&mut self, open: &mut Open, value: Json, ok: bool) -> Step {
        match open {
            Open::Object(members, name) => {
                members.insert(name.clone(), value);
                if !ok {
                    return Step::Done(self.recover(Kind::ObjectEnd));
                }
                let (ok, mut comma) = self.read_token();
                if !ok
                    || !matches!(
                        comma.kind,
                        Kind::ObjectEnd | Kind::ArraySeparator | Kind::Comment
                    )
                {
                    self.error(
                        "Missing ',' or '}' in object declaration",
                        comma.start,
                        None,
                    );
                    return Step::Done(self.recover(Kind::ObjectEnd));
                }
                let mut ok = true;
                while comma.kind == Kind::Comment && ok {
                    (ok, comma) = self.read_token();
                }
                if comma.kind == Kind::ObjectEnd {
                    return Step::Done(true);
                }
                self.next_member(members, name)
            }
            Open::Array(items) => {
                items.push(value);
                if !ok {
                    return Step::Done(self.recover(Kind::ArrayEnd));
                }
                // A comment may follow an element.
                let (mut ok, mut token) = self.read_token();
                while token.kind == Kind::Comment && ok {
                    (ok, token) = self.read_token();
                }
                if !ok || !matches!(token.kind, Kind::ArraySeparator | Kind::ArrayEnd) {
                    self.error("Missing ',' or ']' in array declaration", token.start, None);
                    return Step::Done(self.recover(Kind::ArrayEnd));
                }
                if token.kind == Kind::ArrayEnd {
                    return Step::Done(true);
                }
                self.next_child(open)
            }
        }
    }

    /// A number token: an integer when it has only digits and fits 64 bits
    /// (signed when negative), else a double; a double out of range is not
    /// a number (`1e400`).
    fn decode_number(&mut self, token: Token) -> Option<Json> {
        let text = std::str::from_utf8(&self.doc[token.start..token.end]).unwrap_or("");
        let real = text
            .bytes()
            .enumerate()
            .any(|(i, c)| matches!(c, b'.' | b'e' | b'E' | b'+') || (c == b'-' && i > 0));
        if !real {
            let (negative, digits) = match text.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, text),
            };
            let mut value: u64 = 0;
            let mut fits = true;
            for c in digits.bytes() {
                match value
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(u64::from(c - b'0')))
                {
                    Some(v) => value = v,
                    None => fits = false,
                }
            }
            if negative && value > 1 << 63 {
                fits = false;
            }
            if fits {
                return Some(if negative {
                    Json::Int((value as i64).wrapping_neg())
                } else if value <= i64::MAX as u64 {
                    Json::Int(value as i64)
                } else {
                    Json::UInt(value)
                });
            }
        }
        match text.parse::<f64>() {
            Ok(f) if f.is_finite() => Some(Json::Real(f)),
            _ => {
                self.error(format!("'{text}' is not a number."), token.start, None);
                None
            }
        }
    }

    /// A string token's text, escapes decoded; `None` after recording a bad
    /// escape. An unpaired surrogate escape becomes the three U+FFFD that
    /// FileMaker shows for its UTF-8 bytes.
    fn decode_string(&mut self, token: Token) -> Option<String> {
        let mut out = Vec::with_capacity(token.end - token.start);
        let end = token.end - 1;
        let mut i = token.start + 1;
        while i < end {
            let c = self.doc[i];
            i += 1;
            if c == b'"' {
                break;
            }
            if c != b'\\' {
                out.push(c);
                continue;
            }
            if i == end {
                self.error("Empty escape sequence in string", token.start, Some(i));
                return None;
            }
            let escape = self.doc[i];
            i += 1;
            match escape {
                b'"' => out.push(b'"'),
                b'/' => out.push(b'/'),
                b'\\' => out.push(b'\\'),
                b'b' => out.push(8),
                b'f' => out.push(12),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'u' => {
                    let code = self.decode_code_point(token, &mut i, end)?;
                    match char::from_u32(code) {
                        Some(ch) => out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes()),
                        None => out.extend_from_slice("\u{FFFD}\u{FFFD}\u{FFFD}".as_bytes()),
                    }
                }
                _ => {
                    self.error("Bad escape sequence in string", token.start, Some(i));
                    return None;
                }
            }
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }

    fn decode_code_point(&mut self, token: Token, i: &mut usize, end: usize) -> Option<u32> {
        let unit = self.decode_unit(token, i, end)?;
        if !(0xD800..=0xDBFF).contains(&unit) {
            return Some(unit);
        }
        if end - *i < 6 {
            self.error(
                "additional six characters expected to parse unicode surrogate pair.",
                token.start,
                Some(*i),
            );
            return None;
        }
        let backslash = self.doc[*i] == b'\\';
        *i += 1;
        if backslash {
            let u = self.doc[*i] == b'u';
            *i += 1;
            if u {
                let low = self.decode_unit(token, i, end)?;
                return Some(0x10000 + ((unit & 0x3FF) << 10) + (low & 0x3FF));
            }
        }
        self.error(
            "expecting another \\u token to begin the second half of a unicode surrogate pair",
            token.start,
            Some(*i),
        );
        None
    }

    fn decode_unit(&mut self, token: Token, i: &mut usize, end: usize) -> Option<u32> {
        if end - *i < 4 {
            self.error(
                "Bad unicode escape sequence in string: four digits expected.",
                token.start,
                Some(*i),
            );
            return None;
        }
        let mut unit = 0;
        for _ in 0..4 {
            let c = self.doc[*i];
            *i += 1;
            match char::from(c).to_digit(16) {
                Some(d) => unit = unit * 16 + d,
                None => {
                    self.error(
                        "Bad unicode escape sequence in string: hexadecimal digit expected.",
                        token.start,
                        Some(*i),
                    );
                    return None;
                }
            }
        }
        Some(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> String {
        parse(text, STRICT).unwrap_err()
    }

    #[test]
    fn strict_messages() {
        assert_eq!(
            err(""),
            "* Line 1, Column 1\n  Syntax error: value, object or array expected.\n\
             * Line 1, Column 1\n  A valid JSON document must be either an array or an object value.\n"
        );
        assert_eq!(
            err("{\"a\":1,\"a\":2}"),
            "* Line 1, Column 8\n  Duplicate key: 'a'\n"
        );
        assert_eq!(err("[1 2] x").matches("* Line").count(), 2);
        assert_eq!(err("[[1 2] x").matches("* Line").count(), 1);
        assert_eq!(
            err("[\"\\u12g4\"]"),
            "* Line 1, Column 2\n  Bad unicode escape sequence in string: hexadecimal digit expected.\n\
             See Line 1, Column 8 for detail.\n"
        );
        assert_eq!(
            err("[1,\r\n\r]"),
            "* Line 3, Column 1\n  Syntax error: value, object or array expected.\n"
        );
        assert_eq!(
            err("[\"é😀\",]"),
            "* Line 1, Column 11\n  Syntax error: value, object or array expected.\n"
        );
        assert!(err(&("[".repeat(1001) + &"]".repeat(1001))).starts_with("Exceeded stackLimit"));
        assert!(parse(&("[".repeat(1000) + &"]".repeat(1000)), STRICT).is_ok());
    }

    #[test]
    fn numbers() {
        let one = |t: &str| {
            parse(&format!("[{t}]"), STRICT)
                .unwrap()
                .into_array()
                .unwrap()
                .remove(0)
        };
        assert_eq!(one("01"), Json::Int(1));
        assert_eq!(one("-"), Json::Int(0));
        assert_eq!(one("+1"), Json::Real(1.0));
        assert_eq!(one("-9223372036854775808"), Json::Int(i64::MIN));
        assert_eq!(one("18446744073709551615"), Json::UInt(u64::MAX));
        assert_eq!(
            one("18446744073709551616"),
            Json::Real(18446744073709551616.0)
        );
        assert_eq!(
            err("[1e400]"),
            "* Line 1, Column 2\n  '1e400' is not a number.\n"
        );
    }

    #[test]
    fn lenient_values() {
        assert_eq!(parse("1 2", LENIENT).unwrap(), Json::Int(1));
        assert_eq!(
            parse("[1,]", LENIENT).unwrap(),
            Json::Array(vec![Json::Int(1)])
        );
        assert!(parse("abc", LENIENT).is_err());
        let dup = parse("{\"x\":1,\"x\":2}", LENIENT).unwrap();
        assert_eq!(dup, Json::Object([("x".to_string(), Json::Int(2))].into()));
    }
}
