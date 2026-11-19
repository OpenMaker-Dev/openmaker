//! The solver over `om_formula` trees (calc-values.md 7, 8): operators,
//! control forms, `Let` and `While` slots, variables, the iteration and
//! call limits, `Evaluate` and friends, custom function calls and the
//! function library. Field operands are in `fields.rs`.
//!
//! The tree is walked recursively; every node checks the remaining stack
//! and continues on a heap segment when it runs low (`stacker`), so 50,000
//! nested custom function calls or a deeply nested tree cannot overflow a
//! thread. Tail calls (`1c`, calc-encoding.md 5.2) replace the running
//! function's frame instead of nesting.

use std::collections::HashMap;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;

use om_formula::catalog::{self, Kind, NArgs, Opcode};
use om_formula::{Expr, Key, Names, PluginFunctionId};

use super::fields::{is_aggregate, FieldTarget};
use super::host::{CalcHost, CalcLimits, CustomFunction, HostFunction, Pending};
use super::vars::{Repetition, VarScope};
use super::Context;
use crate::functions::{self, Implementation};
use crate::number::{clamp_user_precision, DEFAULT_PRECISION};
use crate::ops::{self, CompareOp};
use crate::value::Interrupt;
use crate::{CalcError, Container, Ctx, Date, FirstError, Number, Text, Value};

/// Stack left when a node continues on a new segment, and the segment size.
const RED_ZONE: usize = 256 * 1024;
const SEGMENT: usize = 4 * 1024 * 1024;

/// `While` iterations per loop and custom function calls per evaluation
/// (`DBPolishEnv`, calc-values.md 8.4, 8.5).
const LIMIT: u64 = 50_000;

/// Custom function calls, `While` iterations, `Evaluate`s and unstored
/// calculations between two questions to the host whether to stop
/// ([`CalcHost::interrupted`]): few enough that one costly iteration does
/// not delay the stop much, many enough that asking costs nothing
/// measurable.
const INTERRUPT_CHECK: u32 = 16;

/// `Evaluate` text kept compiled: texts at most this long, at most this
/// many, so that the cache cannot hold more than a few megabytes.
const COMPILED_TEXT: usize = 4096;
const COMPILED_ENTRIES: usize = 1024;

/// `Get ( CalculationRepetitionNumber )`, answered from the context.
const CALCULATION_REPETITION_NUMBER: u8 = 68;

pub(super) mod opc {
    use om_formula::Opcode;

    pub const TRUE: Opcode = Opcode(0x22);
    pub const FALSE: Opcode = Opcode(0x23);
    pub const RANDOM: Opcode = Opcode(0x31);
    pub const GET_AS_BOOLEAN: Opcode = Opcode(0x3e);
    pub const EVALUATION_ERROR: Opcode = Opcode(0x4a);
    pub const IS_EMPTY: Opcode = Opcode(0x4e);
    pub const IS_VALID: Opcode = Opcode(0x4f);
    pub const EXTEND: Opcode = Opcode(0x88);
    pub const LAST: Opcode = Opcode(0x8a);
    pub const GET_NTH_RECORD: Opcode = Opcode(0x8b);
    pub const PI: Opcode = Opcode(0x97);
    pub const IS_VALID_EXPRESSION: Opcode = Opcode(0xde);
    pub const GET_FIELD_NAME: Opcode = Opcode(3201);
    pub const GET_BASE_TABLE_NAME: Opcode = Opcode(3216);
    pub const LOOKUP_LOWER: Opcode = Opcode(188);
    pub const LOOKUP_HIGHER: Opcode = Opcode(189);
}

/// Error 7 (`While` iterations or custom function calls over the limit, or
/// the evaluation past its [`CalcLimits`]), 1224 (a field without a record)
/// or 1 (the host stopped the evaluation): the evaluation stops and its
/// result is `?`; `EvaluationError` and `IsValid` do not catch it
/// (calc-values.md 7.2).
#[derive(Debug)]
pub(super) struct Abort {
    pub error: CalcError,
    /// Unknown execution must suspend callers too, even when a nested
    /// unstored calculation normally converts errors into invalid values.
    pub pending: bool,
}

impl Abort {
    pub fn error(error: CalcError) -> Self {
        Self {
            error,
            pending: false,
        }
    }

    pub fn pending() -> Self {
        Self {
            error: CalcError::from_code(1224).expect("nonzero"),
            pending: true,
        }
    }

    /// Whether the abort ends the evaluations around a nested one too (an
    /// unstored calculation otherwise turns it into its `?` value): data
    /// still being fetched, or the host's stop.
    pub fn propagates(&self) -> bool {
        self.pending || self.error == CalcError::CANCELED
    }
}

pub(super) type Flow<T> = Result<T, Abort>;

/// The value of an expression where a field reference is allowed: a field
/// not read yet, a value, or both (a variable bound to a field holds the
/// value read when it was bound and the field, calc-values.md 13).
#[derive(Clone, Debug)]
pub(super) struct Operand {
    pub value: Option<Value>,
    pub field: Option<FieldTarget>,
    /// A bound value whose inputs are still being fetched. Keep this on
    /// locals and custom-function parameters after the original read ends.
    pending: bool,
}

impl Operand {
    pub fn value(value: Value) -> Operand {
        Operand {
            value: Some(value),
            field: None,
            pending: false,
        }
    }

    pub fn field(field: FieldTarget) -> Operand {
        Operand {
            value: None,
            field: Some(field),
            pending: false,
        }
    }

    fn empty() -> Operand {
        Operand::value(Value::empty_text())
    }

    /// The operand with a field that is not the caller's own operand
    /// (`Evaluate` and `GetField` text, a custom function's result): reading
    /// it does not count toward the caller's empty-input rule.
    pub fn unobserved(mut self) -> Operand {
        if let Some(field) = &mut self.field {
            field.observe = false;
        }
        self
    }
}

/// What a node is evaluated for: its value, an operand (a field position),
/// and whether a tail call may return instead of calling.
#[derive(Clone, Copy)]
pub(super) struct Want {
    pub operand: bool,
    pub tail: bool,
}

impl Want {
    pub const VALUE: Want = Want {
        operand: false,
        tail: false,
    };
    pub const OPERAND: Want = Want {
        operand: true,
        tail: false,
    };
}

pub(super) enum Done {
    Operand(Operand),
    /// A tail call to make in place of the running function.
    Tail(Arc<CustomFunction>, Vec<Operand>),
}

