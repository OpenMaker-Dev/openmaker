//! An in-memory host for tests and tools: base tables with fields
//! (repetitions, types, global storage, unstored calculations), records,
//! table occurrences and relationships, custom functions, `Get` answers and
//! variables.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use om_formula::{Formula, Key, NameTable, Names, ParseError};

use super::host::{
    default_call, CalcHost, CalcLimits, CustomFunction, FieldInfo, HostFunction, Lcg, Storage,
};
use super::solver::compile_text;
use super::vars::{VarScope, Variables};
use super::Context;
use crate::ops::{self, CompareOp};
use crate::{CalcError, Ctx, Date, Locale, Value};

/// How a relationship matches records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    /// `left = right`: a field of the left occurrence's table equal to one
    /// of the right table's (any repetition, by the comparison of `=`).
    Equal { left: u64, right: u64 },
    /// `×`: every record.
    All,
}

#[derive(Clone, Debug)]
struct Relationship {
    left: u64,
    right: u64,
    join: Join,
}

/// A record's values by (field, repetition).
type Values = HashMap<(u64, u32), Value>;

#[derive(Clone, Debug, Default)]
struct Table {
    name: String,
    fields: HashMap<u64, FieldInfo>,
    /// Records in creation order: id and values by (field, repetition).
    records: Vec<(u64, Values)>,
    globals: Values,
}

/// `Get ( RecordNumber )`, `Get ( FoundCount )`, `Get ( TotalRecordCount )`,
/// `Get ( RecordID )`.
const RECORD_NUMBER: u8 = 20;
const FOUND_COUNT: u8 = 7;
const TOTAL_RECORD_COUNT: u8 = 19;
const RECORD_ID: u8 = 37;

/// A function a [`MemoryHost`] answers through [`CalcHost::call`]: the
/// argument values, in a context at the default precision.
pub type HostFn = fn(&[Value], &Ctx) -> Result<Value, CalcError>;

/// A file in memory. Found sets are a table's records in creation order,
/// related sets follow relationship paths in creation order, and
/// `Get ( RecordNumber )`, `FoundCount`, `TotalRecordCount` and `RecordID`
/// come from them (0, 0, 0 and empty without a record); other selectors
/// answer what [`MemoryHost::set_get`] gave, or empty.
#[derive(Clone, Debug)]
pub struct MemoryHost {
    pub locale: Locale,
    pub today: Date,
    pub locals: Variables,
    pub globals: Variables,
    /// Host functions asked for, in order (catalog or plug-in names).
    pub calls: Vec<String>,
    /// Those of [`Self::calls`] this host does not answer.
    pub unsupported: Vec<String>,
    /// Evaluations stop once this time has passed
    /// ([`CalcHost::interrupted`]), as a script's time limit stops them on
    /// OpenMaker Server.
    pub deadline: Option<Instant>,
    /// What one evaluation may build ([`CalcHost::limits`]).
    pub limits: CalcLimits,
    functions: HashMap<String, HostFn>,
    names: NameTable,
    occurrences: HashMap<u64, u64>,
    tables: HashMap<u64, Table>,
    relationships: Vec<Relationship>,
    custom_functions: HashMap<u64, Arc<CustomFunction>>,
    selectors: HashMap<u8, Value>,
    random: Lcg,
}

impl MemoryHost {
    pub fn new(locale: Locale, today: Date) -> MemoryHost {
        MemoryHost {
            locale,
            today,
            locals: Variables::new(),
            globals: Variables::new(),
            calls: Vec::new(),
            unsupported: Vec::new(),
            deadline: None,
            limits: CalcLimits::DEFAULT,
            functions: HashMap::new(),
            names: NameTable::default(),
            occurrences: HashMap::new(),
            tables: HashMap::new(),
            relationships: Vec::new(),
            custom_functions: HashMap::new(),
            selectors: HashMap::new(),
            random: Lcg(0x5EED),
        }
    }

    pub fn add_table(&mut self, id: u64, name: &str) {
        self.tables.insert(
            id,
            Table {
                name: name.to_owned(),
                ..Table::default()
            },
        );
    }

    /// A table occurrence of base table `table`.
    pub fn add_occurrence(&mut self, id: u64, name: &str, table: u64) {
        self.occurrences.insert(id, table);
        self.names.add_occurrence(id, name, table);
    }

    pub fn add_field(&mut self, table: u64, id: u64, name: &str, info: FieldInfo) {
        self.names.add_field(table, id, name);
        self.names
            .set_summary_field(table, id, matches!(info.storage, Storage::Summary));
        self.table_mut(table).fields.insert(id, info);
    }

