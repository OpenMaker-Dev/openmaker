//! Source-side calculated relationship keys. A target's match fields
//! continue to be read from storage, so this does not invent indexes for
//! unstored target calculations.

use std::collections::BTreeSet;

use om_calc::eval::{evaluate, Context, Variables};
use om_calc::{CalcError, Ctx, Value};
use om_formula::{Expr, Field};

use super::session::selector;
use super::{
    CalcSchema, CalcSource, FieldClass, FileCalc, FileHost, ReadDepth, Recorder, SessionInfo,
};

/// Nonempty repetitions of `field` on the source record of a relationship.
/// Uses the normal field evaluator (types, repetitions, empty-input rule,
/// custom functions and formula context), with a provider's session when
/// supplied. A circular key is bounded by the host's normal read limit.
///
/// This record-only host has no script variables, window state or dynamic
/// evaluation context. Such dependencies (including in nested fields,
/// custom functions and untaken branches) are refused with error 1225,
/// rather than silently evaluating them with empty/default session state.
pub fn source_match_values<S: CalcSource + ?Sized>(
    source: &S,
    schema: &CalcSchema,
    occurrence: u64,
    record: u64,
    field: u64,
    cx: &Ctx,
    session: Option<&SessionInfo>,
) -> Result<Vec<Value>, CalcError> {
    let Some(def) = schema.field(occurrence, field) else {
        // Its occurrence waits for its file or its table's fields: asked
        // for, the key waits (1224).
        if schema.is_pending(occurrence) {
            source.need_occurrence(occurrence);
            return Err(CalcError::from_code(1224).expect("nonzero"));
        }
        return Err(CalcError::FIELD_MISSING);
    };
    if def.class == FieldClass::Summary {
        return Err(unsupported());
    }
    let _depth = ReadDepth::enter().ok_or(CalcError::OUT_OF_MEMORY)?;
    let mut keys = KeyContext::default();
    keys.field(schema, occurrence, field, session.is_some(), 0)?;
    // A field the key's formula reads of an occurrence still waiting: each
    // is asked for, and the key waits rather than reading it missing.
    if !keys.waiting.is_empty() {
        for &waiting in &keys.waiting {
            source.need_occurrence(waiting);
        }
        return Err(CalcError::from_code(1224).expect("nonzero"));
    }
    // A sized forwarding source lets the default CalcSource method also
    // work through a trait object. Recorder forwards pending-data counts.
    let recorder = Recorder::new(source);
    let fallback = SessionInfo {
        file_name: schema.file_name.clone(),
        ..Default::default()
    };
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(schema, &recorder, cx.today),
        session.unwrap_or(&fallback),
        &mut locals,
        &mut globals,
    );
    let reference = Expr::Field(Field::new(occurrence, field, true));
    let mut values = Vec::new();
    for repetition in 1..=def.repetitions.max(1) {
        let before = source.pending();
        let result = evaluate(
            &reference,
            &mut host,
            &Context {
                repetition,
                ..Context::record(occurrence, record)
            },
        );
        if source.pending().reads > before.reads {
            return Err(CalcError::from_code(1224).expect("nonzero"));
        }
        if result.value.is_invalid() {
            return Err(result.error.unwrap_or(CalcError::UNKNOWN));
        }
        if !result.value.is_empty() {
            values.push(result.value);
        }
    }
    Ok(values)
}

fn unsupported() -> CalcError {
    CalcError::from_code(1225).expect("nonzero")
}

/// Check dependencies before running anything: an IsValid/If fallback must
/// not turn unavailable session state into an apparently valid match key.
/// Expression traversal is iterative; field/function recursion is bounded
/// and each definition is visited only once, including recursive functions.
#[derive(Default)]
struct KeyContext {
    fields: BTreeSet<(u64, u64)>,
    functions: BTreeSet<u64>,
    /// Occurrences a field it read waits on ([`CalcSchema::is_pending`]).
    waiting: BTreeSet<u64>,
}

