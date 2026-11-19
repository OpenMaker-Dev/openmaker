//! FileMaker's SQL engine, FQL (spec `docs/re/execute-sql.md`): what
//! `ExecuteSQL` and `ExecuteSQLe` accept, compute and print.
//!
//! ```text
//! statement --parse (lexer, ATN, ANTLR-compatible prediction)--> Statement
//!           --plan::Binder (names, types, FQL checks)----------> plan
//!           --exec::Exec over a SqlSource----------------------> rows
//!           --result text (8.2)--------------------------------> ExecuteSQL's value
//! ```
//!
//! A host offers ExecuteSQL by implementing [`SqlSource`] over its file
//! (tables, fields, records, value lists) and answering the calculation
//! functions with [`calc::host_call`] from
//! `om_calc::eval::CalcHost::call_reporting`. Write statements (INSERT,
//! UPDATE, DELETE, DDL) are checked and then fail with error 3, as in
//! FileMaker; nothing changes.
//!
//! The `fmp12` feature adds [`fmp12::FileHost`], a source and calculation
//! host over a local `.fmp12` file.
#![forbid(unsafe_code)]

pub mod calc;
pub mod error;
mod exec;
#[cfg(feature = "fmp12")]
pub mod fmp12;
mod func;
pub mod parse;
mod plan;
pub mod source;
mod stack;
pub mod value;

use om_calc::{Ctx, Date, Value};

pub use error::SqlError;
pub use source::{
    BaseTableDef, FieldClass, FieldDef, FieldType, SqlSource, TableDef, ValueListDef,
    ValueListValues,
};
pub use value::Val;

/// Runs a statement: the rows of a query, each its column values. `args`
/// bind to the `?` parameters in order (missing ones are NULL, extra ones
/// ignored).
pub fn query(
    source: &mut dyn SqlSource,
    sql: &str,
    args: &[Value],
) -> Result<Vec<Vec<Val>>, SqlError> {
    // The statement and its plan are dropped inside the guard too.
    stack::guard(|| {
        let statement =
            parse::parse_until(sql, &|| source.interrupted()).map_err(|stopped| match stopped {
                parse::Stopped::Syntax(e) => SqlError::syntax(e.pos),
                parse::Stopped::Interrupted => SqlError::Engine(exec::ABORTED),
            })?;
        let plan = plan::Binder::new(source).statement(&statement)?;
        let plan::Plan::Query(q) = plan;
        exec::Exec::new(source, args).query(&q)
    })
}

/// `ExecuteSQL`'s text (1.2, 8): each row's column texts joined by
/// `field_separator`, rows joined by `row_separator`; empty separators are
/// `,` and CR.
pub fn execute(
    source: &mut dyn SqlSource,
    sql: &str,
    field_separator: &str,
    row_separator: &str,
    args: &[Value],
) -> Result<String, SqlError> {
    let rows = query(source, sql, args)?;
    let locale = source.locale().clone();
    let today: Date = source.now().date();
    let cx = Ctx::new(&locale, today);
    Ok(format_rows(&rows, field_separator, row_separator, &cx))
}

/// The text of rows with separators (empty means `,` and CR).
pub fn format_rows(
    rows: &[Vec<Val>],
    field_separator: &str,
    row_separator: &str,
    cx: &Ctx,
) -> String {
    let fs = if field_separator.is_empty() {
        ","
    } else {
        field_separator
    };
    let rs = if row_separator.is_empty() {
        "\r"
    } else {
        row_separator
    };
    rows.iter()
        .map(|r| r.iter().map(|v| v.output(cx)).collect::<Vec<_>>().join(fs))
        .collect::<Vec<_>>()
        .join(rs)
}
