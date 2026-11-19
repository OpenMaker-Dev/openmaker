//! Summary fields over the found set (file-metadata.md 5) in the client's
//! calculations: [`crate::summary`] computes every kind and option over the
//! source's found set of the summary's occurrence, or the parent record's
//! related set when read through a relationship, in its order; this module
//! reads what it needs from the source.
//!
//! The summarized values are read as a calculation reads them (unstored
//! calculations evaluated, [`FileCalc::read_field`]), the weights of a
//! weighted average on the record, or on the first related record when the
//! additional field's occurrence shows another table. A group is a run of
//! records whose sort fields, up to and including the break field, are
//! equal, and exists only when the found set is sorted by the break field
//! ([`super::CalcSource::found_sort`], or the relationship's sort for a
//! related set): a running summary restarts at each
//! group with the "restart" option, a subtotaled fraction divides by its
//! group's total, and `GetSummary ( s ; break )` summarizes the record's
//! group. Without a known sort these read as unsorted.

use std::cell::{Cell, OnceCell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use om_calc::eval::{Context, FieldId};
use om_calc::ops::{self, Comparison};
use om_calc::{CalcError, Ctx, Value, ValueType};

use super::schema::FieldDef;
use super::{record_missing, FileCalc, HostReads, ReadDepth, UNRESOLVED_SORT_FIELD};
use crate::summary::{Break, Found, Operation, Summary};

/// Summary reuse during one immutable calculation pass. A fresh cache (or
/// [`SummaryCache::clear`]) is required after data, found-set order, sorts,
/// variables or session state change, including a missing-data fetch.
///
/// Ordinary totals retain only their result, rather than a copy of every
/// input for every summary. Running summaries, fractions and `GetSummary`
/// retain the inputs they need to answer for different records or groups.
#[derive(Default)]
pub struct SummaryCache {
    values: RefCell<HashMap<SummaryKey, (Value, HostReads)>>,
    runs: RefCell<HashMap<SummaryKey, Rc<SummaryRun>>>,
}

impl SummaryCache {
    /// End the pass: no result from the previous data or context survives.
    pub fn clear(&self) {
        self.values.borrow_mut().clear();
        self.runs.borrow_mut().clear();
    }
}

/// [`FileCalc::found_groups`]: the found set read, and per break field
/// its sort position and each record's group when sorted by it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundGroups {
    pub records: Arc<[u64]>,
    pub breaks: Vec<Option<(usize, Vec<usize>)>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SummaryKey {
    schema: usize,
    source: usize,
    session: usize,
    today: i64,
    occurrence: u64,
    field: u64,
    repetition: u32,
    /// The parent whose related set is summarized; `None` for the found
    /// set.
    related_from: Option<(u64, u64)>,
}

/// One summary field over one found or related set: what it reads, read once
/// ([`FileCalc::summary_run`]), and its value on any record.
pub(crate) struct SummaryRun {
    summary: Summary,
    value_type: ValueType,
    records: Arc<[u64]>,
    /// Each record's values of the summarized field (the repetitions the
    /// summary reads).
    values: Vec<Vec<Value>>,
    /// Each record's weight, for a weighted average.
    weights: Vec<Value>,
    /// The groups of the additional field when the found set is sorted by
    /// it.
    groups: Option<Vec<usize>>,
    sorted: bool,
    /// A record-independent aggregate is folded only once, including when
    /// this run was first requested by `GetSummary`.
    aggregate: OnceCell<Result<Value, CalcError>>,
    reads: HostReads,
}

impl SummaryRun {
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

    /// The summary as it shows on `record`: a running summary so far, a
    /// fraction of the record's value, the summary over the found set
    /// otherwise.
    pub(crate) fn value(&self, record: u64, cx: &Ctx) -> Result<Value, CalcError> {
        if !self.summary.is_running() && self.summary.operation != Operation::Fraction {
            return self
                .aggregate
                .get_or_init(|| {
                    self.summary
                        .value(&self.found(), None, cx)
                        .map(|value| value.convert_declared(self.value_type, cx))
                })
                .clone();
        }
        let value = self
            .summary
            .value(&self.found(), self.position(record), cx)?;
        Ok(value.convert_declared(self.value_type, cx))
    }

    /// [`SummaryRun::value`] on each of `records` in one pass over the run
    /// ([`Summary::values`]); a record outside it gets what `value` gives.
    fn values(&self, records: &[u64], cx: &Ctx) -> Result<Vec<Value>, CalcError> {
        let all = self.summary.values(&self.found(), cx)?;
        let positions: HashMap<u64, usize> = self
            .records
            .iter()
            .enumerate()
            .map(|(i, r)| (*r, i))
            .collect();
        records
            .iter()
            .map(|r| match positions.get(r).and_then(|&p| all.get(p)) {
                Some(value) => Ok(value.clone().convert_declared(self.value_type, cx)),
                None => self.value(*r, cx),
            })
            .collect()
    }
}

impl<'a> FileCalc<'a> {
    fn summary_key(
        &self,
        occurrence: u64,
        field: u64,
        repetition: u32,
        related_from: Option<(u64, u64)>,
    ) -> SummaryKey {
        SummaryKey {
            schema: std::ptr::from_ref(self.schema) as usize,
            source: std::ptr::from_ref(self.source).cast::<()>() as usize,
            session: self.session.map_or(0, |s| std::ptr::from_ref(s) as usize),
            today: self.today.day_number(),
            occurrence,
            field,
            repetition,
            related_from,
        }
    }

    /// The value of summary field `def` of `occurrence` on `record`,
    /// repetition `repetition` when it summarizes repetitions individually:
    /// over the records of `occurrence` related to `related_from`'s
    /// (occurrence, record), else over the found set.
    pub(super) fn summary_value(
        &self,
        occurrence: u64,
        record: u64,
        def: &FieldDef,
        repetition: u32,
        related_from: Option<(u64, u64)>,
    ) -> Result<Value, CalcError> {
        let Some(_depth) = ReadDepth::enter() else {
            return Ok(Value::invalid());
        };
        let key = self.summary_key(occurrence, def.id, repetition, related_from);
        if let Some((value, reads)) = self
            .summaries
            .and_then(|cache| cache.values.borrow().get(&key).cloned())
        {
            self.record_reads(reads);
            return Ok(value);
        }
        let summary = def
            .summary
            .as_ref()
            .map(Summary::from)
            .ok_or(CalcError::NOT_SUPPORTED)?;
        let fixed = !summary.is_running() && summary.operation != Operation::Fraction;
        // Plain totals need only their final scalar result. Keeping their
        // inputs too would multiply a large found set by every summary.
        let run = if fixed {
            match self
                .summaries
                .and_then(|cache| cache.runs.borrow().get(&key).cloned())
            {
                Some(run) => Some(run),
                None => self
                    .summary_run_in_context(occurrence, def, repetition, related_from)?
                    .map(Rc::new),
            }
        } else {
            self.cached_summary_run(occurrence, def, repetition, related_from)?
        };
        let reads = run.as_ref().map_or(HostReads::default(), |run| run.reads);
        self.record_reads(reads);
        let value = match run {
            Some(run) => run.value(record, &self.table_cx(occurrence))?,
            None => Value::empty(def.value_type),
        };
        if fixed {
            if let Some(cache) = self.summaries {
                cache
                    .values
                    .borrow_mut()
                    .insert(key, (value.clone(), reads));
            }
        }
        Ok(value)
    }

    /// `GetSummary ( summary ; break )` on the context's record: the
    /// summary over the whole found set when the break field is the
    /// summary itself, over the record's group when the found set is
    /// sorted by the break field, and empty otherwise. A related summary
    /// reads the parent's related set on the portal row, else on the first
    /// related record.
    pub fn summary(
        &self,
        context: &Context,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Value, CalcError> {
        let parent = context.record.ok_or_else(record_missing)?;
        let def = self
            .schema
            .field(summary.occurrence, summary.field)
            .ok_or(CalcError::FIELD_MISSING)?;
        let record = if context.occurrence == Some(summary.occurrence) {
            parent
        } else if let Some((_, row)) = context
            .portal
            .filter(|(portal, _)| *portal == summary.occurrence)
        {
            row
        } else {
            let (from, parent) = self
                .schema
                .relationship_origin(context, summary.occurrence)
                .ok_or_else(record_missing)?;
            match self.related_record(from, parent, summary.occurrence, 0)? {
                Some(record) => record,
                None => return Ok(Value::empty(def.value_type)),
            }
        };
        if def.summary.is_none() {
            // A field that is not a summary is its own value.
            return self.field_value(summary.occurrence, record, summary.field, 1);
        }
        let Some(_depth) = ReadDepth::enter() else {
            return Ok(Value::invalid());
        };
        let related_from = self.summary_related_from(context, summary.occurrence);
        let Some(run) = self.cached_summary_run(summary.occurrence, def, 1, related_from)? else {
            return Ok(Value::empty(def.value_type));
        };
        let table = self.table_id(summary.occurrence)?;
        let same_table = self.table_id(break_field.occurrence).ok() == Some(table);
        let groups;
        let by = if same_table && break_field.field == summary.field {
            Break::All
        } else if same_table {
            match self.break_groups(
                summary.occurrence,
                &run.records,
                break_field.field,
                related_from,
            )? {
                Some(found) => {
                    groups = found;
                    Break::Groups(&groups)
                }
                None => Break::Unsorted,
            }
        } else {
            Break::Unsorted
        };
        let cx = self.table_cx(summary.occurrence);
        let value = run
            .summary
            .group_value(&run.found(), run.position(record), by, &cx)?;
        Ok(value.convert_declared(def.value_type, &cx))
    }

    /// [`FileCalc::summary`] on every record of the found set of
    /// `summary`'s occurrence, in found-set order, in one pass over it (an
    /// export's group rows, which one `GetSummary` each would read the
    /// whole found set for); empty for a summary of nothing.
    pub fn group_summaries(
        &self,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Vec<Value>, CalcError> {
        let def = self
            .schema
            .field(summary.occurrence, summary.field)
            .ok_or(CalcError::FIELD_MISSING)?;
        if def.summary.is_none() {
            return Err(CalcError::NOT_SUPPORTED);
        }
        let Some(_depth) = ReadDepth::enter() else {
            return Err(CalcError::NOT_SUPPORTED);
        };
        let Some(run) = self.cached_summary_run(summary.occurrence, def, 1, None)? else {
            return Ok(Vec::new());
        };
        let table = self.table_id(summary.occurrence)?;
        let same_table = self.table_id(break_field.occurrence).ok() == Some(table);
        let cx = self.table_cx(summary.occurrence);
        let found = run.found();
        let count = run.records.len();
        let values = if same_table && break_field.field == summary.field {
            vec![run.summary.group_value(&found, None, Break::All, &cx)?; count]
        } else {
            let groups = match same_table {
                true => {
                    self.break_groups(summary.occurrence, &run.records, break_field.field, None)?
                }
                false => None,
            };
            match groups {
                Some(groups) => run.summary.group_values(&found, &groups, &cx)?,
                None => vec![
                    run.summary
                        .group_value(&found, None, Break::Unsorted, &cx)?;
                    count
                ],
            }
        };
        Ok(values
            .into_iter()
            .map(|v| v.convert_declared(def.value_type, &cx))
            .collect())
    }

    /// [`FileCalc::field_value`] of summary field `summary` (its first
    /// repetition) on each of `records`, in one pass over the found set of
    /// the summary's occurrence (an export's rows, which one read each
    /// would make quadratic in the found set): a running summary so far,
    /// the record's fraction, the found set's summary otherwise.
    pub fn record_summaries(
        &self,
        summary: FieldId,
        records: &[u64],
    ) -> Result<Vec<Value>, CalcError> {
        let def = self
            .schema
            .field(summary.occurrence, summary.field)
            .ok_or(CalcError::FIELD_MISSING)?;
        if def.summary.is_none() {
            return Err(CalcError::NOT_SUPPORTED);
        }
        let Some(_depth) = ReadDepth::enter() else {
            return Err(CalcError::NOT_SUPPORTED);
        };
        let Some(run) = self.cached_summary_run(summary.occurrence, def, 1, None)? else {
            return Ok(vec![Value::empty(def.value_type); records.len()]);
        };
        run.values(records, &self.table_cx(summary.occurrence))
    }

    /// The groups of the found set of `occurrence` at each of `breaks`
    /// (fields of its table), as report sub-summary parts print them: for a
    /// field the found set is sorted by, its sort position and each
    /// record's group, the same runs `GetSummary` reads; `None` for a field
    /// it is not sorted by. Reads every found record's sort values up to
    /// the deepest sorted break, in one prefetch. `NOT_SUPPORTED` when such
    /// a group depends on a sort key of another table.
    pub fn found_groups(&self, occurrence: u64, breaks: &[u64]) -> Result<FoundGroups, CalcError> {
        let table = self
            .schema
            .table_of(occurrence)
            .ok_or(CalcError::TABLE_MISSING)?;
        let records = self.source.found_set(occurrence, table.id)?;
        let sort = self.source.found_sort(occurrence).unwrap_or_default();
        let depths: Vec<Option<usize>> = breaks
            .iter()
            .map(|field| sort.iter().position(|f| f == field))
            .collect();
        let Some(deepest) = depths.iter().flatten().max() else {
            return Ok(FoundGroups {
                records,
                breaks: vec![None; breaks.len()],
            });
        };
        if sort[..=*deepest].contains(&UNRESOLVED_SORT_FIELD) {
            return Err(CalcError::NOT_SUPPORTED);
        }
        self.source.prefetch_records(table.id, &records);
        let mut groups = Vec::with_capacity(breaks.len());
        for (field, depth) in breaks.iter().zip(depths) {
            groups.push(match depth {
                Some(depth) => Some((
                    depth,
                    self.break_groups(occurrence, &records, *field, None)?
                        .ok_or(CalcError::NOT_SUPPORTED)?,
                )),
                None => None,
            });
        }
        Ok(FoundGroups {
            records,
            breaks: groups,
        })
    }

    /// Read what summary field `def` of `occurrence` needs from the found
    /// set, or `related_from`'s related set; `None` for a total of nothing.
    fn cached_summary_run(
        &self,
        occurrence: u64,
        def: &FieldDef,
        repetition: u32,
        related_from: Option<(u64, u64)>,
    ) -> Result<Option<Rc<SummaryRun>>, CalcError> {
        let key = self.summary_key(occurrence, def.id, repetition, related_from);
        if let Some(run) = self
            .summaries
            .and_then(|cache| cache.runs.borrow().get(&key).cloned())
        {
            self.record_reads(run.reads);
            return Ok(Some(run));
        }
        let run = self
            .summary_run_in_context(occurrence, def, repetition, related_from)?
            .map(Rc::new);
        if let (Some(cache), Some(run)) = (self.summaries, &run) {
            cache.runs.borrow_mut().insert(key, Rc::clone(run));
        }
        Ok(run)
    }

    pub(crate) fn summary_run(
        &self,
        occurrence: u64,
        def: &FieldDef,
        repetition: u32,
    ) -> Result<Option<SummaryRun>, CalcError> {
        self.summary_run_in_context(occurrence, def, repetition, None)
    }

    /// [`Self::summary_run`] over the records of `occurrence` related to
    /// `related_from`'s (occurrence, record) when given: a related summary
    /// never reads the related table's found set or every record of it.
    fn summary_run_in_context(
        &self,
        occurrence: u64,
        def: &FieldDef,
        repetition: u32,
        related_from: Option<(u64, u64)>,
    ) -> Result<Option<SummaryRun>, CalcError> {
        let reads = Cell::default();
        let result =
            self.with_reads(&reads)
                .read_summary_run(occurrence, def, repetition, related_from);
        self.record_reads(reads.get());
        result.map(|run| {
            run.map(|mut run| {
                run.reads = reads.get();
                run
            })
        })
    }

    fn read_summary_run(
        &self,
        occurrence: u64,
        def: &FieldDef,
        repetition: u32,
        related_from: Option<(u64, u64)>,
    ) -> Result<Option<SummaryRun>, CalcError> {
        let summary = def
            .summary
            .as_ref()
            .map(Summary::from)
            .ok_or(CalcError::NOT_SUPPORTED)?;
        if summary.summarizes_nothing() {
            return Ok(None);
        }
        let table = self
            .schema
            .table_of(occurrence)
            .ok_or(CalcError::TABLE_MISSING)?;
        let records = match related_from {
            Some((from, record)) => self.source.related_ids(
                self.schema,
                from,
                record,
                occurrence,
                &self.cx_for(from),
            )?,
            None => self.source.found_set(occurrence, table.id)?,
        };
        self.source.prefetch_records(table.id, &records);
        let field_repetitions = table
            .fields
            .get(&summary.field)
            .map_or(1, |f| f.repetitions);
        let repetitions = summary.repetitions(repetition, field_repetitions);
        let mut values = Vec::with_capacity(records.len());
        for &record in records.iter() {
            values.push(
                repetitions
                    .clone()
                    .map(|rep| self.read_field(occurrence, record, summary.field, rep))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        let weights = match summary.additional.filter(|_| summary.is_weighted()) {
            Some(weight) => records
                .iter()
                .map(|&record| self.read_from(occurrence, record, weight))
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
        };
        let groups = match summary.additional.filter(|_| summary.uses_groups()) {
            Some(by) if self.table_id(by.occurrence).ok() == Some(table.id) => {
                self.break_groups(occurrence, &records, by.field, related_from)?
            }
            _ => None,
        };
        Ok(Some(SummaryRun {
            summary,
            value_type: def.value_type,
            sorted: self
                .summary_sort(occurrence, related_from)
                .is_some_and(|sort| !sort.is_empty()),
            records,
            values,
            weights,
            groups,
            aggregate: OnceCell::new(),
            reads: HostReads::default(),
        }))
    }

    /// `field` read for `record` of `occurrence`: on the record when the
    /// field's occurrence shows the same table, else on the first related
    /// record (empty without one).
    fn read_from(&self, occurrence: u64, record: u64, field: FieldId) -> Result<Value, CalcError> {
        if self.table_id(field.occurrence).ok() == self.table_id(occurrence).ok() {
            return self.read_field(field.occurrence, record, field.field, 1);
        }
        match self.related_record(occurrence, record, field.occurrence, 0)? {
            Some(related) => self.read_field(field.occurrence, related, field.field, 1),
            None => Ok(Value::empty_text()),
        }
    }

    /// The fields of `occurrence`'s table the summarized set is sorted by:
    /// the found set's sort, or the sort of the relationship from
    /// `related_from`'s occurrence up to its first field of another table.
    fn summary_sort(
        &self,
        occurrence: u64,
        related_from: Option<(u64, u64)>,
    ) -> Option<Arc<[u64]>> {
        let Some((from, _)) = related_from else {
            return self.source.found_sort(occurrence);
        };
        let path = self.schema.path(from, occurrence)?;
        let sort = path.last()?.sort.as_ref()?;
        let table = self.table_id(occurrence).ok();
        Some(
            sort.fields
                .iter()
                .take_while(|entry| {
                    let of = entry.field.occurrence.unwrap_or(occurrence);
                    self.table_id(of).ok() == table
                })
                .map(|entry| entry.field.field)
                .collect(),
        )
    }

    /// Each record's group by `field` when the found set of `occurrence`
    /// (or `related_from`'s related set) is sorted by it: a new group at
    /// each change of a sort field up to `field`; `None` when the sort does
    /// not include it, or a key up to it is unresolved (a related field).
    fn break_groups(
        &self,
        occurrence: u64,
        records: &[u64],
        field: u64,
        related_from: Option<(u64, u64)>,
    ) -> Result<Option<Vec<usize>>, CalcError> {
        let Some(sort) = self.summary_sort(occurrence, related_from) else {
            return Ok(None);
        };
        let Some(depth) = sort.iter().position(|f| *f == field) else {
            return Ok(None);
        };
        let keys = &sort[..=depth];
        if keys.contains(&UNRESOLVED_SORT_FIELD) {
            return Ok(None);
        }
        let cx = self.table_cx(occurrence);
        let mut groups = Vec::with_capacity(records.len());
        let mut previous: Option<Vec<Value>> = None;
        let mut group = 0;
        for &record in records {
            let values = keys
                .iter()
                .map(|f| self.read_field(occurrence, record, *f, 1))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(previous) = &previous {
                let same = previous.iter().zip(&values).all(|(a, b)| {
                    // Stored sorting gives empty values their own key.
                    // Arithmetic comparison alone coerces an empty number
                    // to zero and would merge those distinct sorted runs.
                    if a.is_empty() || b.is_empty() {
                        a.is_empty() && b.is_empty()
                    } else {
                        matches!(
                            ops::compare(a, b, &cx),
                            Comparison::Ordered(Ordering::Equal)
                        )
                    }
                });
                if !same {
                    group += 1;
                }
            }
            groups.push(group);
            previous = Some(values);
        }
        Ok(Some(groups))
    }
}
