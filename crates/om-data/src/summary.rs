//! Summary fields (file-metadata.md 5): the 30-byte definition FileMaker
//! stores at `[T, 3, 5, F, 09]` (decoded by [`SummaryDef`]) and the value a
//! summary takes over a found set, for every operation: total and running
//! total, average and weighted average, count and running count, minimum,
//! maximum, standard deviation (sample or population), fraction of total
//! (also subtotaled) and list. Every host computes summaries here: the
//! desktop client's calculations and ExecuteSQL (`crate::host`) and
//! OpenMaker Server's scripts.
//!
//! A host reads the found set's values ([`Found`]); the rules here are
//! those the script engine was seen to follow (5.3):
//!
//! - A summary is computed over the found set in its order; empty values do
//!   not count. Totals, averages, minimums, maximums, deviations and lists
//!   are the aggregate functions' (`Sum`, `Average`, `Min`, `StDev`, ...).
//! - A running total or count accumulates in found-set order; with
//!   "restart" it starts over at each group of the additional (break)
//!   field, but only when the found set is sorted by that field. On a record
//!   whose value is empty the running total is empty until something has
//!   accumulated in its group; the running count is the count so far.
//! - A fraction is the record's value over the total, or, subtotaled, over
//!   its group's total when the found set is sorted by the break field
//!   (empty otherwise). An empty value's fraction is empty, except on the
//!   first record of a sorted found set, where FileMaker gives 0.
//! - `GetSummary ( s ; b )` is the summary over the record's group when the
//!   found set is sorted by `b` and empty otherwise; `GetSummary ( s ; s )`
//!   is the summary over the whole found set. For a running summary it is
//!   the running value at the end of the group.

use std::ops::Range;

use om_calc::eval::FieldId;
use om_calc::functions::{self, Implementation};
use om_calc::{CalcError, Ctx, Value, ValueType};

use crate::meta::{summary_flag, SummaryDef};

/// What a summary computes (`SummaryDef::SummaryType`, file-metadata.md
/// 5.1): [`crate::meta::SummaryKind`].
pub use crate::meta::SummaryKind as Operation;

/// A summary field's definition (`DBField::SummaryDef`) as a summary is
/// computed from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub operation: Operation,
    /// `mFlags`: [`Self::OPTION`], [`Self::INDIVIDUALLY`],
    /// [`Self::RUNNING`], [`Self::RESTART`].
    pub flags: u8,
    /// The summarized field, of the summary's own table; 0 for "nothing".
    pub field: u64,
    /// The additional field: the weight of a weighted average, the break
    /// field of a subtotaled fraction or a restarting running summary. Its
    /// occurrence is the one chosen in the dialog; a field spec without an
    /// occurrence has none (5.4: the engine ignores the option then).
    pub additional: Option<FieldId>,
}

impl From<&SummaryDef> for Summary {
    fn from(def: &SummaryDef) -> Summary {
        Summary {
            operation: def.kind,
            flags: def.flags,
            field: def.field.map_or(0, |f| f.field),
            additional: def.additional.and_then(|a| {
                Some(FieldId {
                    occurrence: a.occurrence?,
                    field: a.field,
                })
            }),
        }
    }
}

/// Which records a `GetSummary` covers.
#[derive(Clone, Copy, Debug)]
pub enum Break<'a> {
    /// The whole found set (`GetSummary ( s ; s )`).
    All,
    /// The found set is sorted by the break field: each record's group
    /// number, in found-set order.
    Groups(&'a [usize]),
    /// The found set is not sorted by the break field: empty.
    Unsorted,
}

/// The found set as a summary reads it, in found-set order.
#[derive(Clone, Copy, Debug)]
pub struct Found<'a> {
    /// Each record's values of the summarized field: the repetition the
    /// summary reads, or every repetition when they are summarized all
    /// together ([`Summary::repetitions`]). Empty values may be left in.
    pub values: &'a [Vec<Value>],
    /// Each record's weight (the additional field) for a weighted average;
    /// may be empty otherwise.
    pub weights: &'a [Value],
    /// The groups of the additional field when the found set is sorted by
    /// it: each record's group number.
    pub additional_groups: Option<&'a [usize]>,
    /// The found set is sorted (by any field).
    pub sorted: bool,
}

