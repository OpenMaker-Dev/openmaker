//! Unstored calculations and summary fields in a server script session
//! (calc-values.md 8.8, file-metadata.md 5).
//!
//! An unstored calculation (and a global one) has no value in the record:
//! the evaluator computes it from its formula on the record it is read
//! from, in the formula's table occurrence, through this host, so it sees
//! the session's open record and transaction and follows relationships as
//! any calculation does. The host's `field` ([`ServerHost::field_info`])
//! hands the evaluator the formula; [`ServerHost::read_field`] does the same
//! for reads outside a step's evaluation (sort keys, summarized values,
//! relationship match fields, steps that read a field), through a
//! [`Reader`].
//!
//! A summary field is computed over the window's found set, in its order,
//! by `om_data::summary` (every operation, running summaries, fractions,
//! `GetSummary` break fields): its groups are those of the window's sort
//! when the found set is sorted by the break field. A summary of another
//! table than the window's is reached through a relationship, where
//! FileMaker summarizes the related records; the host cannot tell which
//! record it is read from, so it answers 1225 rather than a wrong value.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use om_calc::eval::{
    evaluate, CalcHost, CalcLimits, Context, CustomFunction, FieldId, FieldInfo, HostFunction,
    Storage, VarScope, Variables,
};
use om_calc::{CalcError, Date, Locale, Value};
use om_data::host::Vars;
use om_data::sql::calc::Variant;
use om_data::summary::{Break, Found, Summary};
use om_formula::{Expr, Names};
use om_hbam::slot::uint_segment;
use om_hbam::{Field, FieldKind};
use om_scripts::specs::{SortEntry, SortSpec};

use super::{values, ServerHost, Window};
use crate::engine::{ReadError, View, NO_LIMIT};

/// Unstored calculations and summaries read inside one another deeper than
/// this read as `?`: FileMaker rejects circular definitions, so only a
/// broken file gets here (as `om_calc`'s own limit for nested unstored
/// calculations).
const MAX_READ_DEPTH: u32 = 32;

/// A summary field's (table, field) and its stored definition.
pub(super) type StoredSummary = ((u64, u64), Vec<u8>);

/// The summary definitions of a file's summary fields, by (table, field),
/// as stored: `[T, 3, 5, F, 09]` (file-metadata.md 5.1), read through
/// `view` for the fields of `catalog`.
pub(super) fn summaries(
    view: &dyn View,
    catalog: &om_hbam::Catalog,
) -> Result<Vec<StoredSummary>, ReadError> {
    let mut out = Vec::new();
    for table in &catalog.tables {
        let t = uint_segment(table.id);
        for field in table.fields.iter().filter(|f| f.kind == FieldKind::Summary) {
            let path: [&[u8]; 5] = [&t, &[3], &[5], &uint_segment(field.id), &[9]];
            let bytes = view
                .subtree(&path, NO_LIMIT)?
                .and_then(|n| n.assembled().map(Cow::into_owned))
                .unwrap_or_default();
            out.push(((table.id, field.id), bytes));
        }
    }
    Ok(out)
}

/// What a summary reads from the found set ([`Found`]), owned.
struct SummaryInput {
    records: Vec<u64>,
    values: Vec<Vec<Value>>,
    weights: Vec<Value>,
    groups: Option<Vec<usize>>,
    sorted: bool,
}

impl SummaryInput {
    fn found(&self) -> Found<'_> {
        Found {
            values: &self.values,
            weights: &self.weights,
            additional_groups: self.groups.as_deref(),
            sorted: self.sorted,
        }
    }

    fn position(&self, record: u64) -> Option<usize> {
        self.records.iter().position(|r| *r == record)
    }
}

impl ServerHost {
    /// The evaluator's view of a field: an unstored or global calculation
    /// with its formula from the file's calculation schema, so reading it
    /// evaluates it. Without the privileges to read records it is a stored
    /// field, whose value is then error 9 (fail closed).
    pub(super) fn field_info(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        let (_, def) = self.schema.field(occurrence, field)?;
        if self.may_view() {
            let calc = self
                .schema
                .calc()
                .and_then(|calc| calc.field(occurrence, field))
                .filter(|calc| calc.is_unstored_calculation() && calc.formula.is_some());
            if let Some(calc) = calc {
                return Some(calc.info());
            }
        }
        let storage = if def.kind == FieldKind::Summary {
            Storage::Summary
        } else if Self::is_global(def) {
            Storage::Global
        } else {
            Storage::Stored
        };
        Some(FieldInfo {
            value_type: values::value_type(def.data_type),
            repetitions: Self::repetitions(def),
            storage,
        })
    }

