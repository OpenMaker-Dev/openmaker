//! Field operands (calc-values.md 8.6-8.8, 12.1, 13): reading a field on
//! the context's record, a related record or the portal row, repetitions,
//! the functions that take a field (`Extend`, `GetRepetition`, `Last`,
//! `GetNthRecord`, `GetSummary`, `Lookup`, `LookupNext`, `GetFieldName`,
//! `GetBaseTableName`, `GetField`), aggregate arguments and unstored
//! calculations.

use std::mem;

use om_formula::catalog::{NArgs, Opcode};
use om_formula::{Expr, Formula};

use super::host::{CalcHost, FieldId, FieldInfo, LookupDirection, Storage};
use super::solver::{arg, opc, Abort, Flow, Operand, Solver, Want};
use super::Context;
use crate::functions;
use crate::number::DEFAULT_PRECISION;
use crate::ops::{self, CompareOp};
use crate::{CalcError, Value, ValueType};

/// Which repetition a field reference reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rep {
    /// The repetition being calculated: 1 outside a repeating calculation;
    /// in repetition n, n of a repeating field and empty for a field that
    /// does not repeat.
    Current,
    /// `field[n]`, `GetRepetition ( field ; n )`: checked against the
    /// field's repetitions when the reference is made.
    Fixed(u32),
    /// `Extend ( field )`: repetition 1 in every repetition.
    Extend,
}

/// A field reference as an operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FieldTarget {
    pub occurrence: u64,
    pub field: u64,
    pub rep: Rep,
    /// Whether reading it counts toward the formula's empty-input rule
    /// ([`FieldReads`]): not for `Evaluate` and `GetField` text or a custom
    /// function's result.
    ///
    /// [`FieldReads`]: super::solver::FieldReads
    pub observe: bool,
}

/// Where a field's occurrence stands from the context.
enum Place {
    /// The context's own occurrence: its current record.
    Context(u64),
    /// The portal row's occurrence: that record.
    Portal(u64),
    /// Another occurrence, reached through relationships from `record`.
    Related { occurrence: u64, record: u64 },
}

/// Unstored calculations nested deeper than this read as `?`: FileMaker
/// rejects circular definitions, so only a broken file gets here.
const MAX_CALCULATION_DEPTH: u32 = 32;

fn no_record() -> Abort {
    Abort::error(CalcError::from_code(1224).expect("nonzero"))
}

/// The aggregate functions, whose lone argument walks every repetition or
/// related record (calc-encoding.md 5.3).
pub(super) fn is_aggregate(op: Opcode) -> bool {
    matches!(op.0, 0x7f..=0x85 | 0x87 | 0xa0 | 0xa1)
}

