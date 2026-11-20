//! A field validated against a related-values value list ("Include only
//! related values starting from", file-metadata.md 6.4): the list is
//! computed for the record as the write leaves it, through the calculation
//! host's own list (`FileCalc::value_list_entries`, the choice path), and
//! a changed value not among its values is refused (506) at the commit.
//!
//! The record's own place in its list is what F91 measured for a related
//! list FileMaker Pro shows: an existing record belongs to its own related
//! set with its unsaved values [O]. That validation reads the same list is
//! inferred. A new record's place is unmeasured (#1035): where it would
//! relate to itself the write is refused (3), as it is for a record the
//! write does not name, for a list from another table, for one reading
//! records the transaction holds that its commit has not computed yet
//! (below), and for a list of a field the list's own read does not give
//! exactly: a summary, a global, a container or a repeating field.
//!
//! A list of an unstored calculation's values lists what the calculation
//! gives on each related record (`FileCalc::read_field_computed`, #1058),
//! the written record's over its unsaved values; a record whose value
//! cannot be computed fails the check (3) rather than leave its values
//! out.
//!
//! At Commit Transaction the list reads the transaction's other records as
//! the commit computed them before this one ([`Holding`], #1063). One it
//! computes later is not known yet: the check is refused (3) where the list
//! reads it, through its relationships (a calculated match key's reads
//! too), the record's own table, or what an unstored calculation's field
//! reads ([`reads_uncomputed`]).
//!
//! Written by its owning file for another file's window (#1000), the
//! calculation reads that opened file's tables through its session, with
//! its session's global values (related_owner.rs `CallerSource`), as the
//! write's lookups do. In a save across both files the owning file's
//! commit comes first (#1006): the opened file's held records are read as
//! its commit will write them (`CallerReads::pending`), and a value that
//! commit may store otherwise, such as its modification timestamp, refuses
//! the check (3) where the list reads it (`CallerReads::unknown`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use om_data::calc::eval::{Context, Pending};
use om_data::calc::{Ctx, ValueType};
use om_data::host::{CalcSchema, CalcSource, FileCalc, RecordValues};
use om_data::meta::ValueSource;
use om_data::rules::{error, Entry, HeldRecords, RecordPlan, Written};
use om_data::DataError;
use om_hbam::Table;

use crate::NativeProvider;

/// Written cells by id, encoded for upload; `None` removes one.
type Encoded = BTreeMap<u64, Option<Vec<u8>>>;

/// How many items a list may have for a write to check a value against it.
const MAX_ITEMS: usize = 100_000;

/// The id a new record has while its list is computed, before the host
/// reserves one: none of a table's records has it.
const UNRESERVED: u64 = u64::MAX - 1;

/// The host's records with the written record as the write leaves it:
/// its id answers its unsaved values, and a new one is among its table's
/// records. Relationships are followed by scanning, as an index knows only
/// stored values.
struct DraftSource<'a> {
    inner: &'a dyn CalcSource,
    table: u64,
    record: u64,
    values: Arc<RecordValues>,
    new: bool,
}

