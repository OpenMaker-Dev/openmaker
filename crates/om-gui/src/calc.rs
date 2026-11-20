//! Calculations in the window: the open file's calculation schema, the data
//! its calculations have read, and the evaluation of layout calculations
//! (hide conditions, conditional formatting, tooltips, placeholders, labels,
//! merge variables) and of fields a record page does not hold (related
//! fields, other repetitions, unstored calculations with the session's
//! variables).
//!
//! Painting never waits for the provider: an evaluation that needs data the
//! cache lacks lists it ([`Calculations::take_wanted`]), the app asks the
//! worker for it and paints again when it arrives. Until then the object
//! keeps what the page shows, and an object with a hide condition is
//! withheld. Scripts wait instead (`script_host`), as FileMaker Pro does.
//!
//! A hide condition decides one of three things ([`Visibility`]): the
//! object shows, FileMaker's own result hides it, or OpenMaker cannot tell
//! because the condition read what this client lacks (data the provider
//! could not supply, unread privilege metadata, a function it does not
//! implement) or did not decode. Results that read the window's context
//! (`Get`) are evaluated again when that context changes.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use om_data::calc::eval::{Context, Variables};
use om_data::calc::{Ctx, Date, Value, ValueType};
use om_data::choice::{self, ChoiceError, Choices};
use om_data::host::layout::{self, ObjectState};
use om_data::host::{
    stored_value, CachedSource, CalcCancel, CalcData, CalcNeed, CalcSchema, CalcSource as _,
    FieldClass, FileCalc, FileHost, HostReads, MemberKey, RecordValues, SessionInfo, SummaryCache,
    WindowMode,
};
use om_data::DataError;
use om_layout::{FieldSpec, LayoutObject};

use crate::document::DocumentState;
use crate::session::{RenderState, Session};

/// A passive portal row. Its layout record stays the calculation context;
/// fields in the portal occurrence read this related record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PortalRow {
    pub occurrence: u64,
    pub record: u64,
    pub number: usize,
}

/// The related record a field object edits in Browse
/// ([`Evaluator::related_record`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelatedRecord {
    /// The occurrence it is reached through, its base table, the record
    /// and the field.
    pub occurrence: u64,
    pub table: u64,
    pub record: u64,
    pub field: u64,
    /// The stored cell key, including the repetition.
    pub cell: u64,
    /// `Occurrence::Field`, the editor's name.
    pub name: String,
    /// The field's stored text and the record's modification count, as
    /// last read.
    pub stored: String,
    pub version: Option<u64>,
}

type ObjectKey = (u64, u64, u64, Option<PortalRow>);

/// A field's list as a hosted file's host lists it: asked for when a control
/// first needs it, and kept only while a control shows it, so each opening
/// asks the host again, as FileMaker Pro does (#464). One the host could
/// not give whole is read from the records instead.
enum HostList {
    Asked,
    Read(Result<Arc<Choices>, ChoiceError>),
    Scan,
}

/// The base table and field of a value list a hosted file's host can list
/// whole (query-program.md 8.7): all values of a stored text field, with no
/// second field and not only related values. Its text as the host sends it
/// is shown; for numbers and dates that text is unmeasured, so they, like
/// the others, are read from the records.
fn host_list_source(schema: &CalcSchema, list: &om_data::meta::ValueList) -> Option<(u64, u64)> {
    use om_data::meta::ValueSource;
    let ValueSource::Field {
        primary,
        secondary: None,
        start: None,
    } = &list.source
    else {
        return None;
    };
    let table = schema.table_of(primary.occurrence?)?;
    let field = table.fields.get(&primary.field)?;
    (field.value_type == ValueType::Text && field.stored && !field.global)
        .then_some((table.id, field.id))
}

/// A cached value list that does not depend on the record: the tables
/// its items are read from, and the items or why there are none.
type SharedList = (BTreeSet<u64>, Result<Arc<Choices>, ChoiceError>);

/// A value list to build on the worker (`list_build`, #745), as the
/// window's paint would: its ticket, the window's date and calculation
/// session, and for a list of related values the record it starts from.
#[derive(Clone, Debug)]
pub(crate) struct ListBuild {
    pub list: u64,
    pub ticket: u64,
    pub today: Date,
    pub info: SessionInfo,
    pub related: Option<RelatedBuild>,
}

/// What a list of related values is built for (#290): the record in the
/// layout's occurrence, the portal row it shows in, and the window's
/// uncommitted values as paint reads them (`Evaluator`'s overlays).
#[derive(Clone, Debug)]
pub(crate) struct RelatedBuild {
    pub occurrence: u64,
    pub parent: u64,
    pub portal: Option<(u64, u64)>,
    pub overlays: Vec<(u64, u64, Arc<RecordValues>)>,
}

/// A list of related values for one record: (list, layout occurrence,
/// record, portal row's occurrence and record).
type RelatedKey = (u64, u64, u64, Option<(u64, u64)>);

/// A list of related values the worker built: whether a calculation
/// computed it ([`calculated_list`]), and its items or why there are none.
type RelatedList = (bool, Result<Arc<Choices>, ChoiceError>);

/// A list of related values asked of the worker: its key, whether a
/// calculation computes it ([`calculated_list`]), and whether the window's
/// unsaved changes, context or date changed since it was asked, so that its
/// answer is not taken.
#[derive(Clone, Copy, Debug)]
struct RelatedAsk {
    key: RelatedKey,
    calculated: bool,
    stale: bool,
}

/// The tables a value list that does not depend on the record reads: none
/// for custom values (and for another file's list, which is refused), its
/// fields' table for all of a field's values, and every table for all of
/// an unstored calculation's values, whose calculation may read any of
/// them (#1058). `None` for a list of related values, which depends on the
/// record: it is built per object and record.
fn shared_list_tables(
    schema: &CalcSchema,
    list: &om_data::meta::ValueList,
) -> Option<BTreeSet<u64>> {
    use om_data::meta::ValueSource;
    match &list.source {
        ValueSource::Field { start: Some(_), .. } => None,
        // Each record's value is computed in the record's own context: none
        // of the window's records decides it (#290).
        ValueSource::Field { .. } if calculated_list(schema, list) => {
            Some(schema.tables().map(|table| table.id).collect())
        }
        ValueSource::Field {
            primary, secondary, ..
        } => Some(
            [Some(primary), secondary.as_ref()]
                .into_iter()
                .flatten()
                .filter_map(|field| schema.table_of(field.occurrence?))
                .map(|table| table.id)
                .collect(),
        ),
        _ => Some(BTreeSet::new()),
    }
}

/// Whether `list` is of a field's values whose first or second field is
/// an unstored calculation (#1058): its items are computed on each record,
/// and may read other tables and the session.
fn calculated_list(schema: &CalcSchema, list: &om_data::meta::ValueList) -> bool {
    use om_data::meta::ValueSource;
    let ValueSource::Field {
        primary, secondary, ..
    } = &list.source
    else {
        return false;
    };
    [Some(primary), secondary.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|field| schema.field(field.occurrence?, field.field))
        .any(|def| def.is_unstored_calculation())
}

/// The record id a portal row of a begun record (`RelatedRow::New` id
/// `id`) reads through: calculations see its uncommitted values there
/// (`Evaluator`'s overlays). No stored record has it: it is at or above
/// [`om_data::host::UNSAVED_FLOOR`].
pub(crate) fn begun_record(id: u64) -> u64 {
    om_data::host::NO_RECORD - 1 - (id % ((1 << 32) - 1))
}
/// (layout, summary field, record, break occurrence, break field).
type GroupKey = (u64, u64, u64, u64, u64);

/// Whether a layout object shows, as its hide condition decides
/// (layout-model.md 5.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Visibility {
    #[default]
    Shown,
    /// The condition holds: FileMaker's own result on what it read, a `?`
    /// result included (calc-values.md, layout calculations).
    Hidden,
    /// OpenMaker cannot tell whether FileMaker hides the object: the
    /// condition did not decode, the window has no calculations, or it read
    /// data the provider could not supply or an answer this client does not
    /// have (`HostReads::unknown`). Not a FileMaker state: Browse shows a
    /// disabled stand-in rather than guessing either way.
    Unresolved,
}

/// What a layout object's calculations give on one record.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ObjectCalcState {
    pub visibility: Visibility,
    /// The other calculations of a shown object; default otherwise.
    pub state: ObjectState,
    /// A shown button bar's active segment calculation (layout-model.md
    /// 5.5); `None` without one.
    pub active_segment: Option<SegmentName>,
    /// Drawn before the last change and kept while its data is read again
    /// ([`Evaluator::object_state_in_portal`]): what decides the object's
    /// clicks and focus is not known yet.
    pub kept: bool,
}

/// What a button bar's active segment calculation gave: the object name of
/// the segment to make active, or nothing OpenMaker can take for
/// FileMaker's (it read what this window lacks, as an unresolved hide
/// condition does).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SegmentName {
    Name(String),
    Unresolved,
}

#[derive(Clone)]
struct CachedObjectState {
    calc: ObjectCalcState,
    visibility_stale: bool,
    /// Its calculations read the window's context: a change there expires
    /// it ([`Calculations::observe_context`]).
    session: bool,
}
/// A cached result, with whether it read the session
/// ([`Calculations::observe_context`]).
type SessionResult<T> = (T, bool);

/// A merge field's value, by (layout, record, portal row, the field as
/// the text names it).
type MergeKey = (u64, u64, Option<PortalRow>, String);

/// The results a change dropped ([`Calculations::keep_last_results`]),
/// by the keys of the caches they came from.
#[derive(Default)]
struct LastResults {
    states: HashMap<ObjectKey, CachedObjectState>,
    values: HashMap<(ObjectKey, i32), SessionResult<Value>>,
    named_values: HashMap<(u64, u64, u64), SessionResult<Value>>,
    merge_values: HashMap<MergeKey, SessionResult<Value>>,
    group_values: HashMap<GroupKey, SessionResult<Value>>,
}

impl LastResults {
    fn len(&self) -> usize {
        self.states.len()
            + self.values.len()
            + self.named_values.len()
            + self.merge_values.len()
            + self.group_values.len()
    }
}

/// How many results [`LastResults`] holds at most before a change: a few
/// pages of a large layout's objects.
const MAX_KEPT_RESULTS: usize = 65_536;

/// The most calculated icon text the results keep, in bytes. Past it, the
/// results holding an icon are dropped, and computed again as they are
/// drawn: a List view scrolled over many records with icons of their own
/// keeps no more than this (OpenMaker's bound).
const ICON_BUDGET: usize = 16 << 20;

/// A portal's displayed related records, keyed by (layout, portal object,
/// parent record), with whether they read the session (its filter's or
/// sort's `Get` functions, [`Calculations::observe_context`]).
type PortalSets = HashMap<(u64, u64, u64), SessionResult<Arc<[u64]>>>;

/// Draft text is stable across paints even though its calculation overlay
/// is rebuilt. Keep its context too: identical edits of different records
/// or occurrences must not share a content revision.
#[derive(PartialEq, Eq)]
struct DraftContext {
    layout: Option<u32>,
    occurrence: u64,
    record: Option<i64>,
    changes: om_data::FieldChanges,
    /// The edits of related records made through the record.
    related: Vec<crate::document::RelatedEdit>,
    /// What the record keeps uncommitted of its related records.
    drafts: crate::document::RelatedDrafts,
    begun: crate::document::BegunRows,
}

/// The window's calculation state for the open file.
#[derive(Default)]
pub(crate) struct Calculations {
    schema: Option<Arc<CalcSchema>>,
    /// The provider gave no calculation schema (it failed, or has none),
    /// unlike one still being read: layout calculations cannot run.
    no_schema: bool,
    /// The window context the cached results were evaluated in.
    context: RefCell<Option<SessionInfo>>,
    /// The date observed on the previous frame, even when the session
    /// reads the live clock rather than storing a fixed timestamp.
    context_date: Cell<Option<Date>>,
    /// Local data/context changes, including replies that retain completed results.
    content_revision: Cell<u64>,
    draft_context: RefCell<Option<DraftContext>>,
    pub cache: RefCell<CalcData>,
    /// Data found missing since the last request.
    wanted: RefCell<BTreeSet<CalcNeed>>,
    /// Asked for and not answered yet.
    requested: BTreeSet<CalcNeed>,
    /// The reads asked for in the window's current layout, view and found
    /// set ([`Calculations::reads_for`]): cancelled when that changes, so
    /// the provider stops reading what the window no longer shows (#987).
    reads: CalcCancel,
    /// What [`Calculations::reads`] were asked for.
    reads_context: Option<u64>,
    /// The provider could not supply these; not asked again until the data
    /// changes.
    failed: BTreeSet<CalcNeed>,
    /// Needs the provider left unanswered while a related file's host
    /// could not be reached (#740): not failed, asked again once
    /// [`Self::retry_at`] passes.
    waiting: BTreeSet<CalcNeed>,
    /// When [`Self::waiting`] is asked again.
    retry_at: Option<std::time::Instant>,
    /// Bodies already read by pending evaluations, retained across their
    /// subsequent branch downloads even over the soft byte budget.
    library_pins: RefCell<BTreeSet<MemberKey>>,
    /// Tables the user was told the provider could not read. Each is told
    /// once for the open file, though it fails again whenever its needs
    /// are asked for: the file's windows share the set
    /// ([`Self::share_reports`]), each with its own cache.
    reported: Rc<RefCell<BTreeSet<u64>>>,
    /// Related-file warnings already shown by any window of this file.
    related_files: Rc<RefCell<BTreeSet<String>>>,
    /// Results by (layout, object, record) since the data, the variables or
    /// the record being edited last changed.
    states: RefCell<HashMap<ObjectKey, CachedObjectState>>,
    /// The calculated icons' text the results above and the kept ones
    /// ([`LastResults`]) took, in bytes, as counted when each was stored
    /// ([`ICON_BUDGET`]).
    icon_bytes: std::cell::Cell<usize>,
    /// Field values, with whether they read the session.
    values: RefCell<HashMap<(ObjectKey, i32), SessionResult<Value>>>,
    /// Table and fallback views have field columns rather than object ids.
    /// Keep their (layout, field, record) keys separate from object values.
    named_values: RefCell<HashMap<(u64, u64, u64), SessionResult<Value>>>,
    /// Merge fields' values in layout text.
    merge_values: RefCell<HashMap<MergeKey, SessionResult<Value>>>,
    /// Complete displayed sets, by layout, portal object and parent record.
    portal_sets: RefCell<PortalSets>,
    /// Summary values of List view's sub-summary bands, by (layout,
    /// summary field, record, break occurrence, break field): the group's
    /// value, kept apart from `values`, where the same object holds the
    /// found set's value.
    group_values: RefCell<HashMap<GroupKey, SessionResult<Value>>>,
    /// The results the last changes dropped, drawn in Browse in
    /// place of one whose data is being read again: a related field, a
    /// merge field, a summary, a calculation or an object's hide condition
    /// and formats keeps its last value after a save rather than drawing
    /// empty until its data arrives (a portal keeps its rows in the
    /// document). They go once a frame drew the window with nothing
    /// outstanding ([`Self::settle_last_results`]). Reports never read
    /// them.
    last: RefCell<LastResults>,
    /// The layout calculations' last `Get ( ModifiedFields )`, with the
    /// schema and inputs it was computed from ([`Self::modified_fields`]).
    modified: RefCell<Option<ModifiedAnswer>>,
    /// Lists of related values (#290), as the worker built them for a
    /// record and portal row with the window's unsaved changes, with
    /// whether a calculation computed them ([`calculated_list`]): never
    /// built in paint. A relationship can lead through any table, so they
    /// go whenever data may have changed; they also go when the window's
    /// unsaved changes do, and a calculated one when the window's context
    /// or the date does.
    related_built: RefCell<HashMap<RelatedKey, RelatedList>>,
    /// The lists of related values asked of the worker and not answered
    /// yet, by ticket: an answer whose ticket is no longer here, or which
    /// is stale, is not taken.
    related_building: RefCell<HashMap<u64, RelatedAsk>>,
    /// The items, or why there are none, of value lists that do not depend
    /// on the record (custom values, or all of a field's values), by list,
    /// with the tables they read: built once from committed data for
    /// every object, record and portal row, and kept until the data of
    /// one of those tables changes ([`SharedList`]).
    lists: RefCell<HashMap<u64, SharedList>>,
    /// A hosted file's field lists as the host lists them
    /// (`DataProvider::host_field_values`), by value list, with whether a
    /// control showed the list this frame ([`HostList`]).
    host_lists: RefCell<HashMap<u64, (HostList, bool)>>,
    /// The lists a control asked the host for this frame: (list, table,
    /// field).
    list_wants: RefCell<Vec<(u64, u64, u64)>>,
    /// Advanced whenever the data may have changed, which drops every host
    /// list: a reply to a request from before is not taken.
    host_epoch: u64,
    /// Value lists of a field's values that do not depend on the record,
    /// as the worker built them (`list_build`, #745), with the tables they
    /// read: never built in paint. They go as [`Self::lists`] does when the
    /// data of a table they read may have changed, but not because a read
    /// about that table was answered: they were built from the provider,
    /// not from this cache, and dropping them then would build them again
    /// while the window reads the same table.
    built: RefCell<HashMap<u64, SharedList>>,
    /// The lists asked of the worker and not answered yet: the request's
    /// ticket and the tables the list reads. An answer whose ticket is no
    /// longer here (the data changed meanwhile) is not taken.
    building: RefCell<HashMap<u64, (u64, BTreeSet<u64>)>>,
    /// The builds to ask the worker for this frame.
    build_wants: RefCell<Vec<ListBuild>>,
    /// Lists of an unstored calculation's values whose build was asked
    /// before the window's context or date changed (#290): its answer is
    /// not taken, and one build for the new context follows it, so that a
    /// context changing frame by frame (a resize) keeps one build at a
    /// time on the worker.
    stale_builds: RefCell<BTreeSet<u64>>,
    /// Lists whose build lacked data a related file's unreachable host
    /// holds (#740): not asked for again before then, whatever grows the
    /// schema meanwhile (another list's build, say), with whether the
    /// window was told it came ([`Self::retry_at`]).
    build_retry: RefCell<HashMap<u64, (std::time::Instant, bool)>>,
    /// The last build ticket given, kept through [`Self::reset`] so that an
    /// answer for the file before is never taken for the next.
    build_tickets: Cell<u64>,
    /// Targeted downloads retain complete display results. Pending evaluations
    /// never cache a result, so later inputs cannot make those results partial.
    targeted: Cell<bool>,
    /// Stored fields explicitly refreshed read fresh calculation data instead
    /// of the older record page, until a page/data change replaces it.
    field_overrides: BTreeSet<(u64, u64)>,
    /// `$` variables outside a script (script level 0).
    pub level0: Variables,
    /// The account the file was opened with.
    pub account: String,
    /// The host's product and version (`Get ( HostApplicationVersion )`).
    pub host_version: String,
    pub privileges: Option<om_data::host::Privileges>,
}

impl Calculations {
    pub fn schema(&self) -> Option<&Arc<CalcSchema>> {
        self.schema.as_ref()
    }

    /// A new file (or none): drop everything.
    pub fn reset(&mut self) {
        self.reads.cancel();
        let revision = self.content_revision.get().wrapping_add(1);
        let epoch = self.host_epoch.wrapping_add(1);
        let tickets = self.build_tickets.get();
        *self = Calculations::default();
        self.content_revision.set(revision);
        self.host_epoch = epoch;
        self.build_tickets.set(tickets);
    }

    /// The data of `tables` (of every table with `None`) may have changed:
    /// the lists the worker built from them go, and one still being built
    /// is asked again when a control shows it (#745).
    fn drop_built_lists(&mut self, tables: Option<&BTreeSet<u64>>) {
        let stale = |read: &BTreeSet<u64>| tables.is_none_or(|t| !read.is_disjoint(t));
        self.built.get_mut().retain(|_, (read, _)| !stale(read));
        let building = self.building.get_mut();
        building.retain(|_, (_, read)| !stale(read));
        // A list of related values may read any table along its
        // relationships: any change of data takes it.
        self.related_built.get_mut().clear();
        let related = self.related_building.get_mut();
        related.clear();
        self.build_wants
            .get_mut()
            .retain(|want| match want.related {
                Some(_) => related.contains_key(&want.ticket),
                None => building
                    .get(&want.list)
                    .is_some_and(|(t, _)| *t == want.ticket),
            });
        self.stale_builds
            .get_mut()
            .retain(|list| building.contains_key(list));
    }

    /// Lists of an unstored calculation's values (#290), whose calculation
    /// may read the window's context and the date, after one of them
    /// changed: a built one goes, with a wait for an unreachable host its
    /// build asked for (the new context may read other data); one being
    /// built keeps its request, its answer not taken ([`Self::stale_builds`]).
    fn drop_calculated_lists(&self) -> bool {
        // Lists of related values a calculation computes: built ones go, one
        // being built is not taken (`RelatedAsk::stale`), as for all of a
        // field's values.
        let dropped = self.drop_related_lists(|ask| ask.calculated);
        let Some(schema) = &self.schema else {
            return dropped;
        };
        let calculated = |id: &u64| {
            schema
                .value_lists()
                .iter()
                .find(|list| list.id == *id)
                .is_some_and(|list| calculated_list(schema, list))
        };
        self.built.borrow_mut().retain(|id, _| !calculated(id));
        self.build_retry
            .borrow_mut()
            .retain(|id, _| !calculated(id));
        self.stale_builds
            .borrow_mut()
            .extend(self.building.borrow().keys().filter(|id| calculated(id)));
        dropped
    }

    /// Lists of related values `which` says went out of date: the built
    /// ones go, and one being built stays asked, its answer not taken, so
    /// that a change frame by frame (typing, a resize) keeps one build per
    /// list and record on the worker. Whether a built one went.
    fn drop_related_lists(&self, which: impl Fn(&RelatedAsk) -> bool) -> bool {
        let mut built = self.related_built.borrow_mut();
        let before = built.len();
        built.retain(|key, (calculated, _)| {
            !which(&RelatedAsk {
                key: *key,
                calculated: *calculated,
                stale: false,
            })
        });
        for ask in self.related_building.borrow_mut().values_mut() {
            if which(ask) {
                ask.stale = true;
            }
        }
        built.len() != before
    }

    /// The data may have changed: the host's lists are asked for again.
    fn drop_host_lists(&mut self) {
        self.host_lists.get_mut().clear();
        self.list_wants.get_mut().clear();
        self.host_epoch = self.host_epoch.wrapping_add(1);
    }

    /// Another window of the same file: a table `file` told the user it
    /// could not read is not told again here.
    pub fn share_reports(&mut self, file: &Calculations) {
        self.reported = Rc::clone(&file.reported);
        self.related_files = Rc::clone(&file.related_files);
    }

    /// The file's calculation schema, or `None` while it is being read.
    pub fn set_schema(&mut self, schema: Option<Arc<CalcSchema>>) {
        self.schema = schema;
        self.no_schema = false;
        self.data_changed();
    }

    /// The file's schema grew: related files opened on demand (#983). No
    /// id changed, so what was read stays; results are computed again, and
    /// what failed for want of the new files is asked for again.
    pub fn grow_schema(&mut self, schema: Option<Arc<CalcSchema>>) {
        self.schema = schema;
        self.no_schema = false;
        self.failed.clear();
        self.waiting.clear();
        self.retry_at = None;
        self.lists.get_mut().clear();
        self.drop_built_lists(None);
        self.drop_host_lists();
        self.forget_results();
    }

    /// The provider could not give a calculation schema, or has none.
    pub fn schema_unavailable(&mut self) {
        self.set_schema(None);
        self.no_schema = true;
    }

    /// Another window of the same file: its schema, read or not.
    pub fn share_schema(&mut self, file: &Calculations) {
        self.set_schema(file.schema.clone());
        self.no_schema = file.no_schema;
    }

    /// Whether layout calculations cannot run in this window, as opposed to
    /// waiting for the schema.
    pub fn unavailable(&self) -> bool {
        self.schema.is_none() && self.no_schema
    }

    /// Records were read or written: what was cached may be stale.
    pub fn data_changed(&mut self) {
        *self.cache.get_mut() = CalcData::default();
        self.lists.get_mut().clear();
        self.drop_built_lists(None);
        self.drop_host_lists();
        self.library_pins.get_mut().clear();
        self.failed.clear();
        self.field_overrides.clear();
        self.forget_results();
    }

    /// A page of `table`'s `records` was read; `generation` is the
    /// provider's calculation data generation after it
    /// (`DataProvider::calc_generation`). The page brings its records and
    /// the window's found set fresh, so those are dropped. What else the
    /// calculations read stays when the provider read the page under the
    /// generation this data was read under (#1042): the provider has then
    /// seen no change notified by the host or shown by the page's records.
    /// Otherwise, or when the page's table is unknown, everything goes, as
    /// on a data change. Results are evaluated again either way: they are
    /// for records, a page and a found set that may have changed.
    pub fn page_read(&mut self, generation: Option<u64>, table: Option<u64>, records: &[u64]) {
        let cache = self.cache.get_mut();
        let Some(table) = table.filter(|_| generation.is_some() && cache.generation == generation)
        else {
            self.data_changed();
            return;
        };
        cache
            .records
            .retain(|(t, record), _| *t != table || !records.contains(record));
        cache.found.clear();
        cache.sorts.clear();
        self.lists
            .get_mut()
            .retain(|_, (read, _)| !read.contains(&table));
        self.drop_built_lists(Some(&BTreeSet::from([table])));
        self.drop_host_lists();
        self.library_pins.get_mut().clear();
        self.failed.clear();
        self.field_overrides.clear();
        self.forget_results();
    }

    /// The results evaluated so far become the ones drawn while their
    /// data is read again ([`Self::last`]), over those kept before. Every
    /// change that has them evaluated again keeps them: a data change, a
    /// data reply, a draft (a save first drops the record's draft, then
    /// its data), variables and the mode.
    /// Count `bytes` of calculated icon text a result keeps: past
    /// [`ICON_BUDGET`], the results holding an icon go first.
    fn keep_icon(&self, bytes: usize) {
        let held = self.icon_bytes.get().saturating_add(bytes);
        if held <= ICON_BUDGET {
            self.icon_bytes.set(held);
            return;
        }
        let iconless = |cached: &CachedObjectState| cached.calc.state.icon_svg.is_none();
        self.states
            .borrow_mut()
            .retain(|_, cached| iconless(cached));
        self.last
            .borrow_mut()
            .states
            .retain(|_, cached| iconless(cached));
        self.icon_bytes.set(bytes);
    }

    fn keep_last_results(&self) {
        let mut last = self.last.borrow_mut();
        // Bounded however long a read stays outstanding: past the bound
        // only the results drawn since the previous change are kept.
        if last.len() > MAX_KEPT_RESULTS {
            *last = LastResults::default();
        }
        last.states.extend(self.states.borrow_mut().drain());
        last.values.extend(self.values.borrow_mut().drain());
        last.named_values
            .extend(self.named_values.borrow_mut().drain());
        last.merge_values
            .extend(self.merge_values.borrow_mut().drain());
        last.group_values
            .extend(self.group_values.borrow_mut().drain());
    }

    /// A frame was drawn and its needs were asked for
    /// ([`Self::take_wanted`]): when none was and nothing is on its way,
    /// every result it drew is fresh, and those kept from before the last
    /// change go.
    pub fn settle_last_results(&mut self) {
        if !self.awaiting_data() {
            *self.last.get_mut() = LastResults::default();
        }
    }

    /// Variables, the mode or a draft changed: evaluate again.
    pub fn forget_results(&self) {
        self.targeted.set(false);
        self.advance_content();
        self.clear_results();
    }

    pub fn retains_targeted_results(&self) -> bool {
        self.targeted.get()
    }