impl Solver<'_> {
    /// A field reference of the formula. Without a current record every
    /// field reference ends the evaluation with error 1224 (calc-values.md
    /// 13).
    pub(super) fn field_target(&mut self, f: &om_formula::Field) -> Flow<FieldTarget> {
        if self.context.record.is_none() {
            return Err(no_record());
        }
        Ok(FieldTarget {
            occurrence: f.occurrence.id().unwrap_or(0),
            field: f.field.id().unwrap_or(0),
            rep: Rep::Current,
            observe: true,
        })
    }

    fn place(&self, occurrence: u64) -> Flow<Place> {
        let record = self.context.record.ok_or_else(no_record)?;
        if self.context.occurrence == Some(occurrence) {
            return Ok(Place::Context(record));
        }
        if let Some((portal, row)) = self.context.portal {
            if portal == occurrence {
                return Ok(Place::Portal(row));
            }
        }
        let (from, record) = self
            .host
            .relationship_origin(&self.context, occurrence)
            .ok_or_else(no_record)?;
        Ok(Place::Related {
            occurrence: from,
            record,
        })
    }

    fn info(&mut self, t: &FieldTarget) -> Option<FieldInfo> {
        let info = self.host.field(t.occurrence, t.field);
        if info.is_none() {
            self.errors.record(CalcError::FIELD_MISSING);
            self.invalid = true;
        }
        info
    }

    /// The repetition a reference reads, `None` when it reads empty.
    fn repetition(&self, info: &FieldInfo, rep: Rep) -> Option<u32> {
        let n = match rep {
            Rep::Fixed(n) => n,
            Rep::Extend => 1,
            Rep::Current => match self.context.repetition {
                0 | 1 => 1,
                n if info.repetitions > 1 => n,
                _ if self.context.extend_fields => 1,
                _ => return None,
            },
        };
        (n <= info.repetitions).then_some(n)
    }

    /// The value of a field reference: the context's record, the portal
    /// row or the first related record.
    pub(super) fn read(&mut self, t: &FieldTarget) -> Flow<Value> {
        let Some(info) = self.info(t) else {
            return Ok(Value::invalid());
        };
        // A same-file global's value does not depend on a related record,
        // even when the two table occurrences have no relationship.
        if matches!(info.storage, Storage::Global)
            && self.context.occurrence.is_some_and(|from| {
                self.host.table_file(from) == self.host.table_file(t.occurrence)
            })
        {
            let record = self.context.record.ok_or_else(no_record)?;
            return self.read_record(&info, t, record);
        }
        let record = match self.place(t.occurrence)? {
            Place::Context(r) | Place::Portal(r) => r,
            Place::Related { occurrence, record } => {
                let before = self.dependency_mark();
                let found = self
                    .host
                    .referenced_record(occurrence, record, t.occurrence);
                self.wait_for_field_target(&info, before)?;
                match found {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        self.invalid = true;
                        let value = Value::empty(info.value_type);
                        self.observe(t, &info, &value);
                        return Ok(value);
                    }
                    Err(e) => {
                        let value = self.fail(e);
                        self.observe(t, &info, &value);
                        return Ok(value);
                    }
                }
            }
        };
        self.read_record(&info, t, record)
    }

    /// An unknown record set cannot skip an unstored formula that may
    /// assign variables. Stored reads can still gather independent needs.
    fn wait_for_field_target(&mut self, info: &FieldInfo, before: (u64, u64)) -> Flow<()> {
        if matches!(&info.storage, Storage::Unstored(_)) {
            self.check_decision(before)?;
        }
        Ok(())
    }

    /// The value of `t` on `record`, with `t`'s repetition rule.
    fn read_record(&mut self, info: &FieldInfo, t: &FieldTarget, record: u64) -> Flow<Value> {
        match self.repetition(info, t.rep) {
            Some(rep) => self.fetch(info, t, record, rep),
            None => {
                let value = Value::empty(info.value_type);
                self.observe(t, info, &value);
                Ok(value)
            }
        }
    }

    fn fetch(&mut self, info: &FieldInfo, t: &FieldTarget, record: u64, rep: u32) -> Flow<Value> {
        let value = if let Storage::Unstored(formula) = &info.storage {
            self.unstored(formula, info.value_type, t, record, rep)?
        } else {
            let r =
                self.host
                    .field_value_in_context(&self.context, t.occurrence, record, t.field, rep);
            self.check(r)
        };
        self.observe(t, info, &value);
        Ok(value)
    }

    /// Counts a read of `t` toward the formula's empty-input rule: a
    /// container is a value even when empty (FileMaker evaluates a formula
    /// over an empty photo), and a read that failed is `?`, not empty.
    fn observe(&mut self, t: &FieldTarget, info: &FieldInfo, value: &Value) {
        if t.observe {
            let reads = &mut self.field_reads;
            reads.any = true;
            reads.nonempty |= info.value_type == ValueType::Container || !value.is_empty();
        }
    }

    /// An unstored calculation `t`: repetition `rep` of its formula on
    /// `record`, in its own evaluation (errors, limits and field reads of
    /// its own), converted to its result type; empty when it does not
    /// evaluate always ([`CalcHost::evaluate_always`]) and every field it
    /// read was empty, unless it ran a field inspection function
    /// ([`FieldReads`]).
    ///
    /// [`FieldReads`]: super::solver::FieldReads
    /// [`CalcHost::evaluate_always`]: super::CalcHost::evaluate_always
    fn unstored(
        &mut self,
        formula: &Formula,
        value_type: ValueType,
        t: &FieldTarget,
        record: u64,
        rep: u32,
    ) -> Flow<Value> {
        if self.depth >= MAX_CALCULATION_DEPTH {
            return Ok(Value::invalid());
        }
        self.count_calculation()?;
        self.check_interrupt()?;
        let skip_empty = !self.host.evaluate_always(t.occurrence, t.field);
        let occurrence = formula.occurrence().unwrap_or(t.occurrence);
        // The set `record` was read in: the parent of a related or portal
        // read, else the one this context's record was read in. A formula
        // evaluated from another occurrence than `t`'s has no such set.
        let related_from = if self.context.occurrence == Some(t.occurrence) {
            self.context.related_from
        } else {
            self.host.relationship_origin(&self.context, t.occurrence)
        }
        .filter(|_| occurrence == t.occurrence);
        let context = Context {
            occurrence: Some(occurrence),
            file: self.host.table_file(t.occurrence),
            record: Some(record),
            portal: None,
            portal_row_number: None,
            related_from,
            repetition: rep,
            result_type: Some(value_type),
            self_value: None,
            extend_fields: false,
        };
        let saved_context = mem::replace(&mut self.context, context);
        let saved_errors = mem::take(&mut self.errors);
        let saved_invalid = mem::replace(&mut self.invalid, false);
        let saved_precision = mem::replace(&mut self.precision, DEFAULT_PRECISION);
        self.depth += 1;
        // FileMaker solves the formula, then clears the result.
        let (out, reads) = self.solve_reads(&formula.polish);
        let out = if skip_empty && reads.all_empty() {
            out.map(|_| Value::empty(value_type))
        } else {
            out
        };
        // The result is converted in its own file's locale, before the
        // caller's context (perhaps another file's) is back.
        let value = match out {
            Ok(v) => Ok(v.convert_declared(value_type, &self.cx())),
            Err(abort) if abort.propagates() => Err(abort),
            Err(_) => Ok(Value::invalid()),
        };
        self.depth -= 1;
        self.context = saved_context;
        self.errors = saved_errors;
        self.invalid = saved_invalid;
        self.precision = saved_precision;
        value
    }

    /// Every value a field stands for as an aggregate's lone argument: each
    /// repetition (whatever index the reference has), on the context's
    /// record or on every related record (calc-values.md 8.6, 8.7).
    fn expand(&mut self, t: &FieldTarget) -> Flow<Option<Vec<Value>>> {
        let Some(info) = self.info(t) else {
            return Ok(None);
        };
        let records = match self.place(t.occurrence)? {
            Place::Context(r) => vec![r],
            Place::Portal(_) | Place::Related { .. } => {
                let before = self.dependency_mark();
                let records = self.related(t.occurrence);
                self.wait_for_field_target(&info, before)?;
                match records {
                    Ok(records) => records,
                    Err(e) => {
                        self.errors.record(e);
                        self.observe(t, &info, &Value::invalid());
                        return Ok(None);
                    }
                }
            }
        };
        // Without a related record the field still reads, empty.
        if records.is_empty() {
            self.observe(t, &info, &Value::empty(info.value_type));
        }
        let mut values = Vec::with_capacity(records.len() * info.repetitions as usize);
        for record in records {
            for rep in 1..=info.repetitions {
                values.push(self.fetch(&info, t, record, rep)?);
            }
        }
        Ok(Some(values))
    }

    /// The records of `occurrence` related to the context's record.
    fn related(&mut self, occurrence: u64) -> Result<Vec<u64>, CalcError> {
        let Some((from, record)) = self.host.relationship_origin(&self.context, occurrence) else {
            return Ok(Vec::new());
        };
        let records = self.host.related_records(from, record, occurrence)?;
        if records.is_empty() {
            self.invalid = true;
        }
        Ok(records)
    }

    /// A field operand where a field is required; anything else (a `$`
    /// variable, a value) is error 102.
    fn need_field(&mut self, op: Operand) -> Option<FieldTarget> {
        if op.field.is_none() {
            self.errors.record(CalcError::FIELD_MISSING);
        }
        op.field
    }

    fn field_operand(&mut self, e: &Expr) -> Flow<Option<FieldTarget>> {
        let op = self.eval_as(e, Want::OPERAND)?.operand();
        Ok(self.need_field(op))
    }

    /// `GetRepetition ( field ; n )` and `field[n]` as a reference: n
    /// truncated, error 14 outside the field's repetitions; `Err` holds the
    /// failed value.
    fn repetition_target(&mut self, args: &[Expr]) -> Flow<Result<FieldTarget, Value>> {
        let target = self.field_operand(arg(args, 0))?;
        let n = self.condition(arg(args, 1))?.to_number(&self.cx());
        let Some(mut target) = target else {
            return Ok(Err(Value::invalid()));
        };
        if n.is_nan() {
            return Ok(Err(Value::invalid()));
        }
        let Some(info) = self.info(&target) else {
            return Ok(Err(Value::invalid()));
        };
        match n.to_i64() {
            Some(i) if i >= 1 && i <= i64::from(info.repetitions) => {
                target.rep = Rep::Fixed(i as u32);
                Ok(Ok(target))
            }
            _ => Ok(Err(self.fail(CalcError::OUT_OF_RANGE))),
        }
    }

    /// `Extend` and `GetRepetition` where a field reference is wanted.
    pub(super) fn field_function_operand(&mut self, op: Opcode, args: &[Expr]) -> Flow<Operand> {
        if op == opc::EXTEND {
            return Ok(match self.field_operand(arg(args, 0))? {
                Some(mut t) => {
                    t.rep = Rep::Extend;
                    Operand::field(t)
                }
                None => Operand::value(Value::invalid()),
            });
        }
        Ok(match self.repetition_target(args)? {
            Ok(t) => Operand::field(t),
            Err(v) => Operand::value(v),
        })
    }

    /// The functions that take a field reference.
    pub(super) fn field_function(&mut self, op: Opcode, args: &[Expr]) -> Flow<Value> {
        match op {
            Opcode::GET_REPETITION | opc::EXTEND => {
                let op = self.field_function_operand(op, args)?;
                self.resolve(op)
            }
            opc::LAST => self.last(args),
            opc::GET_NTH_RECORD => self.get_nth_record(args),
            Opcode::GET_SUMMARY => self.get_summary(args),
            Opcode::LOOKUP | Opcode::LOOKUP_NEXT => self.lookup(op, args),
            opc::GET_FIELD_NAME => self.get_field_name(args),
            _ => self.get_base_table_name(args),
        }
    }

    /// `Last ( field )`: the last non-empty repetition, or the last related
    /// record's value.
    fn last(&mut self, args: &[Expr]) -> Flow<Value> {
        let Some(t) = self.field_operand(arg(args, 0))? else {
            return Ok(Value::invalid());
        };
        let Some(values) = self.expand(&t)? else {
            return Ok(Value::invalid());
        };
        let ty = values.first().map_or(ValueType::Text, Value::value_type);
        Ok(values
            .into_iter()
            .rev()
            .find(|v| !v.is_empty())
            .unwrap_or_else(|| Value::empty(ty)))
    }

    /// `GetNthRecord ( field ; n )`: record n of the found set for a field
    /// of the context's table, of the related set otherwise; n truncated,
    /// error 14 outside the set.
    fn get_nth_record(&mut self, args: &[Expr]) -> Flow<Value> {
        let target = self.field_operand(arg(args, 0))?;
        let n = self.condition(arg(args, 1))?.to_number(&self.cx());
        let Some(t) = target else {
            return Ok(Value::invalid());
        };
        if n.is_nan() {
            return Ok(Value::invalid());
        }
        let Some(info) = self.info(&t) else {
            return Ok(Value::invalid());
        };
        let index = match n.to_i64() {
            Some(i) if i >= 1 => (i - 1) as usize,
            _ => return Ok(self.fail(CalcError::OUT_OF_RANGE)),
        };
        let before = self.dependency_mark();
        let record = match self.place(t.occurrence)? {
            Place::Context(_) => self.host.found_record(t.occurrence, index),
            Place::Portal(_) => match self.context.occurrence.zip(self.context.record) {
                Some((from, record)) => self.host.related_record(from, record, t.occurrence, index),
                None => Ok(None),
            },
            Place::Related { occurrence, record } => {
                self.host
                    .related_record(occurrence, record, t.occurrence, index)
            }
        };
        self.wait_for_field_target(&info, before)?;
        Ok(match record {
            Ok(Some(r)) => self.read_record(&info, &t, r)?,
            Ok(None) => self.fail(CalcError::OUT_OF_RANGE),
            Err(e) => self.fail(e),
        })
    }

    fn get_summary(&mut self, args: &[Expr]) -> Flow<Value> {
        let summary = self.field_operand(arg(args, 0))?;
        let break_field = self.field_operand(arg(args, 1))?;
        let (Some(s), Some(b)) = (summary, break_field) else {
            return Ok(Value::invalid());
        };
        let id = |t: FieldTarget| FieldId {
            occurrence: t.occurrence,
            field: t.field,
        };
        let r = self.host.summary(&self.context, id(s), id(b));
        Ok(self.check(r))
    }

    /// `Lookup ( field {; fail} )` and `LookupNext ( field ; Lower/Higher )`:
    /// the related record's value; with none, the fail expression (only
    /// then evaluated) or error 401, and error 103 for a field of the
    /// context's own table.
    fn lookup(&mut self, op: Opcode, args: &[Expr]) -> Flow<Value> {
        // Stored with a count: LookupNext's direction is evaluated first.
        let direction = if op == Opcode::LOOKUP_NEXT {
            Some(self.lookup_direction(arg(args, 1))?)
        } else {
            None
        };
        let Some(t) = self.field_operand(arg(args, 0))? else {
            return Ok(Value::invalid());
        };
        let Some(info) = self.info(&t) else {
            return Ok(Value::invalid());
        };
        let before = self.dependency_mark();
        let found = match self.place(t.occurrence)? {
            Place::Context(_) => Err(CalcError::from_code(103).expect("nonzero")),
            Place::Portal(r) => Ok(r),
            Place::Related { occurrence, record } => {
                match self
                    .host
                    .lookup_record(occurrence, record, t.occurrence, direction)
                {
                    Ok(Some(r)) => Ok(r),
                    Ok(None) => Err(CalcError::from_code(401).expect("nonzero")),
                    Err(e) => Err(e),
                }
            }
        };
        if op == Opcode::LOOKUP && args.len() > 1 {
            self.check_decision(before)?;
        } else {
            self.wait_for_field_target(&info, before)?;
        }
        match found {
            Ok(record) => self.read_record(&info, &t, record),
            Err(_) if op == Opcode::LOOKUP && args.len() > 1 => self.eval(&args[1]),
            Err(e) => Ok(self.fail(e)),
        }
    }

    fn lookup_direction(&mut self, e: &Expr) -> Flow<LookupDirection> {
        match e {
            Expr::Call { op, .. } if *op == opc::LOOKUP_HIGHER => {
                return Ok(LookupDirection::Higher)
            }
            Expr::Call { op, .. } if *op == opc::LOOKUP_LOWER => return Ok(LookupDirection::Lower),
            _ => {}
        }
        // `Higher` is 1 and `Lower` 0.
        let v = self.condition(e)?;
        let higher = Value::number(functions::keyword("Higher").unwrap_or(1));
        let cx = self.cx();
        Ok(
            if ops::compare_op(&v, CompareOp::Eq, &higher, &cx).is_true(&cx) {
                LookupDirection::Higher
            } else {
                LookupDirection::Lower
            },
        )
    }

    /// `GetFieldName ( field )`: `Occurrence::Field`, with `[n]` for a
    /// reference to repetition n > 1.
    fn get_field_name(&mut self, args: &[Expr]) -> Flow<Value> {
        let Some(t) = self.field_operand(arg(args, 0))? else {
            return Ok(Value::invalid());
        };
        let name = self.names_of(&t, |host, context, t| {
            host.qualified_field_name(context, t.occurrence, t.field)
        });
        Ok(match name {
            Some(mut name) => {
                if let Rep::Fixed(n) = t.rep {
                    if n > 1 {
                        name.push_str(&format!("[{n}]"));
                    }
                }
                Value::text(name)
            }
            None => self.fail(CalcError::FIELD_MISSING),
        })
    }

    fn get_base_table_name(&mut self, args: &[Expr]) -> Flow<Value> {
        let Some(t) = self.field_operand(arg(args, 0))? else {
            return Ok(Value::invalid());
        };
        let name = self.names_of(&t, |host, _, t| host.table_name(t.occurrence));
        Ok(match name {
            Some(name) => Value::text(name),
            None => self.fail(CalcError::FIELD_MISSING),
        })
    }

    /// A name of `t`'s table or field, by `lookup`. One the host lacks is
    /// looked up again once the host has the field itself: reading only a
    /// related table's or field's name needs its file as reading its data
    /// does. Asked for one of its fields, a host opens a file not open yet
    /// (a script's, which then answers from the grown schema) or asks for it
    /// and makes the evaluation wait (the window's), rather than answering
    /// it missing (#1123).
    fn names_of(
        &mut self,
        t: &FieldTarget,
        lookup: impl Fn(&dyn CalcHost, &Context, &FieldTarget) -> Option<String>,
    ) -> Option<String> {
        if let Some(name) = lookup(&*self.host, &self.context, t) {
            return Some(name);
        }
        self.host.field(t.occurrence, t.field)?;
        lookup(&*self.host, &self.context, t)
    }

    /// `GetField ( name )`: the text compiled as a field reference
    /// (`Table::Field`, `${Field}`, `Field[n]`) in the context's occurrence;
    /// empty text is empty, anything that is not a field error 102.
    pub(super) fn get_field(&mut self, e: &Expr) -> Flow<Operand> {
        let text = self.condition(e)?.to_text(&self.cx()).into_string();
        if text.is_empty() {
            return Ok(Operand::value(Value::empty_text()));
        }
        let compiled = self.compile_within(&text)?;
        let op = match compiled.as_deref() {
            Ok(Expr::Field(f)) | Ok(Expr::FieldRef(f)) => Operand::field(self.field_target(f)?),
            Ok(Expr::Call { op, args })
                if *op == Opcode::GET_REPETITION
                    && matches!(args.first(), Some(Expr::FieldRef(_))) =>
            {
                match self.repetition_target(args)? {
                    Ok(t) => Operand::field(t),
                    Err(v) => Operand::value(v),
                }
            }
            _ => Operand::value(self.fail(CalcError::FIELD_MISSING)),
        };
        Ok(op.unobserved())
    }

    /// An aggregate-style function's arguments, each expanded to the values
    /// it stands for (`functions::Aggregate`), evaluated last first. For the
    /// aggregate functions a lone argument walks every repetition or related
    /// record and two or more give each one value; other functions taking a
    /// field parameter (`NPV`) expand their field parameters. `None` when a
    /// field argument fails (a `$` variable, a repetition out of range): the
    /// function's result is then `?`.
    pub(super) fn aggregate_arguments(
        &mut self,
        op: Opcode,
        args: &[Expr],
    ) -> Flow<Option<Vec<Vec<Value>>>> {
        let expanded = if is_aggregate(op) {
            usize::from(args.len() == 1)
        } else {
            match op.info().map(|i| i.nargs) {
                Some(NArgs::UnaryField | NArgs::BinaryField) => 1,
                _ => 0,
            }
        };
        let mut lists = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate().rev() {
            if i < expanded {
                match self.field_operand(a)? {
                    Some(t) => match self.expand(&t)? {
                        Some(values) => lists.push(values),
                        None => return Ok(None),
                    },
                    None => return Ok(None),
                }
            } else {
                lists.push(vec![self.eval(a)?]);
            }
        }
        lists.reverse();
        Ok(Some(lists))
    }
}