impl Done {
    pub fn operand(self) -> Operand {
        match self {
            Done::Operand(op) => op,
            // Only custom function bodies are evaluated with `tail`.
            Done::Tail(..) => unreachable!("tail call outside a function body"),
        }
    }
}

#[derive(Clone, Copy)]
struct Limits {
    calls: u64,
    call_limit: u64,
    loop_limit: u64,
}

impl Limits {
    /// A formula's own limits: the formula counts as the first call, so
    /// 49,999 custom function calls succeed (calc-values.md 8.5).
    fn formula() -> Limits {
        Limits {
            calls: 1,
            call_limit: LIMIT,
            loop_limit: LIMIT,
        }
    }

    /// `SetRecursion ( expr ; n )`: n calls and n iterations per loop, with
    /// a count of its own (the calls inside do not count outside).
    fn recursion(n: u64) -> Limits {
        Limits {
            calls: 0,
            call_limit: n,
            loop_limit: n,
        }
    }
}

/// The locals of the running custom function or formula, by slot
/// (calc-encoding.md 5.5): a Let or While binding declares its slot, a
/// `SetVariable` binding assigns an earlier one. A scope's slots keep their
/// values after it ends, as in FileMaker: a `Let` in a `While` condition
/// that reuses a slot of the loop's logic overwrites what the result reads
/// (calc-values.md 13).
#[derive(Default)]
struct Frame {
    slots: Vec<Operand>,
    /// Slots in scope where the code stands: where a binding list starts.
    top: usize,
}

/// The fields a formula read while it ran, for "Do not evaluate if all
/// referenced fields are empty" (calc-values.md 8.8): an unchosen branch
/// reads nothing, and a formula's own field operands count, not the ones of
/// `Evaluate` text, a custom function's body or a nested calculation.
#[derive(Clone, Copy, Default)]
pub(super) struct FieldReads {
    /// A field was read.
    pub any: bool,
    /// One of them was not empty (a container counts, empty or not, and a
    /// failed read is `?`).
    pub nonempty: bool,
    /// A field inspection function ran (`IsEmpty`, `IsValid`,
    /// `GetAsBoolean`, `GetFieldName`, `GetBaseTableName`): the formula's
    /// result stands whatever it read.
    pub forced: bool,
}

impl FieldReads {
    /// Whether a formula that does not evaluate always gives the empty
    /// value instead of its result.
    pub fn all_empty(self) -> bool {
        self.any && !self.nonempty && !self.forced
    }
}

impl Frame {
    /// A custom function's frame: its parameters in slots 0...
    fn with_parameters(args: Vec<Operand>) -> Frame {
        Frame {
            top: args.len(),
            slots: args,
        }
    }

    fn set(&mut self, slot: usize, value: Operand) {
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, Operand::empty);
        }
        self.slots[slot] = value;
    }

    fn get(&self, slot: usize) -> Operand {
        self.slots.get(slot).cloned().unwrap_or_else(Operand::empty)
    }
}

/// Text compiled for `Evaluate`: the text, the table occurrence and the
/// file ([`Context::file`]) it is compiled in.
type CompileKey = (String, Option<u64>, Option<u64>);

pub(super) struct Solver<'h> {
    pub host: &'h mut dyn CalcHost,
    pub today: Date,
    pub precision: u16,
    pub errors: FirstError,
    /// Set when a related field was read with no related record; `IsValid`
    /// then reports 0 although no error is recorded.
    pub invalid: bool,
    pub context: Context,
    /// Unstored calculations being computed, innermost last.
    pub depth: u32,
    /// What the formula being solved has read so far.
    pub field_reads: FieldReads,
    limits: Limits,
    frame: Frame,
    /// Compiled `Evaluate`, `GetField` and `IsValidExpression` text.
    compiled: HashMap<CompileKey, Result<Rc<Expr>, CalcError>>,
    /// The host's pending data when the evaluation started, and at the last
    /// custom function call or `While` iteration ([`Self::check_pending`]).
    pending_start: Pending,
    pending_seen: Pending,
    /// Counts toward the next question whether to stop
    /// ([`Self::check_interrupt`]).
    ticks: u32,
    /// The host's limits on what the evaluation builds.
    budget: CalcLimits,
    /// Custom function calls and `Evaluate`s running, nested.
    nesting: u32,
}

impl<'h> Solver<'h> {
    pub fn new(host: &'h mut dyn CalcHost, context: Context) -> Solver<'h> {
        let today = host.today();
        let pending = host.pending();
        let budget = host.limits();
        Solver {
            host,
            today,
            precision: DEFAULT_PRECISION,
            errors: FirstError::default(),
            invalid: false,
            context,
            depth: 0,
            field_reads: FieldReads::default(),
            limits: Limits::formula(),
            frame: Frame::default(),
            compiled: HashMap::new(),
            pending_start: pending,
            pending_seen: pending,
            ticks: 0,
            budget,
            nesting: 0,
        }
    }

    /// Conversions in the locale of the file owning the formula being solved
    /// (calc-values.md 9.3), which changes with the context's occurrence.
    pub fn cx(&self) -> Ctx<'_> {
        Ctx {
            locale: self.host.locale_for(&self.context),
            today: self.today,
            precision: self.precision,
            text_limit: self.budget.text,
            interrupt: Interrupt::of(&*self.host),
        }
    }

    /// Records `error` and gives the `?` result.
    pub fn fail(&mut self, error: CalcError) -> Value {
        self.errors.record(error);
        Value::invalid()
    }

    pub fn check(&mut self, result: Result<Value, CalcError>) -> Value {
        result.unwrap_or_else(|e| self.fail(e))
    }

    /// [`Self::check`] for a value an operator or function built: text
    /// past the text limit, a function that refused to build it (error 7)
    /// or a function the host stopped (error 1) ends the evaluation.
    fn built(&mut self, result: Result<Value, CalcError>) -> Flow<Value> {
        match result {
            Err(e) if e == CalcError::OUT_OF_MEMORY || e == CalcError::CANCELED => {
                Err(Abort::error(e))
            }
            Ok(Value::Text(t)) if t.size() > self.budget.text => {
                Err(Abort::error(CalcError::OUT_OF_MEMORY))
            }
            r => Ok(self.check(r)),
        }
    }

    /// Runs `run` one level deeper in custom function calls and
    /// `Evaluate`s, within [`CalcLimits::nesting`].
    fn nested<R>(&mut self, run: impl FnOnce(&mut Self) -> Flow<R>) -> Flow<R> {
        if self.nesting >= self.budget.nesting {
            return Err(Abort::error(CalcError::OUT_OF_MEMORY));
        }
        self.nesting += 1;
        let out = run(self);
        self.nesting -= 1;
        out
    }

