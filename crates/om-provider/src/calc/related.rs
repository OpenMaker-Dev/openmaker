//! Narrow a relationship with the host's value index before reading its
//! records. The index only supplies candidates: the same comparisons as the
//! scan still decide which records relate.

use std::collections::BTreeSet;
use std::sync::Arc;

use om_data::calc::ops::{self, CompareOp};
use om_data::calc::{CalcError, Ctx, Locale, Number, Value, ValueType};
use om_data::host::{
    join_compare_op, CalcSchema, CalcSource, FieldClass, FieldDef, Hop, JoinOp, RecordValues,
    TableDef,
};

use super::NativeSource;

/// Records per JoinProjection when one call reads many records' related
/// sets through one relationship (#982): it bounds the program, the union
/// of candidates downloaded for it, and the comparisons that sort them out.
const JOIN_BATCH: usize = 256;
/// The field keys a join's source rows carry their match keys under: any
/// key the rows use (query-program.md 9.2). A number's neighborhood takes
/// both, its lower and its upper bound.
const LOW: u64 = 1;
const HIGH: u64 = 2;
/// `DBJoinPred::Type` (query-program.md 9.2).
const JOIN_EQUAL: u8 = 0;
const JOIN_LESS_EQUAL: u8 = 3;
const JOIN_GREATER_EQUAL: u8 = 5;

impl NativeSource<'_> {
    /// Before one call answers the related sets of many `records` of
    /// `from` through one relationship to `to` (a footer summary over the
    /// found set, #982), find their candidates together: a JoinProjection
    /// per [`JOIN_BATCH`] records on the value index the record's own find
    /// would use ([`indexed_hop`]), their union downloaded in bulk, and
    /// each record's set kept for its need. Each record keeps only the
    /// candidates its own keys select, so a set does not depend on which
    /// records were joined with it, and the scan's comparison decides as
    /// for one record. Anything this does not settle (an index the host
    /// does not have yet, a refused program, keys one whole-value lookup
    /// cannot stand for, a path of several hops) each need reads as
    /// before; a connection that fails fails the table instead, as one
    /// need's read would, so the needs do not each try it again.
    pub(super) fn prepare_related(
        &self,
        schema: &CalcSchema,
        from: u64,
        records: &[u64],
        to: u64,
        cx: &Ctx,
    ) {
        // A failure here is the need's to report when it reads.
        let _ = self.try_prepare_related(schema, from, records, to, cx);
    }

    fn try_prepare_related(
        &self,
        schema: &CalcSchema,
        from: u64,
        records: &[u64],
        to: u64,
        cx: &Ctx,
    ) -> Result<(), CalcError> {
        // Volatile keys or sorts are read again by every need.
        if !self.reusable_related(from, to) {
            return Ok(());
        }
        let Some(path) = schema.path(from, to) else {
            return Ok(());
        };
        let [hop] = path.as_slice() else {
            return Ok(());
        };
        let Some(plan) = hop_plan(schema, hop, cx) else {
            return Ok(());
        };
        let index = plan.fields[plan.index].2;
        if !matches!(index.value_type, ValueType::Text | ValueType::Number) {
            return Ok(());
        }
        let target = Ctx {
            locale: plan.target,
            ..*cx
        };
        let pending: Vec<u64> = {
            let known = self.related.borrow();
            let mut seen = BTreeSet::new();
            records
                .iter()
                .copied()
                .filter(|record| !known.contains_key(&(from, *record, to)) && seen.insert(*record))
                .collect()
        };
        if pending.len() < 2 {
            return Ok(());
        }
        let to_table = plan.to_table.id;
        let right_globals = plan
            .fields
            .iter()
            .any(|(_, _, right)| right.global)
            .then(|| self.globals(to_table))
            .transpose()?;
        for chunk in pending.chunks(JOIN_BATCH) {
            // A read the window stopped joins no further batch (#987).
            if self.stopped() {
                return Ok(());
            }
            self.download_ahead(plan.from_table.id, chunk)?;
            let mut rows: Vec<om_query::find::JoinRow> = Vec::new();
            let mut joined: Vec<(u64, Vec<Vec<Value>>, Vec<JoinKey>)> = Vec::new();
            for &record in chunk {
                let keys = match_keys(self, schema, from, &plan, record, cx)?;
                // An empty match field relates no records.
                if keys.iter().any(Vec::is_empty) {
                    self.related
                        .borrow_mut()
                        .insert((from, record, to), Arc::from([]));
                    continue;
                }
                let Some(own) = keys[plan.index]
                    .iter()
                    .map(|value| join_key(value, index.value_type, &target))
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                rows.push((record, join_row(&own)));
                joined.push((record, keys, own));
            }
            if rows.is_empty() {
                continue;
            }
            let Some(union) = self.join_candidates(to_table, index.id, index.value_type, &rows)?
            else {
                // The index cannot answer: each need reads as before.
                return Ok(());
            };
            let union: Vec<u64> = union.iter().collect();
            self.download_ahead(to_table, &union)?;
            let union: BTreeSet<u64> = union.into_iter().collect();
            // Creation order, as one record's read puts them.
            let mut candidates = Vec::new();
            for id in creation_order(self, to_table, &union)? {
                candidates.push((id, self.record(to_table, id)?));
            }
            let shelves = Shelves::of(&candidates, index, &target);
            for (record, keys, own) in joined {
                let mut related: Vec<u64> = shelves
                    .positions(&own)
                    .into_iter()
                    .map(|at| &candidates[at])
                    .filter(|(_, row)| {
                        let theirs = values(index, row.as_deref(), None);
                        own.iter()
                            .any(|key| theirs.iter().any(|value| key.selects(value, &target)))
                            && relates(&plan, &keys, row.as_deref(), right_globals.as_deref(), cx)
                    })
                    .map(|(id, _)| *id)
                    .collect();
                if let Some(sort) = &hop.sort {
                    related = om_data::host::sort_records(self, schema, to, related, sort, cx)?;
                }
                self.related
                    .borrow_mut()
                    .insert((from, record, to), related.into());
            }
        }
        Ok(())
    }

    /// The union of the records of `table` whose `field` the host's value
    /// index matches to any of `rows`' keys: one JoinProjection after a
    /// check that the index exists (query-program.md 9.2, 8.5), on the
    /// connection holding the table, or more when the rows' keys do not
    /// fit one request (halves, joined). `None` when the index cannot
    /// answer: it does not exist yet, the host refuses the program with an
    /// error, or one row's keys alone do not fit (the records' own finds
    /// then read as before). A request that fails fails the table.
    fn join_candidates(
        &self,
        table: u64,
        field: u64,
        kind: ValueType,
        rows: &[om_query::find::JoinRow],
    ) -> Result<Option<om_hbam::RidList>, CalcError> {
        match self.join_once(table, field, kind, rows)? {
            Joined::Answer(answer) => Ok(answer),
            Joined::TooBig if rows.len() > 1 => {
                let (first, second) = rows.split_at(rows.len() / 2);
                let Some(mut union) = self.join_candidates(table, field, kind, first)? else {
                    return Ok(None);
                };
                let Some(more) = self.join_candidates(table, field, kind, second)? else {
                    return Ok(None);
                };
                for id in more.iter() {
                    union.insert(id);
                }
                Ok(Some(union))
            }
            Joined::TooBig => Ok(None),
        }
    }

    /// [`Self::join_candidates`] in one program, or [`Joined::TooBig`].
    fn join_once(
        &self,
        table: u64,
        field: u64,
        kind: ValueType,
        rows: &[om_query::find::JoinRow],
    ) -> Result<Joined, CalcError> {
        use om_query::find::{self as finders, IndexKind};
        let predicates: Vec<(u8, u64, u64)> = match kind {
            ValueType::Number => vec![
                (JOIN_GREATER_EQUAL, field, LOW),
                (JOIN_LESS_EQUAL, field, HIGH),
            ],
            _ => vec![(JOIN_EQUAL, field, LOW)],
        };
        let join = |session: &mut om_session::Session, wire: u64| {
            let file_id = session
                .opened_file()
                .ok_or_else(|| om_data::DataError::new("No hosted file is open."))?
                .reply
                .file_id;
            // The join follows the check.
            let key = om_crypto::query_access_key(2, session.connection_key(), file_id);
            // A program larger than one request is not sent (Session::query).
            let Some(program) = finders::checked_join_program(
                file_id,
                wire,
                &[(field, IndexKind::Value)],
                rows,
                &predicates,
                Some(key),
            )
            .filter(|program| program.len() <= om_protocol::PAGE_LIMIT) else {
                return Ok(Joined::TooBig);
            };
            let reply = session
                .query(&program)
                .map_err(|e| crate::failure("join related records", e))?;
            if crate::found::query_error(&reply.slots).is_some() {
                return Ok(Joined::Answer(None));
            }
            match finders::indexes_existed(&reply.slots, 1) {
                Some(existed) if existed == [true] => {}
                Some(_) => return Ok(Joined::Answer(None)),
                None => {
                    return Err(om_data::DataError::new(
                        "The host's join reply could not be read.",
                    ))
                }
            }
            finders::found(&reply.slots)
                .map(|found| Joined::Answer(Some(found)))
                .ok_or_else(|| om_data::DataError::new("The host's join reply could not be read."))
        };
        // with_table fails the table on an error: the request failed.
        self.with_table(table, join)
    }

    pub(super) fn indexed_related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Option<Arc<[u64]>>, CalcError> {
        indexed_related(self, schema, from, record, to, cx)
    }
}