    /// `field` of `occurrence` on `record` as a calculation reads it: an
    /// unstored calculation evaluated on the record, empty when every field
    /// it references is unless it evaluates always ([`Self::evaluates_always`],
    /// calc-values.md 8.8), a summary over the found set, anything else as
    /// stored. The calculation is read as a reference to the field, which
    /// the evaluator computes with those rules.
    pub(super) fn read_field(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        let info = self
            .field_info(occurrence, field)
            .ok_or(CalcError::FIELD_MISSING)?;
        let Storage::Unstored(_) = &info.storage else {
            return CalcHost::field_value(self, occurrence, record, field, repetition);
        };
        if repetition == 0 || repetition > info.repetitions {
            return Ok(Value::empty(info.value_type));
        }
        let Some(_depth) = self.nested_read() else {
            return Ok(Value::invalid());
        };
        let reference = Expr::Field(om_formula::Field::new(occurrence, field, true));
        let context = Context {
            repetition,
            ..Context::record(occurrence, record)
        };
        Ok(evaluate(&reference, &mut Reader::new(self), &context).value)
    }

    /// Whether unstored calculation `field` is evaluated when every field it
    /// references is empty: its "Do not evaluate if all referenced fields
    /// are empty" option is off (option `0x20`, schema-path.md 5.5), as the
    /// file's calculation schema reads it.
    pub(super) fn evaluates_always(&self, occurrence: u64, field: u64) -> bool {
        self.schema
            .calc()
            .and_then(|calc| calc.field(occurrence, field))
            .map_or(true, om_data::host::FieldDef::evaluates_always)
    }

