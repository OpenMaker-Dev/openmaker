//! The embedding vector functions of the AI category that need no model:
//! `AddEmbeddings`, `SubtractEmbeddings`, `NormalizeEmbedding`,
//! `CosineSimilarity`, `GetEmbeddingAsText`, `GetEmbeddingAsFile` (spec
//! `docs/re/calc-functions.md` 5.8; behavior from FileMaker Server's engine,
//! calc-values.md 13, value-core answers). `GetEmbedding` asks a model
//! provider and `GetTokenCount` needs the `cl100k_base` tokenizer: both are
//! the host's.
//!
//! A vector is text holding a JSON array (read strictly, as the JSON
//! functions read documents) of numbers, `true` (1), `false` and `null`
//! (0), or container data: a file whose name ends in `.fve`, holding
//! `FVE.2100` and then the values as little-endian doubles. Arithmetic is
//! in doubles. A result is text when the first argument is text, `[` the
//! values `, ` `]`, each rounded to 20 significant digits and shown as
//! FileMaker shows numbers but with a leading 0 (`[0.5999999999999999778,
//! 0.80000000000000004441]`); otherwise a file named `embf_` and the first
//! five hex digits of the MD5 of its bytes (`emb_` for
//! `GetEmbeddingAsFile`), extension `.fve`. Non-finite values show as 0.

use super::json::reader::{self, STRICT};
use super::json::value::{kind, Json};
use super::{arg, Function, Keyword};
use crate::error::CalcError;
use crate::number::Number;
use crate::value::{Container, Ctx, Value};
use md5::{Digest, Md5};
use std::fmt::Write;

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("AddEmbeddings", add_embeddings),
    Function::pure("CosineSimilarity", cosine_similarity),
    Function::pure("GetEmbeddingAsFile", get_embedding_as_file),
    Function::pure("GetEmbeddingAsText", get_embedding_as_text),
    Function::pure("NormalizeEmbedding", normalize_embedding),
    Function::pure("SubtractEmbeddings", subtract_embeddings),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

type Result<T> = std::result::Result<T, CalcError>;

/// The header of an embedding file.
const MAGIC: &[u8] = b"FVE.2100";

/// 408: container data that is not an embedding file (name, size).
const NOT_EMBEDDING: CalcError = match CalcError::from_code(408) {
    Some(e) => e,
    None => unreachable!(),
};
/// 1708: container data without the `FVE.2100` header.
const BAD_HEADER: CalcError = match CalcError::from_code(1708) {
    Some(e) => e,
    None => unreachable!(),
};
/// 5: `GetEmbeddingAsFile` of text that is not a vector.
const NOT_A_VECTOR: CalcError = match CalcError::from_code(5) {
    Some(e) => e,
    None => unreachable!(),
};

/// An argument read as a vector.
enum Vector {
    /// Empty text.
    Empty,
    /// A JSON array of numbers.
    Text(Vec<f64>),
    /// An embedding file.
    File(Vec<f64>),
    /// Anything else, with the error `GetEmbeddingAsText` reports for it.
    Invalid(CalcError),
}

/// Text and container data are read as above; numbers, dates and times
/// are a vector with no values (`CosineSimilarity ( 1 ; 1 )` is 0).
fn vector(v: &Value, cx: &Ctx) -> Vector {
    match v {
        Value::Container(c) => match file_values(c) {
            Ok(values) => Vector::File(values),
            Err(e) => Vector::Invalid(e),
        },
        Value::Text(_) => {
            let text = v.to_text(cx);
            if text.is_empty() {
                Vector::Empty
            } else {
                json_values(text.as_str()).map_or(Vector::Invalid(CalcError::UNKNOWN), Vector::Text)
            }
        }
        _ => Vector::Text(Vec::new()),
    }
}

/// The values of a JSON array of numbers, booleans and nulls (not empty).
fn json_values(text: &str) -> Option<Vec<f64>> {
    let parsed = reader::parse(text, STRICT);
    let Ok(Json::Array(items)) = &parsed else {
        return None;
    };
    let values = items
        .iter()
        .map(|item| match item {
            Json::Int(i) => Some(*i as f64),
            Json::UInt(u) => Some(*u as f64),
            Json::Real(f) => Some(*f),
            Json::Bool(b) => Some(f64::from(u8::from(*b))),
            Json::Null => Some(0.0),
            _ => None,
        })
        .collect::<Option<Vec<f64>>>()?;
    (!values.is_empty()).then_some(values)
}

/// The values of an embedding file: the header first (1708), then a
/// `.fve` name and whole, non-zero number of doubles (408).
fn file_values(c: &Container) -> Result<Vec<f64>> {
    let data = c.data();
    let Some(body) = data.strip_prefix(MAGIC) else {
        return Err(BAD_HEADER);
    };
    let name = c.text.as_str();
    let fve = name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".fve");
    if !fve || body.is_empty() || body.len() % 8 != 0 {
        return Err(NOT_EMBEDDING);
    }
    Ok(body
        .chunks_exact(8)
        .map(|b| f64::from_le_bytes(b.try_into().expect("8 bytes")))
        .collect())
}