/// A relationship path, hop by hop: each hop relates the records some
/// record of the previous hop relates (file-metadata.md 4.3), narrowed by
/// the host's value index. A hop the index cannot answer (a missing index,
/// a key one whole-value lookup cannot stand for ([`lookup_text`]), no `=`
/// predicate, `×` with other predicates, or calculated target fields)
/// leaves the existing scan in charge of the whole path. No partial index
/// result is returned.
fn indexed_related<S: CalcSource + RecordOrder + ?Sized>(
    source: &S,
    schema: &CalcSchema,
    from: u64,
    record: u64,
    to: u64,
    cx: &Ctx,
) -> Result<Option<Arc<[u64]>>, CalcError> {
    let Some(path) = schema.path(from, to) else {
        return Ok(None);
    };
    let (Some(last), Some(table)) = (path.last(), schema.table_of(to)) else {
        return Ok(None);
    };
    let mut related = BTreeSet::from([record]);
    for hop in &path {
        let mut next = BTreeSet::new();
        if let Some(records) = cartesian_hop(source, schema, hop)? {
            // It relates the same records to each of `related` (never
            // empty here), so they are listed once, not once per record.
            next.extend(records.iter().copied());
        } else {
            for &record in &related {
                let Some(records) = indexed_hop(source, schema, hop, record, cx)? else {
                    return Ok(None);
                };
                next.extend(records.iter().copied());
            }
        }
        // Nothing related here relates anything further along the path.
        if next.is_empty() {
            return Ok(Some(Arc::from([])));
        }
        related = next;
    }
    // Creation order, independently of the window's found set.
    let mut records = creation_order(source, table.id, &related)?;
    // Over several relationships, the last one's sort orders the set.
    if let Some(sort) = &last.sort {
        records = om_data::host::sort_records(source, schema, to, records, sort, cx)?;
    }
    Ok(Some(records.into()))
}

/// A hop whose predicates are all `×`: it relates every record of its
/// destination to each record (file-metadata.md 4.3), so its destination's
/// RID list, read without any record. `None` for any other hop.
fn cartesian_hop<S: CalcSource + ?Sized>(
    source: &S,
    schema: &CalcSchema,
    hop: &Hop,
) -> Result<Option<Arc<[u64]>>, CalcError> {
    let cartesian = !hop.predicates.is_empty()
        && hop
            .predicates
            .iter()
            .all(|predicate| predicate.op == JoinOp::Cartesian);
    match (
        cartesian,
        schema.table_of(hop.from),
        schema.table_of(hop.to),
    ) {
        (true, Some(_), Some(to_table)) => source.records(to_table.id).map(Some),
        _ => Ok(None),
    }
}

/// One hop's predicates as the index path reads them: each comparison with
/// its fields, the `=` predicate whose value index gives the candidates, and
/// the formats of the file holding the target table, which a lookup's text
/// is in.
struct HopPlan<'s> {
    from_table: &'s TableDef,
    to_table: &'s TableDef,
    fields: Vec<(CompareOp, &'s FieldDef, &'s FieldDef)>,
    index: usize,
    target: &'s Locale,
}

/// [`HopPlan`] of `hop`, or `None` when the index cannot answer it: no
/// usable `=` predicate, `×` beside other predicates, or a calculated or
/// summary target.
fn hop_plan<'s>(schema: &'s CalcSchema, hop: &Hop, cx: &Ctx) -> Option<HopPlan<'s>> {
    if hop.predicates.is_empty() {
        return None;
    }
    let from_table = schema.table_of(hop.from)?;
    let to_table = schema.table_of(hop.to)?;
    let mut fields = Vec::with_capacity(hop.predicates.len());
    for predicate in &hop.predicates {
        // `×` beside other predicates stays with the scan.
        let op = join_compare_op(predicate.op)?;
        let left = from_table.fields.get(&predicate.left)?;
        let right = to_table.fields.get(&predicate.right)?;
        if left.class == FieldClass::Summary || !stored_match_field(right) {
            return None;
        }
        fields.push((op, left, right));
    }
    // A lookup's text is in the formats of the file holding the table it
    // finds in (`CalcSource::lookup`), which may be a related file's.
    let target = schema
        .table_context(to_table.id)
        .map_or(&schema.locale, |file| &file.locale);
    // lookup() builds English text keys, date keys, and conservative
    // neighborhoods of positive whole-number keys. Other
    // types and index languages must keep using the scan. Its whole-value
    // finds read the value index: a word index alone cannot answer them.
    let index = fields.iter().position(|(op, left, right)| {
        *op == CompareOp::Eq
            && !right.global
            && right.value_indexed()
            && left.value_type == right.value_type
            && match right.value_type {
                ValueType::Text => {
                    // Language 0 is the field's file's own.
                    let language = match right.language {
                        0 => u32::from(target.language.id()),
                        language => language,
                    };
                    cx.locale.language.id() == 21 && language == 21
                }
                ValueType::Date | ValueType::Number => true,
                _ => false,
            }
    })?;
    Some(HopPlan {
        from_table,
        to_table,
        fields,
        index,
        target,
    })
}