    /// One more level of nested reads, or `None` past [`MAX_READ_DEPTH`].
    fn nested_read(&self) -> Option<ReadDepth<'_>> {
        let depth = self.reading.get();
        if depth >= MAX_READ_DEPTH {
            return None;
        }
        self.reading.set(depth + 1);
        Some(ReadDepth(&self.reading))
    }

    /// The window when it shows `table`: the found set a summary of that
    /// table is computed over.
    fn summary_window(&self, table: u64) -> Result<&Window, CalcError> {
        self.window
            .as_ref()
            .filter(|w| w.table == table)
            .ok_or(CalcError::NOT_SUPPORTED)
    }

    /// The definition of summary field `field` of `table`.
    fn summary_definition(&self, table: u64, field: u64) -> Summary {
        self.schema
            .summaries
            .get(&(table, field))
            .copied()
            .unwrap_or_else(|| Summary::decode(&[]))
    }

    /// A summary field's value on `record` (file-metadata.md 5.3).
    pub(super) fn summary_value(
        &self,
        occurrence: u64,
        table: u64,
        def: &Field,
        record: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        let summary = self.summary_definition(table, def.id);
        let Some(_depth) = self.nested_read() else {
            return Ok(Value::invalid());
        };
        let input = self.summary_input(occurrence, table, &summary, repetition)?;
        let cx = self.cx();
        let value = summary.value(&input.found(), input.position(record), &cx)?;
        Ok(value.convert_declared(values::value_type(def.data_type), &cx))
    }

    /// `GetSummary ( summary ; break )` on the context's record: over the
    /// record's group when the window's found set is sorted by the break
    /// field, over the whole found set when the break field is the summary
    /// itself, else empty.
    pub(super) fn get_summary(
        &self,
        context: &Context,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Value, CalcError> {
        let (table, def) = self
            .schema
            .field(summary.occurrence, summary.field)
            .ok_or(CalcError::FIELD_MISSING)?;
        if def.kind != FieldKind::Summary {
            return Err(CalcError::NOT_SUPPORTED);
        }
        let definition = self.summary_definition(table, def.id);
        let Some(_depth) = self.nested_read() else {
            return Ok(Value::invalid());
        };
        let input = self.summary_input(summary.occurrence, table, &definition, 1)?;
        let in_table = context
            .occurrence
            .and_then(|o| self.schema.table_of(o))
            .is_some_and(|t| t == table);
        let position = context
            .record
            .filter(|_| in_table)
            .and_then(|r| input.position(r));
        let window = self.summary_window(table)?;
        let groups;
        let by = if break_field.field == summary.field
            && self.schema.table_of(break_field.occurrence) == Some(table)
        {
            Break::All
        } else {
            match self.break_groups(window, &input.records, break_field)? {
                Some(found) => {
                    groups = found;
                    Break::Groups(&groups)
                }
                None => Break::Unsorted,
            }
        };
        let cx = self.cx();
        let value = definition.group_value(&input.found(), position, by, &cx)?;
        Ok(value.convert_declared(values::value_type(def.data_type), &cx))
    }

    /// [`Self::get_summary`] on every record of the window's found set, in
    /// found-set order, in one pass over it (an export's group rows, which
    /// one `GetSummary` each would read the whole found set for).
    pub(super) fn group_summaries(
        &self,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Vec<Value>, CalcError> {
        let (table, def) = self
            .schema
            .field(summary.occurrence, summary.field)
            .ok_or(CalcError::FIELD_MISSING)?;
        if def.kind != FieldKind::Summary {
            return Err(CalcError::NOT_SUPPORTED);
        }
        let definition = self.summary_definition(table, def.id);
        let Some(_depth) = self.nested_read() else {
            return Err(CalcError::NOT_SUPPORTED);
        };
        let input = self.summary_input(summary.occurrence, table, &definition, 1)?;
        let window = self.summary_window(table)?;
        let cx = self.cx();
        let count = input.records.len();
        let found = input.found();
        let values = if break_field.field == summary.field
            && self.schema.table_of(break_field.occurrence) == Some(table)
        {
            vec![definition.group_value(&found, None, Break::All, &cx)?; count]
        } else {
            match self.break_groups(window, &input.records, break_field)? {
                Some(groups) => definition.group_values(&found, &groups, &cx)?,
                None => vec![definition.group_value(&found, None, Break::Unsorted, &cx)?; count],
            }
        };
        let ty = values::value_type(def.data_type);
        Ok(values
            .into_iter()
            .map(|v| v.convert_declared(ty, &cx))
            .collect())
    }

    /// [`Self::summary_value`] on each of `records`, in one pass over the
    /// window's found set (an export's rows, which one read each would
    /// make quadratic): a running summary so far, the record's fraction,
    /// the found set's summary otherwise.
    pub(super) fn record_summaries(
        &self,
        summary: FieldId,
        records: &[u64],
    ) -> Result<Vec<Value>, CalcError> {
        let (table, def) = self
            .schema
            .field(summary.occurrence, summary.field)
            .ok_or(CalcError::FIELD_MISSING)?;
        if def.kind != FieldKind::Summary {
            return Err(CalcError::NOT_SUPPORTED);
        }
        let definition = self.summary_definition(table, def.id);
        let Some(_depth) = self.nested_read() else {
            return Err(CalcError::NOT_SUPPORTED);
        };
        let input = self.summary_input(summary.occurrence, table, &definition, 1)?;
        let cx = self.cx();
        let found = input.found();
        let all = definition.values(&found, &cx)?;
        let positions: HashMap<u64, usize> = input
            .records
            .iter()
            .enumerate()
            .map(|(i, r)| (*r, i))
            .collect();
        let ty = values::value_type(def.data_type);
        records
            .iter()
            .map(|r| {
                let value = match positions.get(r).and_then(|&p| all.get(p)) {
                    Some(value) => value.clone(),
                    None => definition.value(&found, None, &cx)?,
                };
                Ok(value.convert_declared(ty, &cx))
            })
            .collect()
    }

    /// The found set's values a summary reads: the summarized field's
    /// repetitions on each record, the weights of a weighted average, the
    /// groups of the additional field when the found set is sorted by it.
    fn summary_input(
        &self,
        occurrence: u64,
        table: u64,
        summary: &Summary,
        repetition: u32,
    ) -> Result<SummaryInput, CalcError> {
        let window = self.summary_window(table)?;
        let records = window.found.clone();
        let field_repetitions = self
            .schema
            .catalog
            .table(table)
            .and_then(|t| t.field(summary.field))
            .map_or(1, Self::repetitions);
        let repetitions = summary.repetitions(repetition, field_repetitions);
        let mut values = Vec::with_capacity(records.len());
        for &record in &records {
            let mut these = Vec::with_capacity(repetitions.len());
            for rep in repetitions.clone() {
                these.push(self.read_field(occurrence, record, summary.field, rep)?);
            }
            values.push(these);
        }
        let weights = match summary.additional.filter(|_| summary.is_weighted()) {
            Some(weight) => records
                .iter()
                .map(|&record| self.read_from(occurrence, record, weight))
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
        };
        let groups = match summary.additional.filter(|_| summary.uses_groups()) {
            Some(by) => self.break_groups(window, &records, by)?,
            None => None,
        };
        Ok(SummaryInput {
            records,
            values,
            weights,
            groups,
            sorted: window.sort.is_some(),
        })
    }

    /// `field` read from `record` of `occurrence`: on the record when the
    /// field's occurrence shows the same table, else on the first related
    /// record (empty without one).
    fn read_from(&self, occurrence: u64, record: u64, field: FieldId) -> Result<Value, CalcError> {
        let table = self.schema.table_of(occurrence);
        if self.schema.table_of(field.occurrence) == table {
            return self.read_field(field.occurrence, record, field.field, 1);
        }
        match self
            .related_set(occurrence, record, field.occurrence)?
            .first()
        {
            Some(&related) => self.read_field(field.occurrence, related, field.field, 1),
            None => Ok(Value::empty_text()),
        }
    }

    /// Each record's group by `field` when the window's found set is
    /// sorted by it: records equal in every sort field up to that one share
    /// a group; `None` when the sort does not include the field.
    fn break_groups(
        &self,
        window: &Window,
        records: &[u64],
        field: FieldId,
    ) -> Result<Option<Vec<usize>>, CalcError> {
        let Some(spec) = &window.sort else {
            return Ok(None);
        };
        let Some(level) = spec
            .fields
            .iter()
            .position(|entry| self.sorts_by(window, entry, field))
        else {
            return Ok(None);
        };
        let prefix = SortSpec {
            fields: spec.fields[..=level].to_vec(),
            ..spec.clone()
        };
        self.groups(window.occurrence, records, &prefix)
            .map(Some)
            .map_err(|e| CalcError::from_code(e.0 as u16).unwrap_or(CalcError::NOT_SUPPORTED))
    }

    /// Whether a sort entry sorts by `field`: the same occurrence, or two
    /// occurrences of the window's table.
    fn sorts_by(&self, window: &Window, entry: &SortEntry, field: FieldId) -> bool {
        let occurrence = entry.field.occurrence.unwrap_or(window.occurrence);
        entry.summary.is_none()
            && entry.field.field == field.field
            && (occurrence == field.occurrence
                || (self.schema.table_of(occurrence) == Some(window.table)
                    && self.schema.table_of(field.occurrence) == Some(window.table)))
    }
}

/// Resets the nesting depth of [`ServerHost::read_field`] when dropped.
struct ReadDepth<'a>(&'a std::cell::Cell<u32>);