    /// Solves `expr` as a formula of its own (the root, `Evaluate` text, an
    /// unstored calculation): no locals in scope, the default limits and
    /// field reads of its own.
    pub fn solve(&mut self, expr: &Expr, operand: bool) -> Flow<Operand> {
        let want = Want {
            operand,
            tail: false,
        };
        self.in_formula(|solver| solver.eval_as(expr, want).map(Done::operand))
            .0
    }

    /// Solves `expr` as [`Self::solve`] does, to its value (reading a field
    /// it ends on), with the fields it read.
    pub fn solve_reads(&mut self, expr: &Expr) -> (Flow<Value>, FieldReads) {
        self.in_formula(|solver| solver.eval(expr))
    }

    /// Runs `run` as a formula of its own, giving back what it read; the
    /// caller's reads and frame are restored.
    fn in_formula<R>(&mut self, run: impl FnOnce(&mut Self) -> Flow<R>) -> (Flow<R>, FieldReads) {
        let frame = mem::take(&mut self.frame);
        let limits = mem::replace(&mut self.limits, Limits::formula());
        let caller = mem::take(&mut self.field_reads);
        let out = run(self);
        let reads = mem::replace(&mut self.field_reads, caller);
        self.frame = frame;
        self.limits = limits;
        (out, reads)
    }

    /// The value of an operand, reading its field if it was not read yet.
    pub fn resolve(&mut self, op: Operand) -> Flow<Value> {
        if op.pending {
            self.read_pending_value();
        }
        match (op.value, op.field) {
            (Some(v), _) => Ok(v),
            (None, Some(field)) => self.read(&field),
            (None, None) => Ok(Value::empty_text()),
        }
    }

    pub fn eval(&mut self, e: &Expr) -> Flow<Value> {
        let result = stacker::maybe_grow(RED_ZONE, SEGMENT, || self.eval_node(e));
        self.check_execution()?;
        result
    }

    /// A nested public evaluator can suspend inside a host callback that
    /// otherwise reduces its result to a bool, text or SQL error. Do not
    /// let the caller resume with assignments that callback never reached.
    pub fn check_execution(&mut self) -> Flow<()> {
        if super::execution_pending(self.host) {
            Err(Abort::pending())
        } else {
            Ok(())
        }
    }

    fn eval_node(&mut self, e: &Expr) -> Flow<Value> {
        match e {
            Expr::Call { op, args } => self.call(*op, args),
            Expr::PluginCall { function, args } => self.plugin(*function, args),
            Expr::Empty
            | Expr::Number(_)
            | Expr::Boolean(_)
            | Expr::Text(_)
            | Expr::Variable(_)
            | Expr::Get(_)
            | Expr::Symbol(_)
            | Expr::List(_) => self.leaf(e),
            _ => self.eval_operand(e),
        }
    }

    #[inline(never)]
    fn leaf(&mut self, e: &Expr) -> Flow<Value> {
        Ok(match e {
            Expr::Number(n) | Expr::Boolean(n) => Value::Number(number(n)),
            Expr::Text(t) => Value::Text(Text::plain(t.value.as_str())),
            Expr::Variable(name) => self.variable(name, 1),
            Expr::Get(selector) => self.get(*selector),
            // An unresolved name never stands in stored code (calc-encoding.md 3.1).
            Expr::Symbol(_) => self.fail(CalcError::TABLE_MISSING),
            Expr::List(_) => self.fail(CalcError::LIST_NOT_ALLOWED),
            _ => Value::empty_text(),
        })
    }

    /// The value of a node that can stand for a field.
    #[inline(never)]
    fn eval_operand(&mut self, e: &Expr) -> Flow<Value> {
        let op = self.eval_as(e, Want::VALUE)?.operand();
        self.resolve(op)
    }

    /// Evaluates `e` for `want`: field positions keep field references, and
    /// in a function body's tail positions a tail call is returned to the
    /// caller's loop.
    pub fn eval_as(&mut self, e: &Expr, want: Want) -> Flow<Done> {
        let result = stacker::maybe_grow(RED_ZONE, SEGMENT, || self.eval_as_node(e, want));
        self.check_execution()?;
        result
    }

    /// Dispatch only, like [`Self::call`].
    fn eval_as_node(&mut self, e: &Expr, want: Want) -> Flow<Done> {
        match e {
            Expr::Call { op, args } if *op == Opcode::IF => self.if_(args, want),
            Expr::Case(pairs) => self.case(pairs, want),
            Expr::Choose { test, results } => self.choose(test, results, want),
            Expr::Let { bindings, body } => self.let_(bindings, body, want),
            Expr::CustomCall {
                tail,
                function,
                args,
            } => self.custom_call(*tail && want.tail, function, args, want.operand),
            Expr::Call { .. } if !want.operand => self.value_done(e),
            _ => self.operand_done(e, want),
        }
    }

    #[inline(never)]
    fn value_done(&mut self, e: &Expr) -> Flow<Done> {
        Ok(Done::Operand(Operand::value(self.eval(e)?)))
    }

    #[inline(never)]
    fn operand_done(&mut self, e: &Expr, want: Want) -> Flow<Done> {
        let op = match e {
            Expr::Field(f) | Expr::FieldRef(f) => Operand::field(self.field_target(f)?),
            Expr::Local(slot) => self.local(usize::from(*slot)),
            Expr::Call { op, args }
                if want.operand && (*op == Opcode::GET_REPETITION || *op == opc::EXTEND) =>
            {
                self.field_function_operand(*op, args)?
            }
            Expr::While {
                initial,
                condition,
                logic,
                result,
            } => self.while_(initial, condition, logic, result, want.operand)?,
            Expr::Evaluate {
                args,
                need_field_ref,
            } => self.evaluate(args, want.operand || *need_field_ref != 0)?,
            Expr::GetField { arg, .. } => self.get_field(arg)?,
            _ => Operand::value(self.eval(e)?),
        };
        Ok(Done::Operand(op))
    }