impl Summary {
    /// The kind's option: weighted average, standard deviation by
    /// population, fraction subtotaled.
    pub const OPTION: u8 = summary_flag::OPTION;
    /// Summarize repetitions individually (else all together).
    pub const INDIVIDUALLY: u8 = summary_flag::INDIVIDUAL_REPETITIONS;
    /// Running total or running count.
    pub const RUNNING: u8 = summary_flag::RUNNING;
    /// Restart a running summary at each group of the additional field.
    pub const RESTART: u8 = summary_flag::RESTART;

    /// The stored definition ([`SummaryDef::decode`]): a shorter value is
    /// zero-padded and a missing one is a total of nothing (field 0).
    pub fn decode(bytes: &[u8]) -> Summary {
        Summary::from(&SummaryDef::decode(bytes))
    }

    /// A total of nothing: the definition names no summarized field; its
    /// value is empty.
    pub fn summarizes_nothing(&self) -> bool {
        self.field == 0
    }

    fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// A running total or count.
    pub fn is_running(&self) -> bool {
        self.has(Self::RUNNING) && matches!(self.operation, Operation::Total | Operation::Count)
    }

    /// Whether the summary needs the groups of its additional field: a
    /// restarting running summary or a subtotaled fraction.
    pub fn uses_groups(&self) -> bool {
        self.additional.is_some()
            && ((self.is_running() && self.has(Self::RESTART))
                || (self.operation == Operation::Fraction && self.has(Self::OPTION)))
    }

    /// Whether the summary needs each record's weight.
    pub fn is_weighted(&self) -> bool {
        self.operation == Operation::Average && self.has(Self::OPTION) && self.additional.is_some()
    }

    /// The repetitions of the summarized field (of `field_repetitions`)
    /// that repetition `repetition` of the summary reads: the same one when
    /// summarized individually, every one in repetition 1 when all
    /// together (Claris; file-metadata.md 5.3 leaves repetitions open).
    pub fn repetitions(&self, repetition: u32, field_repetitions: u32) -> Range<u32> {
        let last = field_repetitions.max(1);
        if self.has(Self::INDIVIDUALLY) {
            if repetition <= last {
                repetition..repetition + 1
            } else {
                0..0
            }
        } else if repetition <= 1 {
            1..last + 1
        } else {
            0..0
        }
    }

    /// The summary's value on the record at `position` of the found set
    /// (`None`: a record outside it, where only the summaries that do not
    /// depend on the record have a value).
    pub fn value(
        &self,
        found: &Found<'_>,
        position: Option<usize>,
        cx: &Ctx,
    ) -> Result<Value, CalcError> {
        let all = 0..found.values.len();
        if self.is_running() {
            let Some(at) = position.filter(|p| *p < found.values.len()) else {
                return Ok(self.empty());
            };
            let start = match found.additional_groups.filter(|_| self.has(Self::RESTART)) {
                Some(groups) => group_range(groups, at).start,
                None => 0,
            };
            return self.aggregate(found, start..at + 1, cx);
        }
        if self.operation == Operation::Fraction {
            let Some(at) = position.filter(|p| *p < found.values.len()) else {
                return Ok(self.empty());
            };
            return self.fraction(found, at..at + 1, cx);
        }
        self.aggregate(found, all, cx)
    }

    /// `GetSummary`: the summary over the group of the record at
    /// `position`, as [`Break`] says.
    pub fn group_value(
        &self,
        found: &Found<'_>,
        position: Option<usize>,
        by: Break<'_>,
        cx: &Ctx,
    ) -> Result<Value, CalcError> {
        let count = found.values.len();
        let range = match by {
            Break::Unsorted => return Ok(self.empty()),
            Break::All => 0..count,
            Break::Groups(groups) => match position.filter(|p| *p < count) {
                Some(at) => group_range(groups, at),
                None => return Ok(self.empty()),
            },
        };
        if range.is_empty() {
            return self.aggregate(found, range, cx);
        }
        if self.is_running() {
            return self.value(found, Some(range.end - 1), cx);
        }
        if self.operation == Operation::Fraction {
            return self.fraction(found, range, cx);
        }
        self.aggregate(found, range, cx)
    }