impl Drop for ReadDepth<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

/// [`CalcHost`] over a session borrowed shared, for an unstored calculation
/// read outside a step's own evaluation. Its `$` and `$$` variables are its
/// own and start empty: the runtime's are out of reach there.
pub(super) struct Reader<'h> {
    host: &'h ServerHost,
    locals: Variables,
    globals: Variables,
}

impl<'h> Reader<'h> {
    pub fn new(host: &'h ServerHost) -> Self {
        Reader {
            host,
            locals: Variables::new(),
            globals: Variables::new(),
        }
    }
}

impl CalcHost for Reader<'_> {
    fn locale(&self) -> &Locale {
        CalcHost::locale(self.host)
    }

    fn today(&self) -> Date {
        CalcHost::today(self.host)
    }

    fn names(&self) -> &dyn Names {
        CalcHost::names(self.host)
    }

    fn interrupted(&self) -> bool {
        CalcHost::interrupted(self.host)
    }

    fn limits(&self) -> CalcLimits {
        CalcHost::limits(self.host)
    }

    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        CalcHost::field(self.host, occurrence, field)
    }

    fn evaluate_always(&self, occurrence: u64, field: u64) -> bool {
        CalcHost::evaluate_always(self.host, occurrence, field)
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        CalcHost::field_value(self.host, occurrence, record, field, repetition)
    }

    fn field_value_in_context(
        &self,
        context: &Context,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        CalcHost::field_value_in_context(self.host, context, occurrence, record, field, repetition)
    }

    fn relationship_origin(&self, context: &Context, related: u64) -> Option<(u64, u64)> {
        CalcHost::relationship_origin(self.host, context, related)
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        CalcHost::related_records(self.host, occurrence, record, related)
    }

    fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        CalcHost::found_record(self.host, occurrence, index)
    }

    fn summary(
        &self,
        context: &Context,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Value, CalcError> {
        CalcHost::summary(self.host, context, summary, break_field)
    }

    fn base_table_name(&self, occurrence: u64) -> Option<&str> {
        CalcHost::base_table_name(self.host, occurrence)
    }

    fn get(&mut self, selector: u8, _context: &Context) -> Result<Value, CalcError> {
        Ok(self.host.get_value(selector))
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        match scope {
            VarScope::Local => &mut self.locals,
            VarScope::Global => &mut self.globals,
        }
    }

    fn custom_function(&self, id: u64) -> Option<Arc<CustomFunction>> {
        CalcHost::custom_function(self.host, id)
    }

    fn container_data(
        &mut self,
        container: &om_calc::Container,
        _context: &Context,
    ) -> Result<om_calc::eval::StoredContainer, CalcError> {
        self.host.load_container(container)
    }

    /// The session's functions as a step's evaluation answers them
    /// (`ServerHost::shared_call`): GetSensor, the Design functions and the
    /// rest; `GetLayoutObjectAttribute` alone is the default's (empty), as
    /// it would evaluate the window's objects.
    fn call(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> Result<Value, CalcError> {
        self.host.shared_call(function, args, context)
    }

    fn call_reporting(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> (Value, Option<CalcError>) {
        if let Some(variant) = Variant::of(&function) {
            let vars = Vars::Scoped {
                locals: &mut self.locals,
                globals: &mut self.globals,
            };
            return self.host.execute_sql_with(variant, args, vars);
        }
        match self.call(function, args, context) {
            Ok(v) => (v, None),
            Err(e) => (Value::invalid(), Some(e)),
        }
    }
}

#[cfg(test)]
mod account_metadata_tests {
    use super::*;
    use crate::scripts::{fixture, tests};
    use om_data::host::session::selector::{ACCOUNT_PASSWORD_DAYS_REMAINING, ACCOUNT_TYPE};

    fn account_host() -> (
        fixture::TempFile,
        Arc<crate::connection::Shared>,
        ServerHost,
    ) {
        let mut tree = fixture::tree(&[], true);
        fixture::add_local_accounts(&mut tree);
        let (year, month, day) = super::super::today().ymd();
        let encoded = 0x800000 | ((year as u32) << 10) | (month << 6) | day;
        tree.put(
            &[&[23], &[1], &[5], &[2], &[0x0a]],
            om_hbam::OwnedNode::leaf(
                Vec::new(),
                vec![
                    5,
                    (encoded >> 16) as u8,
                    (encoded >> 8) as u8,
                    encoded as u8,
                    0x80,
                    0,
                ],
            ),
        );
        tree.put(
            &[&[23], &[2], &[5], &[1], &[0x0c]],
            om_hbam::OwnedNode::leaf(Vec::new(), vec![0, 10]),
        );
        let file = fixture::TempFile::from_tree("nested-account-facts", &tree);
        let shared = tests::shared(&file);
        let mut host = tests::session(&shared);
        // Simulate a script created on an earlier date. Account selectors
        // must use the live date, including in nested readers and SQL.
        host.today = Date::from_ymd(2000, 1, 1).unwrap();
        (file, shared, host)
    }

    #[test]
    fn account_metadata_reaches_nested_fields_and_sql_sessions() {
        let (_file, _shared, mut host) = account_host();
        let context = Context::new();
        let locale = Locale::en_us();
        let cx = om_calc::Ctx::new(&locale, super::super::today());
        for (selector, expected) in [
            (ACCOUNT_TYPE, "FileMaker File"),
            (ACCOUNT_PASSWORD_DAYS_REMAINING, "10"),
        ] {
            let direct = CalcHost::get(&mut host, selector, &context).unwrap();
            let nested = CalcHost::get(&mut Reader::new(&host), selector, &context).unwrap();
            let sql = host.session_info();
            let sql_value = sql
                .account_info
                .answer(selector, sql.evaluation_date())
                .unwrap();
            for value in [direct, nested, sql_value] {
                assert_eq!(value.to_text(&cx).as_str(), expected);
            }
        }
    }
}