/// The values on `record`'s side of each predicate of `plan`, an unstored
/// calculation evaluated.
fn match_keys<S: CalcSource + ?Sized>(
    source: &S,
    schema: &CalcSchema,
    from: u64,
    plan: &HopPlan,
    record: u64,
    cx: &Ctx,
) -> Result<Vec<Vec<Value>>, CalcError> {
    let left_record = source.record(plan.from_table.id, record)?;
    let left_globals = plan
        .fields
        .iter()
        .any(|(_, left, _)| left.global)
        .then(|| source.globals(plan.from_table.id))
        .transpose()?;
    plan.fields
        .iter()
        .map(|(_, left, _)| {
            if left.is_unstored_calculation() {
                source.source_match_values(schema, from, record, left.id, cx)
            } else {
                Ok(values(
                    left,
                    left_record.as_deref(),
                    left_globals.as_deref(),
                ))
            }
        })
        .collect()
}

/// Whether `row` relates to the record whose [`match_keys`] are `keys`:
/// every predicate holds for some value of each side, with the scan's
/// comparison.
fn relates(
    plan: &HopPlan,
    keys: &[Vec<Value>],
    row: Option<&RecordValues>,
    right_globals: Option<&RecordValues>,
    cx: &Ctx,
) -> bool {
    plan.fields
        .iter()
        .enumerate()
        .all(|(index, (op, _, right))| {
            let theirs = values(right, row, right_globals);
            keys[index].iter().any(|ours| {
                theirs
                    .iter()
                    .any(|theirs| ops::compare_op(ours, *op, theirs, cx).is_true(cx))
            })
        })
}

/// The records one hop relates to `record` of its `from` occurrence, in
/// creation order, or `None` when the index cannot answer it. One `=`
/// predicate on a value-indexed field gives the candidates; every
/// predicate, the inequalities (a date range, say) included, then decides
/// on them with the scan's comparison ([`join_compare_op`]). The scan
/// would read the whole destination table instead.
fn indexed_hop<S: CalcSource + RecordOrder + ?Sized>(
    source: &S,
    schema: &CalcSchema,
    hop: &Hop,
    record: u64,
    cx: &Ctx,
) -> Result<Option<Arc<[u64]>>, CalcError> {
    let Some(plan) = hop_plan(schema, hop, cx) else {
        return Ok(None);
    };
    let target = Ctx {
        locale: plan.target,
        ..*cx
    };
    let keys = match_keys(source, schema, hop.from, &plan, record, cx)?;
    // An empty match field relates no records, including empty global
    // values and empty repetitions (file-metadata.md 4.3).
    if keys.iter().any(Vec::is_empty) {
        return Ok(Some(Arc::from([])));
    }
    let index = plan.fields[plan.index].2;
    let Some(texts) = keys[plan.index]
        .iter()
        .map(|value| lookup_text(value, index.value_type, &target))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    let to_table = plan.to_table.id;
    let mut candidates = BTreeSet::new();
    for text in texts {
        let Some(ids) = source.lookup(to_table, index.id, &text)? else {
            return Ok(None);
        };
        candidates.extend(ids.iter().copied());
    }
    if candidates.is_empty() {
        return Ok(Some(Arc::from([])));
    }
    let candidates_to_read: Vec<u64> = candidates.iter().copied().collect();
    source.prefetch(to_table, &candidates_to_read)?;
    let right_globals = plan
        .fields
        .iter()
        .any(|(_, _, right)| right.global)
        .then(|| source.globals(to_table))
        .transpose()?;
    // Creation order, independently of the window's found set. A
    // candidate the table no longer has reads as no row, which relates
    // nothing.
    let mut related = Vec::new();
    for id in creation_order(source, to_table, &candidates)? {
        let row = source.record(to_table, id)?;
        if relates(&plan, &keys, row.as_deref(), right_globals.as_deref(), cx) {
            related.push(id);
        }
    }
    Ok(Some(related.into()))
}

/// How a source lists a table's records ([`CalcSource::records`]).
pub(super) trait RecordOrder {
    /// Whether it lists every table's records in ascending record id
    /// order, as a host's record list does. Creation order is then the
    /// ids' order (file-metadata.md 4.3), so [`creation_order`] can sort a
    /// set of records rather than read the table's whole list (#1100).
    fn records_in_id_order(&self) -> bool;

    /// `table`'s record list when the source holds it without reading it.
    fn known_records(&self, table: u64) -> Option<Arc<[u64]>>;
}

impl RecordOrder for NativeSource<'_> {
    /// Its record lists are the host's (`RidList`, ascending ids), or the
    /// window's table's as the page read adjusted it (`with_held`), still
    /// ascending.
    fn records_in_id_order(&self) -> bool {
        true
    }

    fn known_records(&self, table: u64) -> Option<Arc<[u64]>> {
        self.tables.borrow().get(&table).cloned()
    }
}

/// `ids`, records of `table`, in creation order. A list the source holds
/// orders and filters them, as before: the one the page read keeps leaves
/// out the records the open transaction deleted, which the host's index
/// still lists. Without one, where the source lists records in id order,
/// the ids are sorted rather than the whole list read: the list it would
/// read is the host's, of the same records as its index (#1100).
fn creation_order<S: CalcSource + RecordOrder + ?Sized>(
    source: &S,
    table: u64,
    ids: &BTreeSet<u64>,
) -> Result<Vec<u64>, CalcError> {
    let all = match source.known_records(table) {
        Some(all) => all,
        None if source.records_in_id_order() => return Ok(ids.iter().copied().collect()),
        None => source.records(table)?,
    };
    Ok(all.iter().copied().filter(|id| ids.contains(id)).collect())
}

fn stored_match_field(field: &FieldDef) -> bool {
    field.class != FieldClass::Summary && !field.is_unstored_calculation()
}

fn values(
    field: &FieldDef,
    record: Option<&RecordValues>,
    globals: Option<&RecordValues>,
) -> Vec<Value> {
    let store = if field.global { globals } else { record };
    (1..=field.repetitions)
        .filter_map(|repetition| store?.get(field.id, repetition))
        .filter(|value| !value.is_empty())
        .cloned()
        .collect()
}