    /// [`Summary::value`] on every record of the found set at once, in
    /// found-set order, in one pass over it (an export's rows, which one
    /// `value` each would make quadratic in the found set): a running
    /// summary so far, each record's fraction, the summary over the found
    /// set otherwise.
    pub fn values(&self, found: &Found<'_>, cx: &Ctx) -> Result<Vec<Value>, CalcError> {
        let count = found.values.len();
        if self.is_running() || self.operation == Operation::Fraction {
            // Each record its own group: `group_values` carries the running
            // value from record to record and computes each total once.
            let each: Vec<usize> = (0..count).collect();
            return self.group_values(found, &each, cx);
        }
        Ok(vec![self.aggregate(found, 0..count, cx)?; count])
    }

    /// [`Summary::group_value`] with `Break::Groups(groups)` for every
    /// record of the found set at once, in one pass over it (an export's
    /// group rows): each record gets its group's value. A running summary's
    /// value at the end of each group accumulates from the one before; a
    /// fraction's totals are computed once per group of its break field.
    pub fn group_values(
        &self,
        found: &Found<'_>,
        groups: &[usize],
        cx: &Ctx,
    ) -> Result<Vec<Value>, CalcError> {
        let count = found.values.len().min(groups.len());
        let mut out = Vec::with_capacity(count);
        // The first position of each record's group of the additional
        // field, for restarts and subtotals.
        let additional = found.additional_groups.map(group_starts);
        let mut running: Option<(usize, usize, Value)> = None;
        let mut totals: std::collections::HashMap<usize, Value> = Default::default();
        let mut start = 0;
        while start < count {
            let end = (start..count)
                .find(|&i| groups[i] != groups[start])
                .unwrap_or(count);
            let value = if self.is_running() {
                let from = match additional.as_ref().filter(|_| self.has(Self::RESTART)) {
                    Some(starts) => starts.get(end - 1).copied().unwrap_or(end - 1),
                    None => 0,
                };
                let value = match &running {
                    Some((s, e, so_far)) if *s == from && *e <= end => {
                        let part = self.aggregate(found, *e..end, cx)?;
                        accumulate(so_far, &part, cx)?
                    }
                    _ => self.aggregate(found, from..end, cx)?,
                };
                running = Some((from, end, value.clone()));
                value
            } else if self.operation == Operation::Fraction {
                // The total of the group of the additional field the
                // group starts in (subtotaled), or of the found set; each
                // computed once.
                let all = found.values.len();
                let first = match (&additional, self.has(Self::OPTION)) {
                    (Some(starts), true) => Some(starts[start]),
                    (None, true) => None,
                    (_, false) => Some(all),
                };
                match first {
                    None => self.empty(),
                    Some(first) => {
                        let total = match totals.get(&first) {
                            Some(total) => total.clone(),
                            None => {
                                let range = match &additional {
                                    Some(starts) if first < all => {
                                        let last = (first..all)
                                            .find(|&i| starts.get(i) != Some(&first))
                                            .unwrap_or(all);
                                        first..last
                                    }
                                    _ => 0..all,
                                };
                                let total = aggregate("Sum", &found.values[range], cx)?;
                                totals.insert(first, total.clone());
                                total
                            }
                        };
                        self.fraction_of(found, start..end, &total, cx)?
                    }
                }
            } else {
                self.aggregate(found, start..end, cx)?
            };
            out.extend(std::iter::repeat(value).take(end - start));
            start = end;
        }
        Ok(out)
    }

    /// The empty value of the summary's own result.
    fn empty(&self) -> Value {
        match self.operation {
            Operation::List | Operation::Minimum | Operation::Maximum => Value::empty_text(),
            _ => Value::empty(ValueType::Number),
        }
    }

    /// The operation over the records in `range`, ignoring the running
    /// and fraction options.
    fn aggregate(
        &self,
        found: &Found<'_>,
        range: Range<usize>,
        cx: &Ctx,
    ) -> Result<Value, CalcError> {
        let values = &found.values[range.clone()];
        let name = match self.operation {
            Operation::Total | Operation::Fraction => "Sum",
            Operation::Average if self.is_weighted() => {
                return weighted_average(values, &found.weights[range], cx);
            }
            Operation::Average => "Average",
            Operation::Count => "Count",
            Operation::Minimum => "Min",
            Operation::Maximum => "Max",
            Operation::StdDeviation if self.has(Self::OPTION) => "StDevP",
            Operation::StdDeviation => "StDev",
            Operation::List => "List",
            Operation::Other(_) => return Err(CalcError::NOT_SUPPORTED),
        };
        aggregate(name, values, cx)
    }