/// A double rounded to 20 significant digits; non-finite values are 0.
fn number(x: f64) -> Number {
    if !x.is_finite() {
        return Number::zero();
    }
    Number::parse_text(&format!("{x:.19e}"), '.', ',')
}

/// The text form of a vector.
fn text_form(values: &[f64], cx: &Ctx) -> Value {
    let decimal = cx.locale.decimal_separator;
    let mut out = String::from("[");
    for (i, &x) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let shown = number(x).to_text_with(decimal);
        match shown.strip_prefix('-') {
            Some(rest) if rest.starts_with(decimal) => {
                let _ = write!(out, "-0{rest}");
            }
            _ if shown.starts_with(decimal) => {
                let _ = write!(out, "0{shown}");
            }
            _ => out.push_str(&shown),
        }
    }
    out.push(']');
    Value::text(out)
}

/// An embedding file of `values` named `prefix` and its digest.
fn file_form(values: &[f64], prefix: &str, name: Option<String>) -> Value {
    let mut data = MAGIC.to_vec();
    for x in values {
        data.extend(x.to_le_bytes());
    }
    let name = name.unwrap_or_else(|| {
        let digest = Md5::digest(&data);
        format!(
            "{prefix}{:02X}{:02X}{:X}.fve",
            digest[0],
            digest[1],
            digest[2] >> 4
        )
    });
    Value::Container(Container::file(name, data))
}

/// A result vector in the form of the first argument.
fn result(values: &[f64], as_file: bool, cx: &Ctx) -> Value {
    if as_file {
        file_form(values, "embf_", None)
    } else {
        text_form(values, cx)
    }
}

/// `values` scaled to length 1; `None` for a zero length.
fn normalized(values: &[f64]) -> Option<Vec<f64>> {
    let norm = values.iter().map(|x| x * x).sum::<f64>().sqrt();
    (norm != 0.0 && !norm.is_nan()).then(|| values.iter().map(|x| x / norm).collect())
}

/// Whether a vector counts as normalized: its squared length, as a 32-bit
/// float, is at least 1 and below 1.00001 (`[1.0000049]` is, `[0.99999995]`
/// and `[1.00000499]` are not).
fn is_unit(values: &[f64]) -> bool {
    let squares = values.iter().map(|x| x * x).sum::<f64>() as f32;
    (1.0..1.00001_f32).contains(&squares)
}

/// `AddEmbeddings` and `SubtractEmbeddings`: an empty first vector gives
/// empty text and an empty second one the first argument unchanged (even
/// `"abc"`); vectors of different sizes, invalid ones and a zero result are
/// `?`.
fn combine(args: &[Value], cx: &Ctx, op: fn(f64, f64) -> f64) -> Result<Value> {
    let first = vector(&args[0], cx);
    if matches!(first, Vector::Empty) {
        return Ok(Value::empty_text());
    }
    let b = match vector(&args[1], cx) {
        Vector::Empty => return Ok(args[0].clone()),
        Vector::Text(v) | Vector::File(v) => v,
        Vector::Invalid(_) => return Ok(Value::invalid()),
    };
    let (a, as_file) = match first {
        Vector::Text(v) => (v, false),
        Vector::File(v) => (v, true),
        Vector::Empty | Vector::Invalid(_) => return Ok(Value::invalid()),
    };
    if a.len() != b.len() {
        return Ok(Value::invalid());
    }
    let sum: Vec<f64> = a.iter().zip(&b).map(|(x, y)| op(*x, *y)).collect();
    Ok(normalized(&sum).map_or_else(Value::invalid, |v| result(&v, as_file, cx)))
}

fn add_embeddings(args: &[Value], cx: &Ctx) -> Result<Value> {
    combine(args, cx, |x, y| x + y)
}

fn subtract_embeddings(args: &[Value], cx: &Ctx) -> Result<Value> {
    combine(args, cx, |x, y| x - y)
}

/// `NormalizeEmbedding ( data {; dimension} )`: the first `dimension`
/// values (the number as a 32-bit integer; 0, negative or more than the
/// size means all) scaled to length 1. A vector that already counts as
/// normalized comes back unchanged, whole (text input as the JSON it was
/// read as). Empty text gives empty text; a zero vector or invalid data `?`.
fn normalize_embedding(args: &[Value], cx: &Ctx) -> Result<Value> {
    let (values, as_file) = match vector(&args[0], cx) {
        Vector::Empty => return Ok(Value::empty_text()),
        Vector::Text(v) => (v, false),
        Vector::File(v) => (v, true),
        Vector::Invalid(_) => return Ok(Value::invalid()),
    };
    let dimension = arg(args, 1).map_or(0, |d| d.numeric_operand(cx).as_long_long() as i32);
    let used = match usize::try_from(dimension) {
        Ok(n) if n > 0 && n < values.len() => &values[..n],
        _ => &values[..],
    };
    if is_unit(used) {
        return Ok(match &args[0] {
            Value::Text(t) if !as_file => Value::Text(t.clone().with_json_state(kind::ARRAY as i8)),
            other => other.clone(),
        });
    }
    Ok(normalized(used).map_or_else(Value::invalid, |v| result(&v, as_file, cx)))
}