    /// Dispatch only: every arm is a function of its own, so that the
    /// frames on a deep recursion stay small.
    fn call(&mut self, op: Opcode, args: &[Expr]) -> Flow<Value> {
        // Field inspection functions force the formula's result.
        if matches!(
            op,
            opc::IS_EMPTY
                | opc::IS_VALID
                | opc::GET_AS_BOOLEAN
                | opc::GET_FIELD_NAME
                | opc::GET_BASE_TABLE_NAME
        ) {
            self.field_reads.forced = true;
        }
        match op {
            Opcode::NOT | Opcode::AND | Opcode::OR | Opcode::XOR => self.logical(op, args),
            Opcode::ADD
            | Opcode::SUBTRACT
            | Opcode::MULTIPLY
            | Opcode::DIVIDE
            | Opcode::POWER
            | Opcode::NEGATE
            | Opcode::CONCAT
            | Opcode::LESS
            | Opcode::LESS_OR_EQUAL
            | Opcode::LESS_OR_EQUAL_SIGN
            | Opcode::EQUAL
            | Opcode::NOT_EQUAL
            | Opcode::NOT_EQUAL_SIGN
            | Opcode::GREATER_OR_EQUAL
            | Opcode::GREATER_OR_EQUAL_SIGN
            | Opcode::GREATER => self.operator(op, args),
            Opcode::PARAGRAPH => Ok(Value::text("\r")),
            Opcode::IF => self.if_value(args),
            Opcode::SET_PRECISION => self.set_precision(args),
            Opcode::SET_RECURSION => self.set_recursion(args),
            Opcode::SET_VARIABLE => self.set_variable(args),
            Opcode::GET_VAR_REP => self.get_var_rep(args),
            opc::EVALUATION_ERROR => self.evaluation_error(arg(args, 0)),
            opc::IS_VALID => self.is_valid(arg(args, 0)),
            opc::IS_VALID_EXPRESSION => self.is_valid_expression(arg(args, 0)),
            opc::IS_EMPTY | Opcode::SELF | opc::RANDOM => self.special(op, args),
            Opcode::GET_SUMMARY
            | Opcode::GET_REPETITION
            | Opcode::LOOKUP
            | Opcode::LOOKUP_NEXT
            | opc::EXTEND
            | opc::LAST
            | opc::GET_NTH_RECORD
            | opc::GET_FIELD_NAME
            | opc::GET_BASE_TABLE_NAME => self.field_function(op, args),
            _ => self.function(op, args),
        }
    }

    #[inline(never)]
    fn logical(&mut self, op: Opcode, args: &[Expr]) -> Flow<Value> {
        let left = if matches!(op, Opcode::AND | Opcode::OR) {
            self.condition(arg(args, 0))?.is_true(&self.cx())
        } else {
            self.eval(arg(args, 0))?.is_true(&self.cx())
        };
        let value = match op {
            Opcode::NOT => !left,
            // `and` skips its right side when the left is false, `or` when
            // it is true; `xor` evaluates both (calc-values.md 8.1).
            Opcode::AND | Opcode::OR if left != (op == Opcode::AND) => left,
            Opcode::XOR => left != self.eval(arg(args, 1))?.is_true(&self.cx()),
            _ => self.eval(arg(args, 1))?.is_true(&self.cx()),
        };
        Ok(Value::boolean(value))
    }

    #[inline(never)]
    fn operator(&mut self, op: Opcode, args: &[Expr]) -> Flow<Value> {
        let a = self.eval(arg(args, 0))?;
        if op == Opcode::NEGATE {
            return Ok(ops::negate(&a, &self.cx()));
        }
        let b = self.eval(arg(args, 1))?;
        let cx = self.cx();
        let r = match op {
            Opcode::ADD => ops::add(&a, &b, &cx),
            Opcode::SUBTRACT => ops::subtract(&a, &b, &cx),
            Opcode::DIVIDE => ops::divide(&a, &b, &cx),
            Opcode::POWER => ops::power(&a, &b, &cx),
            Opcode::MULTIPLY => Ok(ops::multiply(&a, &b, &cx)),
            Opcode::CONCAT => ops::concat_within(a, b, &cx),
            _ => Ok(ops::compare_op(&a, compare_op(op), &b, &cx)),
        };
        self.built(r)
    }

    #[inline(never)]
    fn if_value(&mut self, args: &[Expr]) -> Flow<Value> {
        let op = self.if_(args, Want::VALUE)?.operand();
        self.resolve(op)
    }

    #[inline(never)]
    fn special(&mut self, op: Opcode, args: &[Expr]) -> Flow<Value> {
        Ok(match op {
            opc::IS_EMPTY => Value::boolean(self.eval(arg(args, 0))?.is_empty()),
            Opcode::SELF => match self.host.self_value(&self.context) {
                Ok(value) => value,
                Err(error) => self.fail(error),
            },
            _ => Value::Number(Number::from_f64(self.host.random(), DEFAULT_PRECISION)),
        })
    }

    /// Arguments in source order; a `[ ... ]` group (`Substitute`,
    /// `JSONSetElement`) is spliced into the list. A function stored with a
    /// count (`counted`) evaluates them last first, as FileMaker does
    /// (calc-values.md 13).
    fn arguments(&mut self, args: &[Expr], counted: bool) -> Flow<Vec<Value>> {
        let mut values = Vec::with_capacity(args.len());
        let mut push = |solver: &mut Self, e: &Expr| -> Flow<()> {
            match e {
                Expr::List(items) if counted => {
                    for item in items.iter().rev() {
                        values.push(solver.eval(item)?);
                    }
                }
                Expr::List(items) => {
                    for item in items {
                        values.push(solver.eval(item)?);
                    }
                }
                _ => values.push(solver.eval(e)?),
            }
            Ok(())
        };
        if counted {
            for a in args.iter().rev() {
                push(self, a)?;
            }
        } else {
            for a in args {
                push(self, a)?;
            }
        }
        if counted {
            values.reverse();
        }
        Ok(values)
    }