    /// An unstored calculation field of `occurrence`'s table with the result
    /// type and repetitions of `info`: `source` compiled in `occurrence`
    /// (with the names known so far) and evaluated on each record it is read
    /// on.
    pub fn add_calculation(
        &mut self,
        occurrence: u64,
        id: u64,
        name: &str,
        info: FieldInfo,
        source: &str,
    ) -> Result<(), ParseError> {
        let table = self.occurrences.get(&occurrence).copied().unwrap_or(0);
        self.names.add_field(table, id, name);
        let polish = compile_text(
            &self.names,
            source,
            Some(occurrence),
            self.locale.decimal_separator == ',',
            true,
        )?;
        let formula = Formula {
            context: Some(Key::from_id(occurrence)),
            formatted: None,
            polish,
        };
        let info = FieldInfo {
            storage: Storage::Unstored(Arc::new(formula)),
            ..info
        };
        self.table_mut(table).fields.insert(id, info);
        Ok(())
    }

    /// A record (appended: creation order is the order of these calls).
    pub fn add_record(&mut self, table: u64, id: u64) {
        self.table_mut(table).records.push((id, HashMap::new()));
    }

    /// Sets repetition `rep` of a field on a record; global fields ignore
    /// the record.
    pub fn set(&mut self, table: u64, record: u64, field: u64, rep: u32, value: Value) {
        let t = self.table_mut(table);
        if matches!(
            t.fields.get(&field).map(|f| &f.storage),
            Some(Storage::Global)
        ) {
            t.globals.insert((field, rep), value);
            return;
        }
        if let Some((_, values)) = t.records.iter_mut().find(|(r, _)| *r == record) {
            values.insert((field, rep), value);
        }
    }

    /// A relationship between two occurrences, usable in both directions.
    pub fn relate(&mut self, left: u64, right: u64, join: Join) {
        self.relationships.push(Relationship { left, right, join });
    }

    /// A custom function, its body compiled with the names known so far.
    pub fn add_custom_function(
        &mut self,
        id: u64,
        name: &str,
        parameters: &[&str],
        source: &str,
    ) -> Result<(), ParseError> {
        self.names
            .add_custom_function(id, name, parameters.len() as u16);
        let parameters: Vec<String> = parameters.iter().map(|p| p.to_string()).collect();
        let mut ctx = om_formula::Context::new(&self.names);
        ctx.parameters = &parameters;
        ctx.decimal_comma = self.locale.decimal_separator == ',';
        let body = om_formula::compile(source, &ctx)?.expr;
        let function = CustomFunction {
            name: name.to_owned(),
            parameters,
            body,
        };
        self.custom_functions.insert(id, Arc::new(function));
        Ok(())
    }

    /// The answer to `Get ( selector )`.
    pub fn set_get(&mut self, selector: u8, value: Value) {
        self.selectors.insert(selector, value);
    }

    /// Answers the function `name` (a catalog name: `ExecuteSQL`, a library
    /// function a test stands in for, ...; or a plug-in's or built-in's
    /// name) with `f`.
    pub fn add_function(&mut self, name: &str, f: HostFn) {
        self.functions.insert(name.to_lowercase(), f);
    }

    /// Restarts `Random` from `seed`.
    pub fn seed_random(&mut self, seed: u64) {
        self.random = Lcg(seed);
    }

    fn table_mut(&mut self, table: u64) -> &mut Table {
        self.tables.entry(table).or_default()
    }

    fn table_of(&self, occurrence: u64) -> Option<&Table> {
        self.tables.get(self.occurrences.get(&occurrence)?)
    }

    fn value(&self, table: &Table, record: u64, field: u64, rep: u32) -> Option<Value> {
        let info = table.fields.get(&field)?;
        let stored = match info.storage {
            Storage::Global => table.globals.get(&(field, rep)),
            _ => table
                .records
                .iter()
                .find(|(r, _)| *r == record)
                .and_then(|(_, values)| values.get(&(field, rep))),
        };
        Some(
            stored
                .cloned()
                .unwrap_or_else(|| Value::empty(info.value_type)),
        )
    }

    /// The occurrences from `from` to `to` along relationships (each with
    /// the join oriented from the earlier occurrence), shortest first.
    fn path(&self, from: u64, to: u64) -> Option<Vec<(u64, Join)>> {
        let mut previous: HashMap<u64, (u64, Join)> = HashMap::new();
        let mut queue = VecDeque::from([from]);
        while let Some(at) = queue.pop_front() {
            if at == to {
                let mut path = Vec::new();
                let mut node = to;
                while node != from {
                    let (before, join) = previous[&node];
                    path.push((node, join));
                    node = before;
                }
                path.reverse();
                return Some(path);
            }
            for r in &self.relationships {
                let step = if r.left == at {
                    Some((r.right, r.join))
                } else if r.right == at {
                    let join = match r.join {
                        Join::Equal { left, right } => Join::Equal {
                            left: right,
                            right: left,
                        },
                        Join::All => Join::All,
                    };
                    Some((r.left, join))
                } else {
                    None
                };
                if let Some((next, join)) = step {
                    if next != from && !previous.contains_key(&next) {
                        previous.insert(next, (at, join));
                        queue.push_back(next);
                    }
                }
            }
        }
        None
    }
}