/// `CosineSimilarity`: the dot product of the two vectors (normalized
/// vectors give their cosine). Vectors of different sizes, empty or
/// invalid ones are `?`, and so is a product whose magnitude, rounded to
/// six decimals, exceeds 1; an infinite product is 0.
fn cosine_similarity(args: &[Value], cx: &Ctx) -> Result<Value> {
    let (Vector::Text(a) | Vector::File(a), Vector::Text(b) | Vector::File(b)) =
        (vector(&args[0], cx), vector(&args[1], cx))
    else {
        return Ok(Value::invalid());
    };
    if a.len() != b.len() {
        return Ok(Value::invalid());
    }
    let dot: f64 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
    if dot.is_nan() || (dot.is_finite() && (dot.abs() * 1e6).round() > 1e6) {
        return Ok(Value::invalid());
    }
    Ok(Value::Number(number(dot)))
}

/// `GetEmbeddingAsText`: an embedding file's values as text. Empty text
/// gives empty text, other text error -1, other container data 1708 or 408.
fn get_embedding_as_text(args: &[Value], cx: &Ctx) -> Result<Value> {
    match vector(&args[0], cx) {
        Vector::Empty => Ok(Value::empty_text()),
        Vector::File(v) => Ok(text_form(&v, cx)),
        Vector::Text(_) => Err(CalcError::UNKNOWN),
        Vector::Invalid(e) => Err(e),
    }
}

/// `GetEmbeddingAsFile ( text {; fileNameWithExtension} )`: the vector as
/// an embedding file, named as the decoders clean up names (without their
/// `Untitled.dat`), or `emb_` and its digest when no name is given. Empty
/// text gives empty text; text that is not a vector is error 5.
fn get_embedding_as_file(args: &[Value], cx: &Ctx) -> Result<Value> {
    let values = match &args[0] {
        Value::Container(_) => return Err(NOT_A_VECTOR),
        v => {
            let text = v.to_text(cx);
            if text.is_empty() {
                return Ok(Value::empty_text());
            }
            json_values(text.as_str()).ok_or(NOT_A_VECTOR)?
        }
    };
    let name = arg(args, 1)
        .map(|n| n.to_text(cx).into_string())
        .filter(|n| !n.is_empty())
        .map(|n| super::container::base_name(&n));
    Ok(file_form(&values, "emb_", name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::Date;
    use crate::locale::Locale;
    use crate::text::Text;

    fn call(f: super::super::Pure, args: &[Value]) -> (String, i32) {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
        match f(args, &cx) {
            Ok(v) => (v.to_text(&cx).into_string(), 0),
            Err(e) => ("?".into(), e.code()),
        }
    }

    fn t(s: &str) -> Value {
        Value::Text(Text::plain(s))
    }

    #[test]
    fn documented_examples() {
        assert_eq!(
            call(add_embeddings, &[t("[1, 2, 3]"), t("[4, 5, 6]")]).0,
            "[0.40160966445124940405, 0.56225353023174917677, 0.722897396012249005]"
        );
        assert_eq!(
            call(normalize_embedding, &[t("[3, 4]")]).0,
            "[0.5999999999999999778, 0.80000000000000004441]"
        );
    }

    #[test]
    fn file_names_and_bytes() {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
        let Ok(Value::Container(c)) = get_embedding_as_file(&[t("[1, 2, 3]")], &cx) else {
            panic!("no file");
        };
        assert_eq!(c.text.as_str(), "emb_03D8B.fve");
        assert_eq!(&c.data()[..8], b"FVE.2100");
        assert_eq!(c.data().len(), 32);
        assert_eq!(
            call(get_embedding_as_text, &[Value::Container(c)]).0,
            "[1, 2, 3]"
        );
        assert_eq!(call(get_embedding_as_text, &[t("[1]")]), ("?".into(), -1));
        assert_eq!(call(get_embedding_as_file, &[t("[]")]), ("?".into(), 5));
    }

    #[test]
    fn unit_vectors_and_bounds() {
        assert!(is_unit(&[0.6, 0.8]));
        assert!(is_unit(&[1.0000049]));
        assert!(!is_unit(&[1.00000499]));
        assert!(!is_unit(&[0.99999995]));
        assert!(is_unit(&[0.99999999]));
        assert_eq!(
            call(cosine_similarity, &[t("[1.00000045]"), t("[1]")]).0,
            "1.0000004499999999297"
        );
        assert_eq!(
            call(cosine_similarity, &[t("[1.0000005]"), t("[1]")]).0,
            "?"
        );
        assert_eq!(call(cosine_similarity, &[t("[1e308]"), t("[10]")]).0, "0");
    }
}
