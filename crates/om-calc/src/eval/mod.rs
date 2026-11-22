//! The evaluator: solves `om_formula` trees in a context against a
//! [`CalcHost`] (spec `docs/re/calc-values.md` 6-9, 12.1, 13).
//!
//! ```text
//! stored formula  --om_formula::Formula::decode--> Formula --evaluate_formula--+
//! source text     --compile / evaluate_text------> Expr ----evaluate-----------+--> Evaluation
//! ```
//!
//! A [`Context`] says where the calculation runs: the table occurrence, the
//! current record and portal row, the repetition being calculated, the
//! declared result type and the value `Self` stands for. The host supplies
//! the file (fields, records, relationships, custom functions, names), the
//! session (`Get`, `$$` variables, `Random`) and the script engine (`$`
//! variables, script `Get` selectors).
//!
//! The result is a value and the first error of the evaluation (7.2): a
//! failing operation makes its value `?`, records its error and the
//! evaluation goes on; error 7 (iteration or custom function call limit,
//! or the host's [`CalcLimits`] on text size and nesting), error 1224 (a
//! field without a record, or a loop over data the host is still fetching,
//! [`CalcHost::pending`]) and error 1 (the host's stop,
//! [`CalcHost::interrupted`]) end it with `?`.
//!
//! Library functions ([`crate::functions`]) get their arguments in source
//! order; a `[ ... ]` group (`Substitute ( t ; [ a ; b ] ; [ c ; d ] )`,
//! `JSONSetElement ( j ; [ k ; v ; t ] ; ... )`) is spliced into the flat
//! list, so `Substitute` sees `[t, a, b, c, d]` written either way. An
//! aggregate gets each argument as the list of values it stands for: a
//! lone field argument of `Sum`, `List`, ... every repetition of the field
//! on the record, or of every related record, empty values included (the
//! function skips them); with two or more arguments, or a plain
//! expression, one value each.

mod fields;
mod host;
mod memory;
mod solver;
mod vars;

use om_formula::{Compiled, Expr, Formula, ParseError};

pub use host::{
    default_call, qualified_field_name, shared_random, CalcHost, CalcLimits, CustomFunction,
    FieldId, FieldInfo, HostFunction, Lcg, LookupDirection, NullHost, Pending, Storage,
    StoredContainer,
};
pub use memory::{HostFn, Join, MemoryHost};
pub use vars::{Repetition, VarScope, Variables};

use crate::{CalcError, Ctx, Value, ValueType};
use solver::Solver;

/// Where a calculation runs (`DBExpressionEnv`, calc-values.md 9.1).
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// The table occurrence the formula is evaluated from: unqualified
    /// fields are its fields and related fields are reached from it. `None`
    /// for formulas without a table (script steps in some files, custom
    /// menus, a bare `Evaluate`).
    pub occurrence: Option<u64>,
    /// The file that owns the formula (`mFileUser`), as the host numbers
    /// the files it combines; `None` for the file owning `occurrence`. A
    /// field's formula without an occurrence of its own is solved in the
    /// occurrence that reads the field, perhaps another file's, but for
    /// the file owning the field ([`CalcHost::table_file`]).
    pub file: Option<u64>,
    /// The current record of that occurrence (its record ID). Without one,
    /// every field reference ends the evaluation with error 1224.
    pub record: Option<u64>,
    /// The portal row (occurrence, record ID): fields of that occurrence
    /// read this record rather than the first related one.
    pub portal: Option<(u64, u64)>,
    /// The displayed portal row's number, from 1, when evaluating in that
    /// row: `Get ( RecordNumber )` answers it instead of the record's
    /// position in its found set (calc-functions.md 6.1.1).
    pub portal_row_number: Option<u64>,
    /// The parent occurrence and record whose related set this record was
    /// read through. Nested unstored calculations retain this so a summary
    /// of their own table summarizes that related set, rather than a window
    /// found set for the related table.
    pub related_from: Option<(u64, u64)>,
    /// The repetition being calculated: 0 outside a repeating calculation,
    /// n while repetition n of a calculation field is computed
    /// (`Get ( CalculationRepetitionNumber )`, calc-values.md 8.6). A field
    /// calculation evaluates repetition 1 to n, never 0.
    pub repetition: u32,
    /// The calculation's declared result type, applied to the final value
    /// (calc-values.md 6.5).
    pub result_type: Option<ValueType>,
    /// The value `Self` refers to (the field being validated or
    /// auto-entered, the layout object being formatted); `None` makes
    /// `Self` error 1225.
    pub self_value: Option<Value>,
    /// A field that does not repeat reads its value in every repetition, as
    /// FileMaker Server's validation of repetition n of a repeating field
    /// reads it (script oracle s17, s19); a repeating calculation reads it
    /// in repetition 1 only (calc-values.md 8.6). `F[n]`, `GetRepetition`
    /// and `Extend` keep their own rules.
    pub extend_fields: bool,
}

