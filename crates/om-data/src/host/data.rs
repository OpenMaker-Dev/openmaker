//! The data calculations read: records, every record of a table, related
//! sets and global values, through a [`CalcSource`].
//!
//! A provider's source reads the file (downloading what it lacks); a
//! [`Recorder`] around it keeps a copy of everything read as [`CalcData`],
//! which travels to a client that evaluates later without the file, through
//! a [`CachedSource`]. What a cached source lacks it lists as [`CalcNeed`]s
//! for the client to fetch, and reads as missing meanwhile.
//!
//! ExecuteSQL ([`super::sql`]) reads whole found sets: sources that
//! download read a found set's records ahead in batches
//! ([`CalcSource::prefetch`]) and answer `field = value` with a find on the
//! host ([`CalcSource::lookup`]) instead of downloading every record.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use om_calc::eval::{Pending, StoredContainer};
use om_calc::ops::{self, CompareOp};
use om_calc::{CalcError, Container, Ctx, Text, Value, ValueType};
use om_scripts::specs::SortSpec;

use crate::containers::{self, MemberInfo};

use super::schema::{CalcSchema, Hop, JoinOp};
use crate::DataError;

/// One record's stored values by (field, repetition); a value not listed
/// is empty.
#[derive(Clone, Debug, Default)]
pub struct RecordValues {
    pub values: BTreeMap<(u64, u32), Value>,
    /// The record's modification count (`Get ( RecordModificationCount )`).
    pub mod_count: Option<u64>,
    /// The account's set may not view the record: the host withheld its
    /// values, and a portal row shows `<No Access>` in its fields (#695).
    pub no_access: bool,
    /// Final cells of this window's retained prepared save. Display overlays
    /// must not replace them with earlier typed drafts (#1151).
    pub prepared_intent: bool,
    /// The cells of a new record the open transaction holds, which the
    /// host has none of yet (#892): the host's related sets from it are
    /// not its own ([`CachedSource`] scans them over a draft).
    pub held_new: bool,
    /// Fields explicitly refused by a local window's pending read.
    pub unavailable_fields: Option<Arc<BTreeSet<u64>>>,
}

impl RecordValues {
    pub fn get(&self, field: u64, repetition: u32) -> Option<&Value> {
        self.values.get(&(field, repetition))
    }

    pub fn set(&mut self, field: u64, repetition: u32, value: Value) {
        if value.is_empty() {
            self.values.remove(&(field, repetition));
        } else {
            self.values.insert((field, repetition), value);
        }
    }
}

/// The value of stored text in a field of `ty`: numbers keep their text
/// (`12abc` is 12, calc-values.md 6.5); a date, time or timestamp that does
/// not parse stays text, as the field holds it.
pub fn stored_value(text: &str, ty: ValueType, cx: &Ctx) -> Value {
    stored_text(Text::plain(text), ty, cx)
}

/// A stored value with its character styles. Stored dates retain the
/// original text as well as the parsed day, unlike an explicit GetAsDate.
pub fn stored_text(text: Text, ty: ValueType, cx: &Ctx) -> Value {
    if text.is_empty() {
        return Value::empty(ty);
    }
    match ty {
        ValueType::Text => Value::Text(text),
        // Text a script put into the field is text (script oracle s34:
        // `GetContainerAttribute`'s file name is empty, `Length` counts
        // characters).
        ValueType::Container if containers::reference(text.as_str()).is_none() => Value::Text(text),
        // The reference text a record holds; the host names its library
        // member when a calculation reads it (`FileCalc::field_value`).
        ValueType::Container => Value::Container(Container {
            text,
            ..Container::default()
        }),
        _ => match Value::Text(text.clone()).convert_declared(ty, cx) {
            v if v.is_invalid() => Value::Text(text),
            Value::Date(date) => Value::DateText(date, text),
            v => v,
        },
    }
}

/// Stored character styles remain calculation text, so text operations keep
/// the styles of the characters they copy. Declared non-text values retain
/// the same conversion rules as [`stored_value`].
pub fn styled_value(text: &om_layout::StyledText, ty: ValueType, cx: &Ctx) -> Value {
    if !matches!(ty, ValueType::Text | ValueType::Number | ValueType::Date)
        || text.character_runs.is_empty()
    {
        return stored_value(&text.text, ty, cx);
    }
    let mut out = Text::new();
    let mut at = 0;
    for run in &text.character_runs {
        let Some(slice) = text.text.get(run.range.clone()) else {
            continue;
        };
        if run.range.start < at {
            continue;
        }
        if let Some(gap) = text.text.get(at..run.range.start) {
            out.push(&Text::plain(gap));
        }
        let channel = |channel: f32| (channel.clamp(0.0, 1.0) * 255.0).round() as u32;
        out.push(&Text::styled(
            slice,
            om_calc::CharStyle {
                font: run.style.font_family.clone(),
                size: run
                    .style
                    .font_size
                    .filter(|size| size.is_finite() && *size > 0.0 && *size <= f64::from(u16::MAX))
                    .map(|size| size as u16),
                color: run
                    .style
                    .color
                    .map(|c| channel(c.red) << 16 | channel(c.green) << 8 | channel(c.blue)),
                flags: {
                    let faces = run.style.faces.unwrap_or(0);
                    match run.style.face_mask {
                        Some(mask) => u32::from(faces & mask) | (u32::from(mask & !faces) << 16),
                        None => u32::from(faces),
                    }
                },
            },
        ));
        at = run.range.end;
    }
    if let Some(rest) = text.text.get(at..) {
        out.push(&Text::plain(rest));
    }
    stored_text(out, ty, cx)
}

/// Data a calculation could not read yet.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CalcNeed {
    /// A record of a base table.
    Record { table: u64, record: u64 },
    /// Every record of a table, in creation order.
    Records { table: u64 },
    /// The found set of an occurrence (of base table `table`).
    FoundSet { occurrence: u64, table: u64 },
    /// The records of occurrence `to` related to `record` of `from`.
    Related { from: u64, record: u64, to: u64 },
    /// Only the ordered IDs of a related set. Matching and sorting may
    /// still need records; result bodies are requested separately.
    RelatedIds { from: u64, record: u64, to: u64 },
    /// A table's global field values.
    Globals { table: u64 },
    /// The records of a table whose stored `field` may hold `text` (a find
    /// for the whole value, [`CalcSource::lookup`]).
    Lookup {
        table: u64,
        field: u64,
        text: Arc<str>,
    },
    /// The file name and MD5 of member `member` of `table`'s library
    /// (containers.md 6.1, the header fetch).
    Member { table: u64, member: Vec<u8> },
    /// That member's data (6.1, the stream fetch).
    MemberData { table: u64, member: Vec<u8> },
    /// Occurrence `occurrence` shows a table of a related file not opened
    /// yet ([`CalcSchema::is_pending`]): open it. The answer is a schema
    /// that binds the occurrence, or one where it shows the file missing;
    /// no data comes with it.
    Occurrence { occurrence: u64 },
    /// Layout `layout` was listed from the layout catalog's index only
    /// ([`CalcSchema::is_layout_pending`]): read its member. A schema answers
    /// it; no data comes with it.
    Layout { layout: u32 },
}

/// A lookup's key in [`CalcData::lookups`]: table, field and the value's
/// text.
pub type LookupKey = (u64, u64, Arc<str>);

/// A library member: its table and member key.
pub type MemberKey = (u64, Vec<u8>);

/// How many bytes of library member data a [`CalcData`] keeps: past it, a
/// merge keeps only the data it brings (members do not change, so what is
/// dropped reads again when needed).
pub const MEMBER_DATA_BUDGET: usize = 64 << 20;