    /// Functions, keywords and constants: the library, the evaluator's own
    /// fallbacks, then the host.
    #[inline(never)]
    fn function(&mut self, op: Opcode, args: &[Expr]) -> Flow<Value> {
        let Some(info) = op.info() else {
            return Ok(self.fail(CalcError::FUNCTION_NOT_FOUND));
        };
        let name = info.name;
        if info.kind == Kind::Constant {
            if let Some(k) = functions::keyword(name) {
                return Ok(Value::number(k));
            }
            // Constants no library module owns (calc-values.md 13).
            match op {
                opc::TRUE | opc::LOOKUP_HIGHER => return Ok(Value::boolean(true)),
                opc::FALSE | opc::LOOKUP_LOWER => return Ok(Value::boolean(false)),
                _ => {}
            }
        } else if let Some(f) = functions::lookup(name) {
            return self.library(op, f, args);
        } else {
            match op {
                Opcode::GET_AS_NUMBER => {
                    let v = self.eval(arg(args, 0))?;
                    return Ok(match v {
                        Value::Text(t) if t.is_empty() => Value::Text(t),
                        v => Value::Number(v.to_number(&self.cx())),
                    });
                }
                opc::GET_AS_BOOLEAN => {
                    let v = self.eval(arg(args, 0))?;
                    return Ok(Value::boolean(v.is_true(&self.cx())));
                }
                opc::PI => return Ok(Value::Number(Number::pi(self.precision))),
                _ => {}
            }
        }
        // The host answers the rest; an aggregate passes every value its
        // arguments stand for, in order.
        let values = if is_aggregate(op) {
            match self.aggregate_arguments(op, args)? {
                Some(lists) => lists.into_iter().flatten().collect(),
                None => return Ok(Value::invalid()),
            }
        } else if name.eq_ignore_ascii_case("ExecuteSQL")
            || name.eq_ignore_ascii_case("ExecuteSQLe")
            || name.eq_ignore_ascii_case("GetLayoutObjectAttribute")
        {
            self.execution_arguments(name, args, counted(op))?
        } else {
            self.arguments(args, counted(op))?
        };
        let (v, e) = self
            .host
            .call_reporting(HostFunction::Named(name), &values, &self.context);
        // A host function that refused to build its text past the limit
        // (error 7) ends the evaluation, as the evaluator's own do.
        if e == Some(CalcError::OUT_OF_MEMORY) {
            return self.built(Err(CalcError::OUT_OF_MEMORY));
        }
        if let Some(e) = e {
            self.errors.record(e);
        }
        self.built(Ok(v))
    }

    /// These inputs select calculations that can assign variables. SQL's
    /// output separators only format results and can batch independent
    /// reads, as ordinary function arguments do.
    fn execution_arguments(
        &mut self,
        name: &str,
        args: &[Expr],
        counted: bool,
    ) -> Flow<Vec<Value>> {
        let sql =
            name.eq_ignore_ascii_case("ExecuteSQL") || name.eq_ignore_ascii_case("ExecuteSQLe");
        let args: Vec<&Expr> = args
            .iter()
            .flat_map(|expr| match expr {
                Expr::List(items) => items.as_slice(),
                _ => std::slice::from_ref(expr),
            })
            .collect();
        let mut values = Vec::with_capacity(args.len());
        for position in 0..args.len() {
            let index = if counted {
                args.len() - position - 1
            } else {
                position
            };
            let value = if !sql || index == 0 || index >= 3 {
                self.condition(args[index])?
            } else {
                self.eval(args[index])?
            };
            values.push(value);
        }
        if counted {
            values.reverse();
        }
        Ok(values)
    }

    #[inline(never)]
    fn library(&mut self, op: Opcode, f: &functions::Function, args: &[Expr]) -> Flow<Value> {
        let r = match f.implementation {
            Implementation::Pure(pure) => {
                let mut values = self.arguments(args, counted(op))?;
                if functions::reads_container_data(f.name)
                    && !functions::answers_from_reference(f.name, &values, &self.cx())
                {
                    if let Some(v) = self.load_container(f.name, &mut values) {
                        return Ok(v);
                    }
                }
                pure(&values, &self.cx())
            }
            Implementation::Aggregate(f) => match self.aggregate_arguments(op, args)? {
                Some(lists) => f(&lists, &self.cx()),
                None => return Ok(Value::invalid()),
            },
        };
        self.built(r)
    }

    /// Arguments of a function that reads container data, held only by
    /// their reference (a hosted container field's library member): the
    /// host loads their data, or for `VerifyContainer` says whether it is
    /// intact. Returns the result when that settles it (an error from the
    /// host makes it `?`).
    fn load_container(&mut self, name: &str, values: &mut [Value]) -> Option<Value> {
        for (i, value) in values.iter_mut().enumerate() {
            let Value::Container(c) = &*value else {
                continue;
            };
            if !c.is_unloaded() {
                continue;
            }
            match self.host.container_data(c, &self.context) {
                Ok(stored) if i == 0 && name.eq_ignore_ascii_case("VerifyContainer") => {
                    return Some(if stored.external {
                        Value::boolean(stored.intact)
                    } else {
                        Value::invalid()
                    });
                }
                Ok(stored) if stored.external && !stored.intact => {
                    return Some(self.fail(CalcError::NOT_SUPPORTED));
                }
                Ok(stored) => {
                    *value = Value::Container(Container {
                        verified: stored.external.then_some(stored.intact),
                        library: c.library.clone(),
                        ..stored.data
                    });
                }
                Err(e) => return Some(self.fail(e)),
            }
        }
        None
    }

    #[inline(never)]
    fn plugin(&mut self, id: PluginFunctionId, args: &[Expr]) -> Flow<Value> {
        let values = self.arguments(args, true)?;
        let function = match catalog::builtin(id) {
            Some(b) => HostFunction::Builtin(b),
            None => HostFunction::Plugin(id),
        };
        let (v, e) = self.host.call_reporting(function, &values, &self.context);
        if let Some(e) = e {
            self.errors.record(e);
        }
        self.built(Ok(v))
    }

    fn get(&mut self, selector: u8) -> Value {
        if selector == CALCULATION_REPETITION_NUMBER {
            return Value::number(i64::from(self.context.repetition));
        }
        let r = self.host.get(selector, &self.context);
        self.check(r)
    }

    // Control forms (calc-values.md 8.1).

    pub(super) fn dependency_mark(&mut self) -> (u64, u64) {
        let reads = self.host.pending().reads;
        let local = self.host.variables(VarScope::Local).pending_reads();
        let global = self.host.variables(VarScope::Global).pending_reads();
        (reads, local.wrapping_add(global))
    }

    fn read_pending_value(&mut self) {
        // A host may reborrow either or both maps. Recording in both keeps
        // a nested value's provenance in whichever scope it shares. If the
        // maps alias, only the change in the counter matters.
        for scope in [VarScope::Local, VarScope::Global] {
            self.host.variables(scope).read_pending_value();
        }
    }

    /// An execution input must be known before selecting a target, a
    /// fallback, or the environment in which a body will run.
    pub(super) fn check_decision(&mut self, before: (u64, u64)) -> Flow<()> {
        if self.dependency_mark() != before {
            return Err(Abort::pending());
        }
        Ok(())
    }

    /// Error predicates may catch a temporary missing value, and an
    /// unstored calculation may restore its caller's error state. Neither
    /// makes its condition known. Defer only decisions that consumed a
    /// pending input; independent loop indices can still discover a batch
    /// of reads in the same pass.
    pub(super) fn condition(&mut self, test: &Expr) -> Flow<Value> {
        let before = self.dependency_mark();
        let value = self.eval(test)?;
        self.check_decision(before)?;
        Ok(value)
    }