/// The text [`CalcSource::lookup`] finds `value` by, or `None` where one
/// whole-value lookup cannot stand for the scan's comparison. A text is
/// looked up by its value-index key, not read as a find, so characters a
/// find reads as operators (`#`, `.`, `=`, quotes) are its own (#1068). An
/// empty text, edge spaces, and line breaks or other control characters
/// keep the scan: the scan compares a whole repetition, while a value
/// index holds each line.
fn lookup_text(value: &Value, kind: ValueType, cx: &Ctx) -> Option<String> {
    match (kind, value) {
        (ValueType::Text, Value::Text(_)) => {
            let text = value.to_text(cx).into_string();
            if text.is_empty()
                || text.trim() != text
                || text
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
                // Finder operands have one-byte lengths.
                || om_collate::text_key(&text).len() > 253
            {
                None
            } else {
                Some(text)
            }
        }
        (ValueType::Date, Value::Date(date) | Value::DateText(date, _)) => {
            Some(date.format(cx.locale))
        }
        (ValueType::Number, Value::Number(number) | Value::NumberText(number, _)) => {
            let integer = number.to_i64()?;
            if number.compare(&Number::from_i64(integer)) != Some(std::cmp::Ordering::Equal) {
                return None;
            }
            let text = integer.to_string();
            numeric_lookup_criterion(&text)?;
            Some(text)
        }
        _ => None,
    }
}

/// What one JoinProjection gave ([`NativeSource::join_candidates`]): its
/// answer, or that its program would not fit one request.
enum Joined {
    Answer(Option<om_hbam::RidList>),
    TooBig,
}

/// One match key of a record, as its own find would look it up
/// ([`lookup_text`]): a text's whole-value key, or a whole number's
/// neighborhood `n - 1 ... n + 1` ([`numeric_lookup_criterion`]).
enum JoinKey {
    Text(Vec<u8>),
    Number(i64),
}

impl JoinKey {
    /// Whether this key's own find would list a record holding `value`:
    /// the same text key, or a number within the neighborhood. A record
    /// another record's key brought along is kept only so.
    fn selects(&self, value: &Value, cx: &Ctx) -> bool {
        match self {
            JoinKey::Text(key) => {
                matches!(value, Value::Text(_))
                    && om_collate::text_key(value.to_text(cx).into_string().trim()) == *key
            }
            JoinKey::Number(n) => match value {
                Value::Number(number) | Value::NumberText(number, _) => {
                    number.compare(&Number::from_i64(n - 1)) != Some(std::cmp::Ordering::Less)
                        && number.compare(&Number::from_i64(n + 1))
                            != Some(std::cmp::Ordering::Greater)
                }
                _ => false,
            },
        }
    }
}

/// A batch's candidates by what their index field holds, so that a record
/// visits only those its keys could select ([`JoinKey::selects`] still
/// decides): positions in creation order by text key, and by a number's
/// whole part toward zero (one in `n - 1 ... n + 1` has `n - 1`, `n` or
/// `n + 1`).
#[derive(Default)]
struct Shelves {
    text: std::collections::HashMap<Vec<u8>, Vec<usize>>,
    number: std::collections::HashMap<i64, Vec<usize>>,
}

impl Shelves {
    fn of(candidates: &[(u64, Option<Arc<RecordValues>>)], index: &FieldDef, cx: &Ctx) -> Self {
        let mut shelves = Shelves::default();
        for (at, (_, row)) in candidates.iter().enumerate() {
            for value in values(index, row.as_deref(), None) {
                match &value {
                    Value::Text(_) => {
                        let key = om_collate::text_key(value.to_text(cx).into_string().trim());
                        shelves.text.entry(key).or_default().push(at);
                    }
                    Value::Number(number) | Value::NumberText(number, _) => {
                        if let Some(whole) = number.to_i64() {
                            shelves.number.entry(whole).or_default().push(at);
                        }
                    }
                    _ => {}
                }
            }
        }
        shelves
    }

    /// The positions `keys` could select, ascending (creation order).
    fn positions(&self, keys: &[JoinKey]) -> Vec<usize> {
        let mut at: Vec<usize> = Vec::new();
        for key in keys {
            match key {
                JoinKey::Text(key) => at.extend(self.text.get(key).into_iter().flatten()),
                JoinKey::Number(n) => {
                    // Two wider: the comparison rounds to the display
                    // precision, the whole part does not.
                    for whole in n - 2..=n + 2 {
                        at.extend(self.number.get(&whole).into_iter().flatten());
                    }
                }
            }
        }
        at.sort_unstable();
        at.dedup();
        at
    }
}

/// [`JoinKey`] of `value`, or `None` where its own lookup could not stand
/// for the scan either ([`lookup_text`]).
fn join_key(value: &Value, kind: ValueType, cx: &Ctx) -> Option<JoinKey> {
    let text = lookup_text(value, kind, cx)?;
    match kind {
        // A key the index holds only the start of makes the record's own
        // find approximate, which leaves it to the scan.
        ValueType::Text => Some(om_collate::text_key(&text))
            .filter(|key| key.len() < om_collate::INDEX_KEY_LIMIT)
            .map(JoinKey::Text),
        // lookup_text gave a whole number of at least 1; its shelves reach
        // two past the neighborhood.
        ValueType::Number => text
            .parse::<i64>()
            .ok()
            .filter(|n| n.checked_add(2).is_some())
            .map(JoinKey::Number),
        _ => None,
    }
}

/// A join source row's match keys: under [`LOW`] the text keys or each
/// number's lower bound, under [`HIGH`] each number's upper bound.
fn join_row(keys: &[JoinKey]) -> Vec<(u64, Vec<Vec<u8>>)> {
    let mut low = Vec::new();
    let mut high = Vec::new();
    for key in keys {
        match key {
            JoinKey::Text(key) => low.push(key.clone()),
            JoinKey::Number(n) => {
                if let (Some(a), Some(b)) = (
                    om_collate::number_key(&(n - 1).to_string()),
                    om_collate::number_key(&(n + 1).to_string()),
                ) {
                    low.push(a);
                    high.push(b);
                }
            }
        }
    }
    let mut row = vec![(LOW, low)];
    if !high.is_empty() {
        row.push((HIGH, high));
    }
    row
}