/// Data read for calculations, keyed by what was asked.
#[derive(Clone, Debug, Default)]
pub struct CalcData {
    /// The provider's cache generation. A different known generation replaces
    /// earlier data, including in another window that missed the notification.
    /// Metadata only: it does not make an otherwise empty answer successful.
    pub generation: Option<u64>,
    /// `None`: there is no such record.
    pub records: HashMap<(u64, u64), Option<Arc<RecordValues>>>,
    pub tables: HashMap<u64, Arc<[u64]>>,
    /// Found sets by occurrence.
    pub found: HashMap<u64, Arc<[u64]>>,
    /// The found sets' sort fields in priority order. An empty list marks
    /// a known unsorted set; absence means no sort context was recorded.
    pub sorts: HashMap<u64, Arc<[u64]>>,
    /// Eager related answers, including all result records.
    pub related: HashMap<(u64, u64, u64), Arc<[u64]>>,
    /// Ordered IDs, without a promise that result bodies are present.
    pub related_ids: HashMap<(u64, u64, u64), Arc<[u64]>>,
    pub globals: HashMap<u64, Arc<RecordValues>>,
    /// Lookups; `None`: the host has no index to answer it (scan instead).
    pub lookups: HashMap<LookupKey, Option<Arc<[u64]>>>,
    /// Why a provider's answer leaves out the needs of these tables: the
    /// first failure reading each. It describes one answer rather than
    /// data, so [`CalcData::is_empty`] and [`CalcData::merge`] ignore it.
    pub failures: BTreeMap<u64, DataError>,
    /// Library members read (boxed: most data has none).
    pub library: Box<LibraryData>,
    /// The read stopped because the window that asked no longer needs it
    /// ([`CalcCancel`], #987): nothing in this answer is to be applied, and
    /// its needs are to be asked again if the window still wants them.
    pub superseded: bool,
    /// Some needs went unanswered while a related file's host could not
    /// be reached (#740): they are not failures, and are to be asked again
    /// after this long, when the provider tries the host again. It
    /// describes one answer, so [`CalcData::merge`] ignores it.
    pub retry_after: Option<std::time::Duration>,
}

/// A window's request to stop a calculation read it no longer needs (#987):
/// one per window context, given with each `calc_data_for` the window asks
/// for, and cancelled by that window when its layout, view or found set
/// changes or it closes. Clones share it. Only
/// [`crate::DataProvider::calc_data_for`] reads it: other reads, writes,
/// commits and a script's own reads are never stopped by it.
#[derive(Clone, Debug, Default)]
pub struct CalcCancel(Arc<std::sync::atomic::AtomicBool>);

impl CalcCancel {
    /// The window no longer needs what this token was given with.
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the window cancelled it: a provider stops between bounded
    /// network operations and answers [`CalcData::superseded`].
    pub fn cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl CalcData {
    /// The answer of a read stopped by its [`CalcCancel`].
    pub fn superseded() -> CalcData {
        CalcData {
            superseded: true,
            ..CalcData::default()
        }
    }
}

/// Library members read for calculations.
#[derive(Clone, Debug, Default)]
pub struct LibraryData {
    /// Their headers; `None`: the library has no such member.
    pub members: HashMap<MemberKey, Option<Arc<MemberInfo>>>,
    /// Their data, within [`MEMBER_DATA_BUDGET`].
    pub data: HashMap<MemberKey, Option<Arc<StoredContainer>>>,
}

impl CalcData {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
            && self.tables.is_empty()
            && self.found.is_empty()
            && self.sorts.is_empty()
            && self.related.is_empty()
            && self.related_ids.is_empty()
            && self.globals.is_empty()
            && self.lookups.is_empty()
            && self.library.members.is_empty()
            && self.library.data.is_empty()
    }

    /// Refresh record contents while retaining window found-set/sort context
    /// and immutable library data. Ordinary refresh can reuse relationship IDs;
    /// flushing joins also invalidates the lists and index candidates used to
    /// rebuild them. Imported tables share these client-side namespaces.
    pub fn refresh_window(&mut self, flush_joins: bool) {
        self.records.clear();
        self.globals.clear();
        self.failures.clear();
        if flush_joins {
            self.tables.clear();
            self.related_ids.clear();
            self.lookups.clear();
        } else {
            // An eager answer can no longer promise fresh result bodies.
            self.related_ids.extend(self.related.drain());
        }
        self.related.clear();
    }

    /// Fresh inputs for a targeted layout evaluation. A portal also rebuilds
    /// its join from fresh matching/index candidates, retaining other joins
    /// and window found-set/sort context. The tuple uses calculation-schema
    /// occurrence ids and the source record, including imported occurrences.
    pub fn refresh_object(&mut self, join: Option<(u64, u64, u64)>) {
        self.refresh_window(false);
        if let Some(join) = join {
            self.related_ids.remove(&join);
            self.tables.clear();
            self.lookups.clear();
        }
    }

    /// Take all the data `other` holds, replacing what both hold.
    pub fn merge(&mut self, other: CalcData) {
        self.merge_with_members(other, &BTreeSet::new());
    }

    /// Merge an asynchronous answer without evicting bodies that a
    /// pending evaluation already read. As with one oversized answer,
    /// that evaluation's working set may exceed the soft byte budget.
    pub fn merge_with_members(&mut self, other: CalcData, retained: &BTreeSet<MemberKey>) {
        if other.generation.is_some() && self.generation != other.generation {
            *self = CalcData::default();
        }
        self.generation = other.generation.or(self.generation);
        // A replacement found set cannot retain the previous set's sort
        // when the replacement did not carry that context.
        for occurrence in other.found.keys() {
            self.sorts.remove(occurrence);
        }
        self.records.extend(other.records);
        self.tables.extend(other.tables);
        self.found.extend(other.found);
        self.sorts.extend(other.sorts);
        // New IDs supersede an older eager answer for the same key.
        for key in other.related_ids.keys() {
            self.related.remove(key);
        }
        self.related_ids.extend(other.related_ids);
        self.related_ids
            .extend(other.related.iter().map(|(k, v)| (*k, Arc::clone(v))));
        self.related.extend(other.related);
        self.globals.extend(other.globals);
        self.lookups.extend(other.lookups);
        let LibraryData { members, data } = *other.library;
        self.library.members.extend(members);
        let brought: Vec<MemberKey> = data.keys().cloned().collect();
        self.library.data.extend(data);
        if data_size(&self.library.data) > MEMBER_DATA_BUDGET {
            self.library
                .data
                .retain(|key, _| brought.contains(key) || retained.contains(key));
        }
    }

    pub fn has(&self, need: &CalcNeed) -> bool {
        match need {
            CalcNeed::Record { table, record } => self.records.contains_key(&(*table, *record)),
            CalcNeed::Records { table } => self.tables.contains_key(table),
            CalcNeed::FoundSet { occurrence, .. } => self.found.contains_key(occurrence),
            CalcNeed::Related { from, record, to } => {
                self.related.contains_key(&(*from, *record, *to))
            }
            CalcNeed::RelatedIds { from, record, to } => {
                self.related_ids.contains_key(&(*from, *record, *to))
                    || self.related.contains_key(&(*from, *record, *to))
            }
            CalcNeed::Globals { table } => self.globals.contains_key(table),
            CalcNeed::Lookup { table, field, text } => {
                self.lookups
                    .contains_key(&(*table, *field, Arc::clone(text)))
            }
            CalcNeed::Member { table, member } => {
                self.library.members.contains_key(&(*table, member.clone()))
            }
            CalcNeed::MemberData { table, member } => {
                self.library.data.contains_key(&(*table, member.clone()))
            }
            // A schema answers it, not data.
            CalcNeed::Occurrence { .. } | CalcNeed::Layout { .. } => false,
        }
    }