    /// The fraction of the records in `range` (one record, or a group for
    /// `GetSummary`): their total over the found set's total, or over the
    /// total of the first record's group when subtotaled.
    fn fraction(
        &self,
        found: &Found<'_>,
        range: Range<usize>,
        cx: &Ctx,
    ) -> Result<Value, CalcError> {
        let denominator = if self.has(Self::OPTION) {
            match found.additional_groups {
                Some(groups) => group_range(groups, range.start),
                None => return Ok(self.empty()),
            }
        } else {
            0..found.values.len()
        };
        let total = aggregate("Sum", &found.values[denominator], cx)?;
        self.fraction_of(found, range, &total, cx)
    }

    /// The fraction of the records in `range` over `total`.
    fn fraction_of(
        &self,
        found: &Found<'_>,
        range: Range<usize>,
        total: &Value,
        cx: &Ctx,
    ) -> Result<Value, CalcError> {
        let part = aggregate("Sum", &found.values[range.clone()], cx)?;
        if part.is_empty() {
            return Ok(if found.sorted && range.start == 0 {
                Value::number(0)
            } else {
                self.empty()
            });
        }
        if total.is_empty() || total.to_number(cx).is_zero() {
            return Ok(self.empty());
        }
        om_calc::ops::divide(&part, total, cx)
    }
}

/// A running total or count so far with the next part's: empty parts add
/// nothing, as the aggregate over both would leave them out.
fn accumulate(so_far: &Value, part: &Value, cx: &Ctx) -> Result<Value, CalcError> {
    if part.is_empty() {
        return Ok(so_far.clone());
    }
    if so_far.is_empty() {
        return Ok(part.clone());
    }
    om_calc::ops::add(so_far, part, cx)
}

/// Each position's first position in its group.
fn group_starts(groups: &[usize]) -> Vec<usize> {
    let mut out = Vec::with_capacity(groups.len());
    for (i, group) in groups.iter().enumerate() {
        let start = match i {
            0 => 0,
            _ if groups[i - 1] == *group => out[i - 1],
            _ => i,
        };
        out.push(start);
    }
    out
}

/// An aggregate function of the calculation library over `values`.
fn aggregate(name: &str, values: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    let Some(Implementation::Aggregate(f)) = functions::lookup(name).map(|f| f.implementation)
    else {
        return Err(CalcError::NOT_SUPPORTED);
    };
    let list: Vec<Value> = values.iter().flatten().cloned().collect();
    f(&[list], cx)
}

/// Σ value × weight / Σ weight over the values that are numbers (an empty
/// weight counts as 0); empty when no weight adds up.
fn weighted_average(
    values: &[Vec<Value>],
    weights: &[Value],
    cx: &Ctx,
) -> Result<Value, CalcError> {
    let mut sum = Value::empty(ValueType::Number);
    let mut total_weight = Value::empty(ValueType::Number);
    for (record, weight) in values.iter().zip(weights) {
        let weight = Value::Number(weight.to_number(cx)).convert_declared(ValueType::Number, cx);
        let weight = if weight.is_empty() || weight.is_invalid() {
            Value::number(0)
        } else {
            weight
        };
        for value in record {
            let n = value.to_number(cx);
            if n.is_empty() || n.is_nan() {
                continue;
            }
            let product = om_calc::ops::multiply(&Value::Number(n), &weight, cx);
            sum = om_calc::ops::add(&sum, &product, cx)?;
            total_weight = om_calc::ops::add(&total_weight, &weight, cx)?;
        }
    }
    if total_weight.is_empty() || total_weight.to_number(cx).is_zero() {
        return Ok(Value::empty(ValueType::Number));
    }
    om_calc::ops::divide(&sum, &total_weight, cx)
}