impl KeyContext {
    fn field(
        &mut self,
        schema: &CalcSchema,
        occurrence: u64,
        field: u64,
        session: bool,
        depth: u32,
    ) -> Result<(), CalcError> {
        if !self.fields.insert((occurrence, field)) {
            return Ok(());
        }
        let Some(def) = schema.field(occurrence, field) else {
            if schema.is_pending(occurrence) {
                self.waiting.insert(occurrence);
                return Ok(());
            }
            return Err(CalcError::FIELD_MISSING);
        };
        if def.class == FieldClass::Summary {
            return Err(unsupported());
        }
        if def.is_unstored_calculation() {
            let formula = def.formula.as_ref().ok_or(unsupported())?;
            self.expression(schema, &formula.polish, session, depth + 1)?;
        }
        Ok(())
    }

    fn expression(
        &mut self,
        schema: &CalcSchema,
        expression: &Expr,
        session: bool,
        depth: u32,
    ) -> Result<(), CalcError> {
        if depth > 64 {
            return Err(CalcError::OUT_OF_MEMORY);
        }
        let mut stack = vec![expression];
        while let Some(expr) = stack.pop() {
            match expr {
                Expr::Variable(_)
                | Expr::Evaluate { .. }
                | Expr::GetField { .. }
                | Expr::PluginCall { .. } => return Err(unsupported()),
                Expr::Get(selector) if !supported_get(*selector, session) => {
                    return Err(unsupported())
                }
                Expr::Field(field) | Expr::FieldRef(field) => self.field(
                    schema,
                    field.occurrence.id().ok_or(CalcError::FIELD_MISSING)?,
                    field.field.id().ok_or(CalcError::FIELD_MISSING)?,
                    session,
                    depth + 1,
                )?,
                Expr::CustomCall { function, args, .. } => {
                    let id = function.id().ok_or(unsupported())?;
                    if self.functions.insert(id) {
                        let function = schema.custom_function(id).ok_or(unsupported())?;
                        self.expression(schema, &function.body, session, depth + 1)?;
                    }
                    stack.extend(args);
                }
                // SQL can name unstored fields dynamically. Layout-object
                // answers require the GUI's current layout and focus.
                Expr::Call { op, .. } if matches!(op.0, 2301 | 2302 | 2312) => {
                    return Err(unsupported())
                }
                Expr::Call { args, .. } | Expr::List(args) => stack.extend(args),
                Expr::Case(pairs) => stack.extend(pairs.iter().flat_map(|(a, b)| [a, b])),
                Expr::Choose { test, results } => {
                    stack.push(test);
                    stack.extend(results);
                }
                Expr::Let { bindings, body } => {
                    stack.extend(bindings);
                    stack.push(body);
                }
                Expr::While {
                    initial,
                    condition,
                    logic,
                    result,
                } => {
                    stack.extend(initial);
                    stack.push(condition);
                    stack.extend(logic);
                    stack.push(result);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn supported_get(get: u8, session: bool) -> bool {
    match get {
        selector::FILE_NAME | selector::FILE_LOCALE_ELEMENTS | selector::LAYOUT_COUNT
            | selector::TOTAL_RECORD_COUNT | selector::RECORD_ID
            | selector::RECORD_MODIFICATION_COUNT | 68 /* CalculationRepetitionNumber */ => true,
        selector::APPLICATION_VERSION | selector::HOST_APPLICATION_VERSION
            | selector::ACCOUNT_NAME | selector::USER_NAME | selector::HOST_NAME
            | selector::FILE_PATH | selector::MULTI_USER_STATE | selector::NETWORK_PROTOCOL
            | selector::SYSTEM_LANGUAGE | selector::APPLICATION_LANGUAGE | selector::SYSTEM_VERSION
            | selector::SYSTEM_PLATFORM | selector::DEVICE | selector::APPLICATION_ARCHITECTURE
            | selector::SYSTEM_LOCALE_ELEMENTS | selector::CURRENT_DATE | selector::CURRENT_TIME
            | selector::CURRENT_TIMESTAMP | selector::CURRENT_TIME_UTC_MILLISECONDS
            | selector::CURRENT_TIME_UTC_MICROSECONDS | selector::DB_ENGINE_VERSION
            | selector::UUID | selector::UUID_NUMBER | selector::ACCOUNT_TYPE => session,
        _ => false,
    }
}
