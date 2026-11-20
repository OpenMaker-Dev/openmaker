//! The calculation function library (spec `docs/re/calc-functions.md`).
//!
//! One module per Claris category. Each module lists its functions in a
//! `FUNCTIONS` table and its numeric keywords in a `KEYWORDS` table; the
//! evaluator finds them by name through [`lookup`] and [`keyword`].
//!
//! Only functions whose result depends on nothing but their argument values
//! and the [`Ctx`] live here. The evaluator implements the rest itself:
//! forms that evaluate arguments lazily or bind names (`If`, `Case`,
//! `Choose`, `Let`, `While`, `SetPrecision`, `SetRecursion`, `Evaluate`,
//! `EvaluationError`, `IsValidExpression`, `GetField`, `Self`), functions
//! over fields, records and repetitions (`Extend`, `GetRepetition`, `Last`,
//! `GetNthRecord`, `GetSummary`, `Lookup`, `LookupNext`, `IsValid`), and
//! everything that asks the host (`Get`, Design, `Random`, `ExecuteSQL`,
//! layout, Live Text, the AI functions that call a model or need the
//! tokenizer, mobile and plug-in functions). The container analysis
//! functions are here and read container data; the evaluator has the host
//! load a referenced container's data first ([`reads_container_data`]).

use crate::{CalcError, Ctx, Value};

mod aggregate;
mod container;
mod container_analysis;
mod datetime;
mod embedding;
mod financial;
mod formatting;
mod japanese;
mod json;
mod number;
pub mod record_ids;
mod text;
mod trig;

/// A function over evaluated arguments, in source order. The parser has
/// already checked the argument count against `calc-tokens.csv`, so an
/// implementation may index required arguments directly and use [`arg`]
/// for optional ones.
///
/// `Err` makes the result `?` with that error code; a function that yields
/// `?` without an error returns `Ok(Value::invalid())`.
pub type Pure = fn(&[Value], &Ctx) -> Result<Value, CalcError>;

/// An aggregate (`Sum`, `Average`, `Count`, `List`, ...): each argument
/// arrives expanded into the values it stands for, a repeating field's
/// repetitions or a related field's records, and a plain expression as a
/// single value (spec `calc-values.md` 8.6, 8.7).
pub type Aggregate = fn(&[Vec<Value>], &Ctx) -> Result<Value, CalcError>;

#[derive(Clone, Copy)]
pub enum Implementation {
    Pure(Pure),
    Aggregate(Aggregate),
}

/// A library function under its canonical name (as in `calc-tokens.csv`).
#[derive(Clone, Copy)]
pub struct Function {
    pub name: &'static str,
    pub implementation: Implementation,
}

impl Function {
    pub const fn pure(name: &'static str, f: Pure) -> Function {
        Function {
            name,
            implementation: Implementation::Pure(f),
        }
    }

    pub const fn aggregate(name: &'static str, f: Aggregate) -> Function {
        Function {
            name,
            implementation: Implementation::Aggregate(f),
        }
    }
}

/// A named numeric constant of the language (`True`, `Bold`, `JSONString`,
/// ...), as the function that takes it expects it.
pub type Keyword = (&'static str, i64);

const FUNCTIONS: &[&[Function]] = &[
    aggregate::FUNCTIONS,
    container::FUNCTIONS,
    container_analysis::FUNCTIONS,
    datetime::FUNCTIONS,
    embedding::FUNCTIONS,
    financial::FUNCTIONS,
    formatting::FUNCTIONS,
    japanese::FUNCTIONS,
    json::FUNCTIONS,
    number::FUNCTIONS,
    text::FUNCTIONS,
    trig::FUNCTIONS,
];

const KEYWORDS: &[&[Keyword]] = &[
    aggregate::KEYWORDS,
    record_ids::KEYWORDS,
    container::KEYWORDS,
    container_analysis::KEYWORDS,
    datetime::KEYWORDS,
    embedding::KEYWORDS,
    financial::KEYWORDS,
    formatting::KEYWORDS,
    japanese::KEYWORDS,
    json::KEYWORDS,
    number::KEYWORDS,
    text::KEYWORDS,
    trig::KEYWORDS,
];

/// The library function named `name`; names match case-insensitively.
pub fn lookup(name: &str) -> Option<&'static Function> {
    FUNCTIONS
        .iter()
        .flat_map(|category| category.iter())
        .find(|f| f.name.eq_ignore_ascii_case(name))
}

/// The value of the keyword named `name`; names match case-insensitively.
pub fn keyword(name: &str) -> Option<i64> {
    KEYWORDS
        .iter()
        .flat_map(|category| category.iter())
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v)
}

/// Every library function, for catalogs and tests.
pub fn all() -> impl Iterator<Item = &'static Function> {
    FUNCTIONS.iter().flat_map(|category| category.iter())
}

pub use container_analysis::{pdf_worker, stored_streams, typed_file, StoredStreams};

/// Whether the library function named `name` reads the data of container
/// arguments (the container analysis functions, the encoders and crypt
/// functions, the embedding functions and `Length`): the evaluator then has
/// the host load the streams of containers it holds only by reference.
/// Other functions see a container's text, its file name.
pub fn reads_container_data(name: &str) -> bool {
    const DATA: &[&str] = &[
        "Base64Encode",
        "Base64EncodeRFC",
        "CryptAuthCode",
        "CryptDecrypt",
        "CryptDecryptBase64",
        "CryptDigest",
        "CryptEncrypt",
        "CryptEncryptBase64",
        "CryptGenerateSignature",
        "CryptVerifySignature",
        "HexEncode",
        "TextDecode",
        "AddEmbeddings",
        "CosineSimilarity",
        "GetEmbeddingAsText",
        "NormalizeEmbedding",
        "SubtractEmbeddings",
        "Length",
    ];
    container_analysis::reads_container_data(name)
        || DATA.iter().any(|d| d.eq_ignore_ascii_case(name))
}

/// Whether a call of `name`, a function that [`reads_container_data`],
/// with `args` answers without the data: `GetContainerAttribute`'s file
/// name, which a referenced container's text already holds.
pub fn answers_from_reference(name: &str, args: &[Value], cx: &Ctx) -> bool {
    container_analysis::answers_from_reference(name, args, cx)
}

/// The optional argument at `index`, if the call has one.
pub fn arg(args: &[Value], index: usize) -> Option<&Value> {
    args.get(index)
}