    #[inline(never)]
    fn if_(&mut self, args: &[Expr], want: Want) -> Flow<Done> {
        let test = self.condition(arg(args, 0))?;
        let branch = if test.is_true(&self.cx()) { 1 } else { 2 };
        self.eval_as(arg(args, branch), want)
    }

    #[inline(never)]
    fn case(&mut self, pairs: &[(Expr, Expr)], want: Want) -> Flow<Done> {
        for (test, result) in pairs {
            let value = self.condition(test)?;
            if value.is_true(&self.cx()) {
                return self.eval_as(result, want);
            }
        }
        Ok(Done::Operand(Operand::empty()))
    }

    #[inline(never)]
    fn choose(&mut self, test: &Expr, results: &[Expr], want: Want) -> Flow<Done> {
        let index = self.condition(test)?;
        let cx = self.cx();
        if index.to_number(&cx).is_nan() {
            return Ok(Done::Operand(Operand::value(Value::invalid())));
        }
        // Truncated; empty and text without digits count as 0.
        let chosen = index
            .numeric_operand(&cx)
            .to_i64()
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| results.get(i));
        match chosen {
            Some(result) => self.eval_as(result, want),
            None => Ok(Done::Operand(Operand::empty())),
        }
    }

    // Let, While and slots (calc-values.md 8.2, 8.4; calc-encoding.md 5.5).

    fn local(&self, slot: usize) -> Operand {
        self.frame.get(slot)
    }

    /// Evaluates a binding's value (or a custom function argument) eagerly:
    /// a field keeps its reference along with the value read now.
    fn bind(&mut self, e: &Expr) -> Flow<Operand> {
        let before = self.dependency_mark();
        let mut op = self.eval_as(e, Want::OPERAND)?.operand();
        if op.value.is_none() {
            if let Some(field) = op.field {
                op.value = Some(self.read(&field)?);
            }
        }
        op.pending |= self.dependency_mark() != before;
        Ok(op)
    }

    /// Binding `i` of a list starting at `base` is slot `base + i`; a
    /// `SetVariable` binding (a `$` variable, a name bound again) assigns
    /// and declares nothing.
    fn bindings(&mut self, bindings: &[Expr], base: usize) -> Flow<()> {
        for (i, b) in bindings.iter().enumerate() {
            self.frame.top = base + i;
            match b {
                Expr::Call { op, args } if *op == Opcode::SET_VARIABLE => {
                    self.set_variable(args)?;
                }
                _ => {
                    let value = self.bind(b)?;
                    self.frame.set(base + i, value);
                }
            }
        }
        self.frame.top = base + bindings.len();
        Ok(())
    }

    #[inline(never)]
    fn let_(&mut self, bindings: &[Expr], body: &Expr, want: Want) -> Flow<Done> {
        let base = self.frame.top;
        self.bindings(bindings, base)?;
        let out = self.eval_as(body, want);
        self.frame.top = base;
        out
    }

    #[inline(never)]
    fn while_(
        &mut self,
        initial: &[Expr],
        condition: &Expr,
        logic: &[Expr],
        result: &Expr,
        operand: bool,
    ) -> Flow<Operand> {
        let base = self.frame.top;
        self.bindings(initial, base)?;
        let looped = base + initial.len();
        // The logic's names read empty until it runs.
        for slot in looped..looped + logic.len() {
            self.frame.set(slot, Operand::empty());
        }
        let mut iterations = 0u64;
        loop {
            self.frame.top = looped;
            // A condition that is `?` is true (calc-values.md 8.4).
            let test = self.condition(condition)?;
            if !test.is_true(&self.cx()) {
                break;
            }
            iterations += 1;
            if iterations > self.limits.loop_limit {
                return Err(Abort::error(CalcError::OUT_OF_MEMORY));
            }
            self.check_pending()?;
            self.check_interrupt()?;
            self.bindings(logic, looped)?;
        }
        // The result sees the names bound in the logic.
        self.frame.top = looped + logic.len();
        let want = Want {
            operand,
            tail: false,
        };
        let out = self.eval_as(result, want).map(Done::operand);
        self.frame.top = base;
        out
    }

    // Variables (calc-values.md 8.3).

    fn variable_value(&mut self, name: &str, repetition: Repetition) -> Option<Value> {
        let key = self.host.variable_name(name, &self.context);
        if self
            .host
            .variables(VarScope::of(name))
            .is_pending(&key, &repetition)
        {
            self.read_pending_value();
            return Some(Value::invalid());
        }
        self.host
            .variables(VarScope::of(name))
            .get(&key, repetition)
            .cloned()
    }

    fn variable(&mut self, name: &str, repetition: impl Into<Repetition>) -> Value {
        self.variable_value(name, repetition.into())
            .unwrap_or_else(Value::empty_text)
    }

    /// A variable repetition: truncated, of any size, empty is 1, text
    /// without digits 0; `None` for `?` (the variable is neither read nor
    /// set).
    fn repetition_index(&self, v: &Value) -> Option<Repetition> {
        let cx = self.cx();
        if v.is_empty() {
            return Some(Repetition::from(1));
        }
        if v.to_number(&cx).is_nan() {
            return None;
        }
        Repetition::from_number(&v.numeric_operand(&cx))
    }

    /// `SetVariable ( value ; repetition ; target )`: a `$` variable, or a
    /// Let name bound again in the same list (calc-encoding.md 5.5).
    #[inline(never)]
    fn set_variable(&mut self, args: &[Expr]) -> Flow<Value> {
        match arg(args, 2) {
            Expr::Local(slot) => {
                let value = self.bind(arg(args, 0))?;
                self.frame.set(usize::from(*slot), value.clone());
                self.resolve(value)
            }
            Expr::Variable(name) => {
                let before = self.dependency_mark();
                let value = self.eval(arg(args, 0))?;
                let pending = self.dependency_mark() != before;
                let before = self.dependency_mark();
                let repetition = self.eval(arg(args, 1))?;
                if self.dependency_mark() != before {
                    // Until the target repetition is known, later reads
                    // cannot tell which prior variable value it replaces.
                    return Err(Abort::pending());
                }
                if let Some(rep) = self.repetition_index(&repetition) {
                    let key = self.host.variable_name(name, &self.context);
                    if pending {
                        self.host
                            .variables(VarScope::of(name))
                            .mark_pending(&key, rep);
                    } else {
                        self.host
                            .variables(VarScope::of(name))
                            .set(&key, rep, value.clone());
                    }
                }
                Ok(value)
            }
            _ => Ok(self.fail(CalcError::OPERAND_EXPECTED)),
        }
    }

    /// `$v[n]`: `GetVarRep ( n ; $v )`. Unset (or with a `?` index) it is
    /// an empty number, where `$v` alone is empty text (calc-values.md 13).
    #[inline(never)]
    fn get_var_rep(&mut self, args: &[Expr]) -> Flow<Value> {
        let repetition = self.eval(arg(args, 0))?;
        let Expr::Variable(name) = arg(args, 1) else {
            return Ok(self.fail(CalcError::OPERAND_EXPECTED));
        };
        let value = self
            .repetition_index(&repetition)
            .and_then(|rep| self.variable_value(name, rep));
        Ok(value.unwrap_or_else(|| Value::Number(Number::empty())))
    }

    // Precision and limits (calc-values.md 2.6, 8.4, 8.5).

    /// `SetPrecision ( expr ; n )`: n first (it is stored first), clamped to
    /// 16..400, 16 when empty or `?`; numbers are rounded to n.
    #[inline(never)]
    fn set_precision(&mut self, args: &[Expr]) -> Flow<Value> {
        let n = self.condition(arg(args, 1))?.to_number(&self.cx());
        let digits = if n.is_nan() {
            DEFAULT_PRECISION
        } else {
            clamp_user_precision(n.to_i64().unwrap_or(i64::MAX))
        };
        let saved = mem::replace(&mut self.precision, digits);
        let value = self.eval(arg(args, 0));
        self.precision = saved;
        Ok(match value? {
            Value::Number(n) => Value::Number(n.round_to_precision(digits)),
            other => other,
        })
    }

    /// `SetRecursion ( expr ; n )`: n iterations per loop and n custom
    /// function calls inside `expr`; 0, negative, empty or `?` allow none.
    #[inline(never)]
    fn set_recursion(&mut self, args: &[Expr]) -> Flow<Value> {
        let n = self.condition(arg(args, 1))?.to_number(&self.cx());
        let limit = if n.is_nan() || n.is_negative() {
            0
        } else {
            n.to_i64().map_or(u64::MAX, |i| i as u64)
        };
        let saved = mem::replace(&mut self.limits, Limits::recursion(limit));
        let value = self.eval(arg(args, 0));
        self.limits = saved;
        value
    }

    /// Ends the evaluation once it has read data the host is still fetching
    /// and no new need was listed since the previous custom function call
    /// or `While` iteration: the rest would only run over missing values,
    /// while a loop that lists a need each time goes on, so that one pass
    /// asks for them all ([`CalcHost::pending`]).
    fn check_pending(&mut self) -> Flow<()> {
        let now = self.host.pending();
        if now.reads > self.pending_start.reads && now.needs == self.pending_seen.needs {
            return Err(Abort::pending());
        }
        self.pending_seen = now;
        Ok(())
    }

    /// Ends the evaluation with error 1 when the host asks it to stop
    /// ([`CalcHost::interrupted`]), asking every [`INTERRUPT_CHECK`] custom
    /// function calls, `While` iterations, `Evaluate`s and unstored
    /// calculations: these are what can repeat without bound.
    pub(super) fn check_interrupt(&mut self) -> Flow<()> {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % INTERRUPT_CHECK == 0 && self.host.interrupted() {
            return Err(Abort::error(CalcError::CANCELED));
        }
        Ok(())
    }

    // Errors and validity (calc-values.md 7.2).

    #[inline(never)]
    fn evaluation_error(&mut self, e: &Expr) -> Flow<Value> {
        let saved = mem::take(&mut self.errors);
        let saved_invalid = self.invalid;
        self.eval(e)?;
        let code = self.errors.code();
        self.errors = saved;
        self.invalid = saved_invalid;
        Ok(Value::number(i64::from(code)))
    }

    /// `IsValid`: 0 when evaluating the argument raised an error or read a
    /// related field with no related record; the error does not count
    /// outside. A variable bound to a field keeps the value read when bound.
    #[inline(never)]
    fn is_valid(&mut self, e: &Expr) -> Flow<Value> {
        let saved = mem::take(&mut self.errors);
        let saved_invalid = mem::replace(&mut self.invalid, false);
        let op = self.eval_as(e, Want::OPERAND)?.operand();
        self.resolve(op)?;
        let valid = self.errors.get().is_none() && !self.invalid;
        self.errors = saved;
        self.invalid = saved_invalid;
        Ok(Value::boolean(valid))
    }

    #[inline(never)]
    fn is_valid_expression(&mut self, e: &Expr) -> Flow<Value> {
        let text = self.eval(e)?.to_text(&self.cx()).into_string();
        if is_blank(&text) {
            return Ok(Value::boolean(true));
        }
        Ok(Value::boolean(self.compile_within(&text)?.is_ok()))
    }

    // Evaluate (calc-values.md 8.9).

    /// Compiles text as `Evaluate` does: the names and decimal separator of
    /// the file owning the formula, the context's table occurrence, no
    /// `Self`. The cache key's occurrence and file therefore name the file.
    /// Text longer than [`CalcLimits::formula`] is error 7, since its
    /// compiled tree would take many times its size.
    pub fn compile(&mut self, text: &str) -> Result<Rc<Expr>, CalcError> {
        if text.len() > self.budget.formula() {
            return Err(CalcError::OUT_OF_MEMORY);
        }
        let key = (text.to_owned(), self.context.occurrence, self.context.file);
        if let Some(r) = self.compiled.get(&key) {
            return r.clone();
        }
        let r = compile_text(
            self.host.names_for(&self.context),
            text,
            self.context.occurrence,
            self.host.locale_for(&self.context).decimal_separator == ',',
            false,
        )
        .map(Rc::new)
        .map_err(|e| CalcError::from_code(e.code.number()).unwrap_or(CalcError::OPERAND_EXPECTED));
        if text.len() <= COMPILED_TEXT {
            if self.compiled.len() >= COMPILED_ENTRIES {
                self.compiled.clear();
            }
            self.compiled.insert(key, r.clone());
        }
        r
    }

    /// [`Self::compile`], with text past the formula limit ending the
    /// evaluation.
    pub fn compile_within(&mut self, text: &str) -> Flow<Result<Rc<Expr>, CalcError>> {
        match self.compile(text) {
            Err(CalcError::OUT_OF_MEMORY) => Err(Abort::error(CalcError::OUT_OF_MEMORY)),
            r => Ok(r),
        }
    }

    /// `Evaluate ( text {; triggers} )`: compiled with the file's names and
    /// solved in the caller's context (table, record, repetition, precision,
    /// variables; not Let names) with limits and field reads of its own. The
    /// trigger fields only matter to stored calculations and are not
    /// evaluated.
    #[inline(never)]
    fn evaluate(&mut self, args: &[Expr], operand: bool) -> Flow<Operand> {
        let text = self
            .condition(arg(args, 0))?
            .to_text(&self.cx())
            .into_string();
        if is_blank(&text) {
            return Ok(Operand::empty());
        }
        self.check_interrupt()?;
        match self.compile_within(&text)? {
            Ok(expr) => self
                .nested(|solver| solver.solve(&expr, operand))
                .map(Operand::unobserved),
            Err(e) => Ok(Operand::value(self.fail(e))),
        }
    }

    // Custom functions (calc-values.md 8.5).

    #[inline(never)]
    fn custom_call(
        &mut self,
        tail: bool,
        function: &Key,
        args: &[Expr],
        operand: bool,
    ) -> Flow<Done> {
        let Some(f) = function.id().and_then(|id| self.host.custom_function(id)) else {
            let missing = CalcError::from_code(1224).expect("nonzero");
            return Ok(Done::Operand(Operand::value(self.fail(missing))));
        };
        // Stored last first, and evaluated so.
        let mut values = Vec::with_capacity(args.len());
        for a in args.iter().rev() {
            values.push(self.bind(a)?);
        }
        values.reverse();
        if tail {
            return Ok(Done::Tail(f, values));
        }
        self.invoke(f, values, operand).map(Done::Operand)
    }

    /// Runs a custom function, and the tail calls its body makes in place.
    /// Every call counts against the limit and may end an evaluation that
    /// waits for data ([`Self::check_pending`]). The body's field reads are
    /// its own: its arguments were read by the caller.
    #[inline(never)]
    fn invoke(
        &mut self,
        mut f: Arc<CustomFunction>,
        mut args: Vec<Operand>,
        operand: bool,
    ) -> Flow<Operand> {
        if self.nesting >= self.budget.nesting {
            return Err(Abort::error(CalcError::OUT_OF_MEMORY));
        }
        self.nesting += 1;
        let caller = mem::take(&mut self.field_reads);
        let out = loop {
            self.limits.calls += 1;
            if self.limits.calls > self.limits.call_limit {
                break Err(Abort::error(CalcError::OUT_OF_MEMORY));
            }
            if let Err(abort) = self.check_pending().and_then(|()| self.check_interrupt()) {
                break Err(abort);
            }
            args.resize_with(f.parameters.len(), Operand::empty);
            let saved = mem::replace(&mut self.frame, Frame::with_parameters(args));
            let want = Want {
                operand,
                tail: true,
            };
            let out = self.eval_as(&f.body, want);
            self.frame = saved;
            match out {
                Ok(Done::Operand(op)) => break Ok(op.unobserved()),
                Ok(Done::Tail(next, next_args)) => {
                    f = next;
                    args = next_args;
                }
                Err(abort) => break Err(abort),
            }
        };
        self.field_reads = caller;
        self.nesting -= 1;
        out
    }
}