impl Context {
    /// No table occurrence and no record.
    pub fn new() -> Context {
        Context::default()
    }

    /// `record` of `occurrence`.
    pub fn record(occurrence: u64, record: u64) -> Context {
        Context {
            occurrence: Some(occurrence),
            record: Some(record),
            ..Context::default()
        }
    }

    pub fn with_repetition(mut self, repetition: u32) -> Context {
        self.repetition = repetition;
        self
    }

    pub fn with_result_type(mut self, result_type: ValueType) -> Context {
        self.result_type = Some(result_type);
        self
    }

    pub fn with_portal(mut self, occurrence: u64, record: u64) -> Context {
        self.portal = Some((occurrence, record));
        self
    }

    pub fn with_portal_row_number(mut self, number: u64) -> Context {
        self.portal_row_number = Some(number);
        self
    }

    pub fn with_self(mut self, value: Value) -> Context {
        self.self_value = Some(value);
        self
    }
}

/// The outcome of an evaluation.
#[derive(Clone, Debug)]
pub struct Evaluation {
    /// The result, converted to the declared result type if there is one.
    pub value: Value,
    /// The first error, as `EvaluationError` reports it; `None` for none.
    pub error: Option<CalcError>,
    /// A control decision or nested evaluation needs data before execution
    /// can continue. Unlike an ordinary calculation error, a caller must
    /// suspend too, even when it ignores this evaluation's value.
    pub pending_execution: bool,
}

impl Evaluation {
    /// The error code, 0 for none.
    pub fn code(&self) -> i32 {
        self.error.map_or(0, CalcError::code)
    }

    fn failed(error: CalcError) -> Evaluation {
        Evaluation {
            value: Value::invalid(),
            error: Some(error),
            pending_execution: false,
        }
    }
}

/// Evaluates a compiled tree (`om_formula::compile`'s `expr` or a decoded
/// Polish stream) in `context`.
pub fn evaluate(expr: &Expr, host: &mut dyn CalcHost, context: &Context) -> Evaluation {
    let mut remaining = u64::MAX;
    evaluate_with_calculation_budget(expr, host, context, &mut remaining)
        .unwrap_or_else(|_| Evaluation::failed(CalcError::OUT_OF_MEMORY))
}

/// The evaluation stopped after using its allowance for unstored calculations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalculationBudgetExhausted;

fn evaluate_with_calculation_budget(
    expr: &Expr,
    host: &mut dyn CalcHost,
    context: &Context,
    remaining: &mut u64,
) -> Result<Evaluation, CalculationBudgetExhausted> {
    // SQL and layout callbacks create fresh evaluators over reborrowed
    // variables. Keep withheld assignments and control suspension alive
    // until the outermost evaluation ends. A host may use the same map for
    // both scopes; balanced nesting works for that case too.
    for scope in [VarScope::Local, VarScope::Global] {
        host.variables(scope).begin_evaluation();
    }
    let result = evaluate_in_scope(expr, host, context, remaining);
    for scope in [VarScope::Local, VarScope::Global] {
        let vars = host.variables(scope);
        if result.as_ref().is_ok_and(|result| result.pending_execution) {
            vars.suspend_execution();
        }
        vars.end_evaluation();
    }
    result
}

fn execution_pending(host: &mut dyn CalcHost) -> bool {
    host.variables(VarScope::Local).execution_pending()
        || host.variables(VarScope::Global).execution_pending()
}