impl CalcSource for DraftSource<'_> {
    fn record(
        &self,
        table: u64,
        record: u64,
    ) -> Result<Option<Arc<RecordValues>>, om_data::calc::CalcError> {
        if (table, record) == (self.table, self.record) {
            return Ok(Some(Arc::clone(&self.values)));
        }
        self.inner.record(table, record)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, om_data::calc::CalcError> {
        let all = self.inner.records(table)?;
        if table != self.table || !self.new || all.contains(&self.record) {
            return Ok(all);
        }
        Ok(all.iter().copied().chain([self.record]).collect())
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, om_data::calc::CalcError> {
        self.inner.globals(table)
    }

    fn related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, om_data::calc::CalcError> {
        om_data::host::related_by_scan(self, schema, from, record, to, cx)
    }

    fn related_ids(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, om_data::calc::CalcError> {
        self.related(schema, from, record, to, cx)
    }

    fn prefetch_records(&self, table: u64, records: &[u64]) {
        let records: Vec<u64> = records
            .iter()
            .copied()
            .filter(|r| (table, *r) != (self.table, self.record))
            .collect();
        self.inner.prefetch_records(table, &records);
    }

    fn library_member(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<om_data::containers::MemberInfo>>, om_data::calc::CalcError> {
        self.inner.library_member(table, member)
    }

    fn library_data(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<om_data::calc::eval::StoredContainer>>, om_data::calc::CalcError> {
        self.inner.library_data(table, member)
    }

    fn interrupted(&self) -> bool {
        self.inner.interrupted()
    }

    fn pending(&self) -> Pending {
        self.inner.pending()
    }

    fn need_occurrence(&self, occurrence: u64) {
        self.inner.need_occurrence(occurrence);
    }

    fn need_layout(&self, layout: u32) {
        self.inner.need_layout(layout);
    }
}

/// The field a field list lists.
fn list_field(list: &om_data::meta::ValueList) -> u64 {
    match &list.source {
        ValueSource::Field { primary, .. } => primary.field,
        _ => 0,
    }
}

/// [`not_checked`] for a check whose rule OpenMaker does not support
/// (#571): an import ends at its row ([`DataError::unsupported`]) rather
/// than skipping it.
fn unsupported_check(field: &str, why: &str) -> DataError {
    DataError {
        unsupported: true,
        ..not_checked(field, why)
    }
}

fn not_checked(field: &str, why: &str) -> DataError {
    DataError::with_code(
        error::UNAVAILABLE,
        format!(
            "Field {field}: its value is checked against a related-values list, which \
             OpenMaker cannot compute for this write ({why}). No values were changed."
        ),
    )
}

/// The records a transaction holds, as a related-values list reads them
/// (#1063): `held`, every one; `done`, those the commit has computed
/// already, as it writes them in `computed`.
#[derive(Clone, Copy)]
pub(super) struct Holding<'a> {
    pub(super) held: &'a BTreeSet<(u64, u64)>,
    pub(super) done: &'a BTreeSet<(u64, u64)>,
    pub(super) computed: Option<&'a HeldRecords>,
}

/// No record held.
static NONE_HELD: BTreeSet<(u64, u64)> = BTreeSet::new();

impl Holding<'_> {
    /// Outside a transaction.
    pub(super) fn none() -> Holding<'static> {
        Holding {
            held: &NONE_HELD,
            done: &NONE_HELD,
            computed: None,
        }
    }

    /// Every held record, none computed: a list checked before the commit.
    pub(super) fn uncomputed(held: &BTreeSet<(u64, u64)>) -> Holding<'_> {
        Holding {
            held,
            done: &NONE_HELD,
            computed: None,
        }
    }
}

