//! A test-only reader and evaluator for literal expressions, shared by the
//! vector tests until the real formula parser and evaluator exist.
//!
//! It understands number and text literals, `¶`, the operators, `If`,
//! `and`/`or`/`xor`/`not`, `SetPrecision`, `EvaluationError`, the keywords
//! and functions of [`om_calc::functions`], and stand-ins for the date and
//! conversion primitives. Anything else is [`Unsupported`]. Field references,
//! variables, `Let`, `Case` and `Evaluate` are not supported.
#![allow(dead_code)]

use om_calc::functions::{self, Implementation};
use om_calc::ops::{self, CompareOp};
use om_calc::{
    CalcError, Ctx, Date, FirstError, Locale, Number, Text, Time, Timestamp, Value, ValueType,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Num(String),
    /// A digits-and-dots run the locale cannot read as a number: FileMaker
    /// parses it as a name (error 106 in the harness).
    BadNum,
    Str(String),
    Name(String),
    Op(&'static str),
}

pub struct Unsupported;

pub fn lex(src: &str, decimal: char) -> Result<Vec<Tok>, Unsupported> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let digit_at = |j: usize| chars.get(j).is_some_and(|c| c.is_ascii_digit());
        if c.is_ascii_digit() || (c == decimal && digit_at(i + 1)) {
            let start = i;
            while digit_at(i) {
                i += 1;
            }
            if chars.get(i) == Some(&decimal) {
                i += 1;
                while digit_at(i) {
                    i += 1;
                }
            }
            if matches!(chars.get(i), Some('e' | 'E'))
                && (digit_at(i + 1)
                    || (matches!(chars.get(i + 1), Some('+' | '-')) && digit_at(i + 2)))
            {
                i += 2;
                while digit_at(i) {
                    i += 1;
                }
            }
            if decimal != '.' && chars.get(i) == Some(&'.') {
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                    i += 1;
                }
                out.push(Tok::BadNum);
                continue;
            }
            out.push(Tok::Num(chars[start..i].iter().collect()));
            continue;
        }
        if c == '"' {
            let mut s = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    None => return Err(Unsupported),
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    // A Return typed inside a literal is stored as U+E004 and
                    // evaluates as a space.
                    Some('\u{e004}') => {
                        s.push(' ');
                        i += 1;
                    }
                    Some('\\') if matches!(chars.get(i + 1), Some('"' | '\\' | '¶')) => {
                        s.push(chars[i + 1]);
                        i += 2;
                    }
                    Some('¶') => {
                        s.push('\r');
                        i += 1;
                    }
                    // A Return typed inside a literal is stored as U+E004
                    // and evaluates as a space (calc-values.md 12.1).
                    Some('\r') => {
                        s.push(' ');
                        i += 1;
                    }
                    Some(&other) => {
                        s.push(other);
                        i += 1;
                    }
                }
            }
            out.push(Tok::Str(s));
            continue;
        }
        if c == '¶' {
            out.push(Tok::Str("\r".into()));
            i += 1;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Tok::Name(chars[start..i].iter().collect()));
            continue;
        }
        let two: String = chars[i..chars.len().min(i + 2)].iter().collect();
        let op = match two.as_str() {
            "<>" => Some("≠"),
            "<=" => Some("≤"),
            ">=" => Some("≥"),
            "==" => return Err(Unsupported),
            _ => None,
        };
        if let Some(op) = op {
            out.push(Tok::Op(op));
            i += 2;
            continue;
        }
        let op = match c {
            '+' => "+",
            '-' => "-",
            '*' => "*",
            '/' => "/",
            '^' => "^",
            '&' => "&",
            '=' => "=",
            '≠' => "≠",
            '<' => "<",
            '≤' => "≤",
            '>' => ">",
            '≥' => "≥",
            '(' => "(",
            ')' => ")",
            '[' => "[",
            ']' => "]",
            ';' => ";",
            _ => return Err(Unsupported),
        };
        out.push(Tok::Op(op));
        i += 1;
    }
    Ok(out)
}