/// Number `=` rounds to the operands' display precision. A tiny fraction
/// can therefore compare equal to an integer while having a distinct
/// index key. Use established whole-number bounds around the value and
/// retain the existing exact comparison after downloading candidates.
/// No fraction or negative-number key encoding is assumed here.
pub(super) fn numeric_lookup_criterion(text: &str) -> Option<String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let integer: i64 = text.parse().ok()?;
    let low = integer.checked_sub(1).filter(|n| *n >= 0)?;
    let high = integer.checked_add(1)?;
    Some(format!("{low}...{high}"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use om_data::calc::{Date, Locale};
    use om_data::host::{related_by_scan, JoinPredicate, MemorySource, RelationshipDef};

    use super::*;

    const PARENT: u64 = 0x8001;
    const CHILD: u64 = 0x8002;
    const FROM: u64 = 0xd00001;
    const TO: u64 = 0xd00002;

    struct Source {
        data: MemorySource,
        candidates: BTreeMap<String, Option<Arc<[u64]>>>,
        reads: RefCell<Vec<(u64, u64)>>,
        lookups: RefCell<Vec<String>>,
        /// The tables whose record lists were read.
        listed: RefCell<Vec<u64>>,
    }

    /// Its records are in insertion order, which the fixtures make differ
    /// from id order to check that creation order is kept.
    impl RecordOrder for Source {
        fn records_in_id_order(&self) -> bool {
            false
        }

        fn known_records(&self, _table: u64) -> Option<Arc<[u64]>> {
            None
        }
    }

    impl CalcSource for Source {
        fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
            self.reads.borrow_mut().push((table, record));
            self.data.record(table, record)
        }

        fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
            self.listed.borrow_mut().push(table);
            self.data.records(table)
        }

        fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
            self.data.globals(table)
        }

        fn lookup(
            &self,
            _table: u64,
            _field: u64,
            text: &str,
        ) -> Result<Option<Arc<[u64]>>, CalcError> {
            self.lookups.borrow_mut().push(text.into());
            Ok(self.candidates.get(text).cloned().flatten())
        }
    }

    /// A source keeping a scan's match values up to `budget` bytes, as the
    /// window's does (#804).
    struct Keeping<'a> {
        data: &'a MemorySource,
        budget: usize,
        kept: RefCell<Option<(om_data::host::ScanRows, usize)>>,
        /// How many times a table's record list was read.
        listed: std::cell::Cell<usize>,
    }

    impl CalcSource for Keeping<'_> {
        fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
            self.data.record(table, record)
        }

        fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
            self.listed.set(self.listed.get() + 1);
            self.data.records(table)
        }

        fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
            self.data.globals(table)
        }

        fn scan_rows(&self, _table: u64, _fields: &[u64]) -> Option<om_data::host::ScanRows> {
            self.kept
                .borrow()
                .as_ref()
                .map(|(rows, _)| Arc::clone(rows))
        }

        fn scan_keep_budget(&self) -> usize {
            self.budget
        }

        fn keep_scan_rows(
            &self,
            _table: u64,
            _fields: &[u64],
            rows: om_data::host::ScanRows,
            bytes: usize,
        ) {
            *self.kept.borrow_mut() = Some((rows, bytes));
        }
    }

    fn fixture() -> (CalcSchema, Source) {
        fixture_with(RelationshipDef::new(
            1,
            FROM,
            TO,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 1,
                right: 1,
            }],
        ))
    }

    fn fixture_with(relationship: RelationshipDef) -> (CalcSchema, Source) {
        let mut schema = CalcSchema::new("Related", Locale::en_us());
        schema.add_table(PARENT, "Parent");
        schema.add_table(CHILD, "Child");
        schema.add_occurrence(FROM, "Parent", PARENT);
        schema.add_occurrence(TO, "Child", CHILD);
        schema.add_field(
            PARENT,
            FieldDef::data(1, "Key", ValueType::Text).with_repetitions(3),
        );
        let mut key = FieldDef::data(1, "Key", ValueType::Text).with_repetitions(2);
        key.options = 0x8000_0000;
        schema.add_field(CHILD, key);
        schema.add_relationship(relationship);
        let mut data = MemorySource::new();
        let mut parent = RecordValues::default();
        parent.set(1, 1, Value::text("A"));
        parent.set(1, 2, Value::text("B"));
        parent.set(1, 3, Value::text(""));
        data.put_record(PARENT, 1, parent);
        // Deliberately use creation order different from numeric RID order.
        for (id, text, second) in [(30, "A", ""), (10, "wrong", "B"), (20, "A\rB", "")] {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text(text));
            row.set(1, 2, Value::text(second));
            data.put_record(CHILD, id, row);
        }
        // A related set does not inherit the window's omitted records.
        data.set_found_set(TO, &[20], None);
        let candidates: BTreeMap<String, Option<Arc<[u64]>>> = BTreeMap::from([
            ("A".into(), Some(Arc::from([20, 30]))),
            ("B".into(), Some(Arc::from([10, 20]))),
        ]);
        (
            schema,
            Source {
                data,
                candidates,
                reads: RefCell::default(),
                lookups: RefCell::default(),
                listed: RefCell::default(),
            },
        )
    }

    fn today() -> Date {
        Date::from_ymd(2026, 10, 29).unwrap()
    }

    /// A scan offers its match values when they fit the source's budget,
    /// with a global target field's values read once rather than copied
    /// into every row, and the next scan matches against them (#804).
    #[test]
    fn a_scan_keeps_match_values_within_its_budget_and_globals_once() {
        let (mut schema, mut source) = fixture_with(RelationshipDef::new(
            1,
            FROM,
            TO,
            vec![
                JoinPredicate {
                    op: JoinOp::Equal,
                    left: 1,
                    right: 1,
                },
                JoinPredicate {
                    op: JoinOp::Equal,
                    left: 1,
                    right: 2,
                },
            ],
        ));
        schema.add_field(
            CHILD,
            FieldDef::data(2, "gKey", ValueType::Text).with_global(true),
        );
        let mut globals = RecordValues::default();
        globals.set(2, 1, Value::text("B"));
        source.data.set_globals(CHILD, globals);
        let cx = Ctx::new(&schema.locale, today());
        let keeping = |budget| Keeping {
            data: &source.data,
            budget,
            kept: RefCell::default(),
            listed: Default::default(),
        };
        let roomy = keeping(1 << 20);
        let scan = |source: &Keeping| related_by_scan(source, &schema, FROM, 1, TO, &cx).unwrap();
        assert_eq!(&*scan(&roomy), &[30, 10]);
        let (rows, bytes) = roomy.kept.borrow().clone().expect("the scan was kept");
        assert_eq!(
            rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [30, 10, 20]
        );
        assert!(rows.iter().all(|(_, values)| values[1].is_empty()));
        assert_eq!(
            bytes,
            rows.iter()
                .map(|(_, values)| om_data::host::scan_values_bytes(values))
                .sum::<usize>()
        );
        assert_eq!(&*scan(&roomy), &[30, 10]);
        assert_eq!(roomy.listed.get(), 1, "the second scan reads no records");
        // One byte short of what they hold: nothing is kept.
        let tight = keeping(bytes - 1);
        assert_eq!(&*scan(&tight), &[30, 10]);
        assert!(tight.kept.borrow().is_none());
    }

    #[test]
    fn indexed_repetitions_filter_supersets_and_preserve_creation_order() {
        let (schema, source) = fixture();
        let cx = Ctx::new(&schema.locale, today());
        let result = indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .unwrap();
        assert_eq!(&*result, &[30, 10]);
        assert_eq!(
            result,
            related_by_scan(&source.data, &schema, FROM, 1, TO, &cx).unwrap()
        );
        assert_eq!(&*source.lookups.borrow(), &["A", "B"]);
    }

    #[test]
    fn a_calculated_source_key_uses_the_stored_targets_index() {
        use om_data::formula::{self, Formula, Key};
        let (mut schema, mut source) = fixture_with(RelationshipDef::new(
            1,
            FROM,
            TO,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 4,
                right: 1,
            }],
        ));
        let formula = {
            let mut context = formula::Context::new(schema.names());
            context.occurrence = Some(FROM);
            Formula {
                context: Some(Key::from_id(FROM)),
                formatted: None,
                polish: formula::compile("Key", &context).unwrap().expr,
            }
        };
        schema.add_field(
            PARENT,
            FieldDef::calculation(4, "Calculated key", ValueType::Text, formula, false)
                .with_repetitions(3),
        );
        for id in 100..1000 {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text("unrelated"));
            source.data.put_record(CHILD, id, row);
        }
        let cx = Ctx::new(&schema.locale, today());
        let result = indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .unwrap();
        assert_eq!(&*result, &[30, 10]);
        assert_eq!(
            result,
            related_by_scan(&source.data, &schema, FROM, 1, TO, &cx).unwrap()
        );
        assert_eq!(&*source.lookups.borrow(), &["A", "B"]);
        assert!(source
            .reads
            .borrow()
            .iter()
            .all(|(t, r)| *t != CHILD || *r < 100));
    }

    #[test]
    fn numeric_candidates_keep_alternate_text_and_rounding_matches_without_scanning() {
        let (mut schema, mut source) = fixture();
        schema.add_field(PARENT, FieldDef::data(1, "Ref", ValueType::Number));
        let mut field = FieldDef::data(1, "Ref", ValueType::Number);
        field.options = 0x8000_0000;
        schema.add_field(CHILD, field);
        let cx = Ctx::new(&schema.locale, today());
        let numeric = |text| om_data::host::stored_value(text, ValueType::Number, &cx);
        let mut parent = RecordValues::default();
        parent.set(1, 1, numeric("0250000.00"));
        source.data.put_record(PARENT, 1, parent);
        for (id, text) in [
            (30, "250000"),
            (10, "250000.0"),
            (20, "250001"),
            (40, "250000.00000000000000001"),
            (50, "249999"),
        ] {
            let mut row = RecordValues::default();
            let value = if id == 40 {
                // A computed number can retain guard digits beyond its
                // display precision; `=` rounds them away.
                let guard = Number::from_i64(1)
                    .div(&Number::from_i64(100_000_000_000_000_000), 16)
                    .unwrap();
                Value::Number(Number::from_i64(250000).add(&guard, 16))
            } else {
                numeric(text)
            };
            row.set(1, 1, value);
            source.data.put_record(CHILD, id, row);
        }
        for id in 100..10_000 {
            let mut row = RecordValues::default();
            row.set(1, 1, numeric("990000"));
            source.data.put_record(CHILD, id, row);
        }
        source.candidates.clear();
        source
            .candidates
            .insert("250000".into(), Some(Arc::from([30, 10, 20, 40, 50])));
        let actual = indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .unwrap();
        assert_eq!(&*actual, &[30, 10, 40]);
        assert_eq!(
            actual,
            related_by_scan(&source.data, &schema, FROM, 1, TO, &cx).unwrap()
        );
        assert_eq!(&*source.lookups.borrow(), &["250000"]);
        assert!(source
            .reads
            .borrow()
            .iter()
            .all(|(t, r)| *t != CHILD || *r < 100));
    }

    #[test]
    fn numeric_lookup_uses_only_proven_whole_number_bounds() {
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, today());
        assert_eq!(
            numeric_lookup_criterion("250000").as_deref(),
            Some("249999...250001")
        );
        for text in ["0", "-1", "3.25", "", "?", "1e9", "9223372036854775807"] {
            assert!(numeric_lookup_criterion(text).is_none(), "{text}");
        }
        for text in ["0", "-1", "3.25", "", "?", "9223372036854775808"] {
            let value = om_data::host::stored_value(text, ValueType::Number, &cx);
            assert!(
                lookup_text(&value, ValueType::Number, &cx).is_none(),
                "{text}"
            );
        }
        assert_eq!(
            lookup_text(
                &om_data::host::stored_value("0250000.00", ValueType::Number, &cx),
                ValueType::Number,
                &cx
            )
            .as_deref(),
            Some("250000")
        );
    }

    #[test]
    fn only_index_candidates_are_read() {
        let (schema, mut source) = fixture();
        for id in 100..10_000 {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text("unrelated"));
            source.data.put_record(CHILD, id, row);
        }
        let cx = Ctx::new(&schema.locale, today());
        let result = indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .unwrap();
        assert_eq!(&*result, &[30, 10]);
        assert_eq!(
            &*source.reads.borrow(),
            &[(PARENT, 1), (CHILD, 30), (CHILD, 10), (CHILD, 20)]
        );
    }

    #[test]
    fn unsupported_lookup_discards_every_partial_result() {
        let (schema, mut source) = fixture();
        source.candidates.insert("B".into(), None);
        let cx = Ctx::new(&schema.locale, today());
        assert!(indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .is_none());
        assert_eq!(&*source.reads.borrow(), &[(PARENT, 1)]);
    }

    #[test]
    fn global_match_values_and_empty_values_follow_the_scan() {
        let (mut schema, mut source) = fixture();
        schema.add_field(
            PARENT,
            FieldDef::data(1, "Key", ValueType::Text).with_global(true),
        );
        let mut globals = RecordValues::default();
        globals.set(1, 1, Value::text("B"));
        source.data.set_globals(PARENT, globals);
        let cx = Ctx::new(&schema.locale, today());
        assert_eq!(
            indexed_related(&source, &schema, FROM, 1, TO, &cx)
                .unwrap()
                .unwrap(),
            related_by_scan(&source.data, &schema, FROM, 1, TO, &cx).unwrap()
        );
        source.data.set_globals(PARENT, RecordValues::default());
        source.lookups.borrow_mut().clear();
        assert!(indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .unwrap()
            .is_empty());
        assert!(source.lookups.borrow().is_empty());
    }

    #[test]
    fn unsupported_fields_leave_the_scan_untouched() {
        for field in [
            FieldDef::data(1, "Key", ValueType::Text), // no index
            FieldDef {
                // A word index alone: whole-value finds read the value index.
                options: 0x2000_0000,
                ..FieldDef::data(1, "Key", ValueType::Text)
            },
            FieldDef {
                options: 0x8000_0000,
                language: 25, // German index keys
                ..FieldDef::data(1, "Key", ValueType::Text)
            },
            FieldDef {
                options: 0x8000_0000,
                ..FieldDef::data(1, "Key", ValueType::Number)
            },
            FieldDef {
                options: 0x8000_0000,
                class: FieldClass::Calculation,
                stored: false,
                ..FieldDef::data(1, "Key", ValueType::Text)
            },
        ] {
            let (mut schema, source) = fixture();
            schema.add_field(CHILD, field);
            let cx = Ctx::new(&schema.locale, today());
            assert!(indexed_related(&source, &schema, FROM, 1, TO, &cx)
                .unwrap()
                .is_none());
            assert!(source.reads.borrow().is_empty());
            assert!(source.lookups.borrow().is_empty());
        }
    }

    #[test]
    fn sorted_relationships_order_only_matching_index_candidates() {
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        let mut relation = RelationshipDef::new(
            1,
            FROM,
            TO,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 1,
                right: 1,
            }],
        );
        relation.sort_right = Some(SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(TO),
                    field: 1,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: true,
                locale: 0,
            }],
        });
        let (schema, mut source) = fixture_with(relation);
        for id in 100..10_000 {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text("unrelated"));
            source.data.put_record(CHILD, id, row);
        }
        let cx = Ctx::new(&schema.locale, today());
        let actual = indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .unwrap();
        assert_eq!(&*actual, &[10, 30]);
        assert_eq!(
            actual,
            related_by_scan(&source.data, &schema, FROM, 1, TO, &cx).unwrap()
        );
        assert!(source
            .reads
            .borrow()
            .iter()
            .all(|(t, r)| *t != CHILD || *r < 100));
    }

    const GRAND: u64 = 0x8003;
    const FAR: u64 = 0xd00003;

    fn descending(occurrence: u64, field: u64) -> om_scripts::specs::SortSpec {
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(occurrence),
                    field,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: true,
                locale: 0,
            }],
        }
    }

    /// The single-hop fixture, then `TO` → `FAR` over `predicate` from
    /// Child::Link (2) to an indexed Grandchild::Link (1). Parent 1 relates
    /// children 30 (Link L1) and 10 (L2); child 20 (L1) is unrelated. Each
    /// hop may sort its records descending by the destination's Rank (3).
    fn path_fixture(predicate: JoinPredicate, sorts: (bool, bool)) -> (CalcSchema, Source) {
        let mut first = RelationshipDef::new(
            1,
            FROM,
            TO,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 1,
                right: 1,
            }],
        );
        first.sort_right = sorts.0.then(|| descending(TO, 3));
        let (mut schema, mut source) = fixture_with(first);
        schema.add_table(GRAND, "Grandchild");
        schema.add_occurrence(FAR, "Grandchild", GRAND);
        schema.add_field(CHILD, FieldDef::data(2, "Link", ValueType::Text));
        schema.add_field(CHILD, FieldDef::data(3, "Rank", ValueType::Number));
        let mut link = FieldDef::data(1, "Link", ValueType::Text);
        link.options = 0x8000_0000;
        schema.add_field(GRAND, link);
        schema.add_field(GRAND, FieldDef::data(3, "Rank", ValueType::Number));
        let mut second = RelationshipDef::new(2, TO, FAR, vec![predicate]);
        second.sort_right = sorts.1.then(|| descending(FAR, 3));
        schema.add_relationship(second);
        let row = |values: &[(u64, u32, Value)]| {
            let mut row = RecordValues::default();
            for (field, repetition, value) in values {
                row.set(*field, *repetition, value.clone());
            }
            row
        };
        let number = |n| Value::Number(Number::from_i64(n));
        for (id, key, second, link, rank) in [
            (30, "A", "", "L1", 1),
            (10, "wrong", "B", "L2", 3),
            (20, "A\rB", "", "L1", 2),
        ] {
            let values = [
                (1, 1, Value::text(key)),
                (1, 2, Value::text(second)),
                (2, 1, Value::text(link)),
                (3, 1, number(rank)),
            ];
            source.data.put_record(CHILD, id, row(&values));
        }
        // Creation order differs from record id order; 9 is a candidate
        // the index offers for L1 that does not match.
        for (id, link, rank) in [(7, "L2", 1), (5, "L1", 3), (9, "L3", 4), (6, "L1", 2)] {
            let values = [(1, 1, Value::text(link)), (3, 1, number(rank))];
            source.data.put_record(GRAND, id, row(&values));
        }
        for id in 100..2000 {
            let values = [(1, 1, Value::text("unrelated"))];
            source.data.put_record(GRAND, id, row(&values));
        }
        source
            .candidates
            .insert("L1".into(), Some(Arc::from([5, 6, 9])));
        source.candidates.insert("L2".into(), Some(Arc::from([7])));
        (schema, source)
    }

    fn link() -> JoinPredicate {
        JoinPredicate {
            op: JoinOp::Equal,
            left: 2,
            right: 1,
        }
    }

    #[test]
    fn multi_hop_paths_narrow_every_hop_and_match_the_scan() {
        for (sorts, expected) in [
            ((false, false), [7, 5, 6]),
            // Only the last relationship's sort orders the set.
            ((true, false), [7, 5, 6]),
            ((false, true), [5, 6, 7]),
            ((true, true), [5, 6, 7]),
        ] {
            let (schema, source) = path_fixture(link(), sorts);
            let cx = Ctx::new(&schema.locale, today());
            let result = indexed_related(&source, &schema, FROM, 1, FAR, &cx)
                .unwrap()
                .unwrap();
            assert_eq!(&*result, &expected, "{sorts:?}");
            assert_eq!(
                result,
                related_by_scan(&source.data, &schema, FROM, 1, FAR, &cx).unwrap(),
                "{sorts:?}"
            );
            assert_eq!(&*source.lookups.borrow(), &["A", "B", "L2", "L1"]);
            assert!(
                source
                    .reads
                    .borrow()
                    .iter()
                    .all(|&(table, record)| table != GRAND || [5, 6, 7, 9].contains(&record)),
                "{sorts:?}"
            );
        }
    }

    #[test]
    fn an_empty_middle_hop_relates_nothing_without_reading_the_last_table() {
        let (schema, mut source) = path_fixture(link(), (false, true));
        let mut parent = RecordValues::default();
        parent.set(1, 1, Value::text("Z"));
        source.data.put_record(PARENT, 1, parent);
        source.candidates.insert("Z".into(), Some(Arc::from([])));
        let cx = Ctx::new(&schema.locale, today());
        let result = indexed_related(&source, &schema, FROM, 1, FAR, &cx)
            .unwrap()
            .unwrap();
        assert!(result.is_empty());
        assert_eq!(
            result,
            related_by_scan(&source.data, &schema, FROM, 1, FAR, &cx).unwrap()
        );
        assert_eq!(&*source.lookups.borrow(), &["Z"]);
        assert_eq!(&*source.reads.borrow(), &[(PARENT, 1)]);
        assert!(!source.listed.borrow().contains(&GRAND));
    }

    #[test]
    fn a_hop_the_index_cannot_answer_leaves_the_whole_path_to_the_scan() {
        let not_equal = JoinPredicate {
            op: JoinOp::NotEqual,
            ..link()
        };
        let (schema, source) = path_fixture(not_equal, (false, false));
        let cx = Ctx::new(&schema.locale, today());
        assert!(indexed_related(&source, &schema, FROM, 1, FAR, &cx)
            .unwrap()
            .is_none());
        // The same for a key the last table's index cannot narrow.
        let (schema, mut source) = path_fixture(link(), (false, false));
        source.candidates.insert("L1".into(), None);
        assert!(indexed_related(&source, &schema, FROM, 1, FAR, &cx)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_cartesian_hop_lists_its_destination_without_reading_it() {
        let cartesian = JoinPredicate {
            op: JoinOp::Cartesian,
            ..link()
        };
        let (schema, source) = path_fixture(cartesian, (false, false));
        let cx = Ctx::new(&schema.locale, today());
        let result = indexed_related(&source, &schema, FROM, 1, FAR, &cx)
            .unwrap()
            .unwrap();
        assert_eq!(&result[..4], &[7, 5, 9, 6]);
        assert_eq!(result.len(), 4 + 1900);
        assert_eq!(
            result,
            related_by_scan(&source.data, &schema, FROM, 1, FAR, &cx).unwrap()
        );
        assert!(source
            .reads
            .borrow()
            .iter()
            .all(|&(table, _)| table != GRAND));
    }

    #[test]
    fn a_cartesian_hop_lists_its_destination_once_for_all_incoming_records() {
        let cartesian = JoinPredicate {
            op: JoinOp::Cartesian,
            ..link()
        };
        let (schema, mut source) = path_fixture(cartesian, (false, false));
        // Parent 1 relates 50 more children, each relating every grandchild.
        let mut children = vec![20, 30];
        for id in 3000..3050 {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text("A"));
            source.data.put_record(CHILD, id, row);
            children.push(id);
        }
        source
            .candidates
            .insert("A".into(), Some(Arc::from(children)));
        let cx = Ctx::new(&schema.locale, today());
        let result = indexed_related(&source, &schema, FROM, 1, FAR, &cx)
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 4 + 1900);
        assert_eq!(
            result,
            related_by_scan(&source.data, &schema, FROM, 1, FAR, &cx).unwrap()
        );
        // Once for the hop and once for the result's creation order.
        let listed = source.listed.borrow();
        assert_eq!(listed.iter().filter(|&&table| table == GRAND).count(), 2);
    }

    /// An `=` beside a date range (`<=`, `>=`), as a layout's conditional
    /// formats follow to a large related table: the index narrows to the
    /// key's candidates and every predicate decides on them as the scan
    /// does, over repetitions, empty values and the range's bounds. No
    /// other record is read.
    #[test]
    fn an_equal_beside_a_range_reads_its_candidates_and_matches_the_scan() {
        let predicate = |op, left, right| JoinPredicate { op, left, right };
        let (mut schema, mut source) = fixture_with(RelationshipDef::new(
            1,
            FROM,
            TO,
            vec![
                predicate(JoinOp::Equal, 1, 1),
                predicate(JoinOp::LessEqual, 2, 2),
                predicate(JoinOp::GreaterEqual, 3, 2),
            ],
        ));
        schema.add_field(PARENT, FieldDef::data(2, "Start", ValueType::Date));
        schema.add_field(PARENT, FieldDef::data(3, "End", ValueType::Date));
        schema.add_field(
            CHILD,
            FieldDef::data(2, "Due", ValueType::Date).with_repetitions(2),
        );
        let date = |day| Value::Date(Date::from_ymd(2026, 1, day).unwrap());
        let parent = |start: Option<Value>| {
            let mut parent = RecordValues::default();
            parent.set(1, 1, Value::text("A"));
            parent.set(1, 2, Value::text("B"));
            if let Some(start) = start {
                parent.set(2, 1, start);
            }
            parent.set(3, 1, date(10));
            parent
        };
        source.data.put_record(PARENT, 1, parent(Some(date(5))));
        // 30 in range; 10 through its second key and second date; 20
        // without a date; 40 past the range; 50 on its end.
        let mut put = |id: u64, key: &str, dates: &[u32]| {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text(key));
            if id == 10 {
                row.set(1, 2, Value::text("B"));
            }
            for (repetition, day) in (1..).zip(dates) {
                row.set(2, repetition, date(*day));
            }
            source.data.put_record(CHILD, id, row);
        };
        put(30, "A", &[7]);
        put(10, "wrong", &[2, 9]);
        put(20, "A\rB", &[]);
        put(40, "A", &[12]);
        put(50, "A", &[10]);
        for id in 100..1_000 {
            put(id, "unrelated", &[7]);
        }
        source
            .candidates
            .insert("A".into(), Some(Arc::from([20, 30, 40, 50])));
        let cx = Ctx::new(&schema.locale, today());
        let indexed = indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .unwrap();
        assert_eq!(&*indexed, &[30, 10, 50]);
        assert_eq!(
            indexed,
            related_by_scan(&source.data, &schema, FROM, 1, TO, &cx).unwrap()
        );
        let read: BTreeSet<(u64, u64)> = source.reads.borrow().iter().copied().collect();
        assert_eq!(
            read,
            BTreeSet::from([
                (PARENT, 1),
                (CHILD, 10),
                (CHILD, 20),
                (CHILD, 30),
                (CHILD, 40),
                (CHILD, 50)
            ])
        );
        // An empty Start relates nothing, as in the scan.
        source.data.put_record(PARENT, 1, parent(None));
        let cx = Ctx::new(&schema.locale, today());
        assert!(indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .unwrap()
            .is_empty());
        assert!(related_by_scan(&source.data, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn non_equality_relationships_keep_the_scan() {
        let relation = RelationshipDef::new(
            1,
            FROM,
            TO,
            vec![JoinPredicate {
                op: JoinOp::NotEqual,
                left: 1,
                right: 1,
            }],
        );
        let (schema, source) = fixture_with(relation);
        let cx = Ctx::new(&schema.locale, today());
        assert!(indexed_related(&source, &schema, FROM, 1, TO, &cx)
            .unwrap()
            .is_none());
        assert!(source.reads.borrow().is_empty());
        assert!(source.lookups.borrow().is_empty());
    }

    /// A text is looked up by its index key, find operators and all (#1068);
    /// lines and edge spaces keep the scan.
    #[test]
    fn lookup_text_keeps_find_operators_and_leaves_lines_to_the_scan() {
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, today());
        for text in ["A\rB", "A\nB", "A\u{2028}B", "A\tB", " A", "A ", ""] {
            assert!(
                lookup_text(&Value::text(text), ValueType::Text, &cx).is_none(),
                "{text:?}"
            );
        }
        assert!(lookup_text(&Value::text("A".repeat(127)), ValueType::Text, &cx).is_none());
        for text in [
            "Job-123 / 4",
            "50#Offset 25.5",
            "A\\B",
            "A*",
            "A...B",
            "\"A\"",
            "=A",
            "#",
        ] {
            assert_eq!(
                lookup_text(&Value::text(text), ValueType::Text, &cx).as_deref(),
                Some(text)
            );
        }
        assert_eq!(
            lookup_text(&Value::Date(today()), ValueType::Date, &cx).as_deref(),
            Some("10/29/2026")
        );
        // An entered short year is display text, not a lookup key: a find
        // must search the already parsed day, independent of today's window.
        assert_eq!(
            lookup_text(
                &Value::DateText(today(), om_data::calc::Text::plain("10/29/26")),
                ValueType::Date,
                &cx
            )
            .as_deref(),
            Some("10/29/2026")
        );
    }
}