    /// Drop what a change to `record` of `table` may have made stale: the
    /// record, the table's record list and found sets, every related set
    /// and every lookup.
    pub fn invalidate_record(&mut self, table: u64, record: u64) {
        self.records.remove(&(table, record));
        self.tables.remove(&table);
        self.found.clear();
        self.sorts.clear();
        self.related.clear();
        self.related_ids.clear();
        self.lookups.clear();
    }
}

/// The bytes of member data held.
fn data_size(data: &HashMap<MemberKey, Option<Arc<StoredContainer>>>) -> usize {
    data.values()
        .flatten()
        .map(|stored| stored.data.retained_data_size())
        .sum()
}

/// A container field's value as a calculation reads it (containers.md
/// 1.4): the stored reference text of a member of `table`'s library
/// becomes that member's file name, its data left in the library until a
/// function reads it ([`library_data`]). A reference the source cannot
/// follow (no libraries, or a member the library lost) stays its text.
pub fn library_value(
    source: &dyn CalcSource,
    table: u64,
    value: Value,
) -> Result<Value, CalcError> {
    let Value::Container(c) = &value else {
        return Ok(value);
    };
    let Some(member) = c
        .library
        .is_none()
        .then(|| containers::reference(c.text.as_str()))
        .flatten()
    else {
        return Ok(value);
    };
    Ok(match source.library_member(table, &member)? {
        Some(info) => Value::Container(info.value(table, &member)),
        None => value,
    })
}

/// The data of a container a calculation holds by its library member
/// ([`library_value`]); error 1225 when the source has none.
pub fn library_data(
    source: &dyn CalcSource,
    container: &Container,
) -> Result<StoredContainer, CalcError> {
    let library = container.library.as_ref().ok_or(CalcError::NOT_SUPPORTED)?;
    let stored = source
        .library_data(library.table, &library.member)?
        .ok_or(CalcError::NOT_SUPPORTED)?;
    if stored
        .data
        .library
        .as_ref()
        .is_some_and(|actual| actual.as_ref() != library.as_ref())
    {
        return Err(CalcError::NOT_SUPPORTED);
    }
    Ok(StoredContainer::clone(&stored))
}

/// A sort key [`CalcSource::found_sort`] cannot name as a field of the
/// sorted table, such as a related field. Groups at and after it are
/// unknown: summaries read them as unsorted and reports refuse them,
/// rather than group by the remaining keys.
pub const UNRESOLVED_SORT_FIELD: u64 = u64::MAX;

/// What a scan ([`related_by_scan`]) reads of a table: every record in
/// creation order, each with the nonempty values of the hop's target
/// fields, one list per field in the hop's predicate order (every
/// repetition). A global target field's list is empty: its values are the
/// table's, read once per scan rather than copied into every row.
pub type ScanRows = Arc<[(u64, Vec<Vec<Value>>)]>;

/// About how many bytes `values` hold: what a source keeping scan rows
/// counts against its budget ([`CalcSource::scan_keep_budget`]).
pub fn scan_values_bytes(values: &[Vec<Value>]) -> usize {
    let value = |v: &Value| -> usize {
        std::mem::size_of::<Value>()
            + match v {
                Value::Text(t) | Value::NumberText(_, t) | Value::DateText(_, t) => {
                    t.as_str().len()
                }
                Value::Container(c) => {
                    c.text.as_str().len() + c.streams.iter().map(|(_, b)| b.len()).sum::<usize>()
                }
                _ => 0,
            }
    };
    std::mem::size_of::<(u64, Vec<Vec<Value>>)>()
        + values
            .iter()
            .map(|list| std::mem::size_of::<Vec<Value>>() + list.iter().map(value).sum::<usize>())
            .sum::<usize>()
}

/// Where calculations read data. Methods take `&self`, as the evaluator's
/// host does; sources that download keep their caches in cells.
pub trait CalcSource {
    /// A record's stored values; `None` when the table has no such record.
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError>;

    /// Every record of a table in creation order.
    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError>;

    /// A table's global field values.
    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError>;

    /// The found set of `occurrence` (a layout's table occurrence), in
    /// order. Without a find it is every record of the table in creation
    /// order, as on a record cursor (calc-values.md 12.1).
    fn found_set(&self, _occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.records(table)
    }

    /// The fields the found set of `occurrence` is sorted by (field ids of
    /// its table), in priority order. An empty list explicitly marks an
    /// unsorted set; `None` is also treated as unsorted when the source
    /// does not provide that context. Summaries break into groups by these
    /// (file-metadata.md 5.3). A key of another table (a related field)
    /// is [`UNRESOLVED_SORT_FIELD`] in its place.
    fn found_sort(&self, _occurrence: u64) -> Option<Arc<[u64]>> {
        None
    }

    /// Read `records` of `table` ahead, before they are read one by one; a
    /// source that downloads fetches them together.
    fn prefetch_records(&self, _table: u64, _records: &[u64]) {}

    /// What an earlier scan read of `table`'s `fields` and the source kept
    /// ([`CalcSource::keep_scan_rows`]), so a scan matches against it
    /// instead of reading every record again; `None` (the default) when it
    /// keeps nothing for them. A source that keeps rows must drop them
    /// whenever the table may have changed (#804).
    fn scan_rows(&self, _table: u64, _fields: &[u64]) -> Option<ScanRows> {
        None
    }

    /// How many bytes of match values ([`scan_values_bytes`]) the source
    /// would keep from a new scan. A scan collects rows only up to it, and
    /// offers them only when they all fit; the default, 0, keeps none, so
    /// a scan collects nothing.
    fn scan_keep_budget(&self) -> usize {
        0
    }

    /// Offer what a scan read of `table`'s `fields`, `bytes` in all; the
    /// default keeps nothing.
    fn keep_scan_rows(&self, _table: u64, _fields: &[u64], _rows: ScanRows, _bytes: usize) {}

    /// The records of `to` related to `record` of `from`, in the last
    /// relationship's order (creation order when it does not sort). The
    /// default follows the relationship path and compares the match fields
    /// of every record of each table on it.
    fn related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        related_by_scan(self, schema, from, record, to, cx)
    }

    /// Ordered related IDs, without fetching result bodies merely to return
    /// them. The fallback preserves sources that implement only `related`.
    fn related_ids(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        self.related(schema, from, record, to, cx)
    }

    /// Nonempty repetitions of a source-side relationship match field.
    /// Unstored calculations evaluate on their source record; destination
    /// fields continue to use their stored values. Override to supply
    /// connection metadata to [`super::source_match_values`].
    fn source_match_values(
        &self,
        schema: &CalcSchema,
        occurrence: u64,
        record: u64,
        field: u64,
        cx: &Ctx,
    ) -> Result<Vec<Value>, CalcError> {
        super::source_match_values(self, schema, occurrence, record, field, cx, None)
    }

    /// Whether `record` of `table` reads without a download. Sources that
    /// hold everything answer `true`.
    fn has_record(&self, _table: u64, _record: u64) -> bool {
        true
    }

    /// Read the records `ids` of `table` ahead, in as few downloads as the
    /// source can; those it already has are skipped.
    fn prefetch(&self, _table: u64, _ids: &[u64]) -> Result<(), CalcError> {
        Ok(())
    }

    /// The records of `table` whose stored `field` may hold `text` (the
    /// value's text in the formats of the file holding `table`, a related
    /// file's for its tables), from an index or a find on the
    /// host: every record that holds it, perhaps others (the caller checks
    /// each). `None` when the source cannot narrow it (no index, a value a
    /// find cannot express).
    fn lookup(
        &self,
        _table: u64,
        _field: u64,
        _text: &str,
    ) -> Result<Option<Arc<[u64]>>, CalcError> {
        Ok(None)
    }

    /// Whether long work over this source should stop now: the script
    /// that reads it was aborted or ran out of time. ExecuteSQL asks while
    /// it scans, joins and groups (`om_sql::SqlSource::interrupted`).
    fn interrupted(&self) -> bool {
        false
    }

    /// The file name and MD5 of member `member` of `table`'s library
    /// (containers.md 6.1, without its data); `None` when the library has
    /// no such member, or the source reads no libraries (the default).
    fn library_member(
        &self,
        _table: u64,
        _member: &[u8],
    ) -> Result<Option<Arc<MemberInfo>>, CalcError> {
        Ok(None)
    }

    /// That member's data: every stream and whether it is intact
    /// ([`containers::read`]); `None` as for [`CalcSource::library_member`].
    fn library_data(
        &self,
        _table: u64,
        _member: &[u8],
    ) -> Result<Option<Arc<StoredContainer>>, CalcError> {
        Ok(None)
    }

    /// What this source reads as missing while its caller fetches it to
    /// evaluate again ([`om_calc::eval::CalcHost::pending`]); none by
    /// default.
    fn pending(&self) -> Pending {
        Pending::default()
    }

    /// A calculation read occurrence `occurrence`, whose related file has
    /// not been opened yet ([`CalcSchema::is_pending`]): a source that
    /// fetches for its caller asks for it ([`CalcNeed::Occurrence`]), so
    /// the read waits for the file rather than showing it missing. Nothing
    /// by default.
    fn need_occurrence(&self, _occurrence: u64) {}

    /// A calculation read layout `layout`'s objects or fields, whose member
    /// has not been read yet ([`CalcSchema::is_layout_pending`]): asked for
    /// as [`Self::need_occurrence`] asks for a file. Nothing by default.
    fn need_layout(&self, _layout: u32) {}

    /// A calculation read occurrence `occurrence`, or one of its fields,
    /// which the schema does not have and no file being opened will give:
    /// it reads as missing (`error`, 102 or 106), a `?` the calculation
    /// goes on over. A source that tells its reader what failed notes it
    /// ([`Recorder::failure`]); nothing by default.
    fn missing_in_schema(&self, _occurrence: u64, _error: CalcError) {}
}