/// Argument `i`, or an empty argument when a hand-built tree has fewer.
pub(super) fn arg(args: &[Expr], i: usize) -> &Expr {
    args.get(i).unwrap_or(&Expr::Empty)
}

/// Whether a function is stored with a count, its arguments last first
/// (calc-encoding.md 3.1, 7): `VarArgs` functions and the 2300 range.
pub(super) fn counted(op: Opcode) -> bool {
    (2300..2900).contains(&op.0) || op.info().is_some_and(|i| i.nargs == NArgs::VarArgs)
}

fn compare_op(op: Opcode) -> CompareOp {
    match op {
        Opcode::LESS => CompareOp::Lt,
        Opcode::LESS_OR_EQUAL | Opcode::LESS_OR_EQUAL_SIGN => CompareOp::Le,
        Opcode::EQUAL => CompareOp::Eq,
        Opcode::NOT_EQUAL | Opcode::NOT_EQUAL_SIGN => CompareOp::Ne,
        Opcode::GREATER_OR_EQUAL | Opcode::GREATER_OR_EQUAL_SIGN => CompareOp::Ge,
        _ => CompareOp::Gt,
    }
}

fn is_blank(text: &str) -> bool {
    text.chars().all(char::is_whitespace)
}

/// A stored number literal as a value: its words read exactly
/// (`FixPt::ReadAsPolish`, calc-values.md 2.2).
pub(super) fn number(n: &om_formula::Number) -> Number {
    let d = match n {
        om_formula::Number::Empty => return Number::empty(),
        om_formula::Number::NotANumber => return Number::nan(),
        om_formula::Number::Decimal(d) => d,
    };
    let mut bytes = vec![2];
    let count = d.whole.len() as i16;
    bytes.extend_from_slice(&(if d.negative { -count } else { count }).to_be_bytes());
    bytes.extend_from_slice(&d.precision.to_be_bytes());
    let fraction_words = (usize::from(d.precision.clamp(16, 800)) + 2).div_ceil(9);
    let fraction = d.fraction.iter().copied().chain(std::iter::repeat(0));
    for word in d.whole.iter().copied().chain(fraction.take(fraction_words)) {
        bytes.extend_from_slice(&word.to_be_bytes());
    }
    Number::read_polish(&bytes).map_or_else(|_| Number::nan(), |(n, _)| n)
}

/// Stack kept free for the parser. Its recursion stops at
/// [`om_formula::MAX_NESTING`] levels, refusing deeper text with error
/// 1200, and at that depth it took at most 303 KiB in a debug build (159 KiB
/// in a release build; parentheses, function arguments, `Let` and `Case`
/// nested 150 deep), so a fixed amount covers any text: nothing about the
/// text, which may be a script parameter, sizes it.
const PARSE_STACK: usize = 1024 * 1024;

/// Compiles formula text with `om_formula`, on a new stack segment when
/// less than [`PARSE_STACK`] is left.
pub(super) fn compile_text(
    names: &dyn Names,
    text: &str,
    occurrence: Option<u64>,
    decimal_comma: bool,
    support_self: bool,
) -> Result<Expr, om_formula::ParseError> {
    let mut ctx = om_formula::Context::new(names);
    ctx.occurrence = occurrence;
    ctx.decimal_comma = decimal_comma;
    ctx.support_self = support_self;
    stacker::maybe_grow(PARSE_STACK, PARSE_STACK, || {
        om_formula::compile(text, &ctx).map(|c| c.expr)
    })
}