    pub fn refresh_object(&mut self, refresh: &crate::scripting::ObjectRefresh, session: &Session) {
        if session.layout != Some(refresh.layout) {
            return;
        }
        let RenderState::Loaded(render) = &session.render else {
            return;
        };
        let layout = u64::from(refresh.layout);
        let mut states = BTreeSet::new();
        let mut values = BTreeSet::new();
        self.cache.get_mut().refresh_object(refresh.join);
        self.lists.get_mut().clear();
        self.drop_built_lists(None);
        self.drop_host_lists();
        self.cache.get_mut().generation = refresh.generation;
        self.failed.clear();
        self.library_pins.get_mut().clear();
        for &id in &refresh.objects {
            let Some(object) = render.layout.objects.get(id) else {
                continue;
            };
            states.insert((layout, id));
            if object.field_spec().is_none_or(|spec| {
                refresh.repetition == 0 || spec.repetition.max(1) as u32 == refresh.repetition
            }) {
                values.insert((layout, id));
                if object.field_spec().is_some() {
                    self.field_overrides.insert((layout, id));
                }
            }
            self.portal_sets
                .get_mut()
                .retain(|(l, o, _), _| (*l, *o) != (layout, id));
        }
        self.targeted.set(true);
        self.advance_content();
        self.states.get_mut().retain(|(l, o, _, _), cached| {
            if !states.contains(&(*l, *o)) {
                true
            } else if values.contains(&(*l, *o)) {
                false
            } else {
                cached.visibility_stale = true;
                true
            }
        });
        // Its list of related values, if any, is built again: every one,
        // as the objects' lists are not known here.
        if !values.is_empty() {
            self.drop_related_lists(|_| true);
        }
        self.values
            .get_mut()
            .retain(|((l, o, _, _), _), _| !values.contains(&(*l, *o)));
        // Merge fields are kept by name, not by object: all are read again,
        // as they were before they were kept at all.
        self.merge_values.get_mut().clear();
        // A refreshed object in a sub-summary band reads its summaries'
        // groups again, from the fresh data.
        if let (Some(schema), Some(origin)) = (self.schema.as_deref(), layout_occurrence(session)) {
            let fields: BTreeSet<u64> = values
                .iter()
                .filter_map(|(_, id)| render.layout.objects.get(*id))
                .flat_map(|object| crate::report::shown_fields(object, schema, origin))
                .map(|field| field.field)
                .collect();
            self.group_values
                .get_mut()
                .retain(|(l, field, ..), _| *l != layout || !fields.contains(field));
        }
    }

    fn advance_content(&self) {
        self.content_revision
            .set(self.content_revision.get().wrapping_add(1));
    }

    /// The window's context as `Get` reads it: a change expires the results
    /// whose calculations read it. Others keep, as do field values (their
    /// refresh follows record and data changes).
    fn observe_context(&self, info: &SessionInfo) {
        let date = info.evaluation_date();
        let date_changed = self
            .context_date
            .replace(Some(date))
            .is_some_and(|previous| previous != date);
        if date_changed {
            // Date-dependent fields, labels and hide conditions all need
            // fresh results; the account deadline itself need not change.
            self.forget_results();
            self.drop_calculated_lists();
        }
        let mut previous = self.context.borrow_mut();
        if previous
            .as_ref()
            .is_some_and(|previous| previous.same_window_context(info))
        {
            return;
        }
        *previous = Some(info.clone());
        let mut states = self.states.borrow_mut();
        let count = states.len();
        states.retain(|_, cached| !cached.session);
        let mut dropped = false;
        {
            let mut values = self.values.borrow_mut();
            let before = values.len();
            values.retain(|_, (_, session)| !*session);
            dropped |= values.len() != before;
            let mut named = self.named_values.borrow_mut();
            let before = named.len();
            named.retain(|_, (_, session)| !*session);
            dropped |= named.len() != before;
            let mut merged = self.merge_values.borrow_mut();
            let before = merged.len();
            merged.retain(|_, (_, session)| !*session);
            dropped |= merged.len() != before;
            let mut sets = self.portal_sets.borrow_mut();
            let before = sets.len();
            sets.retain(|_, (_, session)| !*session);
            dropped |= sets.len() != before;
            let mut groups = self.group_values.borrow_mut();
            let before = groups.len();
            groups.retain(|_, (_, session)| !*session);
            dropped |= groups.len() != before;
        }
        // A list an unstored calculation computed may have read the
        // session as well (#1058), built on the worker (#290).
        dropped |= self.drop_calculated_lists();
        if dropped || states.len() != count {
            self.advance_content();
        }
    }

    /// The window's unsaved changes as calculations see them: every result
    /// goes when they change, since an edited record may be any result's
    /// related record or key. Between their changes the results keep from
    /// frame to frame, as without a draft: the overlays a frame builds
    /// from the same changes and the same cached data are the same, and a
    /// data reply or a new date drops the results anyway (#1308).
    fn observe_draft(&self, draft: Option<DraftContext>) {
        let mut previous = self.draft_context.borrow_mut();
        if *previous != draft {
            *previous = draft;
            // A list of related values was built over the unsaved changes.
            self.drop_related_lists(|_| true);
            self.forget_results();
        }
    }

    fn clear_results(&self) {
        self.keep_last_results();
        self.states.borrow_mut().clear();
        self.values.borrow_mut().clear();
        self.named_values.borrow_mut().clear();
        self.merge_values.borrow_mut().clear();
        self.portal_sets.borrow_mut().clear();
        self.group_values.borrow_mut().clear();
    }

    /// The provider's answer to `needs`, and what to tell the user about
    /// it: why it failed, or why tables not reported yet could not be read.
    pub fn accept_data(
        &mut self,
        needs: &[CalcNeed],
        result: Result<CalcData, DataError>,
    ) -> Option<String> {
        // A read the window stopped (#987): nothing to apply or report, and
        // its needs are asked again whenever the window still wants them.
        if matches!(&result, Ok(data) if data.superseded) {
            for need in needs {
                self.requested.remove(need);
            }
            return None;
        }
        self.advance_content();
        for need in needs {
            self.requested.remove(need);
        }
        let mut refreshed = false;
        let report = match result {
            Ok(mut data) => {
                // An answer queued before the step cannot restore its old
                // inputs. Remove requested above so missing data can retry.
                if matches!((self.cache.get_mut().generation, data.generation),
                    (Some(current), Some(incoming)) if incoming < current)
                {
                    return None;
                }
                let failures = std::mem::take(&mut data.failures);
                let retry = data.retry_after.take();
                let cache = self.cache.get_mut();
                refreshed = data.generation.is_some() && cache.generation != data.generation;
                if refreshed {
                    self.failed.clear();
                    self.library_pins.get_mut().clear();
                }
                cache.merge_with_members(data, self.library_pins.get_mut());
                for need in needs {
                    if !cache.has(need) {
                        // Waiting for a host that could not be reached:
                        // asked again then, not failed (#740).
                        if retry.is_some() {
                            self.waiting.insert(need.clone());
                        } else {
                            self.failed.insert(need.clone());
                        }
                    }
                }
                if let Some(after) = retry.filter(|_| !self.waiting.is_empty()) {
                    let at = std::time::Instant::now() + after;
                    self.retry_at = Some(self.retry_at.map_or(at, |soon| soon.min(at)));
                }
                self.report(failures)
            }
            Err(error) => {
                self.failed.extend(needs.iter().cloned());
                Some(format!("Calculations could not read their data: {error}"))
            }
        };
        // Complete results remain valid when only missing library data
        // arrives. Keeping them lets a frame finish once, even when later
        // objects evict its large stream bodies from the bounded cache.
        // Record/context replies still invalidate every dependent result.
        if refreshed {
            self.lists.get_mut().clear();
            self.drop_built_lists(None);
            self.drop_host_lists();
        } else {
            // A list keeps through replies about tables it does not read.
            let tables: BTreeSet<u64> = needs
                .iter()
                .filter_map(|need| self.need_table(need))
                .collect();
            self.lists
                .get_mut()
                .retain(|_, (read, _)| read.is_disjoint(&tables));
        }
        if refreshed {
            self.forget_results();
        } else if self.targeted.get() {
            // Complete results belong to the same refreshed input generation.
            // Only the explicitly invalidated objects need to evaluate again.
        } else if !needs
            .iter()
            .all(|need| matches!(need, CalcNeed::Member { .. } | CalcNeed::MemberData { .. }))
        {
            self.clear_results();
        }
        report
    }

    /// The table whose records a reply to `need` brings, if any: a
    /// library member's data changes no record.
    fn need_table(&self, need: &CalcNeed) -> Option<u64> {
        let of = |occurrence: u64| {
            self.schema
                .as_deref()
                .and_then(|schema| schema.table_of(occurrence))
                .map(|table| table.id)
        };
        match need {
            CalcNeed::Record { table, .. }
            | CalcNeed::Records { table }
            | CalcNeed::FoundSet { table, .. }
            | CalcNeed::Globals { table }
            | CalcNeed::Lookup { table, .. } => Some(*table),
            CalcNeed::Related { to, .. } | CalcNeed::RelatedIds { to, .. } => of(*to),
            CalcNeed::Member { .. } | CalcNeed::MemberData { .. } => None,
            // A related file to open or a layout's member to read: a schema
            // answers it.
            CalcNeed::Occurrence { .. } | CalcNeed::Layout { .. } => None,
        }
    }

    /// One message for the related-file warnings not reported yet: the
    /// provider gives them with each calculation schema.
    pub fn report_related_files(&mut self, warnings: Vec<String>) -> Option<String> {
        let lines: Vec<String> = warnings
            .into_iter()
            .filter(|warning| self.related_files.borrow_mut().insert(warning.clone()))
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// One message for the tables of `failures` not reported yet.
    fn report(&mut self, failures: BTreeMap<u64, DataError>) -> Option<String> {
        let lines: Vec<String> = failures
            .into_iter()
            .filter(|(table, _)| self.reported.borrow_mut().insert(*table))
            .map(|(table, error)| {
                let name = match self.schema.as_ref().and_then(|s| s.table(table)) {
                    Some(def) => format!("“{}”", def.name),
                    None => format!("0x{table:x}"),
                };
                format!("Calculations could not read table {name}: {error}")
            })
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// The value lists to build on the worker now (`list_build`): never in
    /// paint (#745).
    pub fn take_list_builds(&mut self) -> Vec<ListBuild> {
        std::mem::take(self.build_wants.get_mut())
    }

    /// Builds [`Self::take_list_builds`] gave that were not sent (the
    /// connection to the host is gone): asked for again when a control
    /// next shows the list, and not waited for meanwhile.
    pub fn unsent_builds(&mut self, builds: &[ListBuild]) {
        let related = self.related_building.get_mut();
        for want in builds.iter().filter(|want| want.related.is_some()) {
            related.remove(&want.ticket);
        }
        let building = self.building.get_mut();
        for want in builds.iter().filter(|want| want.related.is_none()) {
            if building
                .get(&want.list)
                .is_some_and(|(asked, _)| *asked == want.ticket)
            {
                building.remove(&want.list);
            }
        }
    }

    /// The worker's answer for list `list`, asked with `ticket`: its items
    /// or why there are none, `None` when there is nothing to show yet
    /// (the window stopped the reads, #987, or a related host could not be
    /// reached), when it is asked again if still shown. An answer to a
    /// request the data changed since is not taken. `retry_after`: a
    /// related file's host could not be reached (#740), and the list is
    /// not asked for again before then, whichever request it answers.
    pub fn accept_built_list(
        &mut self,
        list: u64,
        ticket: u64,
        result: Option<Result<Arc<Choices>, ChoiceError>>,
        retry_after: Option<std::time::Duration>,
    ) {
        // A list of related values for a record (#290): taken under its
        // key unless the data, the unsaved changes or (calculated) the
        // context changed since it was asked; a related host's wait as a
        // list of all of a field's values.
        if let Some(ask) = self.related_building.get_mut().remove(&ticket) {
            if ask.stale {
                return;
            }
            if let Some(after) = retry_after {
                let at = std::time::Instant::now() + after;
                self.build_retry.get_mut().insert(list, (at, false));
            }
            if let Some(items) = result {
                self.related_built
                    .get_mut()
                    .insert(ask.key, (ask.calculated, items));
            }
            return;
        }
        // A list of an unstored calculation's values asked for a context or
        // date since gone (#290): its wait is not the new context's, which
        // may read other data.
        let stale = self.stale_builds.get_mut().contains(&list);
        if let Some(after) = retry_after.filter(|_| !stale) {
            let at = std::time::Instant::now() + after;
            self.build_retry.get_mut().insert(list, (at, false));
        }
        let building = self.building.get_mut();
        if building
            .get(&list)
            .is_none_or(|(asked, _)| *asked != ticket)
        {
            return;
        }
        let Some((_, tables)) = building.remove(&list) else {
            return;
        };
        // Its answer is not taken either: the next frame asks for the list
        // again.
        if stale {
            self.stale_builds.get_mut().remove(&list);
            return;
        }
        if let Some(items) = result {
            self.built.get_mut().insert(list, (tables, items));
        }
    }

    /// The lists to ask the host for now (`DataProvider::host_field_values`),
    /// as (list, table, field), and the epoch their answers must carry.
    pub fn take_list_wants(&mut self) -> (u64, Vec<(u64, u64, u64)>) {
        (self.host_epoch, std::mem::take(self.list_wants.get_mut()))
    }

    /// The host's answer for list `list`, asked in `epoch`: its values, or
    /// `None` (or an error) when it could not give the whole list, which
    /// the records then give. An answer from before the data last changed
    /// is not taken.
    pub fn accept_host_list(
        &mut self,
        list: u64,
        epoch: u64,
        result: Result<Option<Vec<String>>, DataError>,
    ) {
        if epoch != self.host_epoch {
            return;
        }
        let Some((state, shown)) = self.host_lists.get_mut().get_mut(&list) else {
            return;
        };
        *state = match result {
            Ok(Some(values)) if values.len() > choice::MAX_CHOICES => {
                HostList::Read(Err(ChoiceError::TooLarge))
            }
            Ok(Some(values)) => HostList::Read(Ok(Arc::new(Choices {
                items: values
                    .into_iter()
                    .map(|value| om_data::host::ValueListEntry {
                        label: value.clone(),
                        value,
                    })
                    .collect(),
                custom: false,
            }))),
            Ok(None) | Err(_) => HostList::Scan,
        };
        // Shown until the next frame can show it.
        *shown = true;
    }

    /// A frame was painted: a host's list no control showed goes, so the
    /// next opening asks again; one still being read stays.
    pub fn end_list_frame(&mut self) {
        self.host_lists
            .get_mut()
            .retain(|_, (state, shown)| matches!(state, HostList::Asked) || std::mem::take(shown));
    }

    /// The token for reads asked in `context` (the window's layout, view
    /// and found set): the same while it holds, the reads of an earlier
    /// context cancelled when it changes (#987). Only this window's reads
    /// are stopped; other windows keep theirs.
    pub fn reads_for(&mut self, context: u64) -> CalcCancel {
        if self.reads_context != Some(context) {
            self.reads.cancel();
            self.reads = CalcCancel::default();
            self.reads_context = Some(context);
        }
        self.reads.clone()
    }

    /// The window closes: its reads in flight stop (#987).
    pub fn cancel_reads(&self) {
        self.reads.cancel();
    }

    /// When needs, or value lists being built, waiting for an unreachable
    /// host are asked again (#740): the window draws again then.
    pub fn retry_at(&self) -> Option<std::time::Instant> {
        // A list waiting for an unreachable host is asked for again then
        // too. One whose time came draws the window again at once, once
        // (a control shows it then and asks for it); told, it goes.
        let now = std::time::Instant::now();
        let mut lists = self.build_retry.borrow_mut();
        lists.retain(|_, (at, told)| !(*told && *at <= now));
        let mut list: Option<std::time::Instant> = None;
        for (at, told) in lists.values_mut() {
            let when = if *at <= now {
                *told = true;
                now
            } else {
                *at
            };
            list = Some(list.map_or(when, |soon| soon.min(when)));
        }
        drop(lists);
        match (self.retry_at, list) {
            (Some(needs), Some(list)) => Some(needs.min(list)),
            (needs, list) => needs.or(list),
        }
    }

    /// Whether data the window painted is not in yet: asked for and not
    /// answered, or found missing and about to be asked for. Data that
    /// waits for an unreachable related host, or that the provider could
    /// not supply, is not waited for here: it is asked again later, or
    /// only after the data changes.
    pub fn awaiting_data(&self) -> bool {
        !self.requested.is_empty()
            || !self.wanted.borrow().is_empty()
            || !self.building.borrow().is_empty()
            || !self.related_building.borrow().is_empty()
    }

    /// Needs [`Calculations::take_wanted`] gave that were not sent (the
    /// connection to the host is gone): they are wanted again, asked for
    /// once it is back, and not awaited meanwhile.
    pub fn unsent(&mut self, needs: Vec<CalcNeed>) {
        for need in &needs {
            self.requested.remove(need);
        }
        self.wanted.get_mut().extend(needs);
    }

    /// What to ask the provider for now.
    pub fn take_wanted(&mut self) -> Vec<CalcNeed> {
        if self
            .retry_at
            .is_some_and(|at| at <= std::time::Instant::now())
        {
            self.waiting.clear();
            self.retry_at = None;
        }
        let wanted = std::mem::take(self.wanted.get_mut());
        let needs: Vec<CalcNeed> = wanted
            .into_iter()
            .filter(|n| {
                !self.requested.contains(n) && !self.failed.contains(n) && !self.waiting.contains(n)
            })
            .collect();
        self.requested.extend(needs.iter().cloned());
        if self.requested.is_empty() {
            self.library_pins.get_mut().clear();
        }
        needs
    }
}

/// Preview's measurement of a report plan: each object's text and
/// container data on the found set's records, through the window's cache.
/// What is still being read leaves the plan pending.
struct Measure<'e, 'a> {
    calc: &'e Evaluator<'a>,
    records: &'e [u64],
    pending: bool,
    /// The panel each tab control shows in the window, by control.
    panels: &'e BTreeMap<u64, u64>,
}

impl Measure<'_, '_> {
    fn record(
        &self,
        context: crate::report::ContentContext,
    ) -> Result<u64, crate::report::ReportError> {
        self.records.get(context.record).copied().ok_or_else(|| {
            crate::report::ReportError::unsupported(
                "The found set changed while measuring the report",
            )
        })
    }
}

impl crate::report::ReportContent for Measure<'_, '_> {
    fn front_panel(&mut self, control: &LayoutObject) -> Option<u64> {
        self.panels.get(&control.id).copied()
    }

    fn text(
        &mut self,
        object: &LayoutObject,
        context: crate::report::ContentContext,
    ) -> Result<String, crate::report::ReportError> {
        if object.kind() == om_layout::ObjectKind::Text && object.text_runs.is_empty() {
            return Ok(object.text.clone().unwrap_or_default());
        }
        let record = self.record(context)?;
        match self
            .calc
            .report_text(object, record, context.page as u64, context.group)
        {
            Err(error) if error.code == -1 => {
                self.pending = true;
                Ok(String::new())
            }
            other => other,
        }
    }

    fn container(
        &mut self,
        object: &LayoutObject,
        context: crate::report::ContentContext,
    ) -> Result<Option<Arc<om_data::calc::eval::StoredContainer>>, crate::report::ReportError> {
        if object.kind() != om_layout::ObjectKind::Field {
            return Ok(None);
        }
        let record = self.record(context)?;
        match self.calc.report_container(object, record, context.portal) {
            Err(error) if error.code == -1 => {
                self.pending = true;
                Ok(None)
            }
            other => other,
        }
    }
}

/// What the window tells `Get`: the file, account, layout, mode and found
/// set.
pub(crate) fn session_info(
    session: &Session,
    schema: Option<&CalcSchema>,
    account: &str,
    mode: WindowMode,
) -> SessionInfo {
    let file = session.target.file.as_str();
    let file_name = om_data::file_refs::file_stem(file).to_owned();
    // A layout the set hides has no number: FileMaker Pro answers 0
    // (session-identity.md 2.5, #696).
    let layout_number = session.layout.and_then(|id| {
        let at = session.layouts.iter().position(|l| l.id == id)?;
        Some(if session.shows_layout(id) {
            at as u64 + 1
        } else {
            0
        })
    });
    let layout_table_name = layout_occurrence(session)
        .and_then(|o| schema?.occurrence(o))
        .map(|o| o.name.clone())
        .unwrap_or_default();
    SessionInfo {
        account_name: account.to_owned(),
        account_info: schema
            .map(|s| s.authenticated_account.clone())
            .unwrap_or_default(),
        file_path: match &session.target.local {
            Some(path) => om_data::file_refs::local_file_path(path),
            None => format!("fmnet:/{}/{file_name}.fmp12", session.target.host),
        },
        window_name: file_name.clone(),
        file_name,
        // A local file's host is this computer.
        host_name: match &session.target.local {
            Some(_) => om_data::host::machine::Machine::current().host_name.clone(),
            None => session.target.host.clone(),
        },
        window_mode: mode,
        layout_name: session.layout_name().unwrap_or_default().to_owned(),
        layout_number,
        layout_count: Some(session.layouts.len() as u64),
        // The account's set on this layout and its current record
        // (session-identity.md 2.4).
        layout_access_here: schema
            .and_then(|s| s.authenticated_account.set_access.as_ref())
            .zip(session.layout)
            .map(|(access, layout)| access.layout_access(u64::from(layout))),
        // 0 without a current record; in Find mode, whose cursor is a
        // request, the set's own answer [I].
        record_access_here: match mode {
            WindowMode::Find => None,
            _ if session.total() == Some(0)
                && schema.is_some_and(|s| s.authenticated_account.set_access.is_some()) =>
            {
                Some(0)
            }
            _ => session.record_access_at(session.current),
        },
        layout_table_name,
        found_count: session.total(),
        page_number: session.preview_position().map(|p| p.0 + 1),
        active_record_number: session
            .total()
            .filter(|t| *t > 0)
            .map(|_| session.current + 1),
        utc_offset: om_provider::local_utc_offset(),
        ..SessionInfo::for_this_machine()
    }
}

/// `Get ( ActiveFieldName )` and `Get ( ActiveFieldTableName )` of the
/// field the window names `field`: a related one as Find mode names it
/// (`Occurrence::Field`), else one of the layout's table `layout_table`.
pub(crate) fn focus_names(field: &str, layout_table: &str) -> (String, String) {
    match field.split_once("::") {
        Some((occurrence, name)) => (name.to_owned(), occurrence.to_owned()),
        None => (field.to_owned(), layout_table.to_owned()),
    }
}

/// A `Get ( ModifiedFields )` answer with the schema and inputs it was
/// computed from.
type ModifiedAnswer = (Arc<CalcSchema>, ModifiedInputs, Vec<String>);

/// What a window's `Get ( ModifiedFields )` is computed from
/// ([`window_modified_fields`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModifiedInputs {
    pub table: u64,
    pub drafts: Vec<String>,
    pub typing: Option<String>,
    pub cells: Vec<u64>,
    pub entered: Vec<String>,
}

impl Calculations {
    /// [`window_modified_fields`] for the layout's calculations, computed
    /// again only when its inputs or the schema change: the walk through
    /// the stored calculations that depend on the changed fields would
    /// otherwise run on every frame the window draws.
    pub fn modified_fields(&self, inputs: ModifiedInputs) -> Vec<String> {
        let Some(schema) = self.schema() else {
            return Vec::new();
        };
        if let Some((seen, given, answer)) = &*self.modified.borrow() {
            if Arc::ptr_eq(seen, schema) && *given == inputs {
                return answer.clone();
            }
        }
        let answer = window_modified_fields(
            schema,
            inputs.table,
            inputs.drafts.iter().map(String::as_str),
            inputs.typing.as_deref(),
            inputs.cells.iter().copied(),
            &inputs.entered,
        );
        *self.modified.borrow_mut() = Some((Arc::clone(schema), inputs, answer.clone()));
        answer
    }
}

/// `Get ( ModifiedFields )` of a window's current record of `table` in
/// Browse mode (calc-functions.md 6.3 O3), as scripts and the layout's
/// calculations read it: the fields with a draft (`drafts`, by name) and
/// those whose cells a script set or the user typed into (`cells`; cell
/// `F * 256 + n` is field F's repetition n), but not the field still being
/// typed into (`typing`), which is listed once it is left; the stored
/// calculations on them; and a new record's auto-entered fields
/// (`entered`, the fields that held a value when it was made).
pub(crate) fn window_modified_fields<'a>(
    schema: &CalcSchema,
    table: u64,
    drafts: impl IntoIterator<Item = &'a str>,
    typing: Option<&str>,
    cells: impl IntoIterator<Item = u64>,
    entered: &[String],
) -> Vec<String> {
    let Some(def) = schema.table(table) else {
        return Vec::new();
    };
    let named = |name: &str| def.fields.values().find(|f| f.name == name);
    let mut changed: Vec<u64> = drafts
        .into_iter()
        .filter(|name| typing != Some(*name))
        .filter_map(|name| named(name).map(|f| f.id))
        .collect();
    for cell in cells {
        let field = if def.fields.contains_key(&cell) {
            cell
        } else {
            cell >> 8
        };
        let Some(def) = def.fields.get(&field) else {
            continue;
        };
        if typing != Some(def.name.as_str()) && !changed.contains(&field) {
            changed.push(field);
        }
    }
    let entered: Vec<u64> = entered
        .iter()
        .filter_map(|name| named(name))
        .filter(|f| om_data::host::session::auto_enters(f))
        .map(|f| f.id)
        .collect();
    om_data::host::session::modified_fields(schema, table, &changed, &entered)
}

/// The layout's table occurrence.
pub(crate) fn layout_occurrence(session: &Session) -> Option<u64> {
    match &session.render {
        RenderState::Loaded(layout) => {
            om_layout::occurrence_id(layout.layout.def.as_ref()?.table_alias_key)
        }
        _ => None,
    }
}

/// The current record with the user's unsaved edits, as calculations see
/// it, when its stored values are cached.
pub(crate) fn draft_overlay(
    schema: &CalcSchema,
    cache: &CalcData,
    session: &Session,
    changes: &om_data::FieldChanges,
    occurrence: u64,
    today: Date,
) -> Option<(u64, u64, Arc<RecordValues>)> {
    if changes.is_empty() {
        return None;
    }
    let (_, record) = session.current_record()?;
    let record = u64::try_from(record.id).ok()?;
    let table = schema.table_of(occurrence)?;
    let stored = cache.records.get(&(table.id, record))?.as_deref()?;
    let mut values = stored.clone();
    let cx = Ctx::new(&schema.locale, today);
    for (name, value) in changes {
        if let Some(def) = table.fields.values().find(|f| &f.name == name) {
            let text = value.as_deref().unwrap_or_default();
            values.set(def.id, 1, stored_value(text, def.value_type, &cx));
        }
    }
    Some((table.id, record, Arc::new(values)))
}

/// Add what the current record keeps uncommitted of its related records
/// to `overlays`: stored records a step changed, over their stored values
/// (left out while those are not cached), and records begun through
/// creation rows, at their [`begun_record`] ids.
fn add_draft_overlays(
    schema: &CalcSchema,
    cache: &CalcData,
    (drafts, begun): (&crate::document::RelatedDrafts, &crate::document::BegunRows),
    today: Date,
    overlays: &mut Vec<(u64, u64, Arc<RecordValues>)>,
) {
    let cx = Ctx::new(&schema.locale, today);
    for (table, record, cells) in drafts {
        let (Some(def), Some(stored)) = (
            schema.table(*table),
            cache
                .records
                .get(&(*table, *record))
                .and_then(|v| v.as_deref()),
        ) else {
            continue;
        };
        // Rules may have replaced a typed cell in the retained final intent.
        // Other records, including same-file drafts, keep their usual overlay.
        if stored.prepared_intent {
            continue;
        }
        // The current record's own draft (its later repetitions, #185)
        // goes over the overlay of its named drafts, which it would
        // otherwise replace (a later overlay of a record wins).
        match overlays
            .iter_mut()
            .find(|(t, r, _)| (t, r) == (table, record))
        {
            Some((_, _, values)) => {
                crate::scripting::host::apply_cells(Arc::make_mut(values), def, cells, &cx);
            }
            None => {
                let mut values = stored.clone();
                crate::scripting::host::apply_cells(&mut values, def, cells, &cx);
                overlays.push((*table, *record, Arc::new(values)));
            }
        }
    }
    for (id, table, cells) in begun.values().flatten() {
        let Some(def) = schema.table(*table) else {
            continue;
        };
        let mut values = RecordValues::default();
        crate::scripting::host::apply_cells(&mut values, def, cells, &cx);
        overlays.push((*table, begun_record(*id), Arc::new(values)));
    }
}