/// Error 103, "Relationship is missing": the occurrences are not related.
pub fn relationship_missing() -> CalcError {
    CalcError::from_code(103).expect("nonzero")
}

/// The related set by scanning: for each hop of the path, the records of
/// the next table that some record so far matches through every
/// predicate (file-metadata.md 4.3). A predicate holds when some
/// repetition of one side compares to some repetition of the other with
/// its operator; empty values match nothing, also for `≠` and the
/// inequalities, while `×` relates every record. The set is in creation
/// order, or in the last relationship's sort order when it sorts.
pub fn related_by_scan<S: CalcSource + ?Sized>(
    source: &S,
    schema: &CalcSchema,
    from: u64,
    record: u64,
    to: u64,
    cx: &Ctx,
) -> Result<Arc<[u64]>, CalcError> {
    let path = schema.path(from, to).ok_or_else(relationship_missing)?;
    let mut records = vec![record];
    for hop in &path {
        records = scan_hop(source, schema, hop, &records, cx)?;
        if records.is_empty() {
            break;
        }
    }
    if let Some(sort) = path.last().and_then(|hop| hop.sort.as_ref()) {
        records = super::sort::sort_records(source, schema, to, records, sort, cx)?;
    }
    Ok(records.into())
}

/// Whether `field` of `table` holds other match values in `now` than in
/// `before`, as a scan reads them: every repetition, empty ones left out;
/// none for a global field, which a scan reads from the table.
fn match_values_differ(
    table: &super::schema::TableDef,
    now: &RecordValues,
    before: &RecordValues,
    field: u64,
) -> bool {
    let Some(def) = table.fields.get(&field) else {
        return false;
    };
    if def.global {
        return false;
    }
    fn value(values: &RecordValues, field: u64, rep: u32) -> Option<&Value> {
        values.get(field, rep).filter(|value| !value.is_empty())
    }
    (1..=def.repetitions).any(
        |rep| match (value(now, field, rep), value(before, field, rep)) {
            (None, None) => false,
            (Some(a), Some(b)) => !same_values(std::slice::from_ref(a), std::slice::from_ref(b)),
            _ => true,
        },
    )
}

/// Whether `a` and `b` are the same values. `Value` has no equality of its
/// own; its debug text shows every part of it, so the same text is the
/// same value. Another text reads as a change, at worst a scan the values
/// did not need.
fn same_values(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| format!("{a:?}") == format!("{b:?}"))
}

/// The comparison a predicate's operator makes, left side first, both for
/// the scan and for the index path that narrows it; `None` for `×`, which
/// compares nothing.
pub fn join_compare_op(op: JoinOp) -> Option<CompareOp> {
    Some(match op {
        JoinOp::Equal => CompareOp::Eq,
        JoinOp::NotEqual => CompareOp::Ne,
        JoinOp::Less => CompareOp::Lt,
        JoinOp::LessEqual => CompareOp::Le,
        JoinOp::Greater => CompareOp::Gt,
        JoinOp::GreaterEqual => CompareOp::Ge,
        JoinOp::Cartesian => return None,
    })
}

fn scan_hop<S: CalcSource + ?Sized>(
    source: &S,
    schema: &CalcSchema,
    hop: &Hop,
    sources: &[u64],
    cx: &Ctx,
) -> Result<Vec<u64>, CalcError> {
    // A side waiting for its file or its table's fields is asked for, and
    // the set waits for it (1224), rather than matching no keys.
    let mut waiting = false;
    for occurrence in [hop.from, hop.to] {
        if schema.is_pending(occurrence) {
            source.need_occurrence(occurrence);
            waiting = true;
        }
    }
    if waiting {
        return Err(CalcError::from_code(1224).expect("nonzero"));
    }
    let (Some(from), Some(to)) = (schema.table_of(hop.from), schema.table_of(hop.to)) else {
        return Err(relationship_missing());
    };
    // Cartesian membership needs only a nonempty source set and the RID
    // list. Do this before prefetching either side, including on fallback
    // paths where an earlier equality hop could not use its index.
    if !hop.predicates.is_empty() && hop.predicates.iter().all(|p| p.op == JoinOp::Cartesian) {
        return if sources.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(source.records(to.id)?.to_vec())
        };
    }
    let values = |table: &super::schema::TableDef,
                  values: Option<&RecordValues>,
                  globals: &RecordValues,
                  field: u64|
     -> Vec<Value> {
        let Some(def) = table.fields.get(&field) else {
            return Vec::new();
        };
        let store = if def.global { Some(globals) } else { values };
        (1..=def.repetitions)
            .filter_map(|rep| store?.get(field, rep).cloned())
            .filter(|v| !v.is_empty())
            .collect()
    };
    let from_globals = source.globals(from.id)?;
    let to_globals = source.globals(to.id)?;
    source.prefetch_records(from.id, sources);
    let mut keys: Vec<Vec<Vec<Value>>> = Vec::with_capacity(sources.len());
    for &r in sources {
        let row = source.record(from.id, r)?;
        keys.push(
            hop.predicates
                .iter()
                .map(|p| {
                    if p.op != JoinOp::Cartesian
                        && from
                            .fields
                            .get(&p.left)
                            .is_some_and(|f| f.is_unstored_calculation())
                    {
                        source.source_match_values(schema, hop.from, r, p.left, cx)
                    } else {
                        Ok(values(from, row.as_deref(), &from_globals, p.left))
                    }
                })
                .collect::<Result<_, _>>()?,
        );
    }
    // A compared predicate whose source values are all empty matches no
    // record (each needs a source value), so neither does the hop: the
    // target table is not read. A record begun in a portal has no match
    // values yet (#1308).
    if hop
        .predicates
        .iter()
        .enumerate()
        .any(|(i, p)| p.op != JoinOp::Cartesian && keys.iter().all(|ours| ours[i].is_empty()))
    {
        return Ok(Vec::new());
    }
    // A global target field holds the table's values, the same for every
    // record: read once, never copied into the rows.
    let global: Vec<Option<Vec<Value>>> = hop
        .predicates
        .iter()
        .map(|p| {
            to.fields
                .get(&p.right)
                .filter(|f| f.global)
                .map(|_| values(to, None, &to_globals, p.right))
        })
        .collect();
    let matches = |theirs: &[Vec<Value>]| {
        keys.iter().any(|ours| {
            hop.predicates.iter().enumerate().all(|(i, p)| {
                let Some(op) = join_compare_op(p.op) else {
                    return true;
                };
                let theirs = global[i].as_deref().unwrap_or(&theirs[i]);
                ours[i].iter().any(|a| {
                    theirs
                        .iter()
                        .any(|b| ops::compare_op(a, op, b, cx).is_true(cx))
                })
            })
        })
    };
    // Every record of the target table with its match values: kept by the
    // source from an earlier scan, or read now, and offered for the next
    // scan when they fit the source's budget.
    let targets: Vec<u64> = hop.predicates.iter().map(|p| p.right).collect();
    let mut out = Vec::new();
    if let Some(rows) = source.scan_rows(to.id, &targets) {
        out.extend(
            rows.iter()
                .filter(|(_, theirs)| matches(theirs))
                .map(|(id, _)| *id),
        );
        return Ok(out);
    }
    let candidates = source.records(to.id)?;
    source.prefetch_records(to.id, &candidates);
    let budget = source.scan_keep_budget();
    let mut collected = (budget > 0).then(Vec::new);
    let mut bytes = 0usize;
    for &candidate in candidates.iter() {
        let row = source.record(to.id, candidate)?;
        let theirs: Vec<Vec<Value>> = hop
            .predicates
            .iter()
            .zip(&global)
            .map(|(p, global)| match global {
                Some(_) => Vec::new(),
                None => values(to, row.as_deref(), &to_globals, p.right),
            })
            .collect();
        if matches(&theirs) {
            out.push(candidate);
        }
        if let Some(rows) = collected.as_mut() {
            bytes = bytes.saturating_add(scan_values_bytes(&theirs));
            if bytes > budget {
                collected = None;
            } else {
                rows.push((candidate, theirs));
            }
        }
    }
    if let Some(rows) = collected {
        source.keep_scan_rows(to.id, &targets, rows.into(), bytes);
    }
    Ok(out)
}