/// Whether the values of a list of `field` through `path`, for record
/// `record` of `table`, read a record the transaction holds that the commit
/// has not computed yet (`holding`): one of the record's own table, one
/// the path's match and sort fields read, in the tables it finds records
/// in or through a calculated match key ([`om_data::rules::path_reads_unknown`]),
/// or one the field reads, when it is computed when read
/// ([`om_data::rules::unknown_when_read`]: what an unstored calculation
/// reads, through its own record and others). A held record not computed
/// yet holds its cells without what the commit computes from them.
fn reads_uncomputed(
    schema: &CalcSchema,
    (table, record): (u64, u64),
    (path, field): (&[om_data::host::Hop], u64),
    holding: Holding<'_>,
) -> bool {
    let uncomputed: Vec<(u64, u64)> = holding
        .held
        .iter()
        .copied()
        .filter(|key| *key != (table, record) && !holding.done.contains(key))
        .collect();
    if uncomputed.is_empty() {
        return false;
    }
    let unknown = |t: u64, r: Option<u64>, _: u64| {
        uncomputed
            .iter()
            .any(|(held, at)| *held == t && r.is_none_or(|r| r == *at))
    };
    let listed = match path.last() {
        Some(hop) => schema.table_of(hop.to).map(|t| t.id),
        None => Some(table),
    };
    let Some(listed) = listed else {
        return true;
    };
    let unknown = (&unknown as om_data::rules::UnknownValue<'_>, true);
    unknown.0(table, None, 0)
        || om_data::rules::path_reads_unknown(schema, unknown, (table, Some(record)), path)
        || om_data::rules::unknown_when_read(schema, unknown, (listed, None, field))
}

impl NativeProvider {
    /// Error 3 when a value in `cells` of `record` is checked against a
    /// related-values list that reads other records the transaction holds
    /// (`holding`, as none computed: [`reads_uncomputed`]): an imported
    /// row's list, checked when it was imported, may read them changed
    /// since.
    pub(super) fn refuse_held_related_lists(
        &self,
        table: &Table,
        plan: &RecordPlan,
        entry: Entry,
        (cells, record): (&Encoded, u64),
        holding: Holding<'_>,
    ) -> Result<(), DataError> {
        let schema = plan.schema();
        for (&cell, value) in cells {
            if value.as_ref().is_none_or(|v| v.is_empty()) {
                continue;
            }
            let Some(list) = plan.related_value_list(cell, entry) else {
                continue;
            };
            let ValueSource::Field {
                primary,
                start: Some(start),
                ..
            } = &list.source
            else {
                continue;
            };
            let path = primary
                .occurrence
                .and_then(|primary| schema.path(*start, primary))
                .unwrap_or_default();
            if reads_uncomputed(schema, (table.id, record), (&path, primary.field), holding) {
                let id = if table.field(cell).is_some() {
                    cell
                } else {
                    cell >> 8
                };
                let name = table.field(id).map_or("", |f| f.name.as_str());
                return Err(not_checked(
                    name,
                    "the transaction holds records its list reads",
                ));
            }
        }
        Ok(())
    }

    /// Error 506 when a changed, non-empty value (`values`, over `existing`)
    /// of a field of `table` that `entry` checks against a related-values
    /// list is not among the list's values for the record as the write
    /// leaves it; 3 where the list cannot be computed exactly (module docs).
    /// `written` is the record; `holding`, the records a transaction holds,
    /// read as the commit computed them, or refused where they are read and
    /// not computed yet.
    /// A cell the user typed (`typed`, `ScriptWrite::typed`) is checked by a
    /// user's rules, also in a script's write: "Only during data entry" is
    /// about how the value was entered, and the commit's "Skip data entry
    /// validation" still applies (#1074).
    pub(super) fn check_related_lists(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        (entry, typed): (Entry, &BTreeSet<u64>),
        (existing, values): (&BTreeMap<u64, String>, &Encoded),
        (written, holding): (Written, Holding<'_>),
    ) -> Result<(), DataError> {
        let mut checks = Vec::new();
        for (&cell, value) in values {
            let entry = match typed.contains(&cell) {
                true => Entry {
                    script: false,
                    ..entry
                },
                false => entry,
            };
            let Some(list) = plan.related_value_list(cell, entry) else {
                continue;
            };
            let Some(value) = value else {
                continue;
            };
            let text = om_scsu::decode_scsu(value)
                .map_err(|e| crate::failure("decode a validated value", format!("{e:?}")))?;
            if !text.is_empty() {
                checks.push((cell, list.clone(), text));
            }
        }
        if checks.is_empty() {
            return Ok(());
        }
        let field_of = |cell: u64| {
            let id = match table.field(cell) {
                Some(_) => cell,
                None => cell >> 8,
            };
            (
                id,
                table.field(id).map_or(String::new(), |f| f.name.clone()),
            )
        };
        let (record, new) = match written {
            Written::Stored(id) => (id, false),
            Written::New(id) => (id.unwrap_or(UNRESERVED), true),
            Written::Unknown => {
                let (_, name) = field_of(checks[0].0);
                return Err(not_checked(&name, "the record is not named"));
            }
        };
        // The record as the write leaves it.
        let mut row = existing.clone();
        for (cell, value) in values {
            match value {
                Some(bytes) => {
                    let text = om_scsu::decode_scsu(bytes)
                        .map_err(|e| crate::failure("decode a written value", format!("{e:?}")))?;
                    row.insert(*cell, text);
                }
                None => {
                    row.remove(cell);
                }
            }
        }
        let schema = plan.schema();
        let today = om_data::host::session::today(0);
        let cx = Ctx::new(&schema.locale, today);
        let types = schema.table(table.id);
        let mut draft = RecordValues::default();
        for (cell, text) in &row {
            let (field, repetition) = match table.field(*cell) {
                Some(_) => (*cell, 1),
                None => (*cell >> 8, u32::try_from(*cell & 0xff).unwrap_or(1)),
            };
            let value_type = types
                .and_then(|t| t.fields.get(&field))
                .map_or(ValueType::Text, |d| d.value_type);
            draft.set(
                field,
                repetition,
                om_data::host::stored_value(text, value_type, &cx),
            );
        }
        let draft = Arc::new(draft);
        for (cell, list, text) in checks {
            let (field, name) = field_of(cell);
            let ValueSource::Field {
                primary,
                start: Some(start),
                ..
            } = &list.source
            else {
                return Err(not_checked(&name, "its list is not one of related values"));
            };
            let primary = primary
                .occurrence
                .ok_or_else(|| not_checked(&name, "its list names no occurrence"))?;
            if schema.table_of(*start).map(|t| t.id) != Some(table.id) {
                return Err(not_checked(&name, "its list starts from another table"));
            }
            // The list's read gives exactly one repetition of a stored value,
            // or of an unstored calculation computed on each record (#1058):
            // not a summary's (read without the related set), a global's, a
            // container's or a repeating field's later repetitions.
            if !schema.field(primary, list_field(&list)).is_some_and(|def| {
                (def.stored || def.is_unstored_calculation())
                    && !def.global
                    && def.repetitions == 1
                    && def.value_type != ValueType::Container
                    && matches!(
                        def.class,
                        om_data::host::FieldClass::Data | om_data::host::FieldClass::Calculation
                    )
            }) {
                return Err(not_checked(
                    &name,
                    "its list's field is a summary, a global, a container or repeating",
                ));
            }
            let path = schema
                .path(*start, primary)
                .ok_or_else(|| not_checked(&name, "its list's occurrences are not related"))?;
            // The other records the transaction holds are read as its
            // commit computed them; one it computes only later is not
            // known yet, where the list reads it.
            let listed = list_field(&list);
            if reads_uncomputed(schema, (table.id, record), (&path, listed), holding) {
                return Err(not_checked(
                    &name,
                    "the transaction holds records its list reads that its commit computes \
                     after this one",
                ));
            }
            // Written for the opened file's commit, which comes after this
            // one (#1006): a value of its held records that commit may store
            // otherwise is not read (`CallerReads::unknown`).
            let caller_unknown = self.caller.as_ref().map(|c| &c.unknown);
            if let Some(unknown) = caller_unknown.filter(|u| !u.is_empty()) {
                let marked = |t, r, f| crate::preview::marked(unknown, (t, r, f));
                let reads = (&marked as om_data::rules::UnknownValue<'_>, true);
                let at = path.last().and_then(|hop| schema.table_of(hop.to));
                let at = at.map_or(table.id, |t| t.id);
                if om_data::rules::path_reads_unknown(
                    schema,
                    reads,
                    (table.id, Some(record)),
                    &path,
                ) || om_data::rules::unknown_when_read(schema, reads, (at, None, listed))
                {
                    return Err(not_checked(
                        &name,
                        "its list reads values of the opened file that its commit, after this \
                         one, may store otherwise",
                    ));
                }
            }
            // As a choice list reads it (`choice.rs`): the session's
            // calculations and global values, other files' tables.
            let session_info = self.session_info();
            let calc_session = session_info.clone();
            let globals = self.desktop_globals(schema);
            // Written for another file's window: that file's tables through
            // its session (module docs).
            let caller = self.caller.as_mut();
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| DataError::new("No file is open."))?;
            let native = crate::calc::NativeSource::new(session, schema, today)
                .with_calculation_session(session_info)
                .with_session_globals(globals)
                .with_library(&self.calc.library)
                .with_external(&self.external);
            let root = caller.map(
                |crate::related_owner::CallerReads {
                     session,
                     schema,
                     tables,
                     globals,
                     pending,
                     others,
                     ..
                 }| {
                    // The owner's other related files' tables, through the
                    // sessions lent for the call (#1160).
                    let others: Vec<_> = others
                        .iter_mut()
                        .map(|(_, other)| {
                            (
                                crate::calc::NativeSource::new(
                                    &mut other.session,
                                    &other.schema,
                                    today,
                                ),
                                &other.tables,
                                other.pending.as_deref().map(|held| (held, &*other.schema)),
                            )
                        })
                        .collect();
                    (
                        crate::calc::NativeSource::new(session, schema, today)
                            .with_session_globals(globals.clone()),
                        &*tables,
                        pending.as_deref().map(|pending| (pending, &**schema)),
                        others,
                    )
                },
            );
            // The opened file's held records as its commit will write them
            // (`CallerReads::pending`), over its stored ones.
            let root_pending = root.as_ref().and_then(|(root, _, pending, _)| {
                pending.map(|(held, schema)| om_data::rules::HeldSource::new(root, held, schema))
            });
            let peer_pending: Vec<_> = root.as_ref().map_or_else(Vec::new, |(_, _, _, others)| {
                others
                    .iter()
                    .map(|(source, _, pending)| {
                        pending.map(|(held, schema)| {
                            om_data::rules::HeldSource::new(source, held, schema)
                        })
                    })
                    .collect()
            });
            let computed = holding
                .computed
                .map(|computed| om_data::rules::HeldSource::new(&native, computed, schema));
            let own: &dyn CalcSource = match &computed {
                Some(computed) => computed,
                None => &native,
            };
            let other_sources: Vec<(&dyn CalcSource, &BTreeMap<u64, u64>)> =
                root.as_ref().map_or_else(Vec::new, |(_, _, _, others)| {
                    others
                        .iter()
                        .zip(&peer_pending)
                        .map(|((source, tables, _), pending)| {
                            let source: &dyn CalcSource = match pending {
                                Some(pending) => pending,
                                None => source,
                            };
                            (source, *tables)
                        })
                        .collect()
                });
            let mapped = root.as_ref().map(|(root, tables, _, _)| {
                let root: &dyn CalcSource = match &root_pending {
                    Some(pending) => pending,
                    None => root,
                };
                crate::related_owner::CallerSource {
                    own,
                    root,
                    tables,
                    others: &other_sources,
                }
            });
            let source = DraftSource {
                inner: match &mapped {
                    Some(mapped) => mapped,
                    None => own,
                },
                table: table.id,
                record,
                values: Arc::clone(&draft),
                new,
            };
            // With the session a choice list reads (`choice.rs`): its
            // calculations' `Get` functions answer for this session.
            let calc = FileCalc::new(schema, &source, today).with_session(&calc_session);
            // A calculation that evaluated to `?` has no FileMaker code of
            // its own (-1): the write is refused as uncheckable (3).
            let read = |e: om_data::calc::CalcError| {
                DataError::with_code(
                    if e.code() > 0 {
                        e.code()
                    } else {
                        error::UNAVAILABLE
                    },
                    format!(
                        "Field {name}: its value list could not be read. No values were changed."
                    ),
                )
            };
            // Whether a new record is in its own list is unmeasured.
            if new
                && calc
                    .related_records(*start, record, primary)
                    .map_err(read)?
                    .contains(&record)
            {
                return Err(not_checked(
                    &name,
                    "whether a new record is among its own related values is not known",
                ));
            }
            let context = Context {
                occurrence: Some(*start),
                record: Some(record),
                ..Context::default()
            };
            let items = calc.value_list_entries(&list, &context, MAX_ITEMS);
            // A download that failed is the write's refusal, not the list's.
            let root_failure = root.as_ref().and_then(|(root, _, _, others)| {
                root.take_failure()
                    .or_else(|| others.iter().find_map(|(other, _, _)| other.take_failure()))
            });
            if let Some(failure) = native.take_failure().or(root_failure) {
                return Err(failure);
            }
            let items = items.map_err(read)?;
            let items = items.ok_or_else(|| not_checked(&name, "its list is too long"))?;
            let data_type = table
                .field(field)
                .map_or(om_hbam::catalog::DataType::Text, |f| f.data_type);
            // Text compares as the list's field keys it (#571): in its index
            // language, 0 being the language of the file that owns its table
            // (a related file's own, `table_context`), unknown when that
            // file's locale record does not decode. A field of another type
            // is keyed by value, whatever its language, and compares as it
            // did.
            let keys = schema.table_of(primary).and_then(|source| {
                let def = source.fields.get(&listed)?;
                if def.value_type != ValueType::Text {
                    return Some(om_query::keys::TextKeys::Collated);
                }
                let file_language = match schema.table_context(source.id) {
                    Some(file) => file.file_language(),
                    None => schema.file_language(),
                };
                om_data::rules::value_text_keys(def.language, file_language)
            });
            let mut found = false;
            for item in &items {
                match om_data::rules::same_list_value(data_type, &item.value, &text, &cx, keys) {
                    Some(true) => {
                        found = true;
                        break;
                    }
                    Some(false) => {}
                    None => {
                        return Err(unsupported_check(
                            &name,
                            "its list's field is indexed in a language whose keys OpenMaker \
                             does not build",
                        ))
                    }
                }
            }
            if !found {
                // The list keeps one of the values a calculation compares
                // equal (`FileCalc::list_entries`), as values of its field's
                // type: where its field's keys tell those apart (case or
                // accents in Unicode raw or standard, `v` and `w` in a
                // Swedish file's English keys), the value it left out for an
                // equal one may be this one. A number list's `1e3` is not
                // `1é3` (13), as the list compares them (#591).
                // The list's values are written in the locale of the file
                // that owns the list (`list_entries`), and read back so; the
                // value checked is read in the locale of the file that owns
                // the listed field, as its own values are: a related German
                // `1,500` is 1.5 there, not 1500. They compare as the list
                // compares its values, in the list's locale: a US list over
                // a Swedish field takes `v` and `w` apart.
                let listed_type = schema
                    .field(primary, listed)
                    .map_or(ValueType::Text, |def| def.value_type);
                let listed_locale = schema
                    .table_of(primary)
                    .and_then(|source| schema.table_context(source.id))
                    .map_or(&schema.locale, |file| &file.locale);
                let listed_cx = Ctx::new(listed_locale, today);
                let list_locale = schema
                    .value_list_context(list.id)
                    .map_or(&schema.locale, |file| &file.locale);
                let list_cx = Ctx::new(list_locale, today);
                let checked = om_data::host::stored_value(&text, listed_type, &listed_cx);
                let equal = |listed: &str| {
                    let listed = om_data::host::stored_value(listed, listed_type, &list_cx);
                    matches!(
                        om_data::calc::ops::compare(&listed, &checked, &list_cx),
                        om_data::calc::ops::Comparison::Ordered(std::cmp::Ordering::Equal)
                    )
                };
                if items.iter().any(|item| equal(&item.value)) {
                    return Err(unsupported_check(
                        &name,
                        "its list's field keys apart values its list as computed takes as one",
                    ));
                }
                return Err(plan.refusal(field, error::NOT_IN_LIST));
            }
        }
        Ok(())
    }
}