/// The positions of the group the record at `at` belongs to.
fn group_range(groups: &[usize], at: usize) -> Range<usize> {
    let Some(&group) = groups.get(at) else {
        return at..at;
    };
    let start = groups[..at]
        .iter()
        .rposition(|g| *g != group)
        .map_or(0, |p| p + 1);
    let end = groups[at..]
        .iter()
        .position(|g| *g != group)
        .map_or(groups.len(), |p| at + p);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_calc::{Date, Locale};

    /// A definition: operation, flags, summarized field, additional
    /// occurrence and field.
    fn definition(operation: u8, flags: u8, field: u8, additional: Option<(u32, u8)>) -> Vec<u8> {
        let mut out = vec![
            operation, flags, 2, 1, 0, 0, 0xfe, 0x7f, 2, 1, field, 0, 0, 0, 0, 1,
        ];
        match additional {
            Some((occurrence, field)) => {
                let [_, a, b, c] = occurrence.to_be_bytes();
                out.extend([4, 3, a, b, c, 0, 2, 1, field, 0, 0, 0, 0, 1]);
            }
            None => out.extend([2, 1, 0, 0, 0, 0, 2, 1, 0, 0, 0, 0, 0, 0]),
        }
        out
    }

    #[test]
    fn definitions_name_operation_flags_and_fields() {
        // Tasks template "Number of Topics": count of field 1, padded.
        let count = [
            0x03, 0x00, 0x02, 0x01, 0x00, 0x00, 0xfe, 0x7f, 0x02, 0x01, 0x01, 0xbe, 0xbf,
        ];
        assert_eq!(
            Summary::decode(&count),
            Summary {
                operation: Operation::Count,
                flags: 0,
                field: 1,
                additional: None
            }
        );
        let running = Summary::decode(&definition(1, 0x14, 5, Some((0xd0_0001, 9))));
        assert_eq!(running.operation, Operation::Total);
        assert!(running.is_running() && running.uses_groups());
        assert_eq!(
            running.additional,
            Some(FieldId {
                occurrence: 0xd0_0001,
                field: 9
            })
        );
        // A missing definition is a total of nothing (5.1).
        let missing = Summary::decode(&[]);
        assert_eq!(missing.operation, Operation::Total);
        assert!(missing.summarizes_nothing() && missing.additional.is_none());
        assert_eq!(Summary::decode(&[9]).operation, Operation::List);
        assert_eq!(Summary::decode(&[8]).operation, Operation::Other(8));
    }

    fn numbers(values: &[Option<i64>]) -> Vec<Vec<Value>> {
        values
            .iter()
            .map(|v| vec![v.map_or_else(|| Value::empty(ValueType::Number), Value::number)])
            .collect()
    }

    fn text(v: Value, cx: &Ctx) -> String {
        v.to_text(cx).into_string()
    }

    /// file-metadata.md 5.3's probe: N = 5, 1, 4, 2, empty, 3 and W = 1, 2,
    /// 3, 1, 1, 2 in creation order.
    #[test]
    fn the_probe_found_set_unsorted() {
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
        let values = numbers(&[Some(5), Some(1), Some(4), Some(2), None, Some(3)]);
        let weights: Vec<Value> = [1, 2, 3, 1, 1, 2].map(Value::number).to_vec();
        let found = Found {
            values: &values,
            weights: &weights,
            additional_groups: None,
            sorted: false,
        };
        let summary = |operation: u8, flags: u8| {
            Summary::decode(&definition(operation, flags, 1, Some((0xd0_0001, 2))))
        };
        let last = Some(5);
        let cases = [
            (summary(1, 0), "15"),
            (summary(2, 0), "3"),
            (summary(2, 1), "3"),
            (summary(3, 0), "5"),
            (summary(4, 0), "1"),
            (summary(5, 0), "5"),
            (summary(6, 0), "1.5811388300841897"),
            (summary(6, 1), "1.414213562373095"),
            (summary(7, 0), ".2"),
            (summary(9, 0), "5\r1\r4\r2\r3"),
            (summary(1, 4), "15"),
            (summary(3, 4), "5"),
        ];
        for (summary, expected) in cases {
            let value = summary.value(&found, last, &cx).unwrap();
            assert_eq!(text(value, &cx), expected, "{summary:?}");
        }
        // Running total and count per record; the empty record shows the
        // value so far.
        let running = |operation| -> Vec<String> {
            (0..6)
                .map(|p| {
                    text(
                        summary(operation, 4).value(&found, Some(p), &cx).unwrap(),
                        &cx,
                    )
                })
                .collect()
        };
        assert_eq!(running(1), ["5", "6", "10", "12", "12", "15"]);
        assert_eq!(running(3), ["1", "2", "3", "4", "4", "5"]);
        // Every record at once (an export's rows) reads the same values.
        for summary in [summary(1, 0), summary(1, 4), summary(3, 4), summary(7, 0)] {
            let each: Vec<String> = (0..6)
                .map(|p| text(summary.value(&found, Some(p), &cx).unwrap(), &cx))
                .collect();
            let all: Vec<String> = summary
                .values(&found, &cx)
                .unwrap()
                .into_iter()
                .map(|v| text(v, &cx))
                .collect();
            assert_eq!(all, each, "{summary:?}");
        }
        // Fractions; subtotaled without a sort by its break field: empty.
        let fraction = summary(7, 0).value(&found, Some(0), &cx).unwrap();
        assert_eq!(text(fraction, &cx), ".3333333333333333");
        assert!(summary(7, 0)
            .value(&found, Some(4), &cx)
            .unwrap()
            .is_empty());
        assert!(summary(7, 1)
            .value(&found, Some(0), &cx)
            .unwrap()
            .is_empty());
        // GetSummary: the grand summary, or empty when not sorted.
        let total = summary(1, 0);
        assert_eq!(
            text(
                total.group_value(&found, Some(2), Break::All, &cx).unwrap(),
                &cx
            ),
            "15"
        );
        assert!(total
            .group_value(&found, Some(2), Break::Unsorted, &cx)
            .unwrap()
            .is_empty());
    }

    /// The probe sorted by Status, N: b 2, d 4 (Done), then ∅, a 1, c 3,
    /// e 5 (Pending); the running total restarts by Status.
    #[test]
    fn the_probe_found_set_sorted_by_its_break_field() {
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
        let values = numbers(&[Some(2), Some(4), None, Some(1), Some(3), Some(5)]);
        let groups = [0, 0, 1, 1, 1, 1];
        let found = Found {
            values: &values,
            weights: &[],
            additional_groups: Some(&groups),
            sorted: true,
        };
        let restart = Summary::decode(&definition(1, 0x14, 1, Some((0xd0_0001, 3))));
        let per_record: Vec<String> = (0..6)
            .map(|p| text(restart.value(&found, Some(p), &cx).unwrap(), &cx))
            .collect();
        assert_eq!(per_record, ["2", "6", "", "1", "4", "9"]);
        let all = |summary: Summary| -> Vec<String> {
            let values = summary.values(&found, &cx).unwrap();
            values.into_iter().map(|v| text(v, &cx)).collect()
        };
        assert_eq!(all(restart), per_record);
        let total = Summary::decode(&definition(1, 0, 1, None));
        let by_status: Vec<String> = (0..6)
            .map(|p| {
                let v = total
                    .group_value(&found, Some(p), Break::Groups(&groups), &cx)
                    .unwrap();
                text(v, &cx)
            })
            .collect();
        assert_eq!(by_status, ["6", "6", "9", "9", "9", "9"]);
        // GetSummary of the running total by Status: its value at the end
        // of the group.
        let running_by_status: Vec<String> = (0..6)
            .map(|p| {
                let v = restart
                    .group_value(&found, Some(p), Break::Groups(&groups), &cx)
                    .unwrap();
                text(v, &cx)
            })
            .collect();
        assert_eq!(running_by_status, ["6", "6", "9", "9", "9", "9"]);
        // Fraction subtotaled by Status: the value over its group's total;
        // the empty record is not first, so empty.
        let subtotal = Summary::decode(&definition(7, 1, 1, Some((0xd0_0001, 3))));
        let fractions: Vec<String> = (0..6)
            .map(|p| text(subtotal.value(&found, Some(p), &cx).unwrap(), &cx))
            .collect();
        assert_eq!(
            fractions,
            [
                ".3333333333333333",
                ".6666666666666667",
                "",
                ".1111111111111111",
                ".3333333333333333",
                ".5555555555555556"
            ]
        );
        assert_eq!(all(subtotal), fractions);
    }

    /// The probe sorted by Category, Task: ∅ first, so its fraction and
    /// running count are 0 and its running total empty.
    #[test]
    fn an_empty_first_record_of_a_sorted_found_set() {
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
        let values = numbers(&[None, Some(1), Some(2), Some(3), Some(4), Some(5)]);
        let groups = [0, 0, 0, 1, 1, 1];
        let found = Found {
            values: &values,
            weights: &[],
            additional_groups: Some(&groups),
            sorted: true,
        };
        let get = |operation, flags, at| {
            let summary = Summary::decode(&definition(operation, flags, 1, Some((0xd0_0001, 4))));
            text(summary.value(&found, Some(at), &cx).unwrap(), &cx)
        };
        assert_eq!(get(7, 0, 0), "0");
        assert_eq!(get(7, 1, 0), "0");
        assert_eq!(get(3, 4, 0), "0");
        assert_eq!(get(1, 4, 0), "");
        assert_eq!(get(7, 1, 1), ".3333333333333333");
        assert_eq!(get(7, 1, 5), ".4166666666666667");
    }

    /// `group_values` gives every record what `group_value` gives it, for
    /// every operation and option, with groups finer than, equal to and
    /// coarser than the additional field's.
    #[test]
    fn group_values_in_one_pass_agree_with_each_group_value() {
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
        let values = numbers(&[
            None,
            Some(2),
            Some(4),
            None,
            Some(1),
            Some(3),
            Some(5),
            None,
            Some(7),
        ]);
        let weights: Vec<Value> = [1, 2, 3, 1, 1, 2, 1, 1, 2].map(Value::number).to_vec();
        let additional = [0, 0, 0, 1, 1, 1, 1, 2, 2];
        let breaks: [&[usize]; 3] = [
            &[0, 0, 0, 1, 1, 1, 1, 2, 2],
            &[0, 1, 1, 2, 3, 3, 4, 5, 5],
            &[0, 0, 0, 0, 0, 1, 1, 1, 1],
        ];
        for (operation, flags) in [
            (1, 0),
            (1, 0x04),
            (1, 0x14),
            (2, 0),
            (2, 1),
            (3, 0),
            (3, 0x04),
            (3, 0x14),
            (4, 0),
            (5, 0),
            (6, 0),
            (6, 1),
            (7, 0),
            (7, 1),
            (9, 0),
        ] {
            let summary = Summary::decode(&definition(operation, flags, 1, Some((0xd0_0001, 2))));
            for sorted_by_additional in [true, false] {
                let found = Found {
                    values: &values,
                    weights: &weights,
                    additional_groups: sorted_by_additional.then_some(&additional[..]),
                    sorted: true,
                };
                for groups in breaks {
                    // A group whose value fails (a deviation of one value)
                    // fails them all.
                    let each: Result<Vec<String>, CalcError> = (0..values.len())
                        .map(|p| {
                            summary
                                .group_value(&found, Some(p), Break::Groups(groups), &cx)
                                .map(|v| text(v, &cx))
                        })
                        .collect();
                    let once: Result<Vec<String>, CalcError> = summary
                        .group_values(&found, groups, &cx)
                        .map(|all| all.into_iter().map(|v| text(v, &cx)).collect());
                    assert_eq!(once, each, "{summary:?} by {groups:?}");
                }
            }
        }
    }

    #[test]
    fn repetitions_are_read_together_or_individually() {
        let together = Summary::decode(&definition(1, 0, 1, None));
        assert_eq!(together.repetitions(1, 3), 1..4);
        assert!(together.repetitions(2, 3).is_empty());
        let individually = Summary::decode(&definition(1, 2, 1, None));
        assert_eq!(individually.repetitions(2, 3), 2..3);
        assert!(individually.repetitions(4, 3).is_empty());
    }

    #[test]
    fn groups_are_runs_of_one_number() {
        let groups = [0, 0, 1, 1, 1, 2];
        assert_eq!(group_range(&groups, 0), 0..2);
        assert_eq!(group_range(&groups, 3), 2..5);
        assert_eq!(group_range(&groups, 5), 5..6);
    }
}
