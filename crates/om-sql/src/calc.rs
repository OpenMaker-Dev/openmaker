//! `ExecuteSQL` and `ExecuteSQLe` as calculation functions (execute-sql.md
//! 1): the arguments a calculation passes, the result value and the error
//! code `EvaluationError` reports.
//!
//! A host answers them from `CalcHost::call_reporting`:
//!
//! ```ignore
//! fn call_reporting(&mut self, f: HostFunction<'_>, args: &[Value], cx: &Context)
//!     -> (Value, Option<CalcError>) {
//!     if let Some(r) = om_sql::calc::host_call(self, f, args) {
//!         return r;
//!     }
//!     // ... the host's other functions
//! }
//! ```

use om_calc::eval::HostFunction;
use om_calc::{CalcError, Ctx, Value};

use crate::error::SqlError;
use crate::source::SqlSource;

/// Which of the two functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// `ExecuteSQL`: an error is the value `?`.
    ExecuteSql,
    /// `ExecuteSQLe`: an error is the text `? ERROR: ...`.
    ExecuteSqle,
}

impl Variant {
    /// The variant a host function call names, if it is one of the two.
    pub fn of(function: &HostFunction<'_>) -> Option<Variant> {
        match function {
            HostFunction::Named(n) if n.eq_ignore_ascii_case("ExecuteSQL") => {
                Some(Variant::ExecuteSql)
            }
            HostFunction::Named(n) if n.eq_ignore_ascii_case("ExecuteSQLe") => {
                Some(Variant::ExecuteSqle)
            }
            _ => None,
        }
    }
}

/// Answers `ExecuteSQL`/`ExecuteSQLe` over `source`; `None` for any other
/// function. The result is the value and the error to report beside it.
pub fn host_call(
    source: &mut dyn SqlSource,
    function: HostFunction<'_>,
    args: &[Value],
) -> Option<(Value, Option<CalcError>)> {
    let variant = Variant::of(&function)?;
    Some(execute_sql(source, variant, args))
}

/// `ExecuteSQL ( sqlQuery ; fieldSeparator ; rowSeparator { ; arguments... } )`
/// with evaluated arguments (1.1, 1.2). An argument that is `?` (a
/// calculation error the evaluator has already recorded) makes the result
/// `?`; separators are their text (a `?` separator is the text `?`).
pub fn execute_sql(
    source: &mut dyn SqlSource,
    variant: Variant,
    args: &[Value],
) -> (Value, Option<CalcError>) {
    let locale = source.locale().clone();
    let today = source.now().date();
    let cx = Ctx::new(&locale, today);
    let text = |i: usize| {
        args.get(i)
            .map(|v| v.to_text(&cx).into_string())
            .unwrap_or_default()
    };
    let params = args.get(3..).unwrap_or(&[]);
    if args.first().is_some_and(Value::is_invalid) || params.iter().any(Value::is_invalid) {
        return (Value::invalid(), None);
    }
    let sql = text(0);
    match crate::execute(source, &sql, &text(1), &text(2), params) {
        Ok(result) if result.is_empty() => (Value::empty_text(), None),
        Ok(result) => (Value::text(result), None),
        Err(e) => error_result(variant, &e),
    }
}

/// What the function returns for `error` (1.2): `?` from `ExecuteSQL`, the
/// text `? ERROR: ...` from `ExecuteSQLe`, each with the error's code. A
/// host that refuses a query before it runs (an account without the
/// privileges to read the file) answers with this.
pub fn error_result(variant: Variant, error: &SqlError) -> (Value, Option<CalcError>) {
    let code = error.calc_error();
    match variant {
        Variant::ExecuteSql => (Value::invalid(), Some(code)),
        Variant::ExecuteSqle => (
            Value::text(format!("? ERROR: {}", error.message())),
            Some(code),
        ),
    }
}