/// Add the related records the user edited through the current record
/// (`edits`), with those edits, as calculations see them, to `overlays`;
/// a record already there (the current one, through a self-join) takes
/// them over its own. A record whose stored values are not cached is left
/// out.
fn add_related_overlays(
    schema: &CalcSchema,
    cache: &CalcData,
    edits: &[crate::document::RelatedEdit],
    today: Date,
    overlays: &mut Vec<(u64, u64, Arc<RecordValues>)>,
) {
    let cx = Ctx::new(&schema.locale, today);
    for edit in edits {
        // A record begun in a creation row reads at its `begun_record` id.
        let (record, begun) = match edit.field.row {
            crate::document::RelatedRow::Record(record) => (record, false),
            crate::document::RelatedRow::New { id, .. } => (begun_record(id), true),
        };
        let key = (edit.field.table, record);
        if cache
            .records
            .get(&key)
            .and_then(|row| row.as_deref())
            .is_some_and(|row| row.prepared_intent)
        {
            continue;
        }
        let Some(table) = schema.table(key.0) else {
            continue;
        };
        let cell = edit.field.field;
        let (def, repetition) = match table.fields.get(&cell) {
            Some(def) => (def, 1),
            None => {
                let Some(def) = table.fields.get(&(cell >> 8)) else {
                    continue;
                };
                (def, (cell & 255) as u32)
            }
        };
        let at = match overlays.iter().position(|(t, r, _)| (*t, *r) == key) {
            Some(at) => at,
            None if begun => {
                overlays.push((key.0, key.1, Arc::new(RecordValues::default())));
                overlays.len() - 1
            }
            None => {
                let Some(stored) = cache.records.get(&key).and_then(|v| v.as_deref()) else {
                    continue;
                };
                overlays.push((key.0, key.1, Arc::new(stored.clone())));
                overlays.len() - 1
            }
        };
        Arc::make_mut(&mut overlays[at].2).set(
            def.id,
            repetition,
            stored_value(&edit.value, def.value_type, &cx),
        );
    }
}

/// Evaluates layout calculations for one frame.
pub(crate) struct Evaluator<'a> {
    calcs: &'a Calculations,
    schema: &'a CalcSchema,
    info: SessionInfo,
    layout: u64,
    occurrence: u64,
    today: Date,
    locals: RefCell<Variables>,
    globals: RefCell<Variables>,
    /// The records with the user's unsaved edits: the current record and
    /// the related records edited through it.
    overlays: Vec<(u64, u64, Arc<RecordValues>)>,
    /// The related sets scanned over `overlays` this frame: a set a
    /// draft changes is scanned once, not once per field reading it
    /// (#1308).
    scans: RefCell<om_data::host::ScannedSets>,
    /// The file is on a host, which lists a field's values
    /// ([`HostList`]); a local file's are read from its records.
    hosted: bool,
    summaries: SummaryCache,
    /// The break field of the sub-summary band being painted
    /// ([`Self::in_group`]); `None` elsewhere.
    group: Cell<Option<om_layout::BreakField>>,
}

/// Browse List view's sub-summary groups for one found set: the found set's
/// record ids they were read for, in order, the parts shown around each
/// record, and the shown parts whose content OpenMaker cannot place in its
/// group ([`crate::report::band_refusal`]), with why.
#[derive(Debug)]
pub(crate) struct ListGroups {
    pub records: Arc<[u64]>,
    pub groups: om_layout::pagination::RecordGroups,
    pub refused: BTreeMap<usize, String>,
}

/// [`Evaluator::in_group`] until dropped.
pub(crate) struct GroupScope<'e, 'a> {
    evaluator: &'e Evaluator<'a>,
    outer: Option<om_layout::BreakField>,
}

impl Drop for GroupScope<'_, '_> {
    fn drop(&mut self) {
        self.evaluator.group.set(self.outer);
    }
}

impl<'a> Evaluator<'a> {
    pub fn refreshed_value(&self, object: u64) -> bool {
        self.calcs.field_overrides.contains(&(self.layout, object))
    }
    pub fn content_revision(&self) -> u64 {
        self.calcs.content_revision.get()
    }