fn evaluate_in_scope(
    expr: &Expr,
    host: &mut dyn CalcHost,
    context: &Context,
    remaining: &mut u64,
) -> Result<Evaluation, CalculationBudgetExhausted> {
    let mut solver = Solver::new(host, context.clone(), *remaining);
    let allowance = solver.calculations_remaining;
    let result = solver
        .check_execution()
        .and_then(|()| solver.solve(expr, false))
        .and_then(|op| solver.resolve(op));
    let result = solver.check_execution().and(result);
    *remaining -= allowance - solver.calculations_remaining;
    match result {
        Ok(value) => {
            let value = match context.result_type {
                Some(ty) => value.convert_declared(ty, &solver.cx()),
                None => value,
            };
            Ok(Evaluation {
                value,
                error: solver.errors.get(),
                pending_execution: false,
            })
        }
        Err(abort) if abort.exhausted => Err(CalculationBudgetExhausted),
        Err(abort) => Ok(Evaluation {
            pending_execution: abort.pending,
            ..Evaluation::failed(abort.error)
        }),
    }
}

/// Evaluates a stored formula. A context without a table occurrence takes
/// the formula's own (child 4).
pub fn evaluate_formula(
    formula: &Formula,
    host: &mut dyn CalcHost,
    context: &Context,
) -> Evaluation {
    let mut remaining = u64::MAX;
    evaluate_formula_with_calculation_budget(formula, host, context, &mut remaining)
        .unwrap_or_else(|_| Evaluation::failed(CalcError::OUT_OF_MEMORY))
}

/// Evaluates a stored formula, counting its unstored reads against both the
/// host's per-evaluation limit and `remaining`. The caller can reuse the
/// remaining allowance across evaluations that implement one operation.
pub fn evaluate_formula_with_calculation_budget(
    formula: &Formula,
    host: &mut dyn CalcHost,
    context: &Context,
    remaining: &mut u64,
) -> Result<Evaluation, CalculationBudgetExhausted> {
    if context.occurrence.is_none() && formula.occurrence().is_some() {
        let context = Context {
            occurrence: formula.occurrence(),
            ..context.clone()
        };
        return evaluate_with_calculation_budget(&formula.polish, host, &context, remaining);
    }
    evaluate_with_calculation_budget(&formula.polish, host, context, remaining)
}

/// Evaluates every repetition of a repeating calculation: repetition 1 to
/// `repetitions` of `context`'s record.
pub fn evaluate_repetitions(
    expr: &Expr,
    host: &mut dyn CalcHost,
    context: &Context,
    repetitions: u32,
) -> Vec<Evaluation> {
    (1..=repetitions.max(1))
        .map(|rep| evaluate(expr, host, &context.clone().with_repetition(rep)))
        .collect()
}

/// Compiles formula text against the names of the file owning the
/// context's table occurrence and that file's decimal separator, as the
/// calculation dialog and `Evaluate` do; `support_self` allows `Self` (field
/// and layout calculations, calc-encoding.md 10.1).
pub fn compile(
    text: &str,
    host: &dyn CalcHost,
    context: &Context,
    support_self: bool,
) -> Result<Compiled, ParseError> {
    let mut ctx = om_formula::Context::new(host.names_for(context));
    ctx.occurrence = context.occurrence;
    ctx.decimal_comma = host.locale_for(context).decimal_separator == ',';
    ctx.support_self = support_self;
    om_formula::compile(text, &ctx)
}

/// Compiles and evaluates formula text (as a calculation's text with `Self`
/// allowed when the context has a `Self` value); a syntax error is the `?`
/// result with its error code.
pub fn evaluate_text(text: &str, host: &mut dyn CalcHost, context: &Context) -> Evaluation {
    let compiled = solver::compile_text(
        host.names_for(context),
        text,
        context.occurrence,
        host.locale_for(context).decimal_separator == ',',
        context.self_value.is_some(),
    );
    match compiled {
        Ok(expr) => evaluate(&expr, host, context),
        Err(e) => Evaluation::failed(
            CalcError::from_code(e.code.number()).unwrap_or(CalcError::OPERAND_EXPECTED),
        ),
    }
}

/// The text of a value in the host's locale, as a field shows it.
pub fn display(value: &Value, host: &dyn CalcHost) -> String {
    value
        .to_text(&Ctx::new(host.locale(), host.today()))
        .into_string()
}
