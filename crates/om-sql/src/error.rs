//! FQL errors (execute-sql.md 9): the `FQLnnnn` messages with their
//! positions, engine errors with FileMaker's texts, and the calculation
//! error code each one gives (`EvaluationError`, 9.2).

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use om_calc::CalcError;

use crate::parse::Pos;

const CATALOG: &str = include_str!("../../../docs/re/catalogs/execute-sql-errors.tsv");

/// An error of a statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlError {
    /// `FQLnnnn`: the code, where (`one_based` for syntax errors, whose
    /// column FileMaker shows from 1) and the message's placeholders.
    Fql {
        code: u16,
        pos: Pos,
        one_based: bool,
        args: Vec<String>,
    },
    /// An engine error by its code (`3` for statements ExecuteSQL does not
    /// run, `14`, `15`, `500`-`502`, `8303`, ...; -1 as `CalcError::UNKNOWN`).
    Engine(i32),
    /// Error 100 on an occurrence of a file that is not open, shown as its
    /// extended text `<File Missing>`.
    FileMissing,
}

impl SqlError {
    /// FQL0001 from the lexer or parser.
    pub fn syntax(pos: Pos) -> SqlError {
        SqlError::Fql {
            code: 1,
            pos,
            one_based: true,
            args: Vec::new(),
        }
    }

    /// Any other `FQLnnnn`, at a token's 0-based column.
    pub fn fql(code: u16, pos: Pos, args: &[&str]) -> SqlError {
        SqlError::Fql {
            code,
            pos,
            one_based: false,
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The calculation error ExecuteSQL reports (9.2).
    pub fn calc_error(&self) -> CalcError {
        let code = match self {
            SqlError::Fql {
                code: 1 | 10 | 51, ..
            } => 8310,
            SqlError::Fql { .. } => 8309,
            SqlError::Engine(-1) => return CalcError::UNKNOWN,
            SqlError::Engine(code) => u16::try_from(*code).unwrap_or(0),
            SqlError::FileMissing => 100,
        };
        CalcError::from_code(code).unwrap_or(CalcError::NOT_SUPPORTED)
    }

    /// The text after `? ERROR: ` (9.1).
    pub fn message(&self) -> String {
        match self {
            SqlError::Fql {
                code,
                pos,
                one_based,
                args,
            } => {
                let column = pos.column + u32::from(*one_based);
                let text = fill(fql_message(*code), args);
                format!("FQL{code:04}/({}:{column}): {text}", pos.line)
            }
            SqlError::Engine(code) => format!(" ({code}): {}", engine_message(*code)),
            SqlError::FileMissing => "<File Missing>".to_string(),
        }
    }
}

impl fmt::Display for SqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SqlError {}

impl From<CalcError> for SqlError {
    fn from(e: CalcError) -> SqlError {
        SqlError::Engine(e.code())
    }
}

struct Messages {
    fql: HashMap<u16, &'static str>,
    engine: HashMap<i32, &'static str>,
}

fn messages() -> &'static Messages {
    static MESSAGES: OnceLock<Messages> = OnceLock::new();
    MESSAGES.get_or_init(|| {
        let mut fql = HashMap::new();
        let mut engine = HashMap::new();
        for line in CATALOG.lines().skip(1) {
            let cols: Vec<&str> = line.split('\t').collect();
            let [kind, code, text, ..] = cols[..] else {
                continue;
            };
            match kind {
                "fql" => {
                    if let Ok(n) = code.trim_start_matches("FQL").parse() {
                        fql.insert(n, text);
                    }
                }
                "engine" => {
                    if let Ok(n) = code.parse() {
                        engine.insert(n, text);
                    }
                }
                _ => {}
            }
        }
        Messages { fql, engine }
    })
}

/// `FQL::NonLocalizedStrings(code)`, placeholders `^0`, `^1` unfilled.
pub fn fql_message(code: u16) -> &'static str {
    messages().fql.get(&code).copied().unwrap_or("")
}

/// `FQL::ErrorToString(code)`: the engine's text, `FileMaker error` for
/// codes without one of their own.
pub fn engine_message(code: i32) -> &'static str {
    messages()
        .engine
        .get(&code)
        .copied()
        .unwrap_or("FileMaker error")
}

fn fill(template: &str, args: &[String]) -> String {
    let mut out = template.to_string();
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("^{i}"), a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_follow_filemaker() {
        let pos = Pos {
            line: 1,
            column: 13,
        };
        assert_eq!(
            SqlError::syntax(pos).message(),
            "FQL0001/(1:14): There is an error in the syntax of the query."
        );
        assert_eq!(
            SqlError::fql(
                2,
                Pos {
                    line: 1,
                    column: 21
                },
                &["Foo"]
            )
            .message(),
            "FQL0002/(1:21): The table named \"Foo\" does not exist."
        );
        assert_eq!(
            SqlError::fql(9, pos, &["Tasks", "x"]).message(),
            "FQL0009/(1:13): The column named \"x\" does not exist in table \"Tasks\"."
        );
        assert_eq!(
            SqlError::fql(50, pos, &[]).message(),
            "FQL0050/(1:13): The offset count in OFFSET clause is not valid. "
        );
        assert_eq!(SqlError::Engine(14).message(), " (14): Number out of range");
        assert_eq!(
            SqlError::Engine(31337).message(),
            " (31337): FileMaker error"
        );
        assert_eq!(SqlError::FileMissing.message(), "<File Missing>");
        assert_eq!(SqlError::syntax(pos).calc_error().code(), 8310);
        assert_eq!(SqlError::fql(10, pos, &["x"]).calc_error().code(), 8310);
        assert_eq!(SqlError::fql(7, pos, &["x"]).calc_error().code(), 8309);
        assert_eq!(SqlError::Engine(3).calc_error().code(), 3);
    }
}