impl CalcHost for MemoryHost {
    fn locale(&self) -> &Locale {
        &self.locale
    }

    fn today(&self) -> Date {
        self.today
    }

    fn names(&self) -> &dyn Names {
        &self.names
    }

    fn interrupted(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    fn limits(&self) -> CalcLimits {
        self.limits
    }

    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        self.table_of(occurrence)?.fields.get(&field).cloned()
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        let table = self.table_of(occurrence).ok_or(CalcError::FIELD_MISSING)?;
        self.value(table, record, field, repetition)
            .ok_or(CalcError::FIELD_MISSING)
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        let missing = || CalcError::from_code(103).expect("nonzero");
        let path = self.path(occurrence, related).ok_or_else(missing)?;
        let cx = Ctx::new(&self.locale, self.today);
        let mut at = occurrence;
        let mut records = vec![record];
        for (next, join) in path {
            let (from, to) = (
                self.table_of(at).ok_or_else(missing)?,
                self.table_of(next).ok_or_else(missing)?,
            );
            let mut found = Vec::new();
            for (candidate, _) in &to.records {
                let matches = match join {
                    Join::All => true,
                    // Any repetition of the left key matches any
                    // repetition of the right one.
                    Join::Equal { left, right } => records.iter().any(|&r| {
                        let left_reps = from.fields.get(&left).map_or(1, |f| f.repetitions);
                        let right_reps = to.fields.get(&right).map_or(1, |f| f.repetitions);
                        (1..=left_reps).any(|left_rep| {
                            let key = self
                                .value(from, r, left, left_rep)
                                .unwrap_or_else(Value::empty_text);
                            !key.is_empty()
                                && (1..=right_reps).any(|rep| {
                                    let other = self
                                        .value(to, *candidate, right, rep)
                                        .unwrap_or_else(Value::empty_text);
                                    !other.is_empty()
                                        && ops::compare_op(&key, CompareOp::Eq, &other, &cx)
                                            .is_true(&cx)
                                })
                        })
                    }),
                };
                if matches {
                    found.push(*candidate);
                }
            }
            records = found;
            at = next;
        }
        Ok(records)
    }

    fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        Ok(self
            .table_of(occurrence)
            .and_then(|t| t.records.get(index))
            .map(|(id, _)| *id))
    }

    fn base_table_name(&self, occurrence: u64) -> Option<&str> {
        self.table_of(occurrence).map(|t| t.name.as_str())
    }

    fn get(&mut self, selector: u8, context: &Context) -> Result<Value, CalcError> {
        if let Some(v) = self.selectors.get(&selector) {
            return Ok(v.clone());
        }
        let table = context.occurrence.and_then(|o| self.table_of(o));
        let records = table.map_or(0, |t| t.records.len() as i64);
        let position = context
            .record
            .and_then(|r| table?.records.iter().position(|(id, _)| *id == r));
        Ok(match (selector, context.record) {
            (RECORD_ID, Some(r)) => Value::number(r as i64),
            (RECORD_ID, None) => Value::empty_text(),
            (RECORD_NUMBER, _) => Value::number(position.map_or(0, |p| p as i64 + 1)),
            (FOUND_COUNT | TOTAL_RECORD_COUNT, Some(_)) => Value::number(records),
            (FOUND_COUNT | TOTAL_RECORD_COUNT, None) => Value::number(0),
            _ => Value::empty_text(),
        })
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        match scope {
            VarScope::Local => &mut self.locals,
            VarScope::Global => &mut self.globals,
        }
    }

    fn custom_function(&self, id: u64) -> Option<Arc<CustomFunction>> {
        self.custom_functions.get(&id).cloned()
    }

    fn random(&mut self) -> f64 {
        self.random.next_f64()
    }

    fn call(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        _context: &Context,
    ) -> Result<Value, CalcError> {
        let name = match function {
            HostFunction::Named(name) => name.to_owned(),
            HostFunction::Builtin(b) => b.name.to_owned(),
            HostFunction::Plugin(id) => {
                format!("{}:{}", String::from_utf8_lossy(&id.plugin), id.function)
            }
        };
        self.calls.push(name.clone());
        if let Some(f) = self.functions.get(&name.to_lowercase()) {
            return f(args, &Ctx::new(&self.locale, self.today));
        }
        let r = default_call(function);
        if matches!(r, Err(e) if e == CalcError::NOT_SUPPORTED) {
            self.unsupported.push(name);
        }
        r
    }
}