    /// `None` when the provider has no calculation schema or the layout no
    /// table occurrence.
    pub fn new(
        calcs: &'a Calculations,
        session: &Session,
        document: &DocumentState,
        info: SessionInfo,
        globals: &Variables,
    ) -> Option<Evaluator<'a>> {
        let schema = calcs.schema()?.as_ref();
        let occurrence = layout_occurrence(session).or_else(|| {
            schema
                .layouts()
                .iter()
                .find(|layout| Some(layout.id) == session.layout)?
                .occurrence
        })?;
        let today = info.evaluation_date();
        let changes = document.changes(session);
        let related = session
            .current_record()
            .map(|(_, record)| {
                document.related_edits(session.layout.unwrap_or_default(), record.id)
            })
            .unwrap_or_default();
        let mut overlays: Vec<_> = draft_overlay(
            schema,
            &calcs.cache.borrow(),
            session,
            &changes,
            occurrence,
            today,
        )
        .into_iter()
        .collect();
        add_draft_overlays(
            schema,
            &calcs.cache.borrow(),
            (&document.related_drafts, &document.begun_rows),
            today,
            &mut overlays,
        );
        add_related_overlays(
            schema,
            &calcs.cache.borrow(),
            &related,
            today,
            &mut overlays,
        );
        let unsaved = !document.related_drafts.is_empty() || !document.begun_rows.is_empty();
        let draft = (!changes.is_empty() || !related.is_empty() || unsaved).then(|| DraftContext {
            layout: session.layout,
            occurrence,
            record: session.current_record().map(|(_, record)| record.id),
            changes,
            related,
            drafts: document.related_drafts.clone(),
            begun: document.begun_rows.clone(),
        });
        calcs.observe_draft(draft);
        calcs.observe_context(&info);
        let evaluator = Evaluator {
            calcs,
            schema,
            info,
            layout: u64::from(session.layout.unwrap_or_default()),
            occurrence,
            today,
            locals: RefCell::new(calcs.level0.clone()),
            globals: RefCell::new(globals.clone()),
            overlays,
            hosted: session.target.local.is_none(),
            scans: RefCell::default(),
            summaries: SummaryCache::default(),
            group: Cell::new(None),
        };
        Some(evaluator)
    }

    /// Run `f` on a host over the cache. `None` when it needed data the
    /// cache lacks (now wanted), unless the provider already failed to
    /// supply it; its loops stop meanwhile rather than run to their limit
    /// over the missing data.
    fn with_host<R>(&self, f: impl FnOnce(&mut FileHost<'_>) -> R) -> Option<R> {
        self.with_host_reads(f).map(|(result, _)| result)
    }

    /// [`Self::with_host`], with what `f` read besides data. Its `unknown`
    /// also tells that it read data the provider could not supply.
    fn with_host_reads<R>(&self, f: impl FnOnce(&mut FileHost<'_>) -> R) -> Option<(R, HostReads)> {
        self.host_reads(&self.overlays, f)
    }

    /// [`Self::with_host`] over committed data only: no draft of the
    /// current record or its related records.
    fn with_committed_host<R>(&self, f: impl FnOnce(&mut FileHost<'_>) -> R) -> Option<R> {
        self.host_reads(&[], f).map(|(result, _)| result)
    }

    fn host_reads<R>(
        &self,
        overlays: &[(u64, u64, Arc<RecordValues>)],
        f: impl FnOnce(&mut FileHost<'_>) -> R,
    ) -> Option<(R, HostReads)> {
        let mut source = CachedSource::new(&self.calcs.cache)
            .with_calculation_session(&self.info)
            .retrying(&self.calcs.failed);
        for (table, record, values) in overlays {
            source = source.with_overlay(*table, *record, Arc::clone(values));
        }
        // Only `self.overlays` are scanned over: no other overlays come.
        // Without any, the frame scans only a held new record's sets.
        if !overlays.is_empty() || self.overlays.is_empty() {
            source = source.keeping_scans(&self.scans);
        }
        let (mut locals, mut globals) = (self.locals.borrow_mut(), self.globals.borrow_mut());
        // A frame shares only complete results from an unchanged variable
        // context. A later data reply creates another Evaluator and cache.
        let has_variables = !locals.is_empty() || !globals.is_empty();
        if has_variables {
            self.summaries.clear();
        }
        let mut host = FileHost::new(
            FileCalc::new(self.schema, &source, self.today).with_summary_cache(&self.summaries),
            &self.info,
            &mut locals,
            &mut globals,
        );
        let result = f(&mut host);
        let mut reads = host.read_provenance();
        let missing = source.missing();
        if !missing.is_empty() || has_variables || !locals.is_empty() || !globals.is_empty() {
            self.summaries.clear();
        }
        let pending = missing.iter().any(|n| !self.calcs.failed.contains(n));
        reads.unknown |= missing.iter().any(|n| self.calcs.failed.contains(n));
        if pending || !overlays.is_empty() {
            self.calcs
                .library_pins
                .borrow_mut()
                .extend(source.library_reads());
        }
        self.calcs.wanted.borrow_mut().extend(missing);
        (!pending).then_some((result, reads))
    }

    fn portal_context(&self, parent: u64, row: Option<PortalRow>) -> Context {
        let context = Context::record(self.occurrence, parent);
        let Some(row) = row else {
            return context;
        };
        let context = if row.occurrence == self.occurrence {
            Context::record(self.occurrence, row.record)
        } else {
            context.with_portal(row.occurrence, row.record)
        };
        context.with_portal_row_number(row.number as u64)
    }

    /// Whether the direct relationship is configured to show a blank
    /// creation row. This is only a passive rendering hint; it does not
    /// establish the account's table creation rights or authorize an edit.
    pub fn portal_allows_creation(&self, object: &LayoutObject) -> bool {
        let Some(occurrence) = object
            .portal
            .as_ref()
            .and_then(|portal| om_layout::occurrence_id(portal.table_alias_key))
        else {
            return false;
        };
        if occurrence == self.occurrence || self.schema.table_of(occurrence).is_none() {
            return false;
        }
        self.schema.relationships().iter().any(|relationship| {
            (relationship.left == self.occurrence
                && relationship.right == occurrence
                && relationship.options & 0x10 != 0)
                || (relationship.right == self.occurrence
                    && relationship.left == occurrence
                    && relationship.options & 0x20 != 0)
        })
    }

    /// Resolve the displayed set before row painting: relationship order,
    /// an enabled portal sort, then the portal's filter. Incomplete sets
    /// stay pending rather than being cached as empty or partially filtered.
    pub fn portal_records(&self, object: &LayoutObject, parent: u64) -> Option<Arc<[u64]>> {
        let occurrence = om_layout::occurrence_id(object.portal.as_ref()?.table_alias_key)?;
        let key = (self.layout, object.id, parent);
        if let Some((records, _)) = self.calcs.portal_sets.borrow().get(&key) {
            return Some(Arc::clone(records));
        }
        let (records, mut reads) = self.with_host_reads(|host| {
            let cx = host.file.cx();
            if occurrence == self.occurrence {
                let table = self
                    .schema
                    .table_of(occurrence)
                    .ok_or(om_data::calc::CalcError::TABLE_MISSING)?;
                host.file.source.found_set(occurrence, table.id)
            } else {
                host.file
                    .source
                    .related_ids(self.schema, self.occurrence, parent, occurrence, &cx)
            }
        })?;
        let records = records.ok()?;
        // A related record the account's set may not view is left out, as
        // FileMaker Pro 26.0.3 leaves it out (#695's reading): the provider
        // marks it (`RecordValues::no_access`). The rows, their numbers and
        // the creation row follow the records listed [I for the numbers].
        let records = if occurrence == self.occurrence {
            records
        } else {
            let table = self.schema.table_of(occurrence).map(|table| table.id);
            let cache = self.calcs.cache.borrow();
            let withheld = |record: &u64| {
                table.is_some_and(|table| {
                    cache
                        .records
                        .get(&(table, *record))
                        .and_then(|values| values.as_deref())
                        .is_some_and(|values| values.no_access)
                })
            };
            if records.iter().any(withheld) {
                records.iter().copied().filter(|r| !withheld(r)).collect()
            } else {
                records
            }
        };
        // KSortOrder is independent of the relationship's own ordering.
        // Portal flag 0x8 enables this stored order; a stale stream alone
        // must not turn sorting on.
        let sort = object
            .portal
            .as_ref()
            .filter(|portal| portal.flags & 0x8 != 0)
            .and(object.calcs.portal_sort.as_deref())
            .and_then(om_data::specs::SortSpec::decode);
        let records = if let Some(sort) = sort {
            // sort_records lists every missing sort row together. A row the
            // provider failed to supply has no key; the rows keep the
            // relationship's order rather than leaving the portal blank.
            let (sorted, sort_reads) = self.with_host_reads(|host| {
                om_data::host::sort_records(
                    host.file.source,
                    self.schema,
                    occurrence,
                    records.to_vec(),
                    &sort,
                    &host.file.cx(),
                )
            })?;
            reads = reads.union(sort_reads);
            sorted.map_or(records, Arc::from)
        } else {
            records
        };
        let records = if let Some(filter) = &object.calcs.portal_filter {
            let (filtered, filter_reads) = self.with_host_reads(|host| {
                records
                    .iter()
                    .enumerate()
                    .filter_map(|(index, &record)| {
                        let row = PortalRow {
                            occurrence,
                            record,
                            number: index + 1,
                        };
                        let context = self.portal_context(parent, Some(row));
                        layout::holds(host, filter, &context).then_some(record)
                    })
                    .collect::<Arc<[u64]>>()
            })?;
            reads = reads.union(filter_reads);
            filtered
        } else {
            records
        };
        self.calcs
            .portal_sets
            .borrow_mut()
            .insert(key, (Arc::clone(&records), reads.session));
        Some(records)
    }

    /// The items of a checkbox or radio button set's or a pop-up menu's
    /// value list on `parent`'s row (`om_data::choice`, file-metadata.md
    /// 6.4), each with the value it stores and the text the control shows:
    /// every one, or why there are none to offer. [`ChoiceError::Pending`]
    /// while the data of a field-based list is being fetched; another
    /// file's or a calculated list, and one too large, are errors, never an
    /// empty or shorter list.
    pub fn option_values(
        &self,
        object: &LayoutObject,
        parent: u64,
        portal: Option<PortalRow>,
    ) -> Result<Arc<Choices>, ChoiceError> {
        let list = choice::value_list(self.schema, object)?;
        // A list that does not depend on the record is read from committed
        // data (FileMaker builds a field's list from its index, which a
        // record's uncommitted values are not in [I], #412), once for all
        // objects, records and portal rows.
        if let Some(tables) = shared_list_tables(self.schema, list) {
            // A hosted file's host lists a field's values in one request, as
            // FileMaker Pro asks for them (8.7).
            if let Some((table, field)) =
                host_list_source(self.schema, list).filter(|_| self.hosted)
            {
                let mut host_lists = self.calcs.host_lists.borrow_mut();
                match host_lists.get_mut(&list.id) {
                    None => {
                        host_lists.insert(list.id, (HostList::Asked, true));
                        self.calcs
                            .list_wants
                            .borrow_mut()
                            .push((list.id, table, field));
                        return Err(ChoiceError::Pending);
                    }
                    Some((state, shown)) => {
                        *shown = true;
                        match state {
                            HostList::Asked => return Err(ChoiceError::Pending),
                            HostList::Read(items) => return items.clone(),
                            HostList::Scan => {}
                        }
                    }
                }
            }
            if let Some((_, items)) = self.calcs.lists.borrow().get(&list.id) {
                return items.clone();
            }
            // A field's values are read from every record of its table: the
            // worker builds the list (`list_build`), never paint, which would
            // stop drawing and taking input meanwhile (#745). The control
            // waits until it is built. Custom values read no data.
            if matches!(list.source, om_data::meta::ValueSource::Field { .. }) {
                return self.built_list(list.id, tables);
            }
            let items = self
                .with_committed_host(|host| {
                    choice::choices(&host.file, list, &Context::new()).map(Arc::new)
                })
                .ok_or(ChoiceError::Pending)?;
            self.calcs
                .lists
                .borrow_mut()
                .insert(list.id, (tables, items.clone()));
            return items;
        }
        // A list of related values depends on the record (and portal row)
        // it starts from: the worker builds it for each, never paint, whose
        // frame it would hold for as long as the related set takes (#290).
        let portal = portal.map(|row| (row.occurrence, row.record));
        self.related_list(list, (list.id, self.occurrence, parent, portal))
    }

    /// Shared list `list` (reading `tables`) as the worker built it, or
    /// [`ChoiceError::Pending`] while it is being built: asked of the worker
    /// once (`list_build`, #745).
    fn built_list(&self, list: u64, tables: BTreeSet<u64>) -> Result<Arc<Choices>, ChoiceError> {
        if let Some((_, items)) = self.calcs.built.borrow().get(&list) {
            return items.clone();
        }
        // Waiting for a related host that could not be reached (#740);
        // once its time came, the wait is over and the list is asked for.
        {
            let mut waits = self.calcs.build_retry.borrow_mut();
            if let Some((at, _)) = waits.get(&list) {
                if std::time::Instant::now() < *at {
                    return Err(ChoiceError::Pending);
                }
                waits.remove(&list);
            }
        }
        if let std::collections::hash_map::Entry::Vacant(asked) =
            self.calcs.building.borrow_mut().entry(list)
        {
            let ticket = self.calcs.build_tickets.get().wrapping_add(1);
            self.calcs.build_tickets.set(ticket);
            asked.insert((ticket, tables));
            self.calcs.build_wants.borrow_mut().push(ListBuild {
                list,
                ticket,
                today: self.today,
                info: self.info.clone(),
                related: None,
            });
        }
        Err(ChoiceError::Pending)
    }

    /// List `list` of related values for `key`'s record and portal row as
    /// the worker built it with the window's unsaved changes, or
    /// [`ChoiceError::Pending`] while it is being built: asked of the worker
    /// once per record (`list_build`, #290), never built in paint.
    fn related_list(
        &self,
        list: &om_data::meta::ValueList,
        key: RelatedKey,
    ) -> Result<Arc<Choices>, ChoiceError> {
        if let Some((_, items)) = self.calcs.related_built.borrow().get(&key) {
            return items.clone();
        }
        if self
            .calcs
            .related_building
            .borrow()
            .values()
            .any(|ask| ask.key == key)
        {
            return Err(ChoiceError::Pending);
        }
        // Waiting for a related host that could not be reached (#740).
        {
            let mut waits = self.calcs.build_retry.borrow_mut();
            if let Some((at, _)) = waits.get(&list.id) {
                if std::time::Instant::now() < *at {
                    return Err(ChoiceError::Pending);
                }
                waits.remove(&list.id);
            }
        }
        let ticket = self.calcs.build_tickets.get().wrapping_add(1);
        self.calcs.build_tickets.set(ticket);
        self.calcs.related_building.borrow_mut().insert(
            ticket,
            RelatedAsk {
                key,
                calculated: calculated_list(self.schema, list),
                stale: false,
            },
        );
        let (_, occurrence, parent, portal) = key;
        self.calcs.build_wants.borrow_mut().push(ListBuild {
            list: list.id,
            ticket,
            today: self.today,
            info: self.info.clone(),
            related: Some(RelatedBuild {
                occurrence,
                parent,
                portal,
                overlays: self.overlays.clone(),
            }),
        });
        Err(ChoiceError::Pending)
    }

    /// Whether a pop-up menu of `object` shows the label of the item that
    /// stores its value rather than the value: its list shows values only
    /// from its second field. Such a menu reads its items also closed.
    pub fn shows_labels(&self, object: &LayoutObject) -> bool {
        use om_data::meta::{value_list_option, ValueSource};
        choice::value_list(self.schema, object).is_ok_and(|list| {
            list.options & value_list_option::SECOND_ONLY != 0
                && matches!(
                    &list.source,
                    ValueSource::Field {
                        secondary: Some(_),
                        ..
                    }
                )
        })
    }

    /// The found set's groups at the layout's sub-summary break fields, for
    /// Preview pagination: `None` while their data is being fetched. A
    /// report whose parts resize also needs the found set's record ids to
    /// measure them. No data is read for a layout needing neither.
    pub fn report_groups(
        &self,
        layout: &om_layout::Layout,
        found: u64,
    ) -> Option<Result<crate::report::FoundSetGroups, crate::report::ReportError>> {
        use crate::report::{self, ReportError};
        let breaks = match report::report_breaks(layout, self.schema, self.occurrence) {
            Ok(breaks) if breaks.is_empty() && !report::resizes_parts(layout) => {
                return Some(Ok((Arc::from([]), Vec::new())))
            }
            Ok(breaks) => breaks,
            Err(error) => return Some(Err(error)),
        };
        if found > report::MAX_GROUPED_RECORDS {
            return Some(Err(ReportError::unsupported(format!(
                "Grouped reports and reports whose parts resize support at most {} records",
                report::MAX_GROUPED_RECORDS
            ))));
        }
        let fields: Vec<u64> = breaks.iter().map(|b| b.field).collect();
        let found_groups =
            match self.with_host(|host| host.file.found_groups(self.occurrence, &fields))? {
                Ok(groups) => groups,
                Err(error) => return Some(Err(report::group_error(error))),
            };
        let records = Arc::clone(&found_groups.records);
        if records.len() as u64 != found {
            return Some(Err(ReportError::unsupported(
                "The found set changed while grouping the report",
            )));
        }
        Some(report::selected_groups(&breaks, &found_groups, &records).map(|g| (records, g)))
    }

    /// The found set's sub-summary groups for Browse List view: `Ok(None)`
    /// when the layout has no sub-summary parts or the found set is sorted
    /// by none of their break fields, so none shows; `None` while their data
    /// is being fetched. An unsorted set reads only its record ids and sort.
    /// Grouping reads every found record's sort values, so it is bounded as
    /// reports are ([`crate::report::MAX_GROUPED_RECORDS`]).
    pub fn list_groups(
        &self,
        layout: &om_layout::Layout,
        found: u64,
    ) -> Option<Result<Option<Arc<ListGroups>>, String>> {
        use crate::report;
        let breaks = match report::report_breaks(layout, self.schema, self.occurrence) {
            Ok(breaks) if breaks.is_empty() => return Some(Ok(None)),
            Ok(breaks) => breaks,
            Err(error) => return Some(Err(error.message)),
        };
        let Some(table) = self.schema.table_of(self.occurrence).map(|t| t.id) else {
            return Some(Err(
                "The layout's table is missing from the calculation schema".into(),
            ));
        };
        // The sort arrives with the found set's ids.
        let sorted = match self.with_host(|host| {
            let source = &host.file.source;
            source
                .found_set(self.occurrence, table)
                .map(|ids| (ids.len(), source.found_sort(self.occurrence)))
        })? {
            Ok((count, sort)) => sort
                .is_some_and(|sort| breaks.iter().any(|b| sort.contains(&b.field)))
                .then_some(count),
            Err(error) => return Some(Err(report::group_error(error).message)),
        };
        let Some(count) = sorted else {
            return Some(Ok(None));
        };
        if count as u64 > report::MAX_GROUPED_RECORDS {
            return Some(Err(format!(
                "Grouping supports at most {} sorted records",
                report::MAX_GROUPED_RECORDS
            )));
        }
        let fields: Vec<u64> = breaks.iter().map(|b| b.field).collect();
        let found_groups =
            match self.with_host(|host| host.file.found_groups(self.occurrence, &fields))? {
                Ok(groups) => groups,
                Err(error) => return Some(Err(report::group_error(error).message)),
            };
        let records = Arc::clone(&found_groups.records);
        if records.len() as u64 != found {
            return Some(Err(
                "The found set changed while grouping its records".into()
            ));
        }
        let selected = match report::selected_groups(&breaks, &found_groups, &records) {
            Ok(selected) => selected,
            Err(error) => return Some(Err(error.message)),
        };
        let groups = match om_layout::pagination::RecordGroups::new(layout, selected, records.len())
        {
            Ok(groups) if groups.is_empty() => return Some(Ok(None)),
            Ok(groups) => groups,
            Err(error) => return Some(Err(error)),
        };
        let refused = layout
            .parts
            .iter()
            .enumerate()
            .filter(|(_, part)| {
                matches!(
                    part.kind(),
                    om_layout::PartKind::LeadingSubSummary
                        | om_layout::PartKind::TrailingSubSummary
                )
            })
            .filter_map(|(index, part)| {
                report::band_refusal(layout, part, self.schema, self.occurrence)
                    .map(|why| (index, why))
            })
            .collect();
        Some(Ok(Some(Arc::new(ListGroups {
            records,
            groups,
            refused,
        }))))
    }

    /// Paint a sub-summary band with break field `by` until the scope
    /// drops: there a summary field of the layout's occurrence shows its
    /// group's value ([`Self::field_value_in_portal`],
    /// [`Self::named_value_in_portal`]).
    pub fn in_group(&self, by: om_layout::BreakField) -> GroupScope<'_, 'a> {
        GroupScope {
            evaluator: self,
            outer: self.group.replace(Some(by)),
        }
    }

    /// In a sub-summary band, summary field `field` of the layout's
    /// occurrence on the band's `record`: its group's value, as
    /// `GetSummary ( field ; break )` and Preview read it, never the found
    /// set's. `None` outside a band, in a portal row and for other fields.
    /// While its data is fetched it shows empty, not the record page's
    /// found-set value. Forms [`crate::report::band_refusal`] keeps out of
    /// bands give `?` here rather than another value.
    fn band_summary(
        &self,
        occurrence: u64,
        field: u64,
        repetition: u32,
        record: u64,
        portal: Option<PortalRow>,
    ) -> Option<Value> {
        let by = self.group.get().filter(|_| portal.is_none())?;
        let def = self
            .schema
            .field(occurrence, field)
            .filter(|def| def.summary.is_some())?;
        let reference = om_data::runtime::FieldRef {
            occurrence,
            field,
            repetition,
        };
        let Ok(Some((summary, by))) =
            crate::report::group_summary(def, reference, self.occurrence, Some(by))
        else {
            return Some(Value::invalid());
        };
        let key = (self.layout, field, record, by.occurrence, by.field);
        if let Some((value, _)) = self.calcs.group_values.borrow().get(&key) {
            return Some(value.clone());
        }
        let context = Context::record(self.occurrence, record);
        let Some((value, reads)) =
            self.with_host_reads(|host| host.file.summary(&context, summary, by))
        else {
            let last = self.calcs.last.borrow();
            return Some(
                last.group_values
                    .get(&key)
                    .map_or_else(|| Value::empty(def.value_type), |(value, _)| value.clone()),
            );
        };
        let value = value.unwrap_or_else(|_| Value::invalid());
        self.calcs
            .group_values
            .borrow_mut()
            .insert(key, (value.clone(), reads.session));
        Some(value)
    }

    /// Preview's plan of a report whose parts resize, measured over the
    /// found set's records (`found`, as [`Self::report_groups`] read them):
    /// `None` while their values are being fetched. Every record's data is
    /// asked for at once; a pending value measures provisionally so one pass
    /// asks for everything the plan reads. The plan is that of PDF and Print.
    /// `panels` are the panels the window's tab controls show, by control.
    pub fn report_plan(
        &self,
        layout: &om_layout::Layout,
        paper: om_layout::pagination::PageSize,
        found: &crate::report::FoundSetGroups,
        panels: &BTreeMap<u64, u64>,
    ) -> Option<Result<om_layout::pagination::ReportPlan, crate::report::ReportError>> {
        use crate::report::{self, ReportError};
        let (records, breaks) = found;
        let Some(table) = self.schema.table_of(self.occurrence).map(|t| t.id) else {
            return Some(Err(ReportError::unsupported(
                "The report's table is missing from the calculation schema",
            )));
        };
        let prefetched = self
            .with_host(|host| host.file.source.prefetch_records(table, records))
            .is_some();
        let mut measure = Measure {
            calc: self,
            records,
            pending: !prefetched,
            panels,
        };
        let plan = report::plan_report_with(layout, records.len(), paper, breaks, &mut measure);
        (!measure.pending).then_some(plan)
    }

    /// Report values bypass Browse's object-value cache: page context and
    /// found-set record identity may differ from the current Browse record.
    /// In a sub-summary part (`group`), a summary field of the layout's
    /// occurrence reads its group as `GetSummary ( field ; break )` does.
    pub fn report_text(
        &self,
        object: &LayoutObject,
        record: u64,
        page: u64,
        group: Option<om_layout::BreakField>,
    ) -> Result<String, crate::report::ReportError> {
        self.report_text_in(object, record, page, group, None)
    }

    /// [`Self::report_text`] in portal row `row`, if any.
    pub fn report_text_in(
        &self,
        object: &LayoutObject,
        record: u64,
        page: u64,
        group: Option<om_layout::BreakField>,
        row: Option<crate::report::PortalRowContext>,
    ) -> Result<String, crate::report::ReportError> {
        (self.report_eval(page))
            .report_shown(object, record, group, row)
            .map(|(text, _)| text)
    }

    /// The conditional formats of `object` that hold on a report `record`
    /// and `page`, in stored order. `Self` is a field object's value,
    /// otherwise the object's text, as in Browse. An error with code -1
    /// while their data is read.
    pub fn report_conditions(
        &self,
        object: &LayoutObject,
        record: u64,
        page: u64,
        group: Option<om_layout::BreakField>,
        row: Option<crate::report::PortalRowContext>,
    ) -> Result<Vec<usize>, crate::report::ReportError> {
        if object.calcs.conditions.is_empty() {
            return Ok(Vec::new());
        }
        let report_eval = self.report_eval(page);
        let (text, own) = report_eval.report_shown(object, record, group, row)?;
        let context = crate::report::in_row(Context::record(self.occurrence, record), row)
            .with_self(own.unwrap_or_else(|| Value::text(text)));
        // `LayoutObjectUUID` is the object's, as in Browse.
        let uuid = (object.uuid.as_ref()).map(om_data::host::layout_objects::uuid_text);
        report_eval
            .with_host(|host| {
                host.layout_object = uuid;
                layout::conditions(host, object, &context)
            })
            .ok_or_else(|| crate::report::ReportError {
                code: -1,
                message: "Loading report values…".into(),
            })
    }

    /// This evaluator for report page `page`: its page number, and caches
    /// of its own, since page context may differ from Browse's.
    fn report_eval(&self, page: u64) -> Self {
        let mut info = self.info.clone();
        info.page_number = Some(page + 1);
        info.report_page_number = Some(page + 1);
        Self {
            info,
            overlays: Vec::new(),
            locals: RefCell::new(self.locals.borrow().clone()),
            globals: RefCell::new(self.globals.borrow().clone()),
            scans: RefCell::default(),
            summaries: SummaryCache::default(),
            group: Cell::new(None),
            ..*self
        }
    }

    /// A report object's text and, for a field object, its field's value,
    /// read by a [`Self::report_eval`] evaluator.
    fn report_shown(
        &self,
        object: &LayoutObject,
        record: u64,
        group: Option<om_layout::BreakField>,
        row: Option<crate::report::PortalRowContext>,
    ) -> Result<(String, Option<Value>), crate::report::ReportError> {
        use crate::report::{self, ReportError};
        let report_eval = self;
        let mut own = None;
        let text = report::object_text(object, self.occurrence, self.schema, |field| {
            let def = self
                .schema
                .field(field.occurrence, field.field)
                .ok_or_else(|| ReportError::unsupported("A report field is missing"))?;
            // A container field object prints its data
            // ([`Self::report_container`]); a merge field has no text for it.
            if def.value_type == ValueType::Container {
                return match object.kind() {
                    om_layout::ObjectKind::Field => Ok(String::new()),
                    _ => Err(ReportError::unsupported(
                        "Container merge fields in reports are unsupported",
                    )),
                };
            }
            let expr = om_data::formula::Expr::Field(om_data::formula::Field::new(
                field.occurrence,
                field.field,
                true,
            ));
            let context = report::in_row(
                Context::record(self.occurrence, record).with_repetition(field.repetition),
                row,
            );
            let loading = || ReportError {
                code: -1,
                message: "Loading report values…".into(),
            };
            let incomplete = |code| ReportError {
                code,
                message: "A report field could not be calculated completely".into(),
            };
            let value = match report::group_summary(def, field, self.occurrence, group)? {
                Some((summary, by)) => report_eval
                    .with_host(|host| host.file.summary(&context, summary, by))
                    .ok_or_else(loading)?
                    .map_err(|e| incomplete(e.code()))
                    .and_then(|v| match v.is_invalid() {
                        true => Err(incomplete(3)),
                        false => Ok(v),
                    })?,
                None => {
                    let evaluated = report_eval
                        .with_host(|host| om_data::calc::eval::evaluate(&expr, host, &context))
                        .ok_or_else(loading)?;
                    if evaluated.pending_execution
                        || evaluated.error.is_some()
                        || evaluated.value.is_invalid()
                    {
                        return Err(incomplete(evaluated.code()));
                    }
                    evaluated.value
                }
            };
            let text = crate::document::data_format::display(
                &self.display(&value),
                def.value_type,
                object.data_format.as_ref(),
                &self.context(),
            )
            .into_owned();
            if object.kind() == om_layout::ObjectKind::Field {
                own = Some(value);
            }
            Ok(text)
        })?;
        Ok((text, own))
    }

    /// The data a container field object shows on a report record (in
    /// portal row `row`, if any), its streams read from the window's cache;
    /// `None` for other objects and an empty field. An error with code -1
    /// while the data is read.
    pub fn report_container(
        &self,
        object: &LayoutObject,
        record: u64,
        row: Option<crate::report::PortalRowContext>,
    ) -> Result<Option<Arc<om_data::calc::eval::StoredContainer>>, crate::report::ReportError> {
        use crate::report::ReportError;
        let Some(spec) = object.field_spec() else {
            return Ok(None);
        };
        let (Some(occurrence), Some(field)) =
            (om_layout::occurrence_id(spec.table_key), spec.field_id())
        else {
            return Ok(None);
        };
        if self
            .schema
            .field(occurrence, field)
            .is_none_or(|def| def.value_type != ValueType::Container)
        {
            return Ok(None);
        }
        let loading = || ReportError {
            code: -1,
            message: "Loading report values…".into(),
        };
        let unreadable = |code: i32| ReportError {
            code: if code == 0 { 3 } else { code },
            message: "A report container could not be read".into(),
        };
        let expr =
            om_data::formula::Expr::Field(om_data::formula::Field::new(occurrence, field, true));
        let context = crate::report::in_row(
            Context::record(self.occurrence, record)
                .with_repetition(u32::try_from(spec.repetition.max(1)).unwrap_or(1)),
            row,
        );
        let evaluated = self
            .with_host(|host| om_data::calc::eval::evaluate(&expr, host, &context))
            .ok_or_else(loading)?;
        if evaluated.pending_execution || evaluated.error.is_some() {
            return Err(unreadable(evaluated.code()));
        }
        let Value::Container(data) = evaluated.value else {
            return Ok(None);
        };
        if data.streams.is_empty() && data.library.is_none() {
            return Ok(None);
        }
        let Some(library) = data.library.clone().filter(|_| data.is_unloaded()) else {
            return Ok(Some(Arc::new(om_data::calc::eval::StoredContainer {
                data,
                intact: true,
                external: false,
            })));
        };
        self.library_data(&library)
            .ok_or_else(loading)?
            .map(Some)
            .map_err(|e| unreadable(e.code()))
    }

    /// The header of the library member a container value names (its file
    /// name, path type and MD5, containers.md 6.1). `None` while the
    /// provider reads it: it is wanted, and the window paints again when it
    /// arrives.
    pub fn library_member(
        &self,
        library: &om_data::calc::LibraryRef,
    ) -> Option<Result<Option<Arc<om_data::containers::MemberInfo>>, om_data::calc::CalcError>>
    {
        self.read_library(|source| source.library_member(library.table, &library.member))
    }

    /// That member's data, shared with the cache rather than copied; an
    /// error for data whose MD5 is not the one the value names.
    pub fn library_data(
        &self,
        library: &om_data::calc::LibraryRef,
    ) -> Option<Result<Arc<om_data::calc::eval::StoredContainer>, om_data::calc::CalcError>> {
        let read = self.read_library(|source| source.library_data(library.table, &library.member));
        read.map(|read| {
            read?
                .filter(|stored| {
                    stored
                        .data
                        .library
                        .as_ref()
                        .is_none_or(|actual| actual.md5 == library.md5)
                })
                .ok_or(om_data::calc::CalcError::NOT_SUPPORTED)
        })
    }

    /// `read` over the cache, as [`Self::with_host`] runs calculations.
    fn read_library<R>(
        &self,
        read: impl FnOnce(&CachedSource<'_>) -> Result<R, om_data::calc::CalcError>,
    ) -> Option<Result<R, om_data::calc::CalcError>> {
        let source = CachedSource::new(&self.calcs.cache).retrying(&self.calcs.failed);
        let result = read(&source);
        let missing = source.missing();
        let pending = missing.iter().any(|need| !self.calcs.failed.contains(need));
        self.calcs.wanted.borrow_mut().extend(missing);
        (!pending).then_some(result)
    }

    /// The related record field object `spec` shows on `parent`, which
    /// Browse edits: the record of portal row `portal` when the field is
    /// of the row's occurrence, else, outside portals, the first related
    /// record of the field's occurrence in the relationship's order (as
    /// the field shows it). `None` while its data is being fetched, for a
    /// first repetition of the layout's own occurrence, in a portal row
    /// of another occurrence, without a related
    /// record, and for the window's own record reached through a self-join
    /// (its edit would compete with the record's own draft of the field).
    pub fn related_record(
        &self,
        spec: &FieldSpec,
        parent: u64,
        portal: Option<PortalRow>,
    ) -> Option<RelatedRecord> {
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        let field = spec.field_id()?;
        let repetition = u32::try_from(spec.repetition.max(1)).ok()?;
        let def = self.schema.field(occurrence, field)?;
        if repetition > def.repetitions.max(1)
            || occurrence == self.occurrence && (repetition == 1 || portal.is_some())
        {
            return None;
        }
        let table = self.schema.table_of(occurrence)?.id;
        let record = if occurrence == self.occurrence {
            parent
        } else {
            match portal {
                // A row without a record (a portal's creation row) has none to edit.
                Some(row) => (row.occurrence == occurrence
                    && !om_data::host::unsaved_record(row.record))
                .then_some(row.record)?,
                None => *self
                    .with_host(|host| {
                        let cx = host.file.cx();
                        host.file.source.related_ids(
                            self.schema,
                            self.occurrence,
                            parent,
                            occurrence,
                            &cx,
                        )
                    })?
                    .ok()?
                    .first()?,
            }
        };
        let own = self.schema.table_of(self.occurrence).map(|t| t.id);
        if occurrence != self.occurrence && own == Some(table) && record == parent {
            return None;
        }
        // The stored values, not the window's unsaved ones over them.
        let stored = self
            .calcs
            .cache
            .borrow()
            .records
            .get(&(table, record))
            .cloned();
        let Some(stored) = stored else {
            // Not read yet: it is asked for.
            self.with_host(|host| host.file.source.record(table, record).ok());
            return None;
        };
        let values = stored?;
        let name = format!(
            "{}::{}",
            self.schema.occurrence(occurrence)?.name,
            self.schema.field(occurrence, field)?.name
        );
        Some(RelatedRecord {
            occurrence,
            table,
            record,
            field,
            cell: om_data::rules::cell_of(field, repetition)?,
            name,
            stored: if def.global {
                // The editor's original is the session value without the
                // frame's unsaved overlays. An overlay must not become its
                // own baseline and make an invalid draft look unchanged.
                let globals = self.calcs.cache.borrow().globals.get(&table).cloned();
                let Some(globals) = globals else {
                    self.with_host(|host| host.file.source.globals(table).ok());
                    return None;
                };
                globals
                    .get(field, repetition)
                    .map(|value| self.display(value))
                    .unwrap_or_default()
            } else {
                values
                    .get(field, repetition)
                    .map(|value| self.display(value))
                    .unwrap_or_default()
            },
            version: values.mod_count,
        })
    }

    /// Field object `spec`'s field in a record begun in the creation row of
    /// a portal of `occurrence` (no record yet, nothing stored): `None`
    /// for a field of another occurrence or an undefined repetition.
    pub fn new_related_field(&self, spec: &FieldSpec, occurrence: u64) -> Option<RelatedRecord> {
        let field = spec.field_id()?;
        if om_layout::occurrence_id(spec.table_key)? != occurrence
            || occurrence == self.occurrence
            || u32::try_from(spec.repetition.max(1)).ok()?
                > self.schema.field(occurrence, field)?.repetitions.max(1)
        {
            return None;
        }
        let name = format!(
            "{}::{}",
            self.schema.occurrence(occurrence)?.name,
            self.schema.field(occurrence, field)?.name
        );
        Some(RelatedRecord {
            occurrence,
            table: self.schema.table_of(occurrence)?.id,
            record: 0,
            field,
            cell: om_data::rules::cell_of(field, u32::try_from(spec.repetition.max(1)).ok()?)?,
            name,
            stored: String::new(),
            version: None,
        })
    }

    /// `Occurrence::Field`, the name of the field object `spec` shows.
    pub fn qualified_name(&self, spec: &FieldSpec) -> Option<String> {
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        Some(format!(
            "{}::{}",
            self.schema.occurrence(occurrence)?.name,
            self.schema.field(occurrence, spec.field_id()?)?.name
        ))
    }

    /// The base table of `occurrence`.
    pub fn table_of(&self, occurrence: u64) -> Option<u64> {
        Some(self.schema.table_of(occurrence)?.id)
    }

    /// Whether related field `spec` shows `<No Access>` (#695): one the
    /// account's set gives no access, in a portal row (its creation row's
    /// included) or placed on the layout, as FileMaker Pro 26.0.3 shows it
    /// (#695's reading); or a portal row of a record the set may not view
    /// (the provider marks it, `RecordValues::no_access`), which the
    /// portal leaves out ([`Self::portal_records`]) once its values are
    /// in. A related file's table is judged by that file's set. A field of
    /// the layout's own table is the window's to judge
    /// (`DataProvider::no_access`).
    pub fn related_no_access(&self, spec: &FieldSpec, portal: Option<PortalRow>) -> bool {
        let Some(occurrence) = om_layout::occurrence_id(spec.table_key) else {
            return false;
        };
        let row = match portal {
            Some(row) if row.occurrence == occurrence => Some(row),
            Some(_) => return false,
            None if occurrence == self.occurrence => {
                return self
                    .table_of(occurrence)
                    .is_some_and(|table| self.field_denied(table, spec));
            }
            None => None,
        };
        let Some(table) = self.table_of(occurrence) else {
            return false;
        };
        let withheld = row.is_some_and(|row| {
            self.calcs
                .cache
                .borrow()
                .records
                .get(&(table, row.record))
                .and_then(|values| values.as_deref())
                .is_some_and(|values| values.no_access)
        });
        withheld || self.field_denied(table, spec)
    }

    /// Whether the account's set gives field `spec` of base table `table`
    /// no access, by its file's set for a related file's table.
    fn field_denied(&self, table: u64, spec: &FieldSpec) -> bool {
        let Some(field) = spec.field_id() else {
            return false;
        };
        let (access, ids) = match self.schema.table_context(table) {
            None => (
                self.schema.authenticated_account.set_access.as_ref(),
                (table, field),
            ),
            Some(file) => (
                file.account_info.set_access.as_ref(),
                (file.file_id(table), file.file_id(field)),
            ),
        };
        access.is_some_and(|access| access.hides_field_id(ids.0, ids.1))
    }

    /// Whether the account's custom set gives the records through this
    /// layout no access (session-identity.md 2.2): FileMaker Pro 26.0.3
    /// draws the layout, its record-bound objects and merge fields showing
    /// `<No Access>`, while a script still reads and writes the record as
    /// the table's rules allow (#707's reading).
    pub fn layout_masked(&self) -> bool {
        self.schema
            .authenticated_account
            .set_access
            .as_ref()
            .is_some_and(|access| {
                access.shows_layout(self.layout)
                    && access.layout_records(self.layout)
                        == om_data::host::privilege_set::AccessLevel::No
            })
    }

    /// Whether field `spec` of the creation row of a portal of
    /// `occurrence` is one the account's set gives no access: it shows
    /// `<No Access>` there too, and takes no typing (#695's reading).
    pub fn creation_field_denied(&self, spec: &FieldSpec, occurrence: u64) -> bool {
        om_layout::occurrence_id(spec.table_key) == Some(occurrence)
            && self
                .table_of(occurrence)
                .is_some_and(|table| self.field_denied(table, spec))
    }

    /// Whether `occurrence` shows a table of another file that is not open
    /// (missing, or refusing the account): its fields show
    /// `<File Missing>`.
    pub fn file_missing(&self, occurrence: u64) -> bool {
        self.schema.table_of(occurrence).is_none()
            && self.schema.external_occurrence(occurrence).is_some()
            // One not tried yet is read when its file opens (#983).
            && !self.schema.is_pending(occurrence)
    }

    /// The text a value shows in this file.
    pub fn display(&self, value: &Value) -> String {
        value.to_text(&self.context()).into_string()
    }

    /// The file's locale and today's date, which read a value's text.
    pub fn context(&self) -> Ctx<'a> {
        Ctx::new(&self.schema.locale, self.today)
    }

    /// The type of the field a field object shows, which selects the
    /// object's data format (data-format.md).
    pub fn field_type(&self, spec: &FieldSpec) -> Option<ValueType> {
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        Some(self.schema.field(occurrence, spec.field_id()?)?.value_type)
    }

    /// The calculations of `object` on `record` (`NO_RECORD` without one);
    /// while their data is being read again, the state drawn before the
    /// last change, else `None`. `self_value` is the object's
    /// content.
    pub fn object_state_in_portal(
        &self,
        object: &LayoutObject,
        record: u64,
        portal: Option<PortalRow>,
        self_value: impl FnOnce() -> Value,
    ) -> Option<ObjectCalcState> {
        self.fresh_object_state(object, record, portal, self_value)
            .or_else(|| {
                // Read again: the state drawn last.
                let key = (self.layout, object.id, record, portal);
                let last = self.calcs.last.borrow();
                last.states.get(&key).map(|cached| ObjectCalcState {
                    kept: true,
                    ..cached.calc.clone()
                })
            })
    }

    /// [`Self::object_state_in_portal`] from the data read since the last
    /// change, `None` while it is being read.
    fn fresh_object_state(
        &self,
        object: &LayoutObject,
        record: u64,
        portal: Option<PortalRow>,
        self_value: impl FnOnce() -> Value,
    ) -> Option<ObjectCalcState> {
        let calcs = &object.calcs;
        if calcs.undecodable.contains(&om_layout::calcs::HIDE) {
            return Some(ObjectCalcState {
                visibility: Visibility::Unresolved,
                ..ObjectCalcState::default()
            });
        }
        if calcs.hide.is_none()
            && calcs.conditions.is_empty()
            && calcs.tooltip.is_none()
            && calcs.placeholder.is_none()
            && calcs.label.is_none()
            && calcs.active_segment.is_none()
            && object.active_icon_formula().is_none()
        {
            return Some(ObjectCalcState::default());
        }
        let key = (self.layout, object.id, record, portal);
        let previous = self.calcs.states.borrow().get(&key).cloned();
        let visibility_only = previous
            .as_ref()
            .is_some_and(|cached| cached.visibility_stale);
        if let Some(cached) = previous.as_ref().filter(|cached| !cached.visibility_stale) {
            return Some(cached.calc.clone());
        }
        // Self, the object's content, is read once, before any of these
        // formulas runs, and only when one of them may evaluate it
        // (`Expr::may_read_self`): without that, a hidden summary over a
        // large found set is not computed (#1023). Visibility ignores
        // repetition selection, so a field's read takes fresh inputs even
        // when the displayed repetition was retained.
        let reads_self = [
            &calcs.hide,
            &calcs.tooltip,
            &calcs.placeholder,
            &calcs.label,
            &calcs.active_segment,
        ]
        .into_iter()
        .flatten()
        .chain(calcs.conditions.iter().filter_map(|c| c.formula.as_ref()))
        .chain(object.active_icon_formula())
        .any(|formula| formula.polish.may_read_self());
        let context = self.portal_context(record, portal);
        let (context, self_reads) = match object.field_spec() {
            _ if !reads_self => (context, HostReads::default()),
            Some(spec) => {
                let (value, reads) = self.read_field_value_with_reads(spec, record, portal)?;
                (context.with_self(value), reads)
            }
            None => (context.with_self(self_value()), HostReads::default()),
        };
        // `LayoutObjectUUID` in the object's calculations is the object's
        // UUID (calc-functions.md 6.3 O5).
        let uuid = object
            .uuid
            .as_ref()
            .map(om_data::host::layout_objects::uuid_text);
        let mut session = false;
        let visibility = match &calcs.hide {
            None => Visibility::Shown,
            Some(hide) => {
                let (holds, reads) = self.with_host_reads(|host| {
                    host.layout_object.clone_from(&uuid);
                    host.self_reads = self_reads;
                    layout::holds(host, hide, &context)
                })?;
                session |= reads.session;
                if reads.unknown {
                    Visibility::Unresolved
                } else if holds {
                    Visibility::Hidden
                } else {
                    Visibility::Shown
                }
            }
        };
        let (state, active_segment) = match previous
            .filter(|cached| visibility_only && cached.calc.visibility == visibility)
        {
            // A targeted refresh expired only the visibility; the shown
            // object keeps its other results.
            Some(cached) => {
                session |= cached.session;
                (cached.calc.state, cached.calc.active_segment)
            }
            None if visibility == Visibility::Shown => {
                let (state, reads) = self.with_host_reads(|host| {
                    host.layout_object.clone_from(&uuid);
                    host.self_reads = self_reads;
                    layout::display_state(host, object, &context)
                })?;
                session |= reads.session;
                // Its own reads: what it lacked leaves the segment
                // undetermined, not the bar's other calculations.
                let active_segment = match &calcs.active_segment {
                    None => None,
                    Some(formula) => {
                        let (name, reads) = self.with_host_reads(|host| {
                            host.layout_object = uuid;
                            host.self_reads = self_reads;
                            layout::text(host, formula, &context)
                        })?;
                        session |= reads.session;
                        Some(if reads.unknown {
                            SegmentName::Unresolved
                        } else {
                            SegmentName::Name(name)
                        })
                    }
                };
                (state, active_segment)
            }
            None => (
                ObjectState {
                    hidden: visibility == Visibility::Hidden,
                    ..ObjectState::default()
                },
                None,
            ),
        };
        let calc = ObjectCalcState {
            visibility,
            state,
            active_segment,
            kept: false,
        };
        if let Some(icon) = &calc.state.icon_svg {
            self.calcs.keep_icon(icon.len());
        }
        self.calcs.states.borrow_mut().insert(
            key,
            CachedObjectState {
                calc: calc.clone(),
                visibility_stale: false,
                session,
            },
        );
        Some(calc)
    }

    /// A web viewer's web address on `record`, as text: empty without an
    /// address, `None` while the data it reads is fetched.
    pub fn web_address(&self, object: &LayoutObject, record: u64) -> Option<String> {
        let Some(formula) = object.calcs.web_address.as_ref() else {
            return Some(String::new());
        };
        let context = self.portal_context(record, None);
        let uuid = object
            .uuid
            .as_ref()
            .map(om_data::host::layout_objects::uuid_text);
        self.with_host(|host| {
            host.layout_object = uuid;
            layout::text(host, formula, &context)
        })
    }

    /// Whether a field object's value comes from a calculation rather than
    /// the record page: a related field, a repetition past the first, an
    /// unstored calculation, a summary or a global field.
    pub fn computes(&self, spec: &FieldSpec) -> bool {
        let Some(occurrence) = om_layout::occurrence_id(spec.table_key) else {
            return false;
        };
        let Some(field) = spec.field_id() else {
            return false;
        };
        if occurrence != self.occurrence || spec.repetition > 1 {
            return self.schema.field(occurrence, field).is_some();
        }
        self.schema.field(occurrence, field).is_some_and(|f| {
            f.global || f.class == FieldClass::Summary || f.is_unstored_calculation()
        })
    }

    /// The value of a field object on `record`: its field, through the
    /// relationship graph when the field is related (the first related
    /// record). `None` while its data is being fetched.
    #[cfg(test)]
    pub fn field_value(&self, object: u64, spec: &FieldSpec, record: u64) -> Option<Value> {
        self.field_value_in_portal(object, spec, record, None)
    }

    pub fn field_value_in_portal(
        &self,
        object: u64,
        spec: &FieldSpec,
        record: u64,
        portal: Option<PortalRow>,
    ) -> Option<Value> {
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        let field = spec.field_id()?;
        self.schema.field(occurrence, field)?;
        let repetition = u32::try_from(spec.repetition.max(1)).unwrap_or(u32::MAX);
        if let Some(value) = self.band_summary(occurrence, field, repetition, record, portal) {
            return Some(value);
        }
        let key = (
            (self.layout, object, record, portal),
            spec.repetition.max(1),
        );
        if let Some((value, _)) = self.calcs.values.borrow().get(&key) {
            return Some(value.clone());
        }
        let Some((value, reads)) = self.read_field_value_with_reads(spec, record, portal) else {
            let last = self.calcs.last.borrow();
            return last.values.get(&key).map(|(value, _)| value.clone());
        };
        self.calcs
            .values
            .borrow_mut()
            .insert(key, (value.clone(), reads.session));
        Some(value)
    }

    fn read_field_value_with_reads(
        &self,
        spec: &FieldSpec,
        record: u64,
        portal: Option<PortalRow>,
    ) -> Option<(Value, HostReads)> {
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        let field = spec.field_id()?;
        self.schema.field(occurrence, field)?;
        let rep = u32::try_from(spec.repetition.max(1)).unwrap_or(1);
        let expression =
            om_data::formula::Expr::Field(om_data::formula::Field::new(occurrence, field, true));
        let context = self.portal_context(record, portal).with_repetition(rep);
        self.with_host_reads(|host| {
            om_data::calc::eval::evaluate(&expression, host, &context).value
        })
    }

    /// Whether a named column needs calculation instead of its stored page
    /// value. Stored editable columns keep using the record editor.
    pub fn computes_named(&self, name: &str) -> bool {
        self.schema
            .table_of(self.occurrence)
            .and_then(|table| table.fields.values().find(|field| field.name == name))
            .is_some_and(|field| {
                field.global
                    || field.class == FieldClass::Summary
                    || field.is_unstored_calculation()
            })
    }

    /// A calculated column in Table, generic Form/List, or Preview. Only
    /// the displayed record and field are read; there is no page-wide pass.
    pub fn named_field_value(&self, name: &str, record: u64) -> Option<Value> {
        let field = self
            .schema
            .table_of(self.occurrence)?
            .fields
            .values()
            .find(|field| field.name == name)?;
        if !(field.global || field.class == FieldClass::Summary || field.is_unstored_calculation())
        {
            return None;
        }
        let key = (self.layout, field.id, record);
        if let Some((value, _)) = self.calcs.named_values.borrow().get(&key) {
            return Some(value.clone());
        }
        let Some((value, reads)) = self.with_host_reads(|host| {
            om_data::host::read_field(host, self.occurrence, record, field.id, 1)
        }) else {
            let last = self.calcs.last.borrow();
            return last.named_values.get(&key).map(|(value, _)| value.clone());
        };
        self.calcs
            .named_values
            .borrow_mut()
            .insert(key, (value.clone(), reads.session));
        Some(value)
    }

    pub fn named_value_in_portal(
        &self,
        name: &str,
        parent: u64,
        portal: Option<PortalRow>,
    ) -> Option<Value> {
        use om_data::formula::Names;

        let name_in_text = name;
        let (occurrence, name) = match name.split_once("::") {
            Some((occurrence, field)) => (self.schema.names().find_occurrence(occurrence)?, field),
            None => (self.occurrence, name),
        };
        let names = self.schema.names();
        let (field, repetition) = match names.find_field(occurrence, name) {
            Some(field) => (field, 1),
            None => {
                let (name, repetition) = name.strip_suffix(']')?.rsplit_once('[')?;
                let repetition = repetition.parse::<u32>().ok().filter(|rep| *rep > 0)?;
                (names.find_field(occurrence, name)?, repetition)
            }
        };
        if let Some(value) = self.band_summary(occurrence, field, repetition, parent, portal) {
            return Some(value);
        }
        let key = (self.layout, parent, portal, name_in_text.to_owned());
        if let Some((value, _)) = self.calcs.merge_values.borrow().get(&key) {
            return Some(value.clone());
        }
        let expression =
            om_data::formula::Expr::Field(om_data::formula::Field::new(occurrence, field, true));
        let context = self
            .portal_context(parent, portal)
            .with_repetition(repetition);
        let Some((value, reads)) = self.with_host_reads(|host| {
            om_data::calc::eval::evaluate(&expression, host, &context).value
        }) else {
            let last = self.calcs.last.borrow();
            return last.merge_values.get(&key).map(|(value, _)| value.clone());
        };
        self.calcs
            .merge_values
            .borrow_mut()
            .insert(key, (value.clone(), reads.session));
        Some(value)
    }

    /// Layout text with its merge variables and symbols, `None` while their
    /// data is being fetched.
    pub fn merge_text_in_portal(
        &self,
        text: &str,
        record: Option<u64>,
        portal: Option<PortalRow>,
    ) -> Option<String> {
        if !text.contains("<<$") && !text.contains("{{") {
            return Some(text.to_owned());
        }
        let context = match record {
            Some(record) => self.portal_context(record, portal),
            None => Context {
                occurrence: Some(self.occurrence),
                ..Context::default()
            },
        };
        self.with_host(|host| layout::merge_text(host, text, &context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use om_data::calc::{Locale, ValueType};
    use om_data::formula::{self, Formula, Key};
    use om_data::host::FieldDef;
    use om_data::meta::ValueSource;

    const TABLE: u64 = 0x8001;
    const OCCURRENCE: u64 = 0xd00001;
    const LAYOUT_OCCURRENCE: u64 = 1065089;

    const ROW_TABLE: u64 = 0x8002;
    const ROW_OCCURRENCE: u64 = 0xd00002;
    const ROW_LAYOUT_OCCURRENCE: u64 = 1065090;

    fn formula(schema: &CalcSchema, source: &str) -> Formula {
        let mut context = formula::Context::new(schema.names());
        context.support_self = true;
        Formula {
            context: None,
            formatted: None,
            polish: formula::compile(source, &context).unwrap().expr,
        }
    }

    fn portal_schema() -> CalcSchema {
        let mut schema = CalcSchema::new("Portal rows", Locale::en_us());
        schema.add_table(TABLE, "Jobs");
        schema.add_occurrence(OCCURRENCE, "Jobs", TABLE);
        schema.add_field(TABLE, FieldDef::data(1, "Title", ValueType::Text));
        schema.add_table(ROW_TABLE, "Items");
        schema.add_occurrence(ROW_OCCURRENCE, "Items", ROW_TABLE);
        schema.add_field(ROW_TABLE, FieldDef::data(1, "Label", ValueType::Text));
        schema.add_field(ROW_TABLE, FieldDef::data(2, "Amount", ValueType::Number));
        schema.add_field(
            ROW_TABLE,
            FieldDef::calculation(
                3,
                "Description",
                ValueType::Text,
                formula(&schema, "Items::Label & \":\" & Items::Amount"),
                false,
            ),
        );
        schema
    }

    fn portal_data(calcs: &mut Calculations) {
        let data = calcs.cache.get_mut();
        for (table, record, text, amount) in [
            (TABLE, 1, "Parent", 0),
            (TABLE, 2, "Second parent", 0),
            (ROW_TABLE, 10, "Alder", 4),
            (ROW_TABLE, 20, "Birch", 0),
            (ROW_TABLE, 30, "Cedar", 6),
        ] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(text));
            values.set(2, 1, Value::number(amount));
            data.records.insert((table, record), Some(Arc::new(values)));
        }
        data.related
            .insert((OCCURRENCE, 1, ROW_OCCURRENCE), Arc::from([30, 10, 20]));
    }

    /// As FileMaker Pro 26.0.3 shows them (#695's reading): a portal leaves
    /// out a related record the provider marked as one the account's set
    /// may not view; a related field the set gives no access shows
    /// `<No Access>` in a row and placed on the layout; others do not.
    #[test]
    fn portals_leave_out_withheld_records_and_hide_fields_without_access() {
        use om_data::host::privilege_set::{
            AccessLevel, Actions, Allow, CustomRules, ItemAccess, MenuCommands, TableRules,
        };
        use om_data::host::security::SetAccess;
        let mut schema = portal_schema();
        // Items::Amount (field 2) without access.
        schema.authenticated_account.set_access = Some(SetAccess::Custom(Box::new(CustomRules {
            default: TableRules {
                actions: Actions {
                    create: Allow::Yes,
                    view: Allow::Yes,
                    edit: Allow::Yes,
                    delete: Allow::Yes,
                },
                fields: AccessLevel::Modifiable,
                field_levels: [(vec![2], AccessLevel::No)].into(),
            },
            tables: Default::default(),
            layouts: ItemAccess::default(),
            scripts: ItemAccess::default(),
            value_lists: ItemAccess::default(),
            prints: true,
            exports: true,
            menus: MenuCommands::All,
        })));
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        // Row 20: withheld by the host, marked by the provider.
        let withheld = RecordValues {
            no_access: true,
            ..RecordValues::default()
        };
        calcs
            .cache
            .get_mut()
            .records
            .insert((ROW_TABLE, 20), Some(Arc::new(withheld)));
        let evaluator = evaluator(&calcs, &schema);
        let field = |field_key| FieldSpec {
            table_key: ROW_LAYOUT_OCCURRENCE,
            field_key,
            ..Default::default()
        };
        let row = |record| PortalRow {
            occurrence: ROW_OCCURRENCE,
            record,
            number: 1,
        };
        // Rows 30, 10 and 20 in the relationship's order; 20 is left out.
        assert_eq!(
            &*evaluator.portal_records(&portal_object(), 1).unwrap(),
            &[30, 10]
        );
        let (label, amount) = (field(1), field(2));
        assert!(!evaluator.related_no_access(&label, Some(row(10))));
        assert!(evaluator.related_no_access(&amount, Some(row(10))));
        // Placed on the layout, outside the portal, and in the creation row.
        assert!(!evaluator.related_no_access(&label, None));
        assert!(evaluator.related_no_access(&amount, None));
        assert!(!evaluator.creation_field_denied(&label, ROW_OCCURRENCE));
        assert!(evaluator.creation_field_denied(&amount, ROW_OCCURRENCE));
    }

    /// A layout the account's set shows with its records without access
    /// (`d7`) is masked; one with its records modifiable (`df`) or view
    /// only (`db`) is not (#707).
    #[test]
    fn a_layout_whose_records_have_no_access_is_masked() {
        use om_data::host::privilege_set::{
            AccessLevel, Actions, Allow, CustomRules, ItemAccess, MenuCommands, Quad, TableRules,
        };
        use om_data::host::security::SetAccess;
        let calcs = Calculations::default();
        for (quad, masked) in [(0xd7, true), (0xdf, false), (0xdb, false)] {
            let mut schema = portal_schema();
            let mut layouts = ItemAccess::default();
            // The evaluator's layout, 1.
            layouts.items.insert(vec![1], Quad(quad));
            schema.authenticated_account.set_access =
                Some(SetAccess::Custom(Box::new(CustomRules {
                    default: TableRules {
                        actions: Actions {
                            create: Allow::Yes,
                            view: Allow::Yes,
                            edit: Allow::Yes,
                            delete: Allow::Yes,
                        },
                        fields: AccessLevel::Modifiable,
                        field_levels: Default::default(),
                    },
                    tables: Default::default(),
                    layouts,
                    scripts: ItemAccess::default(),
                    value_lists: ItemAccess::default(),
                    prints: true,
                    exports: true,
                    menus: MenuCommands::All,
                })));
            assert_eq!(
                evaluator(&calcs, &schema).layout_masked(),
                masked,
                "{quad:02x}"
            );
        }
        // No custom set: nothing is masked.
        assert!(!evaluator(&calcs, &portal_schema()).layout_masked());
    }

    fn evaluator<'a>(calcs: &'a Calculations, schema: &'a CalcSchema) -> Evaluator<'a> {
        // Each frame observes its draft (`Evaluator::new`): this one has
        // none.
        calcs.observe_draft(None);
        Evaluator {
            calcs,
            schema,
            info: SessionInfo::default(),
            layout: 1,
            occurrence: OCCURRENCE,
            today: Date::from_ymd(2026, 10, 29).unwrap(),
            locals: RefCell::new(Variables::new()),
            globals: RefCell::new(Variables::new()),
            overlays: Vec::new(),
            hosted: false,
            scans: RefCell::default(),
            summaries: SummaryCache::default(),
            group: Cell::new(None),
        }
    }

    #[test]
    fn completed_container_calculations_survive_body_eviction_across_frames() {
        use om_data::calc::eval::StoredContainer;
        use om_data::calc::{Container, StreamType};
        use om_data::containers::MemberInfo;
        let mut schema = portal_schema();
        schema.add_field(TABLE, FieldDef::data(3, "Photo", ValueType::Container));
        let length = formula(&schema, "Length ( Jobs::Photo )");
        schema.add_field(
            TABLE,
            FieldDef::calculation(4, "Bytes", ValueType::Number, length, false),
        );
        let mut calcs = Calculations::default();
        let bytes = 33 << 20;
        for record in [1, 2] {
            let member = vec![record as u8];
            let info = Arc::new(MemberInfo {
                name: format!("{record}.dat"),
                md5: [record as u8; 16],
                path_type: String::new(),
                owner: Some([9; 16]),
            });
            let mut values = RecordValues::default();
            values.set(3, 1, Value::Container(info.value(TABLE, &member)));
            calcs
                .cache
                .get_mut()
                .records
                .insert((TABLE, record), Some(Arc::new(values)));
            assert!(evaluator(&calcs, &schema)
                .named_field_value("Bytes", record)
                .is_none());
            let needs = calcs.take_wanted();
            assert_eq!(
                needs,
                [CalcNeed::MemberData {
                    table: TABLE,
                    member: member.clone()
                }]
            );
            let mut answer = CalcData::default();
            answer.library.data.insert(
                (TABLE, member),
                Some(Arc::new(StoredContainer {
                    data: Container {
                        streams: vec![(StreamType(*b"FILE"), vec![1; bytes])],
                        ..Container::default()
                    },
                    external: false,
                    intact: true,
                })),
            );
            calcs.accept_data(&needs, Ok(answer));
            let evaluation = evaluator(&calcs, &schema);
            assert_eq!(
                evaluation.display(&evaluation.named_field_value("Bytes", record).unwrap()),
                bytes.to_string()
            );
        }
        assert_eq!(
            calcs.cache.borrow().library.data.len(),
            1,
            "the real byte budget evicted the first body"
        );
        for _ in 0..8 {
            for record in [1, 2] {
                let evaluation = evaluator(&calcs, &schema);
                assert_eq!(
                    evaluation.display(&evaluation.named_field_value("Bytes", record).unwrap()),
                    bytes.to_string()
                );
            }
            assert!(
                calcs.take_wanted().is_empty(),
                "completed frames must not refetch evicted bodies"
            );
        }
        // A record/data change still invalidates the completed result: it
        // is read again, its last value drawn meanwhile.
        calcs.data_changed();
        {
            let evaluation = evaluator(&calcs, &schema);
            assert_eq!(
                evaluation.display(&evaluation.named_field_value("Bytes", 1).unwrap()),
                bytes.to_string()
            );
        }
        assert_eq!(
            calcs.take_wanted(),
            [CalcNeed::Record {
                table: TABLE,
                record: 1
            }]
        );
    }

    /// A need left unanswered while a related file's host could not be
    /// reached (#740) is not failed: it waits until the answer's retry time,
    /// when the window asks for it again.
    #[test]
    fn a_need_waiting_for_an_unreachable_host_is_asked_again_later() {
        let mut schema = portal_schema();
        schema.add_field(TABLE, FieldDef::data(3, "Photo", ValueType::Container));
        let calculation = formula(&schema, "Length ( Jobs::Photo )");
        schema.add_field(
            TABLE,
            FieldDef::calculation(4, "Bytes", ValueType::Number, calculation, false),
        );
        let mut calcs = Calculations::default();
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Bytes", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert!(!needs.is_empty());
        let waiting = |after| CalcData {
            retry_after: Some(after),
            ..CalcData::default()
        };
        assert_eq!(
            calcs.accept_data(&needs, Ok(waiting(std::time::Duration::from_secs(3600)))),
            None
        );
        assert!(calcs.retry_at().is_some());
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Bytes", 1)
            .is_none());
        assert!(calcs.take_wanted().is_empty(), "not before its time");
        calcs.retry_at = Some(std::time::Instant::now());
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Bytes", 1)
            .is_none());
        assert_eq!(calcs.take_wanted(), needs, "asked again, not failed");
        assert_eq!(calcs.retry_at(), None);
        // Not sent (the connection is gone): not awaited, and asked again.
        calcs.unsent(needs.clone());
        assert!(calcs.requested.is_empty());
        assert_eq!(calcs.take_wanted(), needs, "asked once it can be sent");
        assert!(calcs.awaiting_data());
    }

    /// A read the window no longer needs stops (#987): the token stays the
    /// same while the window's context does and is cancelled when it
    /// changes; a superseded answer applies and reports nothing, and its
    /// needs are asked again, not taken as failed.
    #[test]
    fn a_superseded_read_is_asked_again_not_failed() {
        let mut schema = portal_schema();
        schema.add_field(TABLE, FieldDef::data(3, "Photo", ValueType::Container));
        let calculation = formula(&schema, "Length ( Jobs::Photo )");
        schema.add_field(
            TABLE,
            FieldDef::calculation(4, "Bytes", ValueType::Number, calculation, false),
        );
        let mut calcs = Calculations::default();
        let list = calcs.reads_for(1);
        assert!(!calcs.reads_for(1).cancelled());
        let form = calcs.reads_for(2);
        assert!(list.cancelled() && !form.cancelled());
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Bytes", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::Record {
                table: TABLE,
                record: 1
            }]
        );
        assert_eq!(calcs.accept_data(&needs, Ok(CalcData::superseded())), None);
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Bytes", 1)
            .is_none());
        assert_eq!(calcs.take_wanted(), needs);
    }

    #[test]
    fn sequential_container_branches_finish_when_their_working_set_exceeds_the_cache_budget() {
        use om_data::calc::eval::StoredContainer;
        use om_data::calc::{Container, StreamType};
        use om_data::containers::MemberInfo;
        let mut schema = portal_schema();
        schema.add_field(TABLE, FieldDef::data(3, "Photo", ValueType::Container));
        schema.add_field(TABLE, FieldDef::data(5, "Other", ValueType::Container));
        let calculation = formula(
            &schema,
            "If ( IsValid ( Length ( Jobs::Photo ) ) ; Length ( Jobs::Other ) ; 0 )",
        );
        schema.add_field(
            TABLE,
            FieldDef::calculation(4, "Bytes", ValueType::Number, calculation, false),
        );
        let mut values = RecordValues::default();
        for (field, member) in [(3, 1), (5, 2)] {
            let info = MemberInfo {
                name: format!("{member}.dat"),
                md5: [member; 16],
                path_type: String::new(),
                owner: None,
            };
            values.set(field, 1, Value::Container(info.value(TABLE, &[member])));
        }
        let values = Arc::new(values);
        let mut calcs = Calculations::default();
        calcs
            .cache
            .get_mut()
            .records
            .insert((TABLE, 1), Some(Arc::clone(&values)));
        let bytes = 33 << 20;
        for member in [1, 2] {
            assert!(evaluator(&calcs, &schema)
                .named_field_value("Bytes", 1)
                .is_none());
            let needs = calcs.take_wanted();
            assert_eq!(
                needs,
                [CalcNeed::MemberData {
                    table: TABLE,
                    member: vec![member]
                }]
            );
            let mut answer = CalcData::default();
            answer.library.data.insert(
                (TABLE, vec![member]),
                Some(Arc::new(StoredContainer {
                    data: Container {
                        streams: vec![(StreamType(*b"FILE"), vec![member; bytes])],
                        ..Default::default()
                    },
                    intact: true,
                    external: false,
                })),
            );
            calcs.accept_data(&needs, Ok(answer));
        }
        let evaluation = evaluator(&calcs, &schema);
        assert_eq!(
            evaluation.display(
                &evaluation
                    .named_field_value("Bytes", 1)
                    .expect("the pending branch must complete")
            ),
            bytes.to_string()
        );
        assert!(calcs.take_wanted().is_empty());
        // An edited record's results are evaluated again whenever its
        // draft changes. Its active working set must also remain available
        // without another fetch.
        for frame in 0..4 {
            let mut evaluation = evaluator(&calcs, &schema);
            evaluation.overlays = vec![(TABLE, 1, Arc::clone(&values))];
            calcs.observe_draft(Some(draft_context(&format!("draft {frame}"))));
            assert_eq!(
                evaluation.display(&evaluation.named_field_value("Bytes", 1).unwrap()),
                bytes.to_string()
            );
            assert!(calcs.take_wanted().is_empty());
        }
    }

    fn row(record: u64) -> Option<PortalRow> {
        Some(PortalRow {
            occurrence: ROW_OCCURRENCE,
            record,
            number: record as usize / 10,
        })
    }

    fn portal_object() -> LayoutObject {
        LayoutObject {
            id: 100,
            portal: Some(om_layout::PortalObject {
                table_alias_key: ROW_LAYOUT_OCCURRENCE,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn refresh_session(objects: Vec<LayoutObject>) -> Session {
        let mut session = Session::new(
            crate::session::Target {
                host: "synthetic".into(),
                file: "Refresh".into(),
                local: None,
            },
            10,
        );
        session.layout = Some(1);
        session.render = RenderState::Loaded(Box::new(om_data::RenderLayout {
            layout: om_layout::Layout {
                id: 1,
                objects: om_layout::ObjectStore {
                    objects,
                    ..Default::default()
                },
                ..Default::default()
            },
            field_bindings: Default::default(),
        }));
        session
    }

    #[test]
    fn targeted_repetition_keeps_other_contents_but_refreshes_visibility_and_formatting() {
        use crate::scripting::ObjectRefresh;
        let mut schema = portal_schema();
        let mut title = FieldDef::data(1, "Title", ValueType::Text);
        title.repetitions = 2;
        schema.add_field(TABLE, title);
        let mut object = LayoutObject {
            id: 5,
            field_obj: Some(om_layout::FieldObject {
                field_spec: Some(FieldSpec {
                    table_key: LAYOUT_OCCURRENCE,
                    field_key: 1,
                    repetition: 2,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        object.calcs.hide = Some(formula(&schema, "Self = \"New repetition\""));
        object
            .calcs
            .conditions
            .push(om_layout::calcs::ConditionalFormat {
                formula: Some(formula(&schema, "Jobs::Title = \"Fresh\"")),
                ..Default::default()
            });
        let mut other = object.clone();
        other.id = 6;
        let mut formatted = object.clone();
        formatted.id = 7;
        formatted.calcs.hide = None;
        formatted
            .field_obj
            .as_mut()
            .unwrap()
            .field_spec
            .as_mut()
            .unwrap()
            .repetition = 1;
        let session = refresh_session(vec![object.clone(), other.clone(), formatted.clone()]);
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        Arc::make_mut(
            calcs
                .cache
                .get_mut()
                .records
                .get_mut(&(TABLE, 1))
                .unwrap()
                .as_mut()
                .unwrap(),
        )
        .set(1, 2, Value::text("Old repetition"));
        let eval = evaluator(&calcs, &schema);
        let value = |eval: &Evaluator<'_>, object: &LayoutObject| {
            eval.field_value(object.id, object.field_spec().unwrap(), 1)
                .unwrap()
        };
        assert_eq!(eval.display(&value(&eval, &object)), "Old repetition");
        assert!(
            !eval
                .object_state_in_portal(&object, 1, None, || value(&eval, &object))
                .unwrap()
                .state
                .hidden
        );
        assert_eq!(eval.display(&value(&eval, &other)), "Old repetition");
        assert!(eval
            .object_state_in_portal(&formatted, 1, None, || value(&eval, &formatted))
            .unwrap()
            .state
            .conditions
            .is_empty());
        let mut fresh = calcs.cache.borrow().clone();
        let row = Arc::make_mut(
            fresh
                .records
                .get_mut(&(TABLE, 1))
                .unwrap()
                .as_mut()
                .unwrap(),
        );
        row.set(1, 1, Value::text("Fresh"));
        row.set(1, 2, Value::text("New repetition"));
        fresh.generation = Some(2);
        let mut refresh = ObjectRefresh {
            layout: 1,
            objects: vec![5, 7],
            repetition: 1,
            join: None,
            generation: Some(2),
        };
        calcs.refresh_object(&refresh, &session);
        calcs.accept_data(
            &[CalcNeed::Record {
                table: TABLE,
                record: 1,
            }],
            Ok(fresh.clone()),
        );
        let eval = evaluator(&calcs, &schema);
        let state = eval
            .object_state_in_portal(&object, 1, None, || value(&eval, &object))
            .unwrap()
            .state;
        assert!(state.hidden);
        assert_eq!(
            eval.object_state_in_portal(&formatted, 1, None, || value(&eval, &formatted))
                .unwrap()
                .state
                .conditions,
            [0]
        );
        assert_eq!(
            eval.display(&value(&eval, &object)),
            "Old repetition",
            "only repetition 1 was selected"
        );
        refresh.repetition = 0;
        calcs.refresh_object(&refresh, &session);
        calcs.accept_data(
            &[CalcNeed::Record {
                table: TABLE,
                record: 1,
            }],
            Ok(fresh),
        );
        let eval = evaluator(&calcs, &schema);
        assert!(eval.refreshed_value(5));
        assert_eq!(eval.display(&value(&eval, &object)), "New repetition");
        assert_eq!(
            eval.display(&value(&eval, &other)),
            "Old repetition",
            "unrelated object retains its complete display result"
        );
    }

    #[test]
    fn targeted_portal_rebuilds_filter_and_all_row_contents_without_invalidating_other_portals() {
        use crate::scripting::ObjectRefresh;
        let mut schema = portal_schema();
        schema.add_field(
            TABLE,
            FieldDef {
                global: true,
                ..FieldDef::data(4, "Filter", ValueType::Number)
            },
        );
        let mut portal = portal_object();
        portal.calcs.portal_filter = Some(formula(&schema, "Items::Amount >= Jobs::Filter"));
        let mut other = portal_object();
        other.id = 101;
        let field = LayoutObject {
            id: 5,
            parent: Some(100),
            field_obj: Some(om_layout::FieldObject {
                field_spec: Some(FieldSpec {
                    table_key: ROW_LAYOUT_OCCURRENCE,
                    field_key: 1,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let session = refresh_session(vec![portal.clone(), other.clone(), field.clone()]);
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        let mut globals = RecordValues::default();
        globals.set(4, 1, Value::number(5));
        calcs
            .cache
            .get_mut()
            .globals
            .insert(TABLE, Arc::new(globals));
        calcs.cache.get_mut().generation = Some(1);
        let eval = evaluator(&calcs, &schema);
        assert_eq!(&*eval.portal_records(&portal, 1).unwrap(), &[30]);
        assert_eq!(&*eval.portal_records(&other, 1).unwrap(), &[30, 10, 20]);
        for id in [10, 20] {
            assert!(eval
                .field_value_in_portal(5, field.field_spec().unwrap(), 1, row(id))
                .is_some());
        }
        let mut fresh = calcs.cache.borrow().clone();
        fresh.generation = Some(2);
        fresh
            .related
            .insert((OCCURRENCE, 1, ROW_OCCURRENCE), Arc::from([20, 10]));
        Arc::make_mut(fresh.globals.get_mut(&TABLE).unwrap()).set(4, 1, Value::number(0));
        for id in [10, 20] {
            Arc::make_mut(
                fresh
                    .records
                    .get_mut(&(ROW_TABLE, id))
                    .unwrap()
                    .as_mut()
                    .unwrap(),
            )
            .set(1, 1, Value::text(format!("Fresh {id}")));
        }
        calcs.refresh_object(
            &ObjectRefresh {
                layout: 1,
                objects: vec![100, 5],
                repetition: 0,
                join: Some((OCCURRENCE, 1, ROW_OCCURRENCE)),
                generation: Some(2),
            },
            &session,
        );
        assert!(evaluator(&calcs, &schema)
            .portal_records(&portal, 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::RelatedIds {
                from: OCCURRENCE,
                record: 1,
                to: ROW_OCCURRENCE
            }]
        );
        let mut stale = fresh.clone();
        stale.generation = Some(1);
        calcs.accept_data(&needs, Ok(stale));
        assert!(evaluator(&calcs, &schema)
            .portal_records(&portal, 1)
            .is_none());
        let needs = calcs.take_wanted();
        calcs.accept_data(&needs, Ok(fresh));
        let eval = evaluator(&calcs, &schema);
        assert_eq!(&*eval.portal_records(&portal, 1).unwrap(), &[20, 10]);
        assert_eq!(&*eval.portal_records(&other, 1).unwrap(), &[30, 10, 20]);
        for id in [10, 20] {
            assert_eq!(
                eval.display(
                    &eval
                        .field_value_in_portal(5, field.field_spec().unwrap(), 1, row(id))
                        .unwrap()
                ),
                format!("Fresh {id}")
            );
        }
    }

    #[test]
    fn portal_creation_hint_requires_the_resolved_direct_relationship_side() {
        let calcs = Calculations::default();
        let portal = portal_object();
        for (left, right, options, expected) in [
            (OCCURRENCE, ROW_OCCURRENCE, 0x10, true),
            (ROW_OCCURRENCE, OCCURRENCE, 0x20, true),
            (OCCURRENCE, ROW_OCCURRENCE, 0x20, false),
            (ROW_OCCURRENCE, OCCURRENCE, 0x10, false),
            (OCCURRENCE, ROW_OCCURRENCE, 0, false),
        ] {
            let mut schema = portal_schema();
            let mut relationship = om_data::host::RelationshipDef::new(1, left, right, vec![]);
            relationship.options = options;
            schema.add_relationship(relationship);
            assert_eq!(
                evaluator(&calcs, &schema).portal_allows_creation(&portal),
                expected
            );
        }
        let mut schema = portal_schema();
        const MIDDLE: u64 = 0xd00003;
        schema.add_occurrence(MIDDLE, "Middle", TABLE);
        schema.add_relationship(om_data::host::RelationshipDef::new(
            1,
            OCCURRENCE,
            MIDDLE,
            vec![],
        ));
        let mut last = om_data::host::RelationshipDef::new(2, MIDDLE, ROW_OCCURRENCE, vec![]);
        last.options = 0x10;
        schema.add_relationship(last);
        let evaluator = evaluator(&calcs, &schema);
        assert!(!evaluator.portal_allows_creation(&portal));
        for table_alias_key in [LAYOUT_OCCURRENCE, 1065091, 0] {
            let mut other = portal.clone();
            other.portal.as_mut().unwrap().table_alias_key = table_alias_key;
            assert!(!evaluator.portal_allows_creation(&other));
        }
        assert!(calcs.wanted.borrow().is_empty());
    }

    #[test]
    fn unstored_heading_completes_after_its_current_record_arrives() {
        let mut schema = portal_schema();
        let heading = formula(
            &schema,
            "If ( IsEmpty ( Jobs::Title ) ; \"Description\" ; Jobs::Title )",
        );
        schema.add_field(
            TABLE,
            FieldDef {
                // The native empty-input option remains enabled. IsEmpty
                // itself forces evaluation of the fallback heading.
                options: 0,
                ..FieldDef::calculation(4, "Heading", ValueType::Text, heading, false)
            },
        );
        let mut calcs = Calculations::default();
        let spec = FieldSpec {
            table_key: LAYOUT_OCCURRENCE,
            field_key: 4,
            ..Default::default()
        };
        assert!(evaluator(&calcs, &schema)
            .field_value(140, &spec, 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::Record {
                table: TABLE,
                record: 1
            }]
        );
        let mut data = CalcData::default();
        data.records
            .insert((TABLE, 1), Some(Arc::new(RecordValues::default())));
        calcs.accept_data(&needs, Ok(data));
        let evaluator = evaluator(&calcs, &schema);
        let value = evaluator.field_value(140, &spec, 1).unwrap();
        assert_eq!(evaluator.display(&value), "Description");
        assert!(calcs.wanted.borrow().is_empty());
        assert_eq!(
            evaluator.display(&evaluator.field_value(140, &spec, 1).unwrap()),
            "Description"
        );
    }

    #[test]
    fn portal_summary_fields_keep_the_parent_related_set() {
        let mut schema = portal_schema();
        schema.add_field(
            ROW_TABLE,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(om_data::host::SummaryDef::new(
                    om_data::host::SummaryKind::Total,
                    2,
                )),
                ..FieldDef::data(4, "Total", ValueType::Number)
            },
        );
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        // A separate window's found set for Items has a different total.
        calcs
            .cache
            .get_mut()
            .found
            .insert(ROW_OCCURRENCE, Arc::from([20]));
        let evaluator = evaluator(&calcs, &schema);
        let spec = FieldSpec {
            table_key: ROW_LAYOUT_OCCURRENCE,
            field_key: 4,
            ..Default::default()
        };
        for portal in [None, row(10), row(20)] {
            let value = evaluator
                .field_value_in_portal(140, &spec, 1, portal)
                .unwrap();
            assert_eq!(evaluator.display(&value), "10");
        }
        assert!(calcs.wanted.borrow().is_empty());
    }

    #[test]
    fn portal_fields_and_calculations_keep_each_rows_record_and_parent() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        let evaluator = evaluator(&calcs, &schema);
        let spec = FieldSpec {
            table_key: ROW_LAYOUT_OCCURRENCE,
            field_key: 1,
            ..Default::default()
        };
        for (record, expected) in [(10, "Alder"), (20, "Birch"), (10, "Alder")] {
            let value = evaluator
                .field_value_in_portal(5, &spec, 1, row(record))
                .unwrap();
            assert_eq!(evaluator.display(&value), expected);
        }
        let computed = FieldSpec {
            field_key: 3,
            ..spec
        };
        for (record, expected) in [(10, "Alder:4"), (20, "Birch:0")] {
            let value = evaluator
                .field_value_in_portal(6, &computed, 1, row(record))
                .unwrap();
            assert_eq!(evaluator.display(&value), expected);
        }
        let object = LayoutObject {
            id: 5,
            calcs: om_layout::ObjectCalcs {
                hide: Some(formula(&schema, "Items::Amount = 0")),
                tooltip: Some(formula(&schema, "Jobs::Title & \"/\" & Items::Label")),
                conditions: vec![om_layout::ConditionalFormat {
                    formula: Some(formula(&schema, "Self = \"Alder\"")),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let state = evaluator
            .object_state_in_portal(&object, 1, row(10), || Value::text("Alder"))
            .unwrap()
            .state;
        assert!(!state.hidden);
        assert_eq!(state.tooltip.as_deref(), Some("Parent/Alder"));
        assert_eq!(state.conditions, [0]);
        assert!(
            evaluator
                .object_state_in_portal(&object, 1, row(20), || Value::text("Birch"))
                .unwrap()
                .state
                .hidden
        );
        assert!(calcs.wanted.borrow().is_empty());
    }

    #[test]
    fn a_portal_of_the_layout_occurrence_reads_its_row_instead_of_the_parent() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        let evaluator = evaluator(&calcs, &schema);
        let row = Some(PortalRow {
            occurrence: OCCURRENCE,
            record: 2,
            number: 1,
        });
        let spec = FieldSpec {
            table_key: LAYOUT_OCCURRENCE,
            field_key: 1,
            ..Default::default()
        };
        let value = evaluator.field_value_in_portal(5, &spec, 1, row).unwrap();
        assert_eq!(evaluator.display(&value), "Second parent");
        let object = LayoutObject {
            id: 8,
            calcs: om_layout::ObjectCalcs {
                label: Some(formula(&schema, "Jobs::Title")),
                ..Default::default()
            },
            ..Default::default()
        };
        let state = evaluator
            .object_state_in_portal(&object, 1, row, || Value::empty(ValueType::Text))
            .unwrap()
            .state;
        assert_eq!(state.label.as_deref(), Some("Second parent"));
    }

    #[test]
    fn portal_filters_rows_in_source_order_with_row_context() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        let evaluator = evaluator(&calcs, &schema);
        let mut portal = portal_object();
        portal.calcs.portal_filter = Some(formula(
            &schema,
            "Items::Amount > 0 and Jobs::Title = \"Parent\"",
        ));
        assert_eq!(&*evaluator.portal_records(&portal, 1).unwrap(), &[30, 10]);
        assert!(calcs.wanted.borrow().is_empty());
    }

    #[test]
    fn portal_sort_order_overrides_relationship_order_before_filtering() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        let evaluator = evaluator(&calcs, &schema);
        let mut portal = portal_object();
        portal.calcs.portal_sort = Some(label_sort());
        // A preserved sort spec does not enable sorting when its flag is off.
        assert_eq!(
            &*evaluator.portal_records(&portal, 1).unwrap(),
            &[30, 10, 20]
        );
        calcs.forget_results();
        portal.portal.as_mut().unwrap().flags = 0x8;
        assert_eq!(
            &*evaluator.portal_records(&portal, 1).unwrap(),
            &[10, 20, 30]
        );
        calcs.forget_results();
        portal.calcs.portal_filter = Some(formula(&schema, "Items::Amount > 0"));
        assert_eq!(&*evaluator.portal_records(&portal, 1).unwrap(), &[10, 30]);
    }

    #[test]
    fn sorted_portals_batch_all_missing_sort_rows_in_one_request() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        calcs
            .cache
            .get_mut()
            .related
            .insert((OCCURRENCE, 1, ROW_OCCURRENCE), (1..=1000).collect());
        let evaluator = evaluator(&calcs, &schema);
        let mut portal = portal_object();
        portal.portal.as_mut().unwrap().flags = 0x8;
        portal.calcs.portal_sort = Some(label_sort());
        assert!(evaluator.portal_records(&portal, 1).is_none());
        let wanted = calcs.wanted.borrow();
        assert_eq!(wanted.len(), 1000);
        assert!(wanted.contains(&CalcNeed::Record {
            table: ROW_TABLE,
            record: 1
        }));
        assert!(wanted.contains(&CalcNeed::Record {
            table: ROW_TABLE,
            record: 1000
        }));
    }

    #[test]
    fn simple_portal_discovers_ids_then_batches_only_visible_row_needs() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        let portal = portal_object();
        assert!(evaluator(&calcs, &schema)
            .portal_records(&portal, 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::RelatedIds {
                from: OCCURRENCE,
                record: 1,
                to: ROW_OCCURRENCE
            }]
        );
        let mut answer = CalcData::default();
        answer
            .related_ids
            .insert((OCCURRENCE, 1, ROW_OCCURRENCE), (1..=1000).collect());
        calcs.accept_data(&needs, Ok(answer));
        let eval = evaluator(&calcs, &schema);
        assert_eq!(eval.portal_records(&portal, 1).unwrap().len(), 1000);
        assert!(
            calcs.wanted.borrow().is_empty(),
            "ID discovery never asks for offscreen rows"
        );
        let spec = FieldSpec {
            table_key: ROW_LAYOUT_OCCURRENCE,
            field_key: 1,
            ..Default::default()
        };
        for record in [1, 2] {
            let row = Some(PortalRow {
                occurrence: ROW_OCCURRENCE,
                record,
                number: record as usize,
            });
            assert!(eval.field_value_in_portal(5, &spec, 1, row).is_none());
        }
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [1, 2].map(|record| CalcNeed::Record {
                table: ROW_TABLE,
                record
            })
        );
        let mut answer = CalcData::default();
        for record in [1, 2] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(format!("Visible {record}")));
            answer
                .records
                .insert((ROW_TABLE, record), Some(Arc::new(values)));
        }
        calcs.accept_data(&needs, Ok(answer));
        let eval = evaluator(&calcs, &schema);
        assert_eq!(eval.portal_records(&portal, 1).unwrap().len(), 1000);
        assert_eq!(
            eval.display(
                &eval
                    .field_value_in_portal(
                        5,
                        &spec,
                        1,
                        Some(PortalRow {
                            occurrence: ROW_OCCURRENCE,
                            record: 2,
                            number: 2
                        })
                    )
                    .unwrap()
            ),
            "Visible 2"
        );
        assert!(calcs.wanted.borrow().is_empty());
    }

    #[test]
    fn relationship_membership_follows_parent_drafts_and_reverts() {
        let mut schema = portal_schema();
        schema.add_relationship(om_data::host::RelationshipDef::new(
            1,
            OCCURRENCE,
            ROW_OCCURRENCE,
            vec![om_data::host::JoinPredicate {
                left: 1,
                right: 1,
                op: om_data::host::JoinOp::Equal,
            }],
        ));
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        let data = calcs.cache.get_mut();
        let mut parent = RecordValues::default();
        parent.set(1, 1, Value::text("Alder"));
        data.records.insert((TABLE, 1), Some(Arc::new(parent)));
        data.tables.insert(ROW_TABLE, Arc::from([30, 10, 20]));
        data.globals.insert(TABLE, Arc::default());
        data.globals.insert(ROW_TABLE, Arc::default());
        data.related.clear();
        data.related_ids
            .insert((OCCURRENCE, 1, ROW_OCCURRENCE), Arc::from([10]));
        let portal = portal_object();
        assert_eq!(
            &*evaluator(&calcs, &schema)
                .portal_records(&portal, 1)
                .unwrap(),
            &[10]
        );
        for (draft, expected) in [
            ("Birch", vec![20]),
            ("Missing", vec![]),
            ("Cedar", vec![30]),
        ] {
            let eval = draft_evaluator(&calcs, &schema, draft);
            assert_eq!(&*eval.portal_records(&portal, 1).unwrap(), &expected);
            assert!(calcs
                .wanted
                .borrow()
                .iter()
                .all(|need| matches!(need, CalcNeed::Globals { .. })));
            calcs.wanted.borrow_mut().clear();
        }
        assert_eq!(
            &*evaluator(&calcs, &schema)
                .portal_records(&portal, 1)
                .unwrap(),
            &[10]
        );
    }

    #[test]
    fn schema_relationship_sort_refresh_rebuilds_the_displayed_portal_order() {
        use om_data::host::{JoinOp, JoinPredicate, MemorySource, Recorder, RelationshipDef};
        let mut source = MemorySource::new();
        for (record, label) in [(30, "Cedar"), (10, "Alder"), (20, "Birch")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(label));
            source.put_record(ROW_TABLE, record, values);
        }
        let mut calcs = Calculations::default();
        let portal = portal_object();
        for (sorted, expected) in [(false, vec![30, 10, 20]), (true, vec![10, 20, 30])] {
            let mut schema = portal_schema();
            let mut relation = RelationshipDef::new(
                1,
                OCCURRENCE,
                ROW_OCCURRENCE,
                vec![JoinPredicate {
                    left: 1,
                    right: 1,
                    op: JoinOp::Cartesian,
                }],
            );
            if sorted {
                relation.sort_right = om_data::specs::SortSpec::decode(&label_sort());
            }
            schema.add_relationship(relation);
            calcs.set_schema(Some(Arc::new(schema.clone())));
            assert!(evaluator(&calcs, &schema)
                .portal_records(&portal, 1)
                .is_none());
            let needs = calcs.take_wanted();
            assert_eq!(
                needs,
                [CalcNeed::RelatedIds {
                    from: OCCURRENCE,
                    record: 1,
                    to: ROW_OCCURRENCE
                }]
            );
            let recorder = Recorder::new(&source);
            let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 1, 1).unwrap());
            for need in &needs {
                recorder.fetch(&schema, need, &cx).unwrap();
            }
            calcs.accept_data(&needs, Ok(recorder.take()));
            assert_eq!(
                &*evaluator(&calcs, &schema)
                    .portal_records(&portal, 1)
                    .unwrap(),
                &expected
            );
        }
    }

    #[test]
    fn a_draft_related_target_updates_other_parents_without_surviving_revert() {
        let mut schema = portal_schema();
        schema.add_occurrence(ROW_OCCURRENCE, "Items", TABLE);
        schema.add_relationship(om_data::host::RelationshipDef::new(
            1,
            OCCURRENCE,
            ROW_OCCURRENCE,
            vec![om_data::host::JoinPredicate {
                left: 1,
                right: 1,
                op: om_data::host::JoinOp::Equal,
            }],
        ));
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        let data = calcs.cache.get_mut();
        data.tables.insert(TABLE, Arc::from([1, 2]));
        data.globals.insert(TABLE, Arc::default());
        data.related.clear();
        data.related_ids
            .insert((OCCURRENCE, 2, ROW_OCCURRENCE), Arc::from([2]));
        let portal = portal_object();
        assert_eq!(
            &*evaluator(&calcs, &schema)
                .portal_records(&portal, 2)
                .unwrap(),
            &[2]
        );
        assert_eq!(
            &*draft_evaluator(&calcs, &schema, "Second parent")
                .portal_records(&portal, 2)
                .unwrap(),
            &[1, 2]
        );
        assert_eq!(
            &*evaluator(&calcs, &schema)
                .portal_records(&portal, 2)
                .unwrap(),
            &[2]
        );
        assert!(calcs.wanted.borrow().is_empty());
    }

    #[test]
    fn notification_generations_rebuild_displayed_sets_in_each_window() {
        let schema = portal_schema();
        let portal = portal_object();
        // A second window may miss the first notification-bearing answer.
        for trigger in [
            CalcNeed::Record {
                table: ROW_TABLE,
                record: 30,
            },
            CalcNeed::Member {
                table: ROW_TABLE,
                member: vec![42],
            },
        ] {
            let mut calcs = Calculations::default();
            portal_data(&mut calcs);
            calcs.cache.get_mut().generation = Some(1);
            assert_eq!(
                &*evaluator(&calcs, &schema)
                    .portal_records(&portal, 1)
                    .unwrap(),
                &[30, 10, 20]
            );
            let mut fresh = CalcData {
                generation: Some(2),
                ..Default::default()
            };
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text("Updated"));
            fresh
                .records
                .insert((ROW_TABLE, 30), Some(Arc::new(values)));
            fresh.library.members.insert((ROW_TABLE, vec![42]), None);
            calcs.accept_data(&[trigger], Ok(fresh));
            assert!(evaluator(&calcs, &schema)
                .portal_records(&portal, 1)
                .is_none());
            let needs = calcs.take_wanted();
            assert_eq!(
                needs,
                [CalcNeed::RelatedIds {
                    from: OCCURRENCE,
                    record: 1,
                    to: ROW_OCCURRENCE
                }]
            );
            let mut fresh = CalcData {
                generation: Some(2),
                ..Default::default()
            };
            fresh
                .related_ids
                .insert((OCCURRENCE, 1, ROW_OCCURRENCE), Arc::from([30]));
            calcs.accept_data(&needs, Ok(fresh));
            let eval = evaluator(&calcs, &schema);
            assert_eq!(&*eval.portal_records(&portal, 1).unwrap(), &[30]);
            let spec = FieldSpec {
                table_key: ROW_LAYOUT_OCCURRENCE,
                field_key: 1,
                ..Default::default()
            };
            assert_eq!(
                eval.display(&eval.field_value_in_portal(5, &spec, 1, row(30)).unwrap()),
                "Updated"
            );
            assert!(calcs.wanted.borrow().is_empty());
        }
        // A record page read under the generation the data was read under
        // keeps it; one under another generation drops it, as a window
        // whose every input is cached asks nothing else that could (#1042).
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        calcs.cache.get_mut().generation = Some(1);
        calcs.page_read(Some(1), Some(ROW_TABLE), &[]);
        assert!(evaluator(&calcs, &schema)
            .portal_records(&portal, 1)
            .is_some());
        calcs.page_read(Some(2), Some(ROW_TABLE), &[]);
        assert!(evaluator(&calcs, &schema)
            .portal_records(&portal, 1)
            .is_none());
    }

    fn label_sort() -> Vec<u8> {
        om_data::specs::SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![om_data::specs::SortEntry {
                field: om_data::specs::SpecField {
                    occurrence: Some(ROW_OCCURRENCE),
                    field: 1,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }],
        }
        .encode()
    }

    /// The evaluator of a frame in which parent record 1 has a draft.
    fn draft_evaluator<'a>(
        calcs: &'a Calculations,
        schema: &'a CalcSchema,
        title: &str,
    ) -> Evaluator<'a> {
        let mut values = RecordValues::default();
        values.set(1, 1, Value::text(title));
        let evaluator = Evaluator {
            overlays: vec![(TABLE, 1, Arc::new(values))],
            ..evaluator(calcs, schema)
        };
        calcs.observe_draft(Some(draft_context(title)));
        evaluator
    }

    /// The draft of record 1's Title that `Evaluator::new` observes.
    fn draft_context(title: &str) -> DraftContext {
        DraftContext {
            layout: Some(1),
            occurrence: OCCURRENCE,
            record: Some(1),
            changes: [("Title".to_owned(), Some(title.to_owned()))].into(),
            related: Vec::new(),
            drafts: Vec::new(),
            begun: Default::default(),
        }
    }

    #[test]
    fn portal_sets_belong_to_their_parent_and_follow_its_draft() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        calcs
            .cache
            .get_mut()
            .related
            .insert((OCCURRENCE, 2, ROW_OCCURRENCE), Arc::from([20, 30]));
        let mut portal = portal_object();
        portal.calcs.portal_filter = Some(formula(
            &schema,
            "Items::Amount > 0 or Jobs::Title = \"Show all\"",
        ));
        // List view paints both parents in one frame, from one cache.
        let clean = evaluator(&calcs, &schema);
        assert_eq!(&*clean.portal_records(&portal, 1).unwrap(), &[30, 10]);
        assert_eq!(&*clean.portal_records(&portal, 2).unwrap(), &[30]);
        assert_eq!(&*clean.portal_records(&portal, 1).unwrap(), &[30, 10]);
        // The draft's frames see the edited parent, not its cached set.
        for _ in 0..2 {
            let draft = draft_evaluator(&calcs, &schema, "Show all");
            assert_eq!(&*draft.portal_records(&portal, 1).unwrap(), &[30, 10, 20]);
            assert_eq!(&*draft.portal_records(&portal, 2).unwrap(), &[30]);
        }
        // Typing the stored value back (or reverting) ends the draft without
        // a data reply; the draft's set must not outlive it.
        let reverted = evaluator(&calcs, &schema);
        assert_eq!(&*reverted.portal_records(&portal, 1).unwrap(), &[30, 10]);
        assert!(calcs.wanted.borrow().is_empty());
    }

    #[test]
    fn pending_portal_filters_stay_pending_rather_than_empty() {
        let schema = portal_schema();
        let calcs = Calculations::default();
        calcs
            .cache
            .borrow_mut()
            .related
            .insert((OCCURRENCE, 1, ROW_OCCURRENCE), Arc::from([30, 10, 20]));
        let mut portal = portal_object();
        portal.calcs.portal_filter = Some(formula(&schema, "Items::Amount > 0"));
        let evaluator = evaluator(&calcs, &schema);
        for frame in 0..2 {
            assert!(
                evaluator.portal_records(&portal, 1).is_none(),
                "frame {frame} must not see an empty filtered set"
            );
        }
        assert!(calcs.portal_sets.borrow().is_empty());
        assert!(calcs.wanted.borrow().contains(&CalcNeed::Record {
            table: ROW_TABLE,
            record: 30
        }));
    }

    #[test]
    fn a_failed_sort_row_keeps_the_relationship_order_visible() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        calcs.cache.get_mut().records.remove(&(ROW_TABLE, 20));
        calcs.failed.insert(CalcNeed::Record {
            table: ROW_TABLE,
            record: 20,
        });
        let mut portal = portal_object();
        portal.portal.as_mut().unwrap().flags = 0x8;
        portal.calcs.portal_sort = Some(label_sort());
        let evaluator = evaluator(&calcs, &schema);
        assert_eq!(
            &*evaluator.portal_records(&portal, 1).unwrap(),
            &[30, 10, 20]
        );
    }

    #[test]
    fn record_number_in_a_portal_row_is_the_row_number() {
        let schema = portal_schema();
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        // The window's found set puts parent 1 second.
        calcs
            .cache
            .get_mut()
            .found
            .insert(OCCURRENCE, Arc::from([2, 1]));
        let evaluator = evaluator(&calcs, &schema);
        let object = LayoutObject {
            id: 9,
            calcs: om_layout::ObjectCalcs {
                label: Some(formula(&schema, "Get ( RecordNumber )")),
                ..Default::default()
            },
            ..Default::default()
        };
        let numbered = |occurrence, record, number| {
            Some(PortalRow {
                occurrence,
                record,
                number,
            })
        };
        for (portal, expected) in [
            (None, "2"),
            (numbered(ROW_OCCURRENCE, 30, 1), "1"),
            (numbered(ROW_OCCURRENCE, 10, 2), "2"),
            (numbered(ROW_OCCURRENCE, 20, 3), "3"),
            // A portal of the layout's own occurrence numbers its rows too.
            (numbered(OCCURRENCE, 2, 4), "4"),
        ] {
            let state = evaluator
                .object_state_in_portal(&object, 1, portal, || Value::empty(ValueType::Text))
                .unwrap()
                .state;
            assert_eq!(state.label.as_deref(), Some(expected), "{portal:?}");
            assert_eq!(
                evaluator
                    .merge_text_in_portal("Row {{RecordNumber}}", Some(1), portal)
                    .unwrap(),
                format!("Row {expected}")
            );
        }
        assert!(calcs.wanted.borrow().is_empty());
    }

    #[test]
    fn portal_merge_fields_resolve_occurrences_repetitions_and_parent_names() {
        let mut schema = portal_schema();
        schema.add_field(
            ROW_TABLE,
            FieldDef::data(4, "Repeating", ValueType::Text).with_repetitions(2),
        );
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        let values = calcs
            .cache
            .get_mut()
            .records
            .get_mut(&(ROW_TABLE, 10))
            .unwrap()
            .as_mut()
            .unwrap();
        Arc::make_mut(values).set(4, 2, Value::text("Second alder"));
        let evaluator = evaluator(&calcs, &schema);
        for (name, portal, expected) in [
            ("Items::Label", row(20), "Birch"),
            ("Items::Label", row(10), "Alder"),
            ("Title", row(20), "Parent"),
            ("Items::Repeating[2]", row(10), "Second alder"),
        ] {
            let value = evaluator.named_value_in_portal(name, 1, portal).unwrap();
            assert_eq!(evaluator.display(&value), expected);
        }
        assert!(evaluator
            .named_value_in_portal("Label", 1, row(20))
            .is_none());
    }

    #[test]
    fn layout_calculations_globals_and_repetitions_use_catalog_field_ids() {
        let mut schema = CalcSchema::new("High field ids", Locale::en_us());
        schema.add_table(TABLE, "Jobs");
        schema.add_occurrence(OCCURRENCE, "Jobs", TABLE);
        let compiled = formula::compile("42", &formula::Context::new(schema.names())).unwrap();
        schema.add_field(
            TABLE,
            FieldDef::calculation(
                0x8000,
                "Calculated",
                ValueType::Number,
                Formula {
                    context: Some(Key::from_id(OCCURRENCE)),
                    formatted: None,
                    polish: compiled.expr,
                },
                false,
            ),
        );
        schema.add_field(
            TABLE,
            FieldDef::data(0x80ac, "Global", ValueType::Text).with_global(true),
        );
        schema.add_field(
            TABLE,
            FieldDef::data(0x8180, "Repeating", ValueType::Text).with_repetitions(2),
        );
        let schema = Arc::new(schema);
        let mut calcs = Calculations::default();
        calcs.set_schema(Some(Arc::clone(&schema)));
        let mut globals = RecordValues::default();
        globals.set(0x80ac, 1, Value::text("Session value"));
        calcs
            .cache
            .get_mut()
            .globals
            .insert(TABLE, Arc::new(globals));
        let mut record = RecordValues::default();
        record.set(0x8180, 2, Value::text("Second repetition"));
        calcs
            .cache
            .get_mut()
            .records
            .insert((TABLE, 1), Some(Arc::new(record)));
        let evaluator = Evaluator {
            calcs: &calcs,
            schema: &schema,
            info: SessionInfo::default(),
            layout: 1,
            occurrence: OCCURRENCE,
            today: Date::from_ymd(2026, 10, 29).unwrap(),
            locals: RefCell::new(Variables::new()),
            globals: RefCell::new(Variables::new()),
            overlays: Vec::new(),
            hosted: false,
            scans: RefCell::default(),
            summaries: SummaryCache::default(),
            group: Cell::new(None),
        };
        for (object, serial, repetition, expected) in [
            (1, 128, 1, "42"),
            (2, 300, 1, "Session value"),
            (3, 512, 2, "Second repetition"),
        ] {
            let spec = FieldSpec {
                table_key: LAYOUT_OCCURRENCE,
                field_key: serial,
                repetition,
                ..Default::default()
            };
            assert!(evaluator.computes(&spec), "field {serial}");
            let value = evaluator.field_value(object, &spec, 1).unwrap();
            assert_eq!(evaluator.display(&value), expected);
        }
        let named = evaluator.named_field_value("Calculated", 1).unwrap();
        assert_eq!(evaluator.display(&named), "42");
        // A layout object can have the same id as a column's catalog field;
        // their cached values must not share a key space.
        let global = evaluator
            .field_value(
                0x8000,
                &FieldSpec {
                    table_key: LAYOUT_OCCURRENCE,
                    field_key: 300,
                    repetition: 1,
                    ..Default::default()
                },
                1,
            )
            .unwrap();
        assert_eq!(evaluator.display(&global), "Session value");
        assert!(evaluator.computes_named("Calculated"));
        assert!(evaluator.computes_named("Global"));
        assert!(!evaluator.computes_named("Repeating"));
        assert!(calcs.wanted.borrow().is_empty());
        let unsupported = FieldSpec {
            table_key: LAYOUT_OCCURRENCE,
            field_key: u64::MAX,
            ..Default::default()
        };
        assert!(!evaluator.computes(&unsupported));
        assert!(evaluator.field_value(4, &unsupported, 1).is_none());
    }

    #[test]
    fn a_partial_answer_fails_only_the_needs_it_lacks() {
        let mut calcs = Calculations::default();
        let read = CalcNeed::Record {
            table: TABLE,
            record: 1,
        };
        let unreadable = CalcNeed::Record {
            table: 0x8002,
            record: 1,
        };
        calcs
            .wanted
            .get_mut()
            .extend([read.clone(), unreadable.clone()]);
        let needs = calcs.take_wanted();
        let mut data = CalcData::default();
        data.records.insert((TABLE, 1), None);
        calcs.accept_data(&needs, Ok(data));
        assert!(calcs.cache.borrow().has(&read));
        assert_eq!(calcs.failed, BTreeSet::from([unreadable.clone()]));
        // It is not asked for again until the data changes.
        calcs.wanted.get_mut().insert(unreadable.clone());
        assert!(calcs.take_wanted().is_empty());
        calcs.data_changed();
        calcs.wanted.get_mut().insert(unreadable.clone());
        assert_eq!(calcs.take_wanted(), [unreadable]);
    }

    #[test]
    fn each_related_file_warning_is_reported_once() {
        let mut calcs = Calculations::default();
        let missing =
            "Related file “Lines” (named by “Tasks”): the host does not have it.".to_string();
        assert_eq!(
            calcs.report_related_files(vec![missing.clone()]).as_deref(),
            Some(missing.as_str())
        );
        let mut second = Calculations::default();
        second.share_reports(&calcs);
        assert_eq!(second.report_related_files(vec![missing.clone()]), None);
        // The schema again (Manage Database), with one more.
        let more = "Occurrence “Rates” of “Tasks” shows a table that related file “Rates” does \
                    not have as the host sent it; the occurrence's table is unavailable."
            .to_string();
        assert_eq!(
            calcs
                .report_related_files(vec![missing.clone(), more.clone()])
                .as_deref(),
            Some(more.as_str())
        );
        assert_eq!(second.report_related_files(vec![more.clone()]), None);
        drop(calcs);
        assert_eq!(second.report_related_files(vec![missing]), None);
        let mut calcs = second;
        calcs.reset();
        assert!(calcs.report_related_files(vec![more]).is_some());
    }

    #[test]
    fn each_table_an_answer_could_not_read_is_reported_once() {
        let mut schema = CalcSchema::new("Reports", Locale::en_us());
        schema.add_table(TABLE, "Jobs");
        schema.add_table(ROW_TABLE, "Notes");
        let schema = Arc::new(schema);
        let mut calcs = Calculations::default();
        calcs.set_schema(Some(Arc::clone(&schema)));
        let needs = [CalcNeed::Record {
            table: TABLE,
            record: 1,
        }];
        let answer = |failed: &[u64]| {
            let mut data = CalcData::default();
            data.records.insert((TABLE, 1), None);
            for &table in failed {
                data.failures
                    .insert(table, DataError::new("Access is denied."));
            }
            Ok(data)
        };
        assert_eq!(
            calcs.accept_data(&needs, answer(&[ROW_TABLE])).as_deref(),
            Some("Calculations could not read table “Notes”: Access is denied.")
        );
        // The data it read is kept, and the table's failure is not.
        assert!(calcs.cache.borrow().has(&needs[0]));
        assert!(calcs.cache.borrow().failures.is_empty());
        // Asked again once the data changes, the table fails again.
        calcs.data_changed();
        assert_eq!(calcs.accept_data(&needs, answer(&[ROW_TABLE])), None);
        assert_eq!(
            calcs
                .accept_data(&needs, answer(&[ROW_TABLE, 0x7fff]))
                .as_deref(),
            Some("Calculations could not read table 0x7fff: Access is denied.")
        );
        // An answer that failed as a whole is reported each time.
        for _ in 0..2 {
            assert_eq!(
                calcs
                    .accept_data(&needs, Err(DataError::new("Offline.")))
                    .as_deref(),
                Some("Calculations could not read their data: Offline.")
            );
        }
        // The file opened again reports again.
        calcs.reset();
        calcs.set_schema(Some(schema));
        assert!(calcs.accept_data(&needs, answer(&[ROW_TABLE])).is_some());
    }

    fn option_list(id: u64, source: ValueSource) -> om_data::meta::ValueList {
        om_data::meta::ValueList {
            id,
            name: format!("List {id}"),
            source,
            options: 0,
            sort_locale: 0,
            separator: " ".into(),
        }
    }

    fn option_object(id: u64, value_list_key: u64) -> LayoutObject {
        LayoutObject {
            id,
            field_obj: Some(om_layout::FieldObject {
                display_type: 3,
                value_list_key,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// The stored values of a set's items, each labelled by itself.
    fn values(items: Result<Arc<Choices>, ChoiceError>) -> Vec<String> {
        let items = items.expect("the list's items").items.clone();
        assert!(items.iter().all(|item| item.label == item.value));
        items.iter().map(|item| item.value.clone()).collect()
    }

    fn labels() -> om_data::specs::SpecField {
        om_data::specs::SpecField {
            occurrence: Some(ROW_OCCURRENCE),
            field: 1,
            repetition: 1,
        }
    }

    /// A list of an unstored calculation's values (Items::Description)
    /// is built on the worker, never in paint, from committed values
    /// (#290). While it is built, changes of the window's context ask for
    /// no other build: the answer for the old context is not taken, and
    /// one build for the new one follows.
    #[test]
    fn a_calculated_list_is_built_on_the_worker_one_build_at_a_time() {
        let mut schema = portal_schema();
        schema.add_value_list(option_list(
            5,
            ValueSource::Field {
                primary: om_data::specs::SpecField {
                    occurrence: Some(ROW_OCCURRENCE),
                    field: 3,
                    repetition: 1,
                },
                secondary: None,
                start: None,
            },
        ));
        let mut calcs = Calculations {
            schema: Some(Arc::new(schema.clone())),
            ..Calculations::default()
        };
        portal_data(&mut calcs);
        let pending = |calcs: &Calculations| {
            evaluator(calcs, &schema).option_values(&option_object(13, 5), 1, None)
        };
        assert_eq!(pending(&calcs), Err(ChoiceError::Pending));
        let asked = calcs.take_list_builds();
        assert_eq!(asked.len(), 1, "{asked:?}");
        let first = asked[0].ticket;
        // The window resized twice meanwhile.
        for name in ["Wide", "Wider"] {
            calcs.observe_context(&SessionInfo {
                window_name: name.into(),
                ..SessionInfo::default()
            });
            assert_eq!(pending(&calcs), Err(ChoiceError::Pending));
        }
        assert!(calcs.take_list_builds().is_empty(), "one build at a time");
        let built = |names: &[&str]| {
            Some(Ok(Arc::new(Choices {
                items: names
                    .iter()
                    .map(|name| om_data::host::ValueListEntry {
                        label: (*name).into(),
                        value: (*name).into(),
                    })
                    .collect(),
                custom: false,
            })))
        };
        calcs.accept_built_list(5, first, built(&["old"]), None);
        assert_eq!(pending(&calcs), Err(ChoiceError::Pending));
        let again = calcs.take_list_builds();
        assert_eq!(again.len(), 1, "{again:?}");
        assert_ne!(again[0].ticket, first);
        calcs.accept_built_list(5, again[0].ticket, built(&["new"]), None);
        assert_eq!(values(pending(&calcs)), ["new"]);
    }

    #[test]
    fn option_lists_convert_serial_keys_and_fetch_field_values() {
        let mut schema = portal_schema();
        // Serial key 128 is catalog id 0x8000 (file-metadata.md 1).
        schema.add_value_list(option_list(
            0x8000,
            ValueSource::Custom(vec!["Open\rHold".into()]),
        ));
        schema.add_value_list(option_list(
            5,
            ValueSource::Field {
                primary: labels(),
                secondary: None,
                start: None,
            },
        ));
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        {
            let evaluator = evaluator(&calcs, &schema);
            assert_eq!(
                values(evaluator.option_values(&option_object(12, 128), 1, None)),
                ["Open", "Hold"]
            );
            assert_eq!(
                evaluator.option_values(&option_object(14, 6), 1, None),
                Err(ChoiceError::NoValueList)
            );
            // A field's values are read from every record of its table: the
            // worker builds the list (#745), and paint reads nothing for it,
            // pending, rather than showing an empty list.
            for _ in 0..2 {
                assert_eq!(
                    evaluator.option_values(&option_object(13, 5), 1, None),
                    Err(ChoiceError::Pending)
                );
            }
            assert!(calcs.wanted.borrow().is_empty());
        }
        let asked = calcs.take_list_builds();
        assert_eq!(asked.len(), 1, "asked once: {asked:?}");
        let (list, ticket) = (asked[0].list, asked[0].ticket);
        assert_eq!(list, 5);
        assert!(calcs.awaiting_data(), "a list being built is waited for");
        let built = |names: &[&str]| {
            Some(Ok(Arc::new(Choices {
                items: names
                    .iter()
                    .map(|name| om_data::host::ValueListEntry {
                        label: (*name).into(),
                        value: (*name).into(),
                    })
                    .collect(),
                custom: false,
            })))
        };
        calcs.accept_built_list(5, ticket, built(&["Alder", "Birch", "Cedar"]), None);
        assert!(!calcs.awaiting_data());
        {
            let evaluator = evaluator(&calcs, &schema);
            assert_eq!(
                values(evaluator.option_values(&option_object(13, 5), 1, None)),
                ["Alder", "Birch", "Cedar"]
            );
        }
        assert!(calcs.take_list_builds().is_empty(), "kept, not asked again");
        // The data changed: the list is built again, and an answer to the
        // request from before is not taken.
        calcs.data_changed();
        {
            let evaluator = evaluator(&calcs, &schema);
            assert_eq!(
                evaluator.option_values(&option_object(13, 5), 1, None),
                Err(ChoiceError::Pending)
            );
        }
        let again = calcs.take_list_builds();
        assert_eq!(again.len(), 1);
        assert_ne!(again[0].ticket, ticket);
        calcs.accept_built_list(5, ticket, built(&["Stale"]), None);
        assert!(calcs.awaiting_data(), "the stale answer is not the list");
        calcs.accept_built_list(5, again[0].ticket, built(&["Birch"]), None);
        {
            let evaluator = evaluator(&calcs, &schema);
            assert_eq!(
                values(evaluator.option_values(&option_object(13, 5), 1, None)),
                ["Birch"]
            );
        }
        // A build that read a related file's unreachable host (#740), its
        // ticket dropped as the schema grew before its answer came: the
        // list is not asked for again before the host may be reached.
        calcs.data_changed();
        {
            let evaluator = evaluator(&calcs, &schema);
            let _ = evaluator.option_values(&option_object(13, 5), 1, None);
        }
        let waiting = calcs.take_list_builds();
        calcs.grow_schema(Some(Arc::new(schema.clone())));
        let later = std::time::Duration::from_secs(60);
        calcs.accept_built_list(5, waiting[0].ticket, None, Some(later));
        {
            let evaluator = evaluator(&calcs, &schema);
            assert_eq!(
                evaluator.option_values(&option_object(13, 5), 1, None),
                Err(ChoiceError::Pending)
            );
        }
        assert!(
            calcs.take_list_builds().is_empty(),
            "not asked while waiting"
        );
        assert!(calcs.retry_at().is_some(), "the window draws again then");
        // Another list's build grows the schema: this one still waits.
        calcs.grow_schema(Some(Arc::new(schema.clone())));
        {
            let evaluator = evaluator(&calcs, &schema);
            let _ = evaluator.option_values(&option_object(13, 5), 1, None);
        }
        assert!(calcs.take_list_builds().is_empty(), "still waiting");
        // Its time came (here, at once): the window draws again at once,
        // once, and the list shown then is asked for.
        let came = || (std::time::Instant::now(), false);
        calcs.build_retry.get_mut().insert(5, came());
        let soon = std::time::Instant::now() + std::time::Duration::from_millis(50);
        assert!(calcs.retry_at().is_some_and(|at| at <= soon));
        assert_eq!(calcs.retry_at(), None, "told once");
        calcs.build_retry.get_mut().insert(5, came());
        {
            let evaluator = evaluator(&calcs, &schema);
            let _ = evaluator.option_values(&option_object(13, 5), 1, None);
        }
        assert_eq!(
            calcs.take_list_builds().len(),
            1,
            "asked once its time came"
        );
    }

    #[test]
    fn related_option_lists_start_from_the_record_of_their_occurrence() {
        let mut schema = portal_schema();
        // Related to the layout's record (Jobs 1 has Items 30, 10 and 20).
        schema.add_value_list(option_list(
            6,
            ValueSource::Field {
                primary: labels(),
                secondary: None,
                start: Some(OCCURRENCE),
            },
        ));
        let mut calcs = Calculations::default();
        portal_data(&mut calcs);
        // Built on the worker for each record and portal row (#290): paint
        // asks once per row and waits.
        {
            let evaluator = evaluator(&calcs, &schema);
            for portal in [None, row(10)] {
                for _ in 0..2 {
                    assert_eq!(
                        evaluator.option_values(&option_object(15, 6), 1, portal),
                        Err(ChoiceError::Pending),
                        "{portal:?}"
                    );
                }
            }
        }
        let builds = calcs.take_list_builds();
        assert_eq!(builds.len(), 2, "{builds:?}");
        // A portal row is an Items record: the list still starts from the
        // parent, the current Jobs record, as the worker builds it
        // (`list_build`) over the same data.
        let source = CachedSource::new(&calcs.cache);
        let file = FileCalc::new(&schema, &source, Date::from_ymd(2026, 10, 29).unwrap());
        let list = schema.value_lists().iter().find(|l| l.id == 6).unwrap();
        for build in &builds {
            let related = build.related.as_ref().expect("a record's list");
            assert_eq!((related.occurrence, related.parent), (OCCURRENCE, 1));
            let context = choice::context(
                &schema,
                list,
                related.occurrence,
                related.parent,
                related.portal,
            );
            assert_eq!(
                values(choice::choices(&file, list, &context).map(Arc::new)),
                ["Alder", "Birch", "Cedar"],
                "{:?}",
                related.portal
            );
        }
        // Its answer is the row's list until the data changes.
        let ticket = builds[0].ticket;
        let built = Arc::new(Choices {
            items: vec![om_data::host::ValueListEntry {
                label: "Alder".into(),
                value: "Alder".into(),
            }],
            custom: false,
        });
        calcs.accept_built_list(6, ticket, Some(Ok(Arc::clone(&built))), None);
        assert_eq!(
            values(evaluator(&calcs, &schema).option_values(&option_object(15, 6), 1, None)),
            ["Alder"]
        );
        calcs.data_changed();
        assert_eq!(
            evaluator(&calcs, &schema).option_values(&option_object(15, 6), 1, None),
            Err(ChoiceError::Pending)
        );
    }

    fn pending_container_fixture() -> (CalcSchema, Calculations) {
        use om_data::containers::MemberInfo;
        let mut schema = portal_schema();
        let mut values = RecordValues::default();
        for (field, name, member) in [(51, "One", 3u8), (52, "Two", 2), (53, "Dead", 1)] {
            schema.add_field(TABLE, FieldDef::data(field, name, ValueType::Container));
            let info = MemberInfo {
                name: format!("{member}.dat"),
                md5: [member; 16],
                path_type: String::new(),
                owner: None,
            };
            values.set(field, 1, Value::Container(info.value(TABLE, &[member])));
        }
        let mut calcs = Calculations::default();
        calcs
            .cache
            .get_mut()
            .records
            .insert((TABLE, 1), Some(Arc::new(values)));
        (schema, calcs)
    }

    fn container_body_reply(member: u8) -> CalcData {
        use om_data::calc::eval::StoredContainer;
        use om_data::calc::{Container, StreamType};
        let mut data = CalcData::default();
        data.library.data.insert(
            (TABLE, vec![member]),
            Some(Arc::new(StoredContainer {
                data: Container {
                    streams: vec![(StreamType(*b"FILE"), b"yes".to_vec())],
                    ..Default::default()
                },
                external: false,
                intact: true,
            })),
        );
        data
    }

    #[test]
    fn pending_container_conditions_resume_before_selecting_branches() {
        for expression in [
            "If ( Length ( Jobs::One ) > 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "If ( IsValid ( Length ( Jobs::One ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "Case ( IsValid ( Length ( Jobs::One ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "Choose ( IsValid ( Length ( Jobs::One ) ) ; Length ( Jobs::Dead ) ; Length ( Jobs::Two ) )",
            "If ( EvaluationError ( Length ( Jobs::One ) ) = 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            let code = formula(&schema,expression);
            schema.add_field(TABLE,FieldDef::calculation(55,"SelectedBytes",ValueType::Number,code,false));
            // The first dependency has a larger key than the unused branch.
            // Sorting all missing needs and taking the first is insufficient.
            for member in [3u8,2] {
                assert!(evaluator(&calcs,&schema).named_field_value("SelectedBytes",1).is_none(),"{expression}");
                let needs=calcs.take_wanted();
                assert_eq!(needs,[CalcNeed::MemberData {table:TABLE,member:vec![member]}],"{expression}");
                assert!(calcs.accept_data(&needs,Ok(container_body_reply(member))).is_none());
            }
            let current=evaluator(&calcs,&schema);
            let value=current.named_field_value("SelectedBytes",1).unwrap();
            assert_eq!(current.display(&value),"3","{expression}");
            assert!(calcs.take_wanted().is_empty());
        }
    }

    #[test]
    fn pending_container_reads_still_batch_independent_objects() {
        let (mut schema, mut calcs) = pending_container_fixture();
        for (id, name, code) in [
            (55, "FirstBytes", "Length ( Jobs::One )"),
            (56, "SecondBytes", "Length ( Jobs::Two )"),
        ] {
            let code = formula(&schema, code);
            schema.add_field(
                TABLE,
                FieldDef::calculation(id, name, ValueType::Number, code, false),
            );
        }
        let current = evaluator(&calcs, &schema);
        assert!(current.named_field_value("FirstBytes", 1).is_none());
        assert!(current.named_field_value("SecondBytes", 1).is_none());
        assert_eq!(
            calcs.take_wanted(),
            [
                CalcNeed::MemberData {
                    table: TABLE,
                    member: vec![2]
                },
                CalcNeed::MemberData {
                    table: TABLE,
                    member: vec![3]
                },
            ]
        );
    }

    #[test]
    fn pending_container_refusal_becomes_a_permanent_validity_result() {
        let (mut schema, mut calcs) = pending_container_fixture();
        let code=formula(&schema,"If ( IsValid ( Length ( Jobs::One ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )");
        schema.add_field(
            TABLE,
            FieldDef::calculation(55, "SelectedBytes", ValueType::Number, code, false),
        );
        assert!(evaluator(&calcs, &schema)
            .named_field_value("SelectedBytes", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![3]
            }]
        );
        let _ = calcs.accept_data(
            &needs,
            Err(om_data::DataError::new("synthetic permanent refusal")),
        );
        assert!(evaluator(&calcs, &schema)
            .named_field_value("SelectedBytes", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![1]
            }]
        );
        assert!(calcs
            .accept_data(&needs, Ok(container_body_reply(1)))
            .is_none());
        let current = evaluator(&calcs, &schema);
        let value = current.named_field_value("SelectedBytes", 1).unwrap();
        assert_eq!(current.display(&value), "3");
        assert!(calcs.take_wanted().is_empty());
    }

    fn pending_add_calc(schema: &mut CalcSchema, id: u64, name: &str, text: &str) {
        let code = formula(schema, text);
        schema.add_field(
            TABLE,
            FieldDef::calculation(id, name, ValueType::Number, code, false),
        );
    }

    fn pending_raw_one(calcs: &mut Calculations) {
        let mut values = calcs.cache.get_mut().records[&(TABLE, 1)]
            .as_deref()
            .unwrap()
            .clone();
        values.set(
            51,
            1,
            Value::Container(om_data::calc::Container {
                text: om_data::calc::Text::plain(om_data::containers::reference_text(&[3])),
                ..Default::default()
            }),
        );
        calcs
            .cache
            .get_mut()
            .records
            .insert((TABLE, 1), Some(Arc::new(values)));
    }

    fn pending_header_reply(member: u8) -> CalcData {
        let mut data = CalcData::default();
        data.library.members.insert(
            (TABLE, vec![member]),
            Some(Arc::new(om_data::containers::MemberInfo {
                name: format!("{member}.dat"),
                md5: [member; 16],
                path_type: String::new(),
                owner: None,
            })),
        );
        data
    }

    fn pending_expect_steps(
        schema: &CalcSchema,
        calcs: &mut Calculations,
        name: &str,
        header: bool,
        body: bool,
    ) {
        let mut steps = Vec::new();
        if header {
            steps.push((
                CalcNeed::Member {
                    table: TABLE,
                    member: vec![3],
                },
                pending_header_reply(3),
            ));
        }
        if body {
            steps.push((
                CalcNeed::MemberData {
                    table: TABLE,
                    member: vec![3],
                },
                container_body_reply(3),
            ));
        }
        steps.push((
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![2],
            },
            container_body_reply(2),
        ));
        for (expected, reply) in steps {
            assert!(evaluator(calcs, schema)
                .named_field_value(name, 1)
                .is_none());
            let needs = calcs.take_wanted();
            assert_eq!(
                needs,
                [expected],
                "pending condition selected a dependent branch"
            );
            assert!(calcs.accept_data(&needs, Ok(reply)).is_none());
        }
        let current = evaluator(calcs, schema);
        let value = current
            .named_field_value(name, 1)
            .expect("completed dependencies must converge");
        assert_eq!(current.display(&value), "3");
        assert!(calcs.take_wanted().is_empty());
    }

    #[test]
    fn pending_header_guards_do_not_select_body_branches() {
        for text in [
            "If ( IsValid ( Length ( Jobs::One ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "If ( EvaluationError ( Length ( Jobs::One ) ) = 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "Case ( IsValid ( Jobs::One ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "Choose ( IsValid ( Jobs::One ) ; Length ( Jobs::Dead ) ; Length ( Jobs::Two ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_raw_one(&mut calcs);
            pending_add_calc(&mut schema, 56, "HeaderGuard", text);
            pending_expect_steps(&schema, &mut calcs, "HeaderGuard", true, text.contains("Length ( Jobs::One"));
        }
    }

    #[test]
    fn pending_metadata_guard_does_not_download_a_body() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_raw_one(&mut calcs);
        pending_add_calc(&mut schema, 56, "MetadataGuard", "If ( GetAsText ( Jobs::One ) = \"3.dat\" ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )");
        pending_expect_steps(&schema, &mut calcs, "MetadataGuard", true, false);
    }

    #[test]
    fn pending_nested_unstored_body_guard_waits_for_its_condition() {
        for text in [
            "If ( Jobs::Inner = 0 ; Length ( Jobs::Dead ) ; Length ( Jobs::Two ) )",
            "If ( Jobs::Inner > 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "Case ( Jobs::Inner = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(&mut schema, 55, "Inner", "Length ( Jobs::One )");
            pending_add_calc(&mut schema, 56, "NestedGuard", text);
            pending_expect_steps(&schema, &mut calcs, "NestedGuard", false, true);
        }
    }

    #[test]
    fn pending_nested_unstored_header_guard_waits_for_its_condition() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_raw_one(&mut calcs);
        pending_add_calc(&mut schema, 55, "Inner", "Length ( Jobs::One )");
        pending_add_calc(
            &mut schema,
            56,
            "NestedGuard",
            "If ( Jobs::Inner = 0 ; Length ( Jobs::Dead ) ; Length ( Jobs::Two ) )",
        );
        pending_expect_steps(&schema, &mut calcs, "NestedGuard", true, true);
    }

    #[test]
    fn pending_evaluate_and_custom_body_guards_resume() {
        for text in [
            "If ( IsValid ( Evaluate ( \"Length ( Jobs::One )\" ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "If ( IsValid ( ReviewBytes ( ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "If ( IsValid ( Let ( n = Length ( Jobs::One ) ; n ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "If ( IsValid ( While ( [ i = 0 ; n = 0 ] ; i < 2 ; [ i = i + 1 ; n = Length ( Jobs::One ) ] ; n ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            schema.add_custom_function(991, om_data::calc::eval::CustomFunction {
                name: "ReviewBytes".into(), parameters: vec![],
                body: om_data::formula::compile("Length ( Jobs::One )", &{
                    let mut cx = om_data::formula::Context::new(schema.names());
                    cx.occurrence = Some(OCCURRENCE); cx
                }).unwrap().expr,
            });
            pending_add_calc(&mut schema, 56, "ControlGuard", text);
            pending_expect_steps(&schema, &mut calcs, "ControlGuard", false, true);
        }
    }

    #[test]
    fn pending_permanent_missing_body_keeps_error_predicates() {
        for (text, member) in [
            ("If ( IsValid ( Length ( Jobs::One ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )", 1),
            ("If ( EvaluationError ( Length ( Jobs::One ) ) = 1225 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )", 2),
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            calcs.cache.get_mut().library.data.insert((TABLE, vec![3]), None);
            pending_add_calc(&mut schema, 56, "PermanentGuard", text);
            assert!(evaluator(&calcs, &schema).named_field_value("PermanentGuard", 1).is_none());
            let needs = calcs.take_wanted();
            assert_eq!(needs, [CalcNeed::MemberData { table: TABLE, member: vec![member] }]);
            calcs.accept_data(&needs, Ok(container_body_reply(member)));
            let current = evaluator(&calcs, &schema);
            assert_eq!(current.display(&current.named_field_value("PermanentGuard", 1).unwrap()), "3");
            assert!(calcs.take_wanted().is_empty());
        }
    }

    #[test]
    fn pending_independent_records_still_batch_body_needs() {
        let (mut schema, mut calcs) = pending_container_fixture();
        let mut second = RecordValues::default();
        let info = om_data::containers::MemberInfo {
            name: "2.dat".into(),
            md5: [2; 16],
            path_type: String::new(),
            owner: None,
        };
        second.set(51, 1, Value::Container(info.value(TABLE, &[2])));
        calcs
            .cache
            .get_mut()
            .records
            .insert((TABLE, 2), Some(Arc::new(second)));
        pending_add_calc(&mut schema, 56, "BatchBytes", "Length ( Jobs::One )");
        let current = evaluator(&calcs, &schema);
        assert!(current.named_field_value("BatchBytes", 1).is_none());
        assert!(current.named_field_value("BatchBytes", 2).is_none());
        assert_eq!(
            calcs.take_wanted(),
            [2u8, 3].map(|member| CalcNeed::MemberData {
                table: TABLE,
                member: vec![member]
            })
        );
    }

    fn pending_two_record_bodies(calcs: &mut Calculations) {
        let mut second = RecordValues::default();
        let info = om_data::containers::MemberInfo {
            name: "2.dat".into(),
            md5: [2; 16],
            path_type: String::new(),
            owner: None,
        };
        second.set(51, 1, Value::Container(info.value(TABLE, &[2])));
        let data = calcs.cache.get_mut();
        data.records.insert((TABLE, 2), Some(Arc::new(second)));
        data.found.insert(OCCURRENCE, Arc::from([1, 2]));
    }

    #[test]
    fn pending_while_independent_record_bodies_keep_batching() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_two_record_bodies(&mut calcs);
        pending_add_calc(&mut schema,56,"LoopBytes","While ( [ i = 0 ; out = 0 ] ; i < 2 ; [ i = i + 1 ; out = out + Length ( GetNthRecord ( Jobs::One ; i ) ) ] ; out )");
        assert!(evaluator(&calcs, &schema)
            .named_field_value("LoopBytes", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [2u8, 3].map(|member| CalcNeed::MemberData {
                table: TABLE,
                member: vec![member]
            })
        );
        let mut data = container_body_reply(2);
        data.merge(container_body_reply(3));
        calcs.accept_data(&needs, Ok(data));
        let current = evaluator(&calcs, &schema);
        assert_eq!(
            current.display(&current.named_field_value("LoopBytes", 1).unwrap()),
            "6"
        );
        assert!(calcs.take_wanted().is_empty());
    }

    #[test]
    fn pending_recursive_independent_record_bodies_keep_batching() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_two_record_bodies(&mut calcs);
        let mut function = om_data::calc::eval::CustomFunction {
            name: "ReviewSum".into(),
            parameters: vec!["i".into()],
            body: om_data::formula::Expr::Empty,
        };
        schema.add_custom_function(991, function.clone());
        let mut cx = om_data::formula::Context::new(schema.names());
        cx.occurrence = Some(OCCURRENCE);
        cx.parameters = &function.parameters;
        function.body = om_data::formula::compile(
            "If ( i > 2 ; 0 ; Length ( GetNthRecord ( Jobs::One ; i ) ) + ReviewSum ( i + 1 ) )",
            &cx,
        )
        .unwrap()
        .expr;
        schema.add_custom_function(991, function);
        pending_add_calc(&mut schema, 56, "RecursiveBytes", "ReviewSum ( 1 )");
        assert!(evaluator(&calcs, &schema)
            .named_field_value("RecursiveBytes", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [2u8, 3].map(|member| CalcNeed::MemberData {
                table: TABLE,
                member: vec![member]
            })
        );
        let mut data = container_body_reply(2);
        data.merge(container_body_reply(3));
        calcs.accept_data(&needs, Ok(data));
        let current = evaluator(&calcs, &schema);
        assert_eq!(
            current.display(&current.named_field_value("RecursiveBytes", 1).unwrap()),
            "6"
        );
        assert!(calcs.take_wanted().is_empty());
    }

    #[test]
    fn pending_bound_values_keep_their_dependencies_in_conditions() {
        for text in [
            "Let ( n = Length ( Jobs::One ) ; If ( IsValid ( n ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( [ n = Length ( Jobs::One ) ; alias = n ] ; If ( alias > 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( $n = Length ( Jobs::One ) ; If ( IsValid ( $n ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( $$n[2] = Length ( Jobs::One ) ; If ( IsValid ( $$n[2] ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( n = Length ( Jobs::One ) ; SelectBytes ( n ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            let parameters = vec!["n".into()];
            let body = {
                let mut cx = om_data::formula::Context::new(schema.names());
                cx.occurrence = Some(OCCURRENCE);
                cx.parameters = &parameters;
                om_data::formula::compile("If ( IsValid ( n ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )", &cx).unwrap().expr
            };
            schema.add_custom_function(992, om_data::calc::eval::CustomFunction {
                name: "SelectBytes".into(), parameters, body,
            });
            pending_add_calc(&mut schema, 56, "BoundGuard", text);
            pending_expect_steps(&schema, &mut calcs, "BoundGuard", false, true);
        }
    }

    #[test]
    fn pending_boolean_and_loop_conditions_wait_before_reading_their_bodies() {
        for text in [
            "If ( IsValid ( Length ( Jobs::One ) ) and Length ( Jobs::Two ) > 0 ; 3 ; Length ( Jobs::Dead ) )",
            "If ( not IsValid ( Length ( Jobs::One ) ) ; Length ( Jobs::Dead ) ; Length ( Jobs::Two ) )",
            "If ( not IsValid ( Length ( Jobs::One ) ) or Length ( Jobs::Two ) > 0 ; 3 ; Length ( Jobs::Dead ) )",
            "While ( [ n = 0 ] ; IsValid ( Length ( Jobs::One ) ) and n = 0 ; [ n = Length ( Jobs::Two ) ] ; n )",
            "While ( [ i = 0 ; n = Length ( Jobs::One ) ] ; i = 0 and IsValid ( n ) ; [ i = i + 1 ; n = Length ( Jobs::Two ) ] ; n )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(&mut schema, 56, "ControlGuard", text);
            pending_expect_steps(&schema, &mut calcs, "ControlGuard", false, true);
        }
    }

    #[test]
    fn pending_branch_assignments_suspend_later_variable_decisions() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(&mut schema, 56, "AssignmentGuard", "Let ( [ $n = 0 ; decided = If ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "AssignmentGuard", false, true);
    }

    #[test]
    fn pending_variable_repetitions_suspend_later_reads_of_the_target() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(&mut schema, 56, "AssignmentGuard", "Let ( [ $i = Length ( Jobs::One ) ; $n[$i] = 3 ] ; If ( $n[3] = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "AssignmentGuard", false, true);
    }

    #[test]
    fn pending_nested_unknown_branch_does_not_resume_successor_assignments() {
        for inner in [
            "If ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )",
            "Case ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )",
            "Choose ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 1 ; 0 ) ; Let ( $n = 3 ; 0 ) )",
            "While ( [ i = 0 ] ; IsValid ( Length ( Jobs::One ) ) and i = 0 ; [ i = i + 1 ; x = Let ( $n = 3 ; 0 ) ] ; 0 )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(&mut schema, 55, "InnerSideEffect", inner);
            pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = Jobs::InnerSideEffect ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
            pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
        }
    }

    #[test]
    fn pending_nested_unknown_repetition_does_not_resume_successor_reads() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(
            &mut schema,
            55,
            "InnerSideEffect",
            "Let ( [ $i = Length ( Jobs::One ) ; $n[$i] = 3 ] ; 0 )",
        );
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n[3] = 0 ; ignored = Jobs::InnerSideEffect ] ; If ( $n[3] = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }

    #[test]
    fn pending_nested_boolean_assignment_does_not_use_stale_state() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(
            &mut schema,
            55,
            "InnerSideEffect",
            "IsValid ( Length ( Jobs::One ) ) and Let ( $n = 3 ; 1 )",
        );
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = Jobs::InnerSideEffect ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }

    #[test]
    fn pending_nested_alias_parameters_preserve_dependency() {
        for body in [
            "Let ( [ n = Length ( Jobs::One ) ; alias = n + 0 ; again = alias ] ; If ( EvaluationError ( again ) = 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( [ $$N[2] = Length ( Jobs::One ) ; alias = $$n[2] ] ; If ( alias > 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( $n = Length ( Jobs::One ) ; Evaluate ( \"If ( IsValid ( $N ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )\" ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(&mut schema, 55, "Inner", body);
            pending_add_calc(&mut schema, 56, "Outer", "Jobs::Inner");
            pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
        }
    }

    #[test]
    fn pending_permanent_header_failure_is_catchable_without_looping() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_raw_one(&mut calcs);
        pending_add_calc(&mut schema, 56, "Outer", "If ( EvaluationError ( GetAsText ( Jobs::One ) ) > 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )");
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Outer", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::Member {
                table: TABLE,
                member: vec![3]
            }]
        );
        calcs.accept_data(&needs, Err(DataError::new("synthetic header read failure")));
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Outer", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![2]
            }]
        );
        calcs.accept_data(&needs, Ok(container_body_reply(2)));
        let current = evaluator(&calcs, &schema);
        assert_eq!(
            current.display(&current.named_field_value("Outer", 1).unwrap()),
            "3"
        );
        assert!(calcs.take_wanted().is_empty());
    }

    #[test]
    fn pending_known_overwrite_and_independent_many_record_scan_batch() {
        let (mut schema, mut calcs) = pending_container_fixture();
        for record in 1u8..=8 {
            let mut values = RecordValues::default();
            let info = om_data::containers::MemberInfo {
                name: format!("{record}.dat"),
                md5: [record; 16],
                path_type: String::new(),
                owner: None,
            };
            values.set(51, 1, Value::Container(info.value(TABLE, &[record])));
            calcs
                .cache
                .get_mut()
                .records
                .insert((TABLE, u64::from(record)), Some(Arc::new(values)));
        }
        calcs
            .cache
            .get_mut()
            .found
            .insert(OCCURRENCE, (1u64..=8).collect::<Vec<_>>().into());
        pending_add_calc(&mut schema, 56, "Outer", "While ( [ i = 0 ; out = 0 ] ; i < 8 ; [ i = i + 1 ; out = out + Let ( [ $n = Length ( GetNthRecord ( Jobs::One ; i ) ) ; $n = 3 ] ; $n ) ] ; out )");
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Outer", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            (1u8..=8)
                .map(|member| CalcNeed::MemberData {
                    table: TABLE,
                    member: vec![member]
                })
                .collect::<Vec<_>>()
        );
        let mut reply = CalcData::default();
        for member in 1..=8 {
            reply.merge(container_body_reply(member));
        }
        calcs.accept_data(&needs, Ok(reply));
        let current = evaluator(&calcs, &schema);
        assert_eq!(
            current.display(&current.named_field_value("Outer", 1).unwrap()),
            "24"
        );
    }

    #[test]
    fn pending_assignment_does_not_publish_guessed_host_value() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(
            &mut schema,
            56,
            "Outer",
            "Let ( $$n = Length ( Jobs::One ) ; $$n )",
        );
        let current = evaluator(&calcs, &schema);
        current
            .globals
            .borrow_mut()
            .set("$$n", 1, Value::number(91));
        assert!(current.named_field_value("Outer", 1).is_none());
        assert_eq!(
            current
                .globals
                .borrow()
                .get("$$n", 1)
                .unwrap()
                .to_number(&Ctx::new(&schema.locale, current.today))
                .to_i64(),
            Some(91)
        );
        drop(current);
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![3]
            }]
        );
        calcs.accept_data(&needs, Ok(container_body_reply(3)));
        let current = evaluator(&calcs, &schema);
        assert_eq!(
            current.display(&current.named_field_value("Outer", 1).unwrap()),
            "3"
        );
        assert_eq!(
            current
                .globals
                .borrow()
                .get("$$n", 1)
                .unwrap()
                .to_number(&Ctx::new(&schema.locale, current.today))
                .to_i64(),
            Some(3)
        );
    }

    #[test]
    fn pending_evaluate_text_does_not_resume_stale_successors() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = Evaluate ( \"Let ( $n = \" & Length ( Jobs::One ) & \" ; 0 )\" ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }

    #[test]
    fn pending_get_field_text_waits_before_executing_an_unstored_field() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(&mut schema, 55, "Inner3", "Let ( $n = 3 ; 0 )");
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = GetField ( \"Jobs::Inner\" & Length ( Jobs::One ) ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }

    #[test]
    fn permanent_dynamic_expression_errors_leave_successors_catchable() {
        for expression in [
            "If ( EvaluationError ( Evaluate ( \"Let (\" ) ) > 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "If ( not IsValid ( GetField ( \"MissingField\" ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "Let ( [ $n = 3 ; ignored = Evaluate ( \"Let (\" ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( [ $n = 3 ; ignored = GetField ( \"MissingField\" ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(&mut schema, 56, "Outer", expression);
            pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
        }
    }

    #[test]
    fn pending_field_helpers_propagate_nested_control_suspensions() {
        for field in [
            "Jobs::InnerSideEffect",
            "Sum ( Jobs::InnerSideEffect )",
            "Last ( Jobs::InnerSideEffect )",
            "GetNthRecord ( Jobs::InnerSideEffect ; 1 )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            calcs
                .cache
                .get_mut()
                .found
                .insert(OCCURRENCE, Arc::from([1]));
            pending_add_calc(
                &mut schema,
                55,
                "InnerSideEffect",
                "If ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )",
            );
            pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ $n = 0 ; ignored = {field} ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
            pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
        }
    }

    #[test]
    fn pending_nested_loop_progress_does_not_resume_stale_successors() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(&mut schema, 55, "InnerSideEffect", "While ( [ i = 0 ] ; i < 8 ; [ i = i + 1 ; ignored = Length ( Jobs::One ) ; $n = $n + 1 ] ; 0 )");
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = Jobs::InnerSideEffect ] ; If ( $n = 8 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }

    #[test]
    fn permanent_nested_limit_errors_do_not_suspend_callers() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(
            &mut schema,
            55,
            "Inner",
            "SetRecursion ( While ( [ i = 0 ] ; i < 2 ; [ i = i + 1 ] ; i ) ; 1 )",
        );
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 3 ; ignored = Jobs::Inner ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
    }
    #[test]
    fn pending_nth_record_selector_does_not_skip_nested_assignment() {
        let (mut schema, mut calcs) = pending_container_fixture();
        let values = calcs.cache.get_mut().records[&(TABLE, 1)].clone();
        calcs.cache.get_mut().records.insert((TABLE, 3), values);
        calcs
            .cache
            .get_mut()
            .found
            .insert(OCCURRENCE, Arc::from([1, 2, 3]));
        pending_add_calc(&mut schema, 55, "Inner", "Let ( $n = 3 ; 0 )");
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = GetNthRecord ( Jobs::Inner ; Length ( Jobs::One ) ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }

    #[test]
    fn pending_repetition_selector_does_not_skip_nested_assignment() {
        let (mut schema, mut calcs) = pending_container_fixture();
        let code = formula(&schema, "Let ( $n = 3 ; 0 )");
        schema.add_field(
            TABLE,
            FieldDef::calculation(55, "Inner", ValueType::Number, code, false).with_repetitions(3),
        );
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = GetRepetition ( Jobs::Inner ; Length ( Jobs::One ) ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }

    #[test]
    fn pending_lookup_and_lookupnext_propagate_pending_nested_assignment() {
        for call in [
            "Lookup ( Items::Inner ; 99 )",
            "LookupNext ( Items::Inner ; Higher )",
            "Sum ( Items::Inner )",
            "Last ( Items::Inner )",
            "GetNthRecord ( Items::Inner ; 1 )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            schema.add_relationship(om_data::host::RelationshipDef::new(
                1,
                OCCURRENCE,
                ROW_OCCURRENCE,
                vec![],
            ));
            calcs
                .cache
                .get_mut()
                .related
                .insert((OCCURRENCE, 1, ROW_OCCURRENCE), Arc::from([10]));
            calcs
                .cache
                .get_mut()
                .related
                .insert((ROW_OCCURRENCE, 10, OCCURRENCE), Arc::from([1]));
            calcs
                .cache
                .get_mut()
                .records
                .insert((ROW_TABLE, 10), Some(Arc::new(RecordValues::default())));
            let code = formula(
                &schema,
                "If ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )",
            );
            schema.add_field(
                ROW_TABLE,
                FieldDef::calculation(55, "Inner", ValueType::Number, code, false),
            );
            pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ $n = 0 ; ignored = {call} ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
            pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
        }
    }

    #[test]
    fn pending_multilevel_suspension_survives_ignored_error_wrappers() {
        for wrapper in [
            "Jobs::Middle",
            "IsValid ( Jobs::Middle )",
            "EvaluationError ( Jobs::Middle )",
            "SetPrecision ( Jobs::Middle ; 60 )",
            "SetRecursion ( Jobs::Middle ; 9 )",
            "Evaluate ( \"Jobs::Middle\" )",
            "GetField ( \"Jobs::Middle\" )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(
                &mut schema,
                54,
                "Inner",
                "If ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )",
            );
            pending_add_calc(
                &mut schema,
                55,
                "Middle",
                "Let ( ignored = Jobs::Inner ; 0 )",
            );
            pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ $n = 0 ; ignored = {wrapper} ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
            pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
        }
    }

    #[test]
    fn pending_dynamic_text_aliases_and_custom_calls_wait() {
        for expression in [
            "Let ( [ t = \"Let ( $n = \" & Length ( Jobs::One ) & \" ; 0 )\" ; alias = t ; $n = 0 ; ignored = Evaluate ( alias ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( [ t = \"Jobs::Inner\" & Length ( Jobs::One ) ; alias = t ; $n = 0 ; ignored = GetField ( alias ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
            "Let ( [ $n = 0 ; ignored = RunPendingText ( \"Let ( $n = \" & Length ( Jobs::One ) & \" ; 0 )\" ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(&mut schema, 55, "Inner3", "Let ( $n = 3 ; 0 )");
            let parameters = vec!["t".to_string()];
            let mut cx = formula::Context::new(schema.names()); cx.parameters = &parameters;
            let body = formula::compile("Evaluate ( t )", &cx).unwrap().expr;
            schema.add_custom_function(995, om_data::calc::eval::CustomFunction { name: "RunPendingText".into(), parameters, body });
            pending_add_calc(&mut schema, 56, "Outer", expression);
            pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
        }
    }

    #[test]
    fn pending_text_becomes_permanent_invalid_and_is_caught() {
        for text in [
            "If ( EvaluationError ( Evaluate ( \"Let (\" & Length ( Jobs::One ) ) ) > 0 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "If ( not IsValid ( GetField ( \"Missing\" & Length ( Jobs::One ) ) ) ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) )",
            "Let ( [ $n = 3 ; ignored = Evaluate ( \"Let (\" & Length ( Jobs::One ) ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(&mut schema, 56, "Outer", text);
            pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
        }
    }

    #[test]
    fn pending_permanent_failure_after_pending_is_catchable_through_nesting() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(
            &mut schema,
            54,
            "Inner",
            "If ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 1 ; 0 ) ; Let ( $n = 3 ; 0 ) )",
        );
        pending_add_calc(&mut schema, 55, "Middle", "Jobs::Inner");
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = Jobs::Middle ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Outer", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![3]
            }]
        );
        calcs.accept_data(
            &needs,
            Err(om_data::DataError::new("synthetic permanent body error")),
        );
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Outer", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![2]
            }]
        );
        calcs.accept_data(&needs, Ok(container_body_reply(2)));
        let current = evaluator(&calcs, &schema);
        assert_eq!(
            current.display(&current.named_field_value("Outer", 1).unwrap()),
            "3"
        );
        assert!(calcs.take_wanted().is_empty());
    }

    #[test]
    fn pending_recursion_limit_does_not_become_permanent_nested_abort() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(&mut schema, 55, "Inner", "SetRecursion ( While ( [ i = 0 ] ; i < 1 ; [ i = i + 1 ; $n = 3 ] ; 0 ) ; Length ( Jobs::One ) )");
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = Jobs::Inner ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }
    #[test]
    fn known_selectors_and_limit_use_only_two_with_one_available() {
        for selector in [
            "GetNthRecord ( Jobs::Inner ; Length ( Jobs::One ) )",
            "GetRepetition ( Jobs::Inner ; Length ( Jobs::One ) )",
            "Jobs::Limited",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            let values = calcs.cache.get_mut().records[&(TABLE, 1)].clone();
            calcs.cache.get_mut().records.insert((TABLE, 3), values);
            calcs
                .cache
                .get_mut()
                .found
                .insert(OCCURRENCE, Arc::from([1, 2, 3]));
            let code = formula(&schema, "Let ( $n = 3 ; 0 )");
            schema.add_field(
                TABLE,
                FieldDef::calculation(54, "Inner", ValueType::Number, code, false)
                    .with_repetitions(3),
            );
            pending_add_calc(&mut schema, 55, "Limited", "SetRecursion ( While ( [ i = 0 ] ; i < 1 ; [ i = i + 1 ; $n = 3 ] ; 0 ) ; Length ( Jobs::One ) )");
            pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ $n = 0 ; ignored = {selector} ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
            let need = [CalcNeed::MemberData {
                table: TABLE,
                member: vec![3],
            }];
            calcs.accept_data(&need, Ok(container_body_reply(3)));
            pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
        }
    }

    #[test]
    fn known_permanent_invalid_selectors_leave_known_state_catchable() {
        for call in [
            "GetNthRecord ( Jobs::Inner ; 0 )",
            "GetRepetition ( Jobs::Inner ; 4 )",
            "GetNthRecord ( Jobs::Inner ; 1 / 0 )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            pending_add_calc(&mut schema, 55, "Inner", "Let ( $n = 1 ; 0 )");
            pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ $n = 3 ; ignored = IsValid ( {call} ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
            pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
        }
    }

    #[test]
    fn pending_precision_waits_before_running_a_precision_dependent_assignment() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = SetPrecision ( Let ( $n = If ( 1 + 10 ^ -20 > 1 ; 3 ; 1 ) ; 0 ) ; Length ( Jobs::One ) ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Outer", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![3]
            }]
        );
        let mut data = container_body_reply(3);
        let stored = data
            .library
            .data
            .get_mut(&(TABLE, vec![3]))
            .unwrap()
            .as_mut()
            .unwrap();
        Arc::make_mut(stored).data.streams[0].1 = vec![b'x'; 60];
        calcs.accept_data(&needs, Ok(data));
        pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
    }

    #[test]
    fn pending_found_set_waits_before_skipping_an_unstored_target() {
        let (mut schema, mut calcs) = pending_container_fixture();
        pending_add_calc(&mut schema, 55, "Inner", "Let ( $n = 3 ; 0 )");
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = GetNthRecord ( Jobs::Inner ; 1 ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        assert!(evaluator(&calcs, &schema)
            .named_field_value("Outer", 1)
            .is_none());
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::FoundSet {
                occurrence: OCCURRENCE,
                table: TABLE
            }]
        );
        let mut data = CalcData::default();
        data.found.insert(OCCURRENCE, Arc::from([1]));
        calcs.accept_data(&needs, Ok(data));
        pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
    }

    #[test]
    fn pending_related_sets_wait_before_skipping_unstored_targets_or_running_fallbacks() {
        for call in [
            "Items::Inner",
            "Lookup ( Items::Inner ; Let ( $n = 1 ; 0 ) )",
            "LookupNext ( Items::Inner ; Higher )",
            "Sum ( Items::Inner )",
            "Last ( Items::Inner )",
            "GetNthRecord ( Items::Inner ; 1 )",
        ] {
            let (mut schema, mut calcs) = pending_container_fixture();
            schema.add_relationship(om_data::host::RelationshipDef::new(
                1,
                OCCURRENCE,
                ROW_OCCURRENCE,
                vec![],
            ));
            let code = formula(&schema, "Let ( $n = 3 ; 0 )");
            schema.add_field(
                ROW_TABLE,
                FieldDef::calculation(55, "Inner", ValueType::Number, code, false),
            );
            pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ $n = 0 ; ignored = {call} ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
            assert!(evaluator(&calcs, &schema)
                .named_field_value("Outer", 1)
                .is_none());
            let needs = calcs.take_wanted();
            assert_eq!(
                needs,
                [CalcNeed::RelatedIds {
                    from: OCCURRENCE,
                    record: 1,
                    to: ROW_OCCURRENCE
                }],
                "{call}"
            );
            let mut data = CalcData::default();
            data.related
                .insert((OCCURRENCE, 1, ROW_OCCURRENCE), Arc::from([10]));
            calcs.accept_data(&needs, Ok(data));
            pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
        }
    }

    #[test]
    fn pending_lookup_direction_waits_before_running_its_target() {
        let (mut schema, mut calcs) = pending_container_fixture();
        schema.add_relationship(om_data::host::RelationshipDef::new(
            1,
            OCCURRENCE,
            ROW_OCCURRENCE,
            vec![],
        ));
        calcs
            .cache
            .get_mut()
            .related
            .insert((OCCURRENCE, 1, ROW_OCCURRENCE), Arc::from([10]));
        let code = formula(&schema, "Let ( $n = 3 ; 0 )");
        schema.add_field(
            ROW_TABLE,
            FieldDef::calculation(55, "Inner", ValueType::Number, code, false),
        );
        pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = LookupNext ( Items::Inner ; Length ( Jobs::One ) > 0 ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
        pending_expect_steps(&schema, &mut calcs, "Outer", false, true);
    }

    #[test]
    fn pending_lookup_fallback_waits_for_a_stored_targets_record_set() {
        for result in 0..3 {
            let (mut schema, mut calcs) = pending_container_fixture();
            schema.add_relationship(om_data::host::RelationshipDef::new(
                1,
                OCCURRENCE,
                ROW_OCCURRENCE,
                vec![],
            ));
            schema.add_field(ROW_TABLE, FieldDef::data(55, "Stored", ValueType::Number));
            let (initial, fallback) = if result == 0 { (3, 1) } else { (0, 3) };
            pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ $n = {initial} ; ignored = Lookup ( Items::Stored ; Let ( $n = {fallback} ; 0 ) ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
            assert!(evaluator(&calcs, &schema)
                .named_field_value("Outer", 1)
                .is_none());
            let needs = calcs.take_wanted();
            assert_eq!(
                needs,
                [CalcNeed::RelatedIds {
                    from: OCCURRENCE,
                    record: 1,
                    to: ROW_OCCURRENCE
                }]
            );
            if result == 2 {
                calcs.accept_data(
                    &needs,
                    Err(DataError::new("synthetic permanent lookup failure")),
                );
            } else {
                let mut data = CalcData::default();
                let records: Arc<[u64]> = if result == 0 {
                    Arc::from([10])
                } else {
                    Arc::from([])
                };
                data.related
                    .insert((OCCURRENCE, 1, ROW_OCCURRENCE), records);
                let mut values = RecordValues::default();
                values.set(55, 1, Value::number(9));
                data.records.insert((ROW_TABLE, 10), Some(Arc::new(values)));
                calcs.accept_data(&needs, Ok(data));
            }
            pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
        }
    }
    #[test]
    fn draft_changes_and_reverts_update_calculations_without_invalidating_stable_paints() {
        use crate::session::{Session, Target};
        use om_data::{Field, FieldKind, LayoutInfo, Record, RecordPage};

        let mut schema = CalcSchema::new("Drafts", Locale::en_us());
        schema.add_table(10, "Data");
        schema.add_occurrence(20, "Data", 10);
        schema.add_layout(7, "Form", Some(20));
        schema.add_field(10, FieldDef::data(1, "Input", ValueType::Text));
        schema.add_field(
            10,
            FieldDef::calculation(
                2,
                "Result",
                ValueType::Text,
                formula(&schema, "Data::Input"),
                false,
            ),
        );
        let mut calcs = Calculations::default();
        calcs.set_schema(Some(Arc::new(schema)));
        let mut values = RecordValues::default();
        values.set(1, 1, Value::text("Stored"));
        calcs
            .cache
            .get_mut()
            .records
            .insert((10, 1), Some(Arc::new(values)));
        let mut session = Session::new(
            Target {
                host: "draft.invalid".into(),
                file: "Drafts".into(),
                local: None,
            },
            25,
        );
        let key = session
            .set_layouts(vec![LayoutInfo {
                id: 7,
                name: "Form".into(),
            }])
            .unwrap();
        session
            .accept_page(
                key,
                Ok(RecordPage {
                    versions: Default::default(),
                    fields: ["Input", "Result"]
                        .into_iter()
                        .map(|name| Field {
                            name: name.into(),
                            kind: FieldKind::Text,
                        })
                        .collect(),
                    records: vec![Record {
                        id: 1,
                        values: vec![Some("Stored".into()), None],
                    }],
                    total: 1,
                }),
            )
            .unwrap();
        let evaluate = |doc: &DocumentState| {
            let eval = Evaluator::new(
                &calcs,
                &session,
                doc,
                SessionInfo::default(),
                &Variables::new(),
            )
            .unwrap();
            let value = eval.named_field_value("Result", 1).unwrap();
            (eval.content_revision(), eval.display(&value))
        };
        let mut doc = DocumentState::default();
        let initial = evaluate(&doc);
        assert_eq!(initial.1, "Stored");
        assert_eq!(evaluate(&doc), initial);

        doc.put_draft(7, 1, "Input", "Stored".into(), "short".into());
        let short = evaluate(&doc);
        assert_eq!(short.1, "short");
        assert_ne!(short.0, initial.0);
        assert_eq!(
            evaluate(&doc),
            short,
            "unchanged drafts must let measured commands proceed"
        );
        // A frame of the same draft keeps its results rather than
        // evaluating them again (#1308).
        Evaluator::new(
            &calcs,
            &session,
            &doc,
            SessionInfo::default(),
            &Variables::new(),
        )
        .unwrap();
        assert!(!calcs.named_values.borrow().is_empty());

        doc.put_draft(7, 1, "Input", "Stored".into(), "a longer draft".into());
        let long = evaluate(&doc);
        assert_eq!(long.1, "a longer draft");
        assert_ne!(long.0, short.0);
        assert_eq!(evaluate(&doc), long);

        // Returning to an earlier draft is a new change too.
        doc.put_draft(7, 1, "Input", "Stored".into(), "short".into());
        let shortened = evaluate(&doc);
        assert_eq!(shortened.1, "short");
        assert_ne!(shortened.0, long.0);
        assert_eq!(evaluate(&doc), shortened);

        doc.put_draft(7, 1, "Input", "Stored".into(), String::new());
        let empty = evaluate(&doc);
        assert_eq!(empty.1, "");
        assert_ne!(empty.0, shortened.0);
        assert_eq!(evaluate(&doc), empty);

        doc.revert(&session);
        let reverted = evaluate(&doc);
        assert_eq!(reverted.1, "Stored");
        assert_ne!(reverted.0, empty.0);
        assert_eq!(evaluate(&doc), reverted, "a cleared draft must settle too");
    }

    mod host_boundaries;
    mod visibility;
}