#[derive(Debug)]
pub enum Expr {
    Num(String),
    BadNum,
    Str(String),
    Call(String, Vec<Expr>),
    /// `[ a ; b ]`, a list argument (`Substitute`'s pairs); the evaluator
    /// passes its items as consecutive arguments.
    List(Vec<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn eat_op(&mut self, ops: &[&'static str]) -> Option<&'static str> {
        if let Some(Tok::Op(op)) = self.peek() {
            if let Some(found) = ops.iter().find(|o| *o == op) {
                self.pos += 1;
                return Some(found);
            }
        }
        None
    }

    fn eat_word(&mut self, words: &[&'static str]) -> Option<&'static str> {
        if let Some(Tok::Name(name)) = self.peek() {
            if let Some(found) = words.iter().find(|w| w.eq_ignore_ascii_case(name)) {
                self.pos += 1;
                return Some(found);
            }
        }
        None
    }

    fn or(&mut self) -> Result<Expr, Unsupported> {
        let mut left = self.and()?;
        while let Some(op) = self.eat_word(&["or", "xor"]) {
            let right = self.and()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, Unsupported> {
        let mut left = self.not()?;
        while self.eat_word(&["and"]).is_some() {
            let right = self.not()?;
            left = Expr::Bin("and", Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, Unsupported> {
        if self.eat_word(&["not"]).is_some() {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr, Unsupported> {
        let mut left = self.concat()?;
        while let Some(op) = self.eat_op(&["=", "≠", "<", "≤", ">", "≥"]) {
            let right = self.concat()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn concat(&mut self) -> Result<Expr, Unsupported> {
        let mut left = self.additive()?;
        while self.eat_op(&["&"]).is_some() {
            let right = self.additive()?;
            left = Expr::Bin("&", Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, Unsupported> {
        let mut left = self.multiplicative()?;
        while let Some(op) = self.eat_op(&["+", "-"]) {
            let right = self.multiplicative()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, Unsupported> {
        let mut left = self.power()?;
        while let Some(op) = self.eat_op(&["*", "/"]) {
            let right = self.power()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn power(&mut self) -> Result<Expr, Unsupported> {
        let mut left = self.unary()?;
        while self.eat_op(&["^"]).is_some() {
            let right = self.unary()?;
            left = Expr::Bin("^", Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, Unsupported> {
        if self.eat_op(&["-"]).is_some() {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op(&["+"]).is_some() {
            return self.unary();
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, Unsupported> {
        match self.next().ok_or(Unsupported)? {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::BadNum => Ok(Expr::BadNum),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Op("[") => {
                let mut items = Vec::new();
                loop {
                    items.push(self.or()?);
                    match self.next() {
                        Some(Tok::Op(";")) => continue,
                        Some(Tok::Op("]")) => break,
                        _ => return Err(Unsupported),
                    }
                }
                Ok(Expr::List(items))
            }
            Tok::Op("(") => {
                let e = self.or()?;
                match self.next() {
                    Some(Tok::Op(")")) => Ok(e),
                    _ => Err(Unsupported),
                }
            }
            Tok::Name(name) => {
                let mut args = Vec::new();
                if self.eat_op(&["("]).is_some() {
                    loop {
                        args.push(self.or()?);
                        match self.next() {
                            Some(Tok::Op(";")) => continue,
                            Some(Tok::Op(")")) => break,
                            _ => return Err(Unsupported),
                        }
                    }
                }
                Ok(Expr::Call(name, args))
            }
            Tok::Op(_) => Err(Unsupported),
        }
    }
}

pub fn parse(src: &str, decimal: char) -> Result<Expr, Unsupported> {
    let mut p = Parser {
        toks: lex(src, decimal)?,
        pos: 0,
    };
    let e = p.or()?;
    if p.pos != p.toks.len() {
        return Err(Unsupported);
    }
    Ok(e)
}

pub fn checked(r: Result<Value, CalcError>, errors: &mut FirstError) -> Value {
    errors.check(r).unwrap_or_else(Value::invalid)
}

/// GetAsDate and friends: empty text comes back unchanged, blank text
/// gives the typed empty value.
fn get_as<T>(
    v: Value,
    ty: ValueType,
    convert: impl FnOnce(&Value) -> Result<Option<T>, CalcError>,
    wrap: impl FnOnce(T) -> Value,
    errors: &mut FirstError,
) -> Value {
    if matches!(&v, Value::Text(t) if t.is_empty()) {
        return v;
    }
    match convert(&v) {
        Ok(Some(x)) => wrap(x),
        Ok(None) => Value::empty(ty),
        Err(e) => {
            errors.record(e);
            Value::invalid()
        }
    }
}

pub enum Stop {
    Unsupported,
    /// A name that does not resolve (error 106) makes the whole result `?`.
    Abort,
}

impl From<Unsupported> for Stop {
    fn from(_: Unsupported) -> Stop {
        Stop::Unsupported
    }
}

pub fn eval(e: &Expr, cx: &Ctx, errors: &mut FirstError) -> Result<Value, Stop> {
    Ok(match e {
        Expr::Num(n) => checked(
            Number::from_literal(n, cx.locale.decimal_separator).map(Value::Number),
            errors,
        ),
        Expr::BadNum => {
            errors.record(CalcError::TABLE_MISSING);
            return Err(Stop::Abort);
        }
        Expr::Str(s) => Value::Text(Text::plain(s.clone())),
        Expr::Neg(x) => ops::negate(&eval(x, cx, errors)?, cx),
        Expr::Not(x) => Value::boolean(!eval(x, cx, errors)?.is_true(cx)),
        Expr::Bin(op, a, b) => {
            let a = eval(a, cx, errors)?;
            match *op {
                "and" => {
                    if !a.is_true(cx) {
                        return Ok(Value::boolean(false));
                    }
                    return Ok(Value::boolean(eval(b, cx, errors)?.is_true(cx)));
                }
                "or" => {
                    if a.is_true(cx) {
                        return Ok(Value::boolean(true));
                    }
                    return Ok(Value::boolean(eval(b, cx, errors)?.is_true(cx)));
                }
                _ => {}
            }
            let b = eval(b, cx, errors)?;
            match *op {
                "xor" => Value::boolean(a.is_true(cx) != b.is_true(cx)),
                "+" => checked(ops::add(&a, &b, cx), errors),
                "-" => checked(ops::subtract(&a, &b, cx), errors),
                "*" => ops::multiply(&a, &b, cx),
                "/" => checked(ops::divide(&a, &b, cx), errors),
                "^" => checked(ops::power(&a, &b, cx), errors),
                "&" => ops::concat(&a, &b, cx),
                cmp => {
                    let op = match cmp {
                        "=" => CompareOp::Eq,
                        "≠" => CompareOp::Ne,
                        "<" => CompareOp::Lt,
                        "≤" => CompareOp::Le,
                        ">" => CompareOp::Gt,
                        _ => CompareOp::Ge,
                    };
                    ops::compare_op(&a, op, &b, cx)
                }
            }
        }
        Expr::Call(name, args) => call(name, args, cx, errors)?,
        Expr::List(_) => return Err(Stop::Unsupported),
    })
}

fn call(name: &str, args: &[Expr], cx: &Ctx, errors: &mut FirstError) -> Result<Value, Stop> {
    let name = name.to_ascii_lowercase();
    // Functions that control evaluation.
    match (name.as_str(), args) {
        ("true", []) => return Ok(Value::boolean(true)),
        ("false", []) => return Ok(Value::boolean(false)),
        ("if", [c, t, rest @ ..]) if rest.len() <= 1 => {
            return if eval(c, cx, errors)?.is_true(cx) {
                eval(t, cx, errors)
            } else {
                match rest.first() {
                    Some(f) => eval(f, cx, errors),
                    None => Ok(Value::empty_text()),
                }
            };
        }
        ("setprecision", [x, n]) => {
            let n = eval(n, cx, errors)?.to_number(cx);
            let p = om_calc::number::clamp_user_precision(n.to_i64().unwrap_or(16));
            let inner = cx.with_precision(p);
            return Ok(match eval(x, &inner, errors)? {
                Value::Number(n) => Value::Number(n.round_to_precision(p)),
                other => other,
            });
        }
        ("evaluationerror", [x]) => {
            let mut inner = FirstError::default();
            match eval(x, cx, &mut inner) {
                Ok(_) | Err(Stop::Abort) => {}
                Err(Stop::Unsupported) => return Err(Stop::Unsupported),
            }
            return Ok(Value::number(i64::from(inner.code())));
        }
        _ => {}
    }
    if let Some(k) = functions::keyword(&name) {
        if args.is_empty() {
            return Ok(Value::number(k));
        }
    }
    // An aggregate's only argument, and NPV's payments, must be a field:
    // the parser rejects anything else with 1214 (`Sum(5)`, `NPV(100;.1)`),
    // and the reader has no fields.
    if let Some(f) = functions::lookup(&name) {
        if matches!(f.implementation, Implementation::Aggregate(_))
            && (args.len() == 1 || name == "npv")
        {
            errors.record(CalcError::FIELD_REQUIRED);
            return Err(Stop::Abort);
        }
    }
    let mut values = Vec::with_capacity(args.len());
    for a in args {
        if let Expr::List(items) = a {
            for item in items {
                values.push(eval(item, cx, errors)?);
            }
            continue;
        }
        values.push(eval(a, cx, errors)?);
    }
    // Library functions first; the primitives below stand in for the ones
    // not registered yet.
    if let Some(f) = functions::lookup(&name) {
        // The parser rejects a wrong argument count (1202, 1204) before any
        // function runs; the reader leaves such rows out.
        if !arity_ok(&name, values.len()) {
            return Err(Stop::Unsupported);
        }
        let r = match f.implementation {
            Implementation::Pure(f) => f(&values, cx),
            Implementation::Aggregate(f) => {
                let lists: Vec<Vec<Value>> = values.into_iter().map(|v| vec![v]).collect();
                f(&lists, cx)
            }
        };
        return Ok(checked(r, errors));
    }
    let num = |v: &Value| v.numeric_operand(cx);
    Ok(match (name.as_str(), values.as_slice()) {
        ("getasnumber", [x]) => match x {
            Value::Text(t) if t.is_empty() => x.clone(),
            _ => Value::Number(x.to_number(cx)),
        },
        ("getastext", [x]) => Value::Text(x.to_text(cx)),
        ("isempty", [x]) => Value::boolean(x.is_empty()),
        ("length", [x]) => Value::number(x.to_text(cx).length() as i64),
        ("getasdate", [x]) => get_as(
            x.clone(),
            ValueType::Date,
            |v| v.to_date(cx),
            Value::Date,
            errors,
        ),
        ("getastime", [x]) => get_as(
            x.clone(),
            ValueType::Time,
            |v| v.to_time(cx),
            Value::Time,
            errors,
        ),
        ("getastimestamp", [x]) => get_as(
            x.clone(),
            ValueType::Timestamp,
            |v| v.to_timestamp(cx),
            Value::Timestamp,
            errors,
        ),
        ("date", [m, d, y]) => {
            let parts: Vec<Number> = [m, d, y].iter().map(|v| num(v)).collect();
            let ints: Option<Vec<i64>> = parts
                .iter()
                .map(|n| if n.is_empty() { None } else { n.int().to_i64() })
                .collect();
            let date = ints.and_then(|i| Date::normalized(i[0], i[1], i[2]));
            checked(date.map(Value::Date).ok_or(CalcError::INVALID_DATE), errors)
        }
        ("time", [h, m, s]) => checked(
            Time::from_hms(&num(h), &num(m), &num(s))
                .map(Value::Time)
                .ok_or(CalcError::INVALID_TIME),
            errors,
        ),
        ("timestamp", [d, t]) => {
            if t.is_empty() {
                Value::empty(ValueType::Timestamp)
            } else {
                let date = match d.to_date(cx) {
                    Ok(Some(date)) => Ok(date),
                    Ok(None) => Err(CalcError::INVALID_DATE),
                    Err(e) => Err(e),
                };
                let ts = date.and_then(|date| {
                    let time = t.to_time(cx)?.ok_or(CalcError::INVALID_TIME)?;
                    Timestamp::from_date_time(date, time).ok_or(CalcError::INVALID_DATE)
                });
                checked(ts.map(Value::Timestamp), errors)
            }
        }
        ("year" | "month" | "day" | "dayofweek", [x]) => match x.to_date(cx) {
            Ok(Some(d)) => {
                let (y, m, day) = d.ymd();
                Value::number(match name.as_str() {
                    "year" => y,
                    "month" => i64::from(m),
                    "day" => i64::from(day),
                    _ => i64::from(d.weekday()),
                })
            }
            Ok(None) => Value::empty_text(),
            Err(e) => {
                errors.record(e);
                Value::invalid()
            }
        },
        _ => return Err(Stop::Unsupported),
    })
}

/// Whether `calc-tokens.csv` allows `count` arguments for the function
/// named `name` (lower case); a variable-argument function with a
/// `max_args` of 0 takes any number.
fn arity_ok(name: &str, count: usize) -> bool {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static ARITY: OnceLock<HashMap<String, (usize, Option<usize>)>> = OnceLock::new();
    let table = ARITY.get_or_init(|| {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/re/catalogs/calc-tokens.csv"
        );
        let csv = std::fs::read_to_string(path).expect("calc-tokens.csv");
        csv.lines()
            .skip(1)
            .filter_map(|line| {
                let cols: Vec<&str> = line.splitn(11, ',').collect();
                let min = cols.get(8)?.parse().ok()?;
                let max: usize = cols.get(9)?.parse().ok()?;
                let unlimited = cols[6] == "VarArgs" && max == 0;
                Some((
                    cols[3].to_ascii_lowercase(),
                    (min, (!unlimited).then_some(max)),
                ))
            })
            .collect()
    });
    match table.get(name) {
        Some(&(min, max)) => count >= min && max.map_or(true, |max| count <= max),
        None => true,
    }
}

/// Decodes the harness escapes `\r`, `\n`, `\t`, `\\` and, as `calceval`
/// reads and writes them, `\uXXXX` (control and private-use characters);
/// any other backslash stays.
pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let hex: String = chars.clone().skip(1).take(4).collect();
            let unit = chars
                .clone()
                .next()
                .filter(|&u| u == 'u' && hex.len() == 4)
                .and_then(|_| u32::from_str_radix(&hex, 16).ok())
                .and_then(|u| {
                    // A lone surrogate is held as its stand-in.
                    char::from_u32(u).or_else(|| om_calc::text::lone_surrogate(u as u16))
                });
            if let Some(decoded) = unit {
                out.push(decoded);
                chars.nth(4);
                continue;
            }
            match chars.next() {
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn type_name(v: &Value) -> &'static str {
    match v.value_type() {
        ValueType::Text => "text",
        ValueType::Number => "number",
        ValueType::Date => "date",
        ValueType::Time => "time",
        ValueType::Timestamp => "timestamp",
        ValueType::Container => "container",
    }
}

/// Evaluates `expr` in a file of `locale` on 10/28/2026, returning the value
/// and the evaluation's error code, or `None` when the reader cannot express
/// it.
pub fn run(expr: &str, locale: &Locale) -> Option<(Value, i32)> {
    let today = Date::from_ymd(2026, 10, 28).unwrap();
    let cx = Ctx::new(locale, today);
    let ast = parse(expr, locale.decimal_separator).ok()?;
    let mut errors = FirstError::default();
    let value = match eval(&ast, &cx, &mut errors) {
        Ok(value) => value,
        Err(Stop::Abort) => Value::invalid(),
        Err(Stop::Unsupported) => return None,
    };
    Some((value, errors.code()))
}

/// The result text of `value` in `locale`.
pub fn text(value: &Value, locale: &Locale) -> String {
    let today = Date::from_ymd(2026, 10, 28).unwrap();
    value.to_text(&Ctx::new(locale, today)).into_string()
}

/// Checks a `tools/fms-calc-harness` result file (expression, error,
/// types, result text) evaluated in a file of `locale`: every row the
/// reader can express must match the result text and the error code.
/// Returns (rows, covered, failures).
pub fn check_harness_results(path: &str, locale: &Locale) -> (usize, usize, Vec<String>) {
    let tsv = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (mut rows, mut covered, mut failures) = (0, 0, Vec::new());
    for line in tsv.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.splitn(4, '\t').collect();
        let [expr, error, _types, expected] = cols[..] else {
            continue;
        };
        rows += 1;
        let expr = unescape(expr);
        let Some((value, code)) = run(&expr, locale) else {
            continue;
        };
        covered += 1;
        let got = text(&value, locale);
        let want = unescape(expected);
        if got != want || code.to_string() != error {
            failures.push(format!(
                "{expr}: got {got:?} error {code}, want {want:?} error {error}"
            ));
        }
    }
    (rows, covered, failures)
}

/// Checks the fixture set `tests/data/<name>.<locale>.tsv` for the five
/// harness locales with [`check_harness_results`]: every failing row must be
/// listed in `known` (expression, reason), and every listed expression must
/// still fail somewhere. Prints rows, covered and passed per fixture.
pub fn check_fixture_set(name: &str, known: &[(&str, &str)]) {
    let is_known = |f: &str| {
        known
            .iter()
            .position(|(e, _)| f.starts_with(&format!("{e}: ")))
    };
    let mut unexpected = Vec::new();
    let mut seen = vec![false; known.len()];
    for tag in ["en-US", "de-DE", "fr-FR", "sv-SE", "ja-JP"] {
        let path = format!("{}/tests/data/{name}.{tag}.tsv", env!("CARGO_MANIFEST_DIR"));
        let locale = Locale::for_tag(tag).unwrap();
        let (rows, covered, failures) = check_harness_results(&path, &locale);
        let mut listed = 0;
        for f in failures.iter() {
            match is_known(f) {
                Some(i) => {
                    seen[i] = true;
                    listed += 1;
                }
                None => unexpected.push(format!("[{tag}] {f}")),
            }
        }
        println!(
            "{name}.{tag}: {rows} rows, {covered} covered, {} passed, {listed} known",
            covered - failures.len()
        );
    }
    for f in &unexpected {
        println!("FAIL {f}");
    }
    assert!(unexpected.is_empty(), "{} rows failed", unexpected.len());
    for ((e, _), seen) in known.iter().zip(seen) {
        assert!(seen, "{e} now passes or is gone: remove it from KNOWN");
    }
}