/// A source that keeps a copy of everything read through it, and the
/// first read that failed.
pub struct Recorder<'a, S: CalcSource + ?Sized = dyn CalcSource + 'a> {
    inner: &'a S,
    data: RefCell<CalcData>,
    failed: Cell<Option<CalcError>>,
}

impl<'a, S: CalcSource + ?Sized> Recorder<'a, S> {
    pub fn new(inner: &'a S) -> Recorder<'a, S> {
        Recorder {
            inner,
            data: RefCell::new(CalcData::default()),
            failed: Cell::new(None),
        }
    }

    /// What was read so far.
    pub fn take(&self) -> CalcData {
        std::mem::take(&mut self.data.borrow_mut())
    }

    /// The error of the first read through it that failed: data not held
    /// (1224, being fetched or not to be had), refused or missing. A
    /// calculation goes on over such a read as `?`.
    pub fn failure(&self) -> Option<CalcError> {
        self.failed.get()
    }

    fn note<T>(&self, read: Result<T, CalcError>) -> Result<T, CalcError> {
        if let Err(error) = &read {
            if self.failed.get().is_none() {
                self.failed.set(Some(*error));
            }
        }
        read
    }

    /// Read `need` (and, for a related set, its records) into the copy.
    pub fn fetch(&self, schema: &CalcSchema, need: &CalcNeed, cx: &Ctx) -> Result<(), CalcError> {
        match *need {
            CalcNeed::Record { table, record } => {
                self.record(table, record)?;
            }
            CalcNeed::Records { table } => {
                self.records(table)?;
            }
            CalcNeed::FoundSet { occurrence, table } => {
                self.found_set(occurrence, table)?;
            }
            CalcNeed::Globals { table } => {
                self.globals(table)?;
            }
            CalcNeed::Related { from, record, to } => {
                let related = self.related(schema, from, record, to, cx)?;
                if let Some(table) = schema.table_of(to).map(|t| t.id) {
                    self.prefetch(table, &related)?;
                    for &r in related.iter() {
                        self.record(table, r)?;
                    }
                    // A failed body read must never mark this answer complete.
                    self.data
                        .borrow_mut()
                        .related
                        .insert((from, record, to), related);
                }
            }
            CalcNeed::RelatedIds { from, record, to } => {
                self.related_ids(schema, from, record, to, cx)?;
            }
            CalcNeed::Lookup {
                table,
                field,
                ref text,
            } => {
                self.lookup(table, field, text)?;
            }
            CalcNeed::Member { table, ref member } => {
                self.library_member(table, member)?;
            }
            CalcNeed::MemberData { table, ref member } => {
                self.library_data(table, member)?;
            }
            CalcNeed::Occurrence { occurrence } => self.inner.need_occurrence(occurrence),
            CalcNeed::Layout { layout } => self.inner.need_layout(layout),
        }
        Ok(())
    }
}

impl<S: CalcSource + ?Sized> CalcSource for Recorder<'_, S> {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        let r = self.note(self.inner.record(table, record))?;
        self.data
            .borrow_mut()
            .records
            .insert((table, record), r.clone());
        Ok(r)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        let r = self.note(self.inner.records(table))?;
        self.data.borrow_mut().tables.insert(table, Arc::clone(&r));
        Ok(r)
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        let r = self.note(self.inner.globals(table))?;
        self.data.borrow_mut().globals.insert(table, Arc::clone(&r));
        Ok(r)
    }

    fn found_set(&self, occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        let r = self.note(self.inner.found_set(occurrence, table))?;
        let sort = self.inner.found_sort(occurrence).unwrap_or_default();
        let mut data = self.data.borrow_mut();
        data.found.insert(occurrence, Arc::clone(&r));
        data.sorts.insert(occurrence, sort);
        Ok(r)
    }

    fn found_sort(&self, occurrence: u64) -> Option<Arc<[u64]>> {
        let sort = self.inner.found_sort(occurrence);
        self.data
            .borrow_mut()
            .sorts
            .insert(occurrence, sort.clone().unwrap_or_default());
        sort
    }

    fn prefetch_records(&self, table: u64, records: &[u64]) {
        self.inner.prefetch_records(table, records);
    }

    fn scan_rows(&self, table: u64, fields: &[u64]) -> Option<ScanRows> {
        self.inner.scan_rows(table, fields)
    }

    fn scan_keep_budget(&self) -> usize {
        self.inner.scan_keep_budget()
    }

    fn keep_scan_rows(&self, table: u64, fields: &[u64], rows: ScanRows, bytes: usize) {
        self.inner.keep_scan_rows(table, fields, rows, bytes);
    }

    fn source_match_values(
        &self,
        schema: &CalcSchema,
        occurrence: u64,
        record: u64,
        field: u64,
        cx: &Ctx,
    ) -> Result<Vec<Value>, CalcError> {
        self.note(
            self.inner
                .source_match_values(schema, occurrence, record, field, cx),
        )
    }

    fn related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        let r = self.note(self.inner.related(schema, from, record, to, cx))?;
        self.data
            .borrow_mut()
            .related_ids
            .insert((from, record, to), Arc::clone(&r));
        Ok(r)
    }

    fn related_ids(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        let r = self.note(self.inner.related_ids(schema, from, record, to, cx))?;
        self.data
            .borrow_mut()
            .related_ids
            .insert((from, record, to), Arc::clone(&r));
        Ok(r)
    }

    fn has_record(&self, table: u64, record: u64) -> bool {
        self.inner.has_record(table, record)
    }

    fn prefetch(&self, table: u64, ids: &[u64]) -> Result<(), CalcError> {
        self.note(self.inner.prefetch(table, ids))
    }

    fn lookup(&self, table: u64, field: u64, text: &str) -> Result<Option<Arc<[u64]>>, CalcError> {
        let r = self.note(self.inner.lookup(table, field, text))?;
        self.data
            .borrow_mut()
            .lookups
            .insert((table, field, Arc::from(text)), r.clone());
        Ok(r)
    }

    fn interrupted(&self) -> bool {
        self.inner.interrupted()
    }

    fn library_member(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<MemberInfo>>, CalcError> {
        let r = self.note(self.inner.library_member(table, member))?;
        self.data
            .borrow_mut()
            .library
            .members
            .insert((table, member.to_vec()), r.clone());
        Ok(r)
    }

    fn library_data(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<StoredContainer>>, CalcError> {
        let r = self.note(self.inner.library_data(table, member))?;
        self.data
            .borrow_mut()
            .library
            .data
            .insert((table, member.to_vec()), r.clone());
        Ok(r)
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

    fn missing_in_schema(&self, occurrence: u64, error: CalcError) {
        self.inner.missing_in_schema(occurrence, error);
        let _ = self.note::<()>(Err(error));
    }
}

/// Fetches what a [`CachedSource`] lacks, synchronously (a client running a
/// script waits for its provider, as FileMaker Pro does).
pub type Fetch<'a> = &'a dyn Fn(&[CalcNeed]) -> Option<CalcData>;

/// Related sets rebuilt or sorted again over a [`CachedSource`]'s
/// overlays, by (from occurrence, record, to occurrence).
pub type ScannedSets = HashMap<(u64, u64, u64), Arc<[u64]>>;

/// A source over data already read: [`CalcData`] plus overlays (a record
/// being edited). What it lacks it lists in [`CachedSource::missing`] and
/// fetches with its [`Fetch`] if it has one, else reads as error 1224
/// ("record missing" for the calculation) until it has been fetched.
pub struct CachedSource<'a> {
    calculation_session: Option<&'a super::SessionInfo>,
    cache: &'a RefCell<CalcData>,
    fetch: Option<Fetch<'a>>,
    overlay: HashMap<(u64, u64), Arc<RecordValues>>,
    /// Related sets rebuilt over the overlays, kept by the caller
    /// ([`CachedSource::keeping_scans`]).
    scans: Option<&'a RefCell<ScannedSets>>,
    /// The found set a window shows, by occurrence, and its sort fields.
    found: HashMap<u64, Arc<[u64]>>,
    sorts: HashMap<u64, Arc<[u64]>>,
    missing: RefCell<BTreeSet<CalcNeed>>,
    library_reads: RefCell<BTreeSet<MemberKey>>,
    /// What the provider could not supply, when the caller fetches the
    /// rest and evaluates again ([`CachedSource::retrying`]).
    failed: Option<&'a BTreeSet<CalcNeed>>,
    pending: Cell<Pending>,
}

impl<'a> CachedSource<'a> {
    pub fn new(cache: &'a RefCell<CalcData>) -> CachedSource<'a> {
        CachedSource {
            calculation_session: None,
            cache,
            fetch: None,
            overlay: HashMap::new(),
            scans: None,
            found: HashMap::new(),
            sorts: HashMap::new(),
            missing: RefCell::new(BTreeSet::new()),
            library_reads: RefCell::new(BTreeSet::new()),
            failed: None,
            pending: Cell::new(Pending::default()),
        }
    }

    /// Metadata for source-side calculated relationship keys in a draft.
    pub fn with_calculation_session(mut self, session: &'a super::SessionInfo) -> Self {
        self.calculation_session = Some(session);
        self
    }

    pub fn with_fetch(mut self, fetch: Fetch<'a>) -> CachedSource<'a> {
        self.fetch = Some(fetch);
        self
    }

    /// The caller fetches what is missing and evaluates again once it
    /// arrives (a window that paints without waiting), except `failed`,
    /// which the provider could not supply: reads of the rest are pending
    /// ([`CalcSource::pending`]), so an evaluation that read them stops its
    /// loops rather than running them over missing values. Without this,
    /// or with a fetch, what is missing stays missing.
    pub fn retrying(mut self, failed: &'a BTreeSet<CalcNeed>) -> CachedSource<'a> {
        self.failed = Some(failed);
        self
    }

    /// Read `values` for `record` of `table` instead of the cached record.
    pub fn with_overlay(mut self, table: u64, record: u64, values: Arc<RecordValues>) -> Self {
        self.overlay.insert((table, record), values);
        self
    }

    /// Keep the related sets rebuilt or sorted again over the overlays in
    /// `scans`, and read them from there: a caller evaluating many
    /// calculations over the same overlays and cached data computes each
    /// set once. The caller drops them when either changes.
    pub fn keeping_scans(mut self, scans: &'a RefCell<ScannedSets>) -> Self {
        self.scans = Some(scans);
        self
    }

    /// The found set of `occurrence`.
    pub fn with_found_set(mut self, occurrence: u64, records: Arc<[u64]>) -> Self {
        self.found.insert(occurrence, records);
        self
    }

    /// The fields the found set of `occurrence` is sorted by.
    pub fn with_found_sort(mut self, occurrence: u64, fields: Arc<[u64]>) -> Self {
        self.sorts.insert(occurrence, fields);
        self
    }

    /// What was asked for and not available.
    pub fn missing(&self) -> Vec<CalcNeed> {
        self.missing.borrow().iter().cloned().collect()
    }

    pub fn has_missing(&self) -> bool {
        !self.missing.borrow().is_empty()
    }

    /// Body dependencies read so far, including reads made before a later
    /// missing branch. An asynchronous caller can retain these until its
    /// pending evaluation completes.
    pub fn library_reads(&self) -> BTreeSet<MemberKey> {
        self.library_reads.borrow().clone()
    }

    /// A schema-answered need ([`CalcNeed::Occurrence`], [`CalcNeed::Layout`]):
    /// fetched at once when the source fetches for a script, which then
    /// evaluates again over the schema that answer grew; listed as missing
    /// either way, so the caller sees what it waited for.
    fn need(&self, need: CalcNeed) {
        if let Some(fetch) = self.fetch {
            let _ = fetch(std::slice::from_ref(&need));
        }
        self.lack(need);
    }

    /// Lists `need` as missing, and counts the read as pending when the
    /// caller will fetch it.
    fn lack(&self, need: CalcNeed) {
        let pending =
            self.fetch.is_none() && self.failed.is_some_and(|failed| !failed.contains(&need));
        let new = self.missing.borrow_mut().insert(need);
        if pending {
            let mut p = self.pending.get();
            p.reads += 1;
            p.needs += u64::from(new);
            self.pending.set(p);
        }
    }

    /// The records of `to` related to `record` of `from` over the
    /// overlays: the stored set `stored` reads, unless the overlays may
    /// relate other records ([`Self::relates_differently`]) or `record` is
    /// a new record the open transaction holds ([`Self::held_new`]), with
    /// or without overlays, when the set is rebuilt by
    /// [`related_by_scan`], which reads every record of each table on the
    /// path. The stored set is put in the overlays' order when the last
    /// relationship sorts by a value they may change
    /// ([`Self::may_reorder`]).
    fn related_over_overlays(
        &self,
        schema: &CalcSchema,
        (from, record, to): (u64, u64, u64),
        cx: &Ctx,
        stored: impl FnOnce() -> Result<Arc<[u64]>, CalcError>,
    ) -> Result<Arc<[u64]>, CalcError> {
        // A held new record's keys, auto-entered ones too, relate records
        // before anything is typed into it: the host has no such record.
        let held_new = self.held_new(schema, from, record);
        if self.overlay.is_empty() && !held_new {
            return stored();
        }
        let Some(path) = schema.path(from, to) else {
            return stored();
        };
        // Only sets the overlays changed are kept: the others are the
        // stored ones, read at once.
        let key = (from, record, to);
        if let Some(records) = self
            .scans
            .and_then(|scans| scans.borrow().get(&key).cloned())
        {
            return Ok(records);
        }
        let records = if held_new || self.relates_differently(schema, &path, record, cx) {
            related_by_scan(self, schema, from, record, to, cx)?
        } else {
            let records = stored()?;
            match path.last().and_then(|hop| hop.sort.as_ref()) {
                Some(sort) if records.len() > 1 && self.may_reorder(schema, to, sort, &records) => {
                    super::sort::sort_records(self, schema, to, records.to_vec(), sort, cx)?.into()
                }
                _ => return Ok(records),
            }
        };
        if let Some(scans) = self.scans {
            scans.borrow_mut().insert(key, Arc::clone(&records));
        }
        Ok(records)
    }

    /// Whether the overlays may make the path relate other records than
    /// the stored set holds (#1308). Only a change of the values a hop
    /// compares can:
    /// - of its source record, or for a later hop of any record of its
    ///   table, or a first hop from a record with no stored values (one
    ///   begun and not committed);
    /// - of a stored record of its target table, a scan's candidate;
    /// - of a source match field that is an unstored calculation, which
    ///   may read any record: on the first hop its values over the
    ///   overlays and over the stored data are compared, past it any
    ///   overlay counts as a change (the hop's sources are not known).
    ///
    /// A record with no stored values is no candidate: a scan reads the
    /// target table's stored records. A global match field is read from
    /// its table, not from the overlay. An overlay whose stored values are
    /// not cached counts as a change.
    fn relates_differently(
        &self,
        schema: &CalcSchema,
        path: &[Hop],
        record: u64,
        cx: &Ctx,
    ) -> bool {
        if self.calculated_keys_differ(schema, path, record, cx) {
            return true;
        }
        let cache = self.cache.borrow();
        let stored = |table: u64, record: u64| {
            cache
                .records
                .get(&(table, record))
                .and_then(|values| values.as_deref())
        };
        for (i, hop) in path.iter().enumerate() {
            let (Some(left), Some(right)) = (schema.table_of(hop.from), schema.table_of(hop.to))
            else {
                continue;
            };
            let compared: Vec<_> = hop
                .predicates
                .iter()
                .filter(|p| p.op != JoinOp::Cartesian)
                .collect();
            for (&(table, id), values) in &self.overlay {
                if table == left.id && (i > 0 || id == record) {
                    match stored(table, id) {
                        Some(before) => {
                            if compared
                                .iter()
                                .any(|p| match_values_differ(left, values, before, p.left))
                            {
                                return true;
                            }
                        }
                        // A later hop's sources are stored records.
                        None if i > 0 && super::unsaved_record(id) => {}
                        None => return true,
                    }
                }
                if table == right.id && !super::unsaved_record(id) {
                    match stored(table, id) {
                        Some(before) => {
                            if compared
                                .iter()
                                .any(|p| match_values_differ(right, values, before, p.right))
                            {
                                return true;
                            }
                        }
                        None => return true,
                    }
                }
            }
        }
        false
    }

    /// Whether a source match field that is an unstored calculation may
    /// take other values over the overlays ([`Self::relates_differently`]).
    /// What the stored data's evaluation lacks is wanted as this source's.
    fn calculated_keys_differ(
        &self,
        schema: &CalcSchema,
        path: &[Hop],
        record: u64,
        cx: &Ctx,
    ) -> bool {
        for (i, hop) in path.iter().enumerate() {
            let Some(left) = schema.table_of(hop.from) else {
                continue;
            };
            let calculated: Vec<u64> = hop
                .predicates
                .iter()
                .filter(|p| p.op != JoinOp::Cartesian)
                .map(|p| p.left)
                .filter(|field| {
                    left.fields
                        .get(field)
                        .is_some_and(|f| f.is_unstored_calculation())
                })
                .collect();
            if calculated.is_empty() {
                continue;
            }
            if i > 0 || super::unsaved_record(record) {
                return true;
            }
            let stored = self.without_overlays();
            let differ = calculated.into_iter().any(|field| {
                let now = self.source_match_values(schema, hop.from, record, field, cx);
                let before = stored.source_match_values(schema, hop.from, record, field, cx);
                !matches!((now, before), (Ok(now), Ok(before)) if same_values(&now, &before))
            });
            for need in stored.missing() {
                self.lack(need);
            }
            self.library_reads
                .borrow_mut()
                .extend(stored.library_reads.into_inner());
            if differ {
                return true;
            }
        }
        false
    }

    /// Whether `record` of `from` is a new record the open transaction
    /// holds ([`RecordValues::held_new`]): its stored set was read from a
    /// host that has no such record, so it is scanned, as before #1308.
    fn held_new(&self, schema: &CalcSchema, from: u64, record: u64) -> bool {
        schema.table_of(from).is_some_and(|table| {
            self.cache
                .borrow()
                .records
                .get(&(table.id, record))
                .and_then(|values| values.as_deref())
                .is_some_and(|values| values.held_new)
        })
    }

    /// This source over the stored data alone: no overlays.
    fn without_overlays(&self) -> CachedSource<'a> {
        CachedSource {
            calculation_session: self.calculation_session,
            cache: self.cache,
            fetch: self.fetch,
            overlay: HashMap::new(),
            scans: None,
            found: self.found.clone(),
            sorts: self.sorts.clone(),
            missing: RefCell::default(),
            library_reads: RefCell::default(),
            failed: self.failed,
            pending: Cell::default(),
        }
    }

    /// Whether a sort of `records` of `to` by `sort` may differ over the
    /// overlays: one of them is in the set, or a sort field is not a
    /// stored field of `to`'s table (a related field, an unstored
    /// calculation, a summary or a value list's order may read any
    /// record).
    fn may_reorder(&self, schema: &CalcSchema, to: u64, sort: &SortSpec, records: &[u64]) -> bool {
        let Some(table) = schema.table_of(to) else {
            return true;
        };
        let plain = sort.fields.iter().all(|entry| {
            entry.summary.is_none()
                && entry.value_list.is_none()
                && entry
                    .field
                    .occurrence
                    .is_none_or(|occurrence| occurrence == to)
                && table.fields.get(&entry.field.field).is_some_and(|field| {
                    !field.is_unstored_calculation() && field.class != super::FieldClass::Summary
                })
        });
        !plain
            || self
                .overlay
                .keys()
                .any(|(t, r)| *t == table.id && records.contains(r))
    }

    fn cached<T>(
        &self,
        need: CalcNeed,
        get: impl Fn(&CalcData) -> Option<T>,
    ) -> Result<T, CalcError> {
        if let Some(v) = get(&self.cache.borrow()) {
            return Ok(v);
        }
        if let Some(fetch) = self.fetch {
            if let Some(data) = fetch(std::slice::from_ref(&need)) {
                self.cache.borrow_mut().merge(data);
                if let Some(v) = get(&self.cache.borrow()) {
                    return Ok(v);
                }
            }
        }
        self.lack(need);
        Err(CalcError::from_code(1224).expect("nonzero"))
    }
}

impl CalcSource for CachedSource<'_> {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        if let Some(values) = self.overlay.get(&(table, record)) {
            return Ok(Some(Arc::clone(values)));
        }
        self.cached(CalcNeed::Record { table, record }, |d| {
            d.records.get(&(table, record)).cloned()
        })
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.cached(CalcNeed::Records { table }, |d| {
            d.tables.get(&table).cloned()
        })
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        self.cached(CalcNeed::Globals { table }, |d| {
            d.globals.get(&table).cloned()
        })
    }

    fn found_set(&self, occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        if let Some(records) = self.found.get(&occurrence) {
            return Ok(Arc::clone(records));
        }
        self.cached(CalcNeed::FoundSet { occurrence, table }, |d| {
            d.found.get(&occurrence).cloned()
        })
    }

    fn found_sort(&self, occurrence: u64) -> Option<Arc<[u64]>> {
        self.sorts
            .get(&occurrence)
            .cloned()
            .or_else(|| self.cache.borrow().sorts.get(&occurrence).cloned())
    }

    /// Fetch the records the cache lacks with one request; what is still
    /// lacking (all of them without a fetch) is listed as missing at once,
    /// for one request rather than one per pass.
    fn prefetch_records(&self, table: u64, records: &[u64]) {
        let needs: Vec<CalcNeed> = {
            let cache = self.cache.borrow();
            records
                .iter()
                .filter(|r| {
                    !self.overlay.contains_key(&(table, **r))
                        && !cache.records.contains_key(&(table, **r))
                })
                .map(|&record| CalcNeed::Record { table, record })
                .collect()
        };
        if needs.is_empty() {
            return;
        }
        if let Some(data) = self.fetch.and_then(|fetch| fetch(&needs)) {
            self.cache.borrow_mut().merge(data);
        }
        let cache = self.cache.borrow();
        for need in needs.into_iter().filter(|need| !cache.has(need)) {
            self.lack(need);
        }
    }

    fn source_match_values(
        &self,
        schema: &CalcSchema,
        occurrence: u64,
        record: u64,
        field: u64,
        cx: &Ctx,
    ) -> Result<Vec<Value>, CalcError> {
        super::source_match_values(
            self,
            schema,
            occurrence,
            record,
            field,
            cx,
            self.calculation_session,
        )
    }

    fn related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        self.related_over_overlays(schema, (from, record, to), cx, || {
            self.cached(CalcNeed::Related { from, record, to }, |d| {
                d.related.get(&(from, record, to)).cloned()
            })
        })
    }

    fn related_ids(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        self.related_over_overlays(schema, (from, record, to), cx, || {
            self.cached(CalcNeed::RelatedIds { from, record, to }, |d| {
                d.related_ids
                    .get(&(from, record, to))
                    .or_else(|| d.related.get(&(from, record, to)))
                    .cloned()
            })
        })
    }

    fn has_record(&self, table: u64, record: u64) -> bool {
        self.overlay.contains_key(&(table, record))
            || self.cache.borrow().records.contains_key(&(table, record))
    }

    /// Every record the cache lacks in one fetch; without a fetch they are
    /// all listed as missing at once, for one request.
    fn prefetch(&self, table: u64, ids: &[u64]) -> Result<(), CalcError> {
        self.prefetch_records(table, ids);
        if ids.iter().all(|id| self.has_record(table, *id)) {
            Ok(())
        } else {
            Err(CalcError::from_code(1224).expect("nonzero"))
        }
    }

    /// The host's answer, with the records being edited here: their
    /// values may hold `text` now.
    fn lookup(&self, table: u64, field: u64, text: &str) -> Result<Option<Arc<[u64]>>, CalcError> {
        let key: LookupKey = (table, field, Arc::from(text));
        let need = CalcNeed::Lookup {
            table,
            field,
            text: Arc::clone(&key.2),
        };
        let found = self.cached(need, |d| d.lookups.get(&key).cloned())?;
        let Some(found) = found else {
            return Ok(None);
        };
        let edited: Vec<u64> = self
            .overlay
            .keys()
            .filter(|(t, r)| *t == table && !found.contains(r))
            .map(|(_, r)| *r)
            .collect();
        if edited.is_empty() {
            return Ok(Some(found));
        }
        let mut ids: Vec<u64> = found.iter().copied().chain(edited).collect();
        ids.sort_unstable();
        Ok(Some(ids.into()))
    }

    fn library_member(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<MemberInfo>>, CalcError> {
        let key: MemberKey = (table, member.to_vec());
        let need = CalcNeed::Member {
            table,
            member: member.to_vec(),
        };
        self.cached(need, |d| d.library.members.get(&key).cloned())
    }

    fn library_data(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<StoredContainer>>, CalcError> {
        let key: MemberKey = (table, member.to_vec());
        let need = CalcNeed::MemberData {
            table,
            member: member.to_vec(),
        };
        let read = self.cached(need, |d| d.library.data.get(&key).cloned());
        if matches!(&read, Ok(Some(_))) {
            self.library_reads.borrow_mut().insert(key);
        }
        read
    }

    fn pending(&self) -> Pending {
        self.pending.get()
    }

    fn need_occurrence(&self, occurrence: u64) {
        self.need(CalcNeed::Occurrence { occurrence });
    }

    fn need_layout(&self, layout: u32) {
        self.need(CalcNeed::Layout { layout });
    }
}

/// Records held in memory: tests and the sample provider.
#[derive(Clone, Debug, Default)]
pub struct MemorySource {
    /// Each table's records in creation order.
    tables: HashMap<u64, Vec<(u64, Arc<RecordValues>)>>,
    /// Where each record is in its table's list, by table and record: a
    /// read finds it at once, also among many thousands.
    positions: HashMap<(u64, u64), usize>,
    globals: HashMap<u64, Arc<RecordValues>>,
    /// Found sets by occurrence, with their sort fields.
    found: HashMap<u64, SortedFound>,
}

/// A found set in order and the fields it is sorted by.
type SortedFound = (Arc<[u64]>, Option<Arc<[u64]>>);

impl MemorySource {
    pub fn new() -> MemorySource {
        MemorySource::default()
    }

    /// Append (or replace) a record of `table`.
    pub fn put_record(&mut self, table: u64, record: u64, values: RecordValues) {
        let rows = self.tables.entry(table).or_default();
        match self.positions.get(&(table, record)) {
            Some(&at) => rows[at].1 = Arc::new(values),
            None => {
                self.positions.insert((table, record), rows.len());
                rows.push((record, Arc::new(values)));
            }
        }
    }

    pub fn remove_record(&mut self, table: u64, record: u64) {
        if let Some(rows) = self.tables.get_mut(&table) {
            rows.retain(|(id, _)| *id != record);
            self.positions.remove(&(table, record));
            for (at, (id, _)) in rows.iter().enumerate() {
                self.positions.insert((table, *id), at);
            }
        }
    }

    pub fn set_globals(&mut self, table: u64, values: RecordValues) {
        self.globals.insert(table, Arc::new(values));
    }

    /// The found set of `occurrence`, in order, sorted by `sort` (field
    /// ids) or unsorted.
    pub fn set_found_set(&mut self, occurrence: u64, records: &[u64], sort: Option<&[u64]>) {
        self.found
            .insert(occurrence, (records.into(), sort.map(Arc::from)));
    }
}

impl CalcSource for MemorySource {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        Ok(self
            .positions
            .get(&(table, record))
            .and_then(|&at| self.tables.get(&table)?.get(at))
            .map(|(_, values)| Arc::clone(values)))
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        Ok(self
            .tables
            .get(&table)
            .map(|rows| rows.iter().map(|(id, _)| *id).collect())
            .unwrap_or_else(|| Arc::from([])))
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        Ok(self.globals.get(&table).cloned().unwrap_or_default())
    }

    fn found_set(&self, occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        match self.found.get(&occurrence) {
            Some((records, _)) => Ok(Arc::clone(records)),
            None => self.records(table),
        }
    }

    fn found_sort(&self, occurrence: u64) -> Option<Arc<[u64]>> {
        self.found.get(&occurrence)?.1.clone()
    }
}