#[cfg(test)]
mod nested_context_tests {
    use super::*;
    use crate::eval::{CalcHost, MemoryHost, Pending, VarScope, Variables};
    use crate::{Date, Locale};
    use om_formula::Names;
    use std::cell::Cell;

    struct PendingHost {
        inner: MemoryHost,
        reads: Cell<u64>,
    }
    impl CalcHost for PendingHost {
        fn locale(&self) -> &Locale {
            self.inner.locale()
        }
        fn today(&self) -> Date {
            self.inner.today()
        }
        fn names(&self) -> &dyn Names {
            self.inner.names()
        }
        fn field(&self, o: u64, f: u64) -> Option<FieldInfo> {
            self.inner.field(o, f)
        }
        fn field_value(&self, o: u64, r: u64, f: u64, p: u32) -> Result<Value, CalcError> {
            if o == 2 && f == 1 {
                self.reads.set(self.reads.get() + 1);
                return Err(CalcError::NOT_SUPPORTED);
            }
            self.inner.field_value(o, r, f, p)
        }
        fn table_file(&self, o: u64) -> Option<u64> {
            Some(o * 10)
        }
        fn variables(&mut self, s: VarScope) -> &mut Variables {
            self.inner.variables(s)
        }
        fn pending(&self) -> Pending {
            Pending {
                reads: self.reads.get(),
                needs: u64::from(self.reads.get() > 0),
            }
        }
    }
    fn host(inner: &str) -> PendingHost {
        let mut h = MemoryHost::new(Locale::en_us(), Date::from_ymd(2026, 11, 1).unwrap());
        for o in [1, 2] {
            h.add_table(o, if o == 1 { "Root" } else { "Nested" });
            h.add_occurrence(o, if o == 1 { "Root" } else { "Nested" }, o);
            h.add_record(o, 10);
        }
        h.add_field(2, 1, "Missing", FieldInfo::stored(ValueType::Number, 3));
        h.add_calculation(
            2,
            2,
            "Inner",
            FieldInfo::stored(ValueType::Number, 3),
            inner,
        )
        .unwrap();
        PendingHost {
            inner: h,
            reads: Cell::new(0),
        }
    }
    fn restored(inner: &str, pending: bool) {
        let mut h = host(inner);
        let context = Context {
            occurrence: Some(1),
            file: Some(11),
            record: Some(10),
            portal: Some((2, 10)),
            portal_row_number: Some(5),
            related_from: Some((3, 22)),
            repetition: 2,
            result_type: Some(ValueType::Text),
            self_value: Some(Value::number(23)),
            extend_fields: true,
        };
        let before = format!("{context:?}");
        let mut solver = Solver::new(&mut h, context, u64::MAX);
        solver.errors.record(CalcError::DIVIDE_BY_ZERO);
        solver.invalid = true;
        solver.precision = 57;
        solver.depth = 7;
        let out = solver.read(&FieldTarget {
            occurrence: 2,
            field: 2,
            rep: Rep::Fixed(2),
            observe: false,
        });
        if pending {
            assert!(out.unwrap_err().pending);
        } else {
            assert!(out.is_ok());
        }
        assert_eq!(format!("{:?}", solver.context), before);
        assert_eq!(solver.errors.code(), 15);
        assert!(solver.invalid);
        assert_eq!(solver.precision, 57);
        assert_eq!(solver.depth, 7);
        let expr = solver.compile("5").unwrap();
        assert_eq!(
            solver.eval(&expr).unwrap().to_number(&solver.cx()).to_i64(),
            Some(5)
        );
    }
    #[test]
    fn pending_restores_every_nested_context_property() {
        for text in [
            "If ( IsValid ( Nested::Missing ) ; 1 ; 2 )",
            "SetPrecision ( EvaluationError ( If ( Nested::Missing ; 1 ; 2 ) ) ; 90 )",
            "Evaluate ( \"If ( Nested::Missing ; 1 ; 2 )\" )",
            "GetField ( \"Nested::Missing[\" & Nested::Missing & \"]\" )",
        ] {
            restored(text, true);
        }
    }
    #[test]
    fn permanent_abort_restores_context_and_remains_an_invalid_value() {
        restored(
            "SetRecursion ( While ( [ i = 0 ] ; i < 2 ; [ i = i + 1 ] ; i ) ; 1 )",
            false,
        );
    }
}
