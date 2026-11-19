//! A calculation host over a file's data (calc-values.md 8.6-8.8, 9): the
//! [`om_calc::eval::CalcHost`] a client evaluates field, layout and script
//! calculations with, backed by a provider.
//!
//! ```text
//! CalcSchema   tables, fields (types, repetitions, storage, formulas),
//!   |          occurrences, relationships, custom functions, layouts
//! CalcSource   records, every record of a table, related sets, globals
//!   |            provider source (downloads)  -> Recorder -> CalcData
//!   |            CachedSource (CalcData, a client without the file)
//! FileCalc     schema + source: the data half of CalcHost
//! FileHost     FileCalc + SessionInfo (Get) + variables: a whole CalcHost
//! FileSql      FileCalc as ExecuteSQL's source (sql.rs)
//! ```
//!
//! Every field of a record is readable, not only those a layout shows;
//! related fields go through the relationship graph (every operator, all
//! predicates of a relationship together, the last relationship's sort
//! order or creation order; file-metadata.md 4); the found set answers
//! `GetNthRecord`, `Get ( FoundCount )` and `Get ( RecordNumber )`;
//! unstored calculations evaluate when read, empty when every field they
//! reference is unless set to evaluate always; summary fields compute every
//! kind over the found set with `crate::summary`, with `GetSummary` break
//! groups when the found set's sort is known (file-metadata.md 5); the
//! Design functions and value
//! lists answer from the schema (`design`); custom functions, names and
//! the file's locale come from the schema; `ExecuteSQL` and `ExecuteSQLe`
//! query the file through [`sql::FileSql`]. A formula of a related file
//! imported into the schema ([`CalcSchema::import_file_schema`]) resolves
//! names, locale, `$$` variables, the file `Get` selectors, ExecuteSQL's
//! catalog and the Design functions in that file, the file owning the
//! formula even when it has no occurrence of its own (calc-values.md 13,
//! [`Context::file`]). Data the source cannot supply fails with
//! FileMaker's error (101 record missing, 103 relationship missing, 1225
//! not supported) rather than reading as empty.

mod data;
mod design;
pub mod layout;
pub mod layout_objects;
pub mod machine;
mod match_keys;
pub mod needs;
pub mod privilege_set;
mod schema;
pub mod security;
pub mod session;
mod sort;
pub mod sql;
mod summary;
pub mod window;

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::Arc;

use om_calc::eval::{
    default_call, evaluate, CalcHost, CalcLimits, Context, CustomFunction, FieldId, FieldInfo,
    HostFunction, LookupDirection, Pending, VarScope, Variables,
};
use om_calc::{CalcError, Ctx, Date, Locale, Value};
use om_formula::{Expr, Names};

pub use data::{
    join_compare_op, library_data, library_value, related_by_scan, relationship_missing,
    scan_values_bytes, stored_text, stored_value, styled_value, CachedSource, CalcCancel, CalcData,
    CalcNeed, CalcSource, Fetch, LookupKey, MemberKey, MemorySource, RecordValues, Recorder,
    ScanRows, ScannedSets, MEMBER_DATA_BUDGET, UNRESOLVED_SORT_FIELD,
};
pub use design::{layout_object_names, ValueListEntry};
pub use match_keys::source_match_values;
pub use schema::{
    CalcSchema, ExternalOccurrence, FieldClass, FieldDef, FileSchemaContext, Hop, ImportedFileMap,
    JoinOp, JoinPredicate, LayoutEntry, LayoutField, LayoutKind, OccurrenceDef, RelationshipDef,
    SummaryDef, SummaryKind, TableDef,
};
pub use session::{AccountInfo, OpenFile, Privileges, SessionInfo, WindowMode};
pub use sort::sort_records;
pub use summary::{FoundGroups, SummaryCache};
pub use window::{Frame, WindowInfo};

/// The record a window's calculations are evaluated on when it has no
/// current record (Find mode, an empty found set): a field reads empty
/// there (a desktop host gives Find mode's criterion itself), a global
/// field its value, nothing is related to it, and `Get ( RecordID )` is
/// 0, as FileMaker reads them (script-runtime.md 13.21), where a context
/// without a record ends a field reference with error 1224 (calc-values.md
/// 13). No record has this id.
pub const NO_RECORD: u64 = u64::MAX;

/// Record ids from here up name no stored record: [`NO_RECORD`], and the
/// ids a client gives records begun and not committed yet (the desktop's
/// portal creation rows). A host's record ids are far below it.
pub const UNSAVED_FLOOR: u64 = u64::MAX - (1 << 32);

/// Whether `record` names no stored record ([`UNSAVED_FLOOR`]).
pub fn unsaved_record(record: u64) -> bool {
    record >= UNSAVED_FLOOR
}

/// Error 101, "Record is missing".
pub fn record_missing() -> CalcError {
    CalcError::from_code(101).expect("nonzero")
}

/// Repetition `repetition` of `field` of `occurrence` on `record`, read
/// through `host` as a calculation referencing it reads it: an unstored
/// calculation is evaluated there (empty when every field it references is,
/// unless it evaluates always; [`CalcHost::evaluate_always`]), a summary is
/// computed over the found set, anything else is the stored value; a
/// failure is `?`. For field values a host shows or uses itself: record
/// pages, field objects, finds, summaries.
pub fn read_field(
    host: &mut dyn CalcHost,
    occurrence: u64,
    record: u64,
    field: u64,
    repetition: u32,
) -> Value {
    // The reference reads repetition `repetition` of a repeating field in
    // that repetition's context (calc-values.md 8.6).
    let reference = Expr::Field(om_formula::Field::new(occurrence, field, true));
    let context = Context {
        repetition,
        ..Context::record(occurrence, record)
    };
    evaluate(&reference, host, &context).value
}

/// Unstored calculations and summaries the host reads itself (a summary's
/// values, [`FileCalc::read_field`]), nested deeper than this, read as `?`:
/// FileMaker rejects circular definitions, so only a broken file gets here.
pub(crate) const MAX_READ_DEPTH: u32 = 32;

thread_local! {
    static READ_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// One more level of the host's own nested reads; `None` past
/// [`MAX_READ_DEPTH`]. The level ends when the guard is dropped.
pub(crate) struct ReadDepth(());

impl ReadDepth {
    pub(crate) fn enter() -> Option<ReadDepth> {
        READ_DEPTH.with(|depth| {
            (depth.get() < MAX_READ_DEPTH).then(|| {
                depth.set(depth.get() + 1);
                ReadDepth(())
            })
        })
    }
}

impl Drop for ReadDepth {
    fn drop(&mut self) {
        READ_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// The data half of a calculation host: field definitions from the schema,
/// values from the source.
#[derive(Clone, Copy)]
pub struct FileCalc<'a> {
    pub schema: &'a CalcSchema,
    pub source: &'a dyn CalcSource,
    pub today: Date,
    /// What `Get` answers when the host evaluates an unstored calculation
    /// itself ([`Self::read_field`]); [`FileHost`] gives its own session.
    pub session: Option<&'a SessionInfo>,
    /// Optional summary reuse during one immutable calculation pass.
    summaries: Option<&'a SummaryCache>,
    reads: Option<&'a Cell<HostReads>>,
}

impl<'a> FileCalc<'a> {
    pub fn new(schema: &'a CalcSchema, source: &'a dyn CalcSource, today: Date) -> FileCalc<'a> {
        FileCalc {
            schema,
            source,
            today,
            session: None,
            summaries: None,
            reads: None,
        }
    }

    /// The same data with `session`'s `Get` answers.
    pub fn with_session(self, session: &'a SessionInfo) -> FileCalc<'a> {
        FileCalc {
            session: Some(session),
            ..self
        }
    }

    /// Reuse summaries during this pass. The caller must clear or replace
    /// the cache after data, found sets, sorts, variables or session state
    /// change, including when missing calculation data arrives.
    pub fn with_summary_cache(self, cache: &'a SummaryCache) -> FileCalc<'a> {
        FileCalc {
            summaries: Some(cache),
            ..self
        }
    }

    fn with_reads<'b>(&'b self, reads: &'b Cell<HostReads>) -> FileCalc<'b> {
        FileCalc {
            reads: Some(reads),
            ..*self
        }
    }

    fn record_reads(&self, reads: HostReads) {
        if let Some(target) = self.reads {
            target.set(target.get().union(reads));
        }
    }

    pub fn cx(&self) -> Ctx<'a> {
        Ctx::new(&self.schema.locale, self.today)
    }

    /// [`Self::cx`] in the locale of the file owning `occurrence`: the
    /// file whose formulas and relationships use it.
    pub fn cx_for(&self, occurrence: u64) -> Ctx<'a> {
        let locale = self
            .schema
            .file_context(occurrence)
            .map_or(&self.schema.locale, |file| &file.locale);
        Ctx::new(locale, self.today)
    }

    /// [`Self::cx`] in the locale of the file owning the base table
    /// `occurrence` shows: the file whose engine computes that table's
    /// values, such as its summaries, whichever file's occurrence reads
    /// them (a value carries its file's locale, calc-values.md 9.2).
    pub fn table_cx(&self, occurrence: u64) -> Ctx<'a> {
        let locale = self
            .schema
            .table_of(occurrence)
            .and_then(|table| self.schema.table_context(table.id))
            .map_or(&self.schema.locale, |file| &file.locale);
        Ctx::new(locale, self.today)
    }

    fn table_id(&self, occurrence: u64) -> Result<u64, CalcError> {
        let table = self.schema.table_of(occurrence).map(|t| t.id);
        if table.is_none() {
            self.await_file(occurrence, CalcError::TABLE_MISSING);
        }
        table.ok_or(CalcError::TABLE_MISSING)
    }

    pub fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        let info = self.schema.field(occurrence, field).map(FieldDef::info);
        if info.is_none() {
            self.await_file(occurrence, CalcError::FIELD_MISSING);
        }
        info
    }

    /// Whether `occurrence` waits for its file or its table's fields
    /// ([`CalcSchema::is_pending`]): when it does, it is asked for
    /// ([`CalcSource::need_occurrence`]), so a reader that reads the schema
    /// directly waits as the evaluator's reads do ([`Self::field`]).
    fn waits(&self, occurrence: u64) -> bool {
        let pending = self.schema.is_pending(occurrence);
        if pending {
            self.source.need_occurrence(occurrence);
        }
        pending
    }

    /// `field`'s definition in `occurrence`'s table, for a reader that
    /// reads it directly. A waiting occurrence ([`Self::waits`]) has none:
    /// its read ends with 1224, data still to be fetched, its need listed.
    /// A field the schema lacks otherwise is missing (102).
    fn field_def(&self, occurrence: u64, field: u64) -> Result<&'a FieldDef, CalcError> {
        match self.schema.field(occurrence, field) {
            Some(def) => Ok(def),
            None if self.waits(occurrence) => Err(CalcError::from_code(1224).expect("nonzero")),
            None => Err(CalcError::FIELD_MISSING),
        }
    }

    /// An occurrence whose related file has not been opened yet
    /// ([`CalcSchema::is_pending`]) is asked for, so the read waits for
    /// the file rather than reading it missing. Anything else the schema
    /// lacks reads as missing (`error`), which the source is told
    /// ([`CalcSource::missing_in_schema`]).
    fn await_file(&self, occurrence: u64, error: CalcError) {
        if self.schema.is_pending(occurrence) {
            self.source.need_occurrence(occurrence);
        } else {
            self.source.missing_in_schema(occurrence, error);
        }
    }

    /// A stored, global or summary field's value (unstored calculations
    /// the evaluator computes itself from [`FieldDef::info`]).
    pub fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        let def = self.field_def(occurrence, field)?;
        let table = self.table_id(occurrence)?;
        if def.class == FieldClass::Summary {
            return self.summary_value(occurrence, record, def, repetition, None);
        }
        let empty = || Value::empty(def.value_type);
        let mut version = None;
        let value = if def.global {
            let globals = self.source.globals(table)?;
            globals
                .get(field, repetition)
                .cloned()
                .unwrap_or_else(empty)
        } else if record == NO_RECORD {
            empty()
        } else {
            match self.source.record(table, record)? {
                Some(values) => {
                    version = values.mod_count;
                    values.get(field, repetition).cloned().unwrap_or_else(empty)
                }
                None => return Err(record_missing()),
            }
        };
        // A container names its library member (containers.md 1.4).
        let mut value = library_value(self.source, table, value)?;
        if let (Value::Container(container), Some(version)) = (&mut value, version) {
            if let Some(library) = &mut container.library {
                std::sync::Arc::make_mut(library).source = Some(om_calc::value::ContainerSource {
                    table,
                    record,
                    field,
                    repetition,
                    version,
                });
            }
        }
        Ok(value)
    }

    /// [`Self::field_value`] read from `context`: a summary of a related
    /// or portal occurrence summarizes the parent record's related set (a
    /// portal chooses only the row it shows on), and a summary of the
    /// context's own occurrence the set its record was read in
    /// ([`Context::related_from`]), else the found set.
    pub fn field_value_in_context(
        &self,
        context: &Context,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        let def = self.field_def(occurrence, field)?;
        if def.class == FieldClass::Summary {
            let related_from = self.summary_related_from(context, occurrence);
            return self.summary_value(occurrence, record, def, repetition, related_from);
        }
        self.field_value(occurrence, record, field, repetition)
    }

    /// The parent whose related set a summary of `occurrence` read from
    /// `context` summarizes; `None` for the found set.
    fn summary_related_from(&self, context: &Context, occurrence: u64) -> Option<(u64, u64)> {
        if context.occurrence == Some(occurrence) {
            context.related_from
        } else {
            self.schema.relationship_origin(context, occurrence)
        }
    }

    /// `field` of `occurrence` on `record` as a calculation reads it: an
    /// unstored calculation evaluated on the record (in its formula's
    /// occurrence, with the "do not evaluate if all referenced fields are
    /// empty" option), a summary over the found set, anything else as
    /// stored. For what the host reads itself: a summary's values, sort
    /// keys, finds.
    pub fn read_field(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        let def = self.field_def(occurrence, field)?;
        if !def.is_unstored_calculation() || def.formula.is_none() {
            return self.field_value(occurrence, record, field, repetition);
        }
        if repetition == 0 || repetition > def.repetitions.max(1) {
            return Ok(Value::empty(def.value_type));
        }
        let Some(_depth) = ReadDepth::enter() else {
            return Ok(Value::invalid());
        };
        let fallback = SessionInfo::default();
        let session = self.session.unwrap_or(&fallback);
        let (mut locals, mut globals) = (Variables::new(), Variables::new());
        let mut host = FileHost::new(*self, session, &mut locals, &mut globals);
        let value = read_field(&mut host, occurrence, record, field, repetition);
        self.record_reads(host.read_provenance());
        Ok(value)
    }

    /// [`Self::read_field`] for a reader that must not take a value
    /// OpenMaker could not compute for one: a value list's items (#1058).
    /// A stored, global or summary field reads as [`Self::field_value`]
    /// reads it. An unstored calculation fails where it would read as `?`
    /// or with a `?` inside it:
    /// - error 1224 when it read data the source is still fetching
    ///   ([`CalcSource::pending`]): the need is listed, and the reader runs
    ///   again once it arrives, as for a stored value not read yet;
    /// - the read's error when a read it made failed for good (data the
    ///   provider could not supply, refused or missing, or a field or
    ///   table the schema does not have,
    ///   [`CalcSource::missing_in_schema`]), which the calculation would
    ///   carry on over as `?`;
    /// - an unknown error (-1) when it evaluated to `?` (`1 / 0`).
    pub fn read_field_computed(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        let def = self.field_def(occurrence, field)?;
        if !def.is_unstored_calculation() || def.formula.is_none() {
            return self.field_value(occurrence, record, field, repetition);
        }
        // The calculation keeps its own errors from its reader (FileMaker
        // shows a field's `?` without an error): what it read tells.
        let reads = data::Recorder::new(self.source);
        let file = FileCalc {
            source: &reads,
            ..*self
        };
        let before = self.source.pending();
        let value = file.read_field(occurrence, record, field, repetition)?;
        if self.source.pending().reads > before.reads {
            return Err(CalcError::from_code(1224).expect("nonzero"));
        }
        if let Some(error) = reads.failure() {
            return Err(error);
        }
        if value.is_invalid() {
            return Err(CalcError::UNKNOWN);
        }
        Ok(value)
    }

    /// The records of `related` related to `record` of `occurrence`.
    pub fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        if record == NO_RECORD {
            return Ok(Vec::new());
        }
        let cx = self.cx_for(occurrence);
        Ok(self
            .source
            .related_ids(self.schema, occurrence, record, related, &cx)?
            .to_vec())
    }

    /// One related record, in the related set's order (the first is what
    /// `Rel::field` shows).
    pub fn related_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        index: usize,
    ) -> Result<Option<u64>, CalcError> {
        Ok(self
            .related_records(occurrence, record, related)?
            .get(index)
            .copied())
    }

    pub fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        let table = self.table_id(occurrence)?;
        Ok(self
            .source
            .found_set(occurrence, table)?
            .get(index)
            .copied())
    }

    /// The name of the base table `occurrence` shows, in its own file.
    pub fn base_table_name(&self, occurrence: u64) -> Option<&'a str> {
        let table = self.schema.table_of(occurrence)?;
        self.schema.local_table_name(table.id)
    }

    pub fn custom_function(&self, id: u64) -> Option<Arc<CustomFunction>> {
        self.schema.custom_function(id)
    }

    /// `Get ( FoundCount )`: what the window shows, else the found set of
    /// the context's occurrence; 0 without a record (calc-values.md 13).
    pub fn found_count(&self, context: &Context, window: Option<u64>) -> Result<Value, CalcError> {
        let (Some(occurrence), Some(_)) = (context.occurrence, context.record) else {
            return Ok(Value::number(0));
        };
        if let Some(count) = window {
            return Ok(Value::number(count as i64));
        }
        let table = self.table_id(occurrence)?;
        Ok(Value::number(
            self.source.found_set(occurrence, table)?.len() as i64,
        ))
    }

    /// `Get ( RecordNumber )`: the record's position in the found set, or
    /// the portal row's number in a portal row (calc-functions.md 6.1.1).
    pub fn record_number(&self, context: &Context) -> Result<Value, CalcError> {
        let (Some(occurrence), Some(record)) = (context.occurrence, context.record) else {
            return Ok(Value::number(0));
        };
        if let Some(row) = context.portal_row_number {
            return Ok(Value::number(i64::try_from(row).unwrap_or(i64::MAX)));
        }
        let table = self.table_id(occurrence)?;
        let found = self.source.found_set(occurrence, table)?;
        Ok(Value::number(
            found
                .iter()
                .position(|&r| r == record)
                .map_or(0, |p| p as i64 + 1),
        ))
    }

    pub fn total_count(&self, context: &Context) -> Result<Value, CalcError> {
        let (Some(occurrence), Some(_)) = (context.occurrence, context.record) else {
            return Ok(Value::number(0));
        };
        let table = self.table_id(occurrence)?;
        Ok(Value::number(self.source.records(table)?.len() as i64))
    }

    pub fn modification_count(&self, context: &Context) -> Result<Value, CalcError> {
        let (Some(occurrence), Some(record)) = (context.occurrence, context.record) else {
            return Ok(Value::empty_text());
        };
        if record == NO_RECORD {
            return Ok(Value::number(0));
        }
        let table = self.table_id(occurrence)?;
        let values = self.source.record(table, record)?.ok_or(record_missing())?;
        Ok(Value::number(values.mod_count.unwrap_or(0) as i64))
    }
}

/// The variable maps a host's calculations read and set.
pub enum Vars<'a> {
    /// `$` variables in one map, `$$` variables in the other.
    Scoped {
        locals: &'a mut Variables,
        globals: &'a mut Variables,
    },
    /// One map for both (names keep their dollar signs).
    Shared(&'a mut Variables),
}

impl Vars<'_> {
    pub fn scope(&mut self, scope: VarScope) -> &mut Variables {
        match (self, scope) {
            (Vars::Scoped { locals, .. }, VarScope::Local) => locals,
            (Vars::Scoped { globals, .. }, VarScope::Global) => globals,
            (Vars::Shared(vars), _) => vars,
        }
    }

    /// The same maps for a nested host.
    pub fn reborrow(&mut self) -> Vars<'_> {
        match self {
            Vars::Scoped { locals, globals } => Vars::Scoped { locals, globals },
            Vars::Shared(vars) => Vars::Shared(vars),
        }
    }
}

/// A whole calculation host: a file's data, a session's `Get` answers and
/// variables. Clients with a script runtime pass its variable maps.
pub struct FileHost<'a> {
    pub file: FileCalc<'a>,
    pub session: &'a SessionInfo,
    pub vars: Vars<'a>,
    /// How many `ExecuteSQL` calls this evaluation is nested in (a
    /// calculation field a query reads may query again).
    pub sql_depth: u8,
    /// The UUID of the layout object whose calculation this is
    /// (`LayoutObjectUUID`), in FileMaker's form.
    pub layout_object: Option<String>,
    /// What the evaluations on this host read besides data.
    pub reads: HostReads,
    /// Dependencies of the object's Self, acquired only when it is read.
    pub self_reads: HostReads,
    /// Whether evaluations on this host must stop
    /// ([`CalcHost::interrupted`]); `None`: never.
    pub interrupted: Option<&'a dyn Fn() -> bool>,
    /// What evaluations on this host may build ([`CalcHost::limits`]).
    pub limits: CalcLimits,
    nested_reads: Cell<HostReads>,
}

/// What a host's evaluations read besides record data: the session's
/// window and account (a result that can change while no data does), and
/// answers this host gave without what FileMaker answers from (a privilege
/// selector whose metadata could not be read, a function or plugin this
/// host does not implement). Hosts that must not take such a result for
/// FileMaker's (a layout object's hide condition) can tell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostReads {
    pub session: bool,
    pub unknown: bool,
}

impl HostReads {
    pub fn union(self, other: Self) -> Self {
        Self {
            session: self.session || other.session,
            unknown: self.unknown || other.unknown,
        }
    }
}

impl<'a> FileHost<'a> {
    pub fn new(
        file: FileCalc<'a>,
        session: &'a SessionInfo,
        locals: &'a mut Variables,
        globals: &'a mut Variables,
    ) -> FileHost<'a> {
        FileHost::with_vars(file, session, Vars::Scoped { locals, globals })
    }

    pub fn with_vars(file: FileCalc<'a>, session: &'a SessionInfo, vars: Vars<'a>) -> FileHost<'a> {
        FileHost {
            file: file.with_session(session),
            session,
            vars,
            sql_depth: 0,
            layout_object: None,
            reads: HostReads::default(),
            self_reads: HostReads::default(),
            interrupted: None,
            limits: CalcLimits::DEFAULT,
            nested_reads: Cell::default(),
        }
    }

    /// Include nested summary inputs and cached summary dependencies.
    pub fn read_provenance(&self) -> HostReads {
        self.reads.union(self.nested_reads.get())
    }

    /// The imported related file owning the formula solved in `context`;
    /// `None` for this file's own.
    fn imported_file(&self, context: &Context) -> Option<&'a FileSchemaContext> {
        let schema: &'a CalcSchema = self.file.schema;
        schema.context_file(context)
    }
}

/// `Get ( FilePath )` of related file `name`: the file beside this one on
/// the same host, which is where a relative file reference finds it.
fn related_file_path(path: &str, name: &str) -> String {
    path.rfind('/')
        .map(|end| format!("{}{name}.fmp12", &path[..=end]))
        .unwrap_or_default()
}

impl CalcHost for FileHost<'_> {
    fn locale(&self) -> &Locale {
        &self.file.schema.locale
    }

    fn interrupted(&self) -> bool {
        self.interrupted.is_some_and(|f| f())
    }

    fn limits(&self) -> CalcLimits {
        self.limits
    }

    fn today(&self) -> Date {
        self.file.today
    }

    fn names(&self) -> &dyn Names {
        self.file.schema.names()
    }

    fn names_for(&self, context: &Context) -> &dyn Names {
        match self.imported_file(context) {
            Some(file) => &file.names,
            None => self.names(),
        }
    }

    fn locale_for(&self, context: &Context) -> &Locale {
        self.imported_file(context)
            .map_or(&self.file.schema.locale, |file| &file.locale)
    }

    fn table_file(&self, occurrence: u64) -> Option<u64> {
        self.file.schema.occurrence_table_file(occurrence)
    }

    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        self.file.field(occurrence, field)
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        self.file
            .with_reads(&self.nested_reads)
            .field_value(occurrence, record, field, repetition)
    }

    fn field_value_in_context(
        &self,
        context: &Context,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        self.file
            .with_reads(&self.nested_reads)
            .field_value_in_context(context, occurrence, record, field, repetition)
    }

    fn evaluate_always(&self, occurrence: u64, field: u64) -> bool {
        self.file
            .schema
            .field(occurrence, field)
            .map_or(true, FieldDef::evaluates_always)
    }

    fn relationship_origin(&self, context: &Context, related: u64) -> Option<(u64, u64)> {
        self.file.schema.relationship_origin(context, related)
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        self.file.related_records(occurrence, record, related)
    }

    fn related_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        index: usize,
    ) -> Result<Option<u64>, CalcError> {
        self.file.related_record(occurrence, record, related, index)
    }

    fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        self.file.found_record(occurrence, index)
    }

    fn lookup_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        _direction: Option<LookupDirection>,
    ) -> Result<Option<u64>, CalcError> {
        self.file.related_record(occurrence, record, related, 0)
    }

    fn summary(
        &self,
        context: &Context,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Value, CalcError> {
        self.file
            .with_reads(&self.nested_reads)
            .summary(context, summary, break_field)
    }

    fn pending(&self) -> Pending {
        self.file.source.pending()
    }

    fn base_table_name(&self, occurrence: u64) -> Option<&str> {
        self.file.base_table_name(occurrence)
    }

    fn self_value(&mut self, context: &Context) -> Result<Value, CalcError> {
        self.reads = self.reads.union(self.self_reads);
        context.self_value.clone().ok_or(CalcError::NOT_SUPPORTED)
    }

    fn get(&mut self, selector: u8, context: &Context) -> Result<Value, CalcError> {
        self.reads.session = true;
        if let Some(file) = self.imported_file(context) {
            use session::selector::{FILE_LOCALE_ELEMENTS, FILE_NAME, FILE_PATH};
            if matches!(
                selector,
                session::selector::ACCOUNT_TYPE
                    | session::selector::ACCOUNT_PASSWORD_DAYS_REMAINING
            ) {
                self.reads.unknown |= file.account_info.unknown(selector);
                let today = self
                    .session
                    .now
                    .map_or_else(|| session::today(self.session.utc_offset), |now| now.date());
                return Ok(file
                    .account_info
                    .answer(selector, today)
                    .expect("account selector"));
            }
            if let Some(value) = file.privilege_selector(selector) {
                self.reads.unknown |= file.unknown_privilege(selector);
                return Ok(value);
            }
            match selector {
                FILE_NAME => return Ok(Value::text(&file.file_name)),
                FILE_PATH => {
                    return Ok(Value::text(file.file_path.clone().unwrap_or_else(|| {
                        related_file_path(&self.session.file_path, &file.file_name)
                    })))
                }
                FILE_LOCALE_ELEMENTS => return Ok(Value::text(file.locale.elements_json(true))),
                _ => {}
            }
        }
        self.reads.unknown |= self.session.unknown_answer(selector);
        self.session
            .get(selector, context, &self.file)
            .unwrap_or_else(|| Ok(Value::empty_text()))
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        self.vars.scope(scope)
    }

    /// A related file's `$$` variables share the map under a key no
    /// variable name can have (names start with `$`).
    fn variable_name<'n>(&self, name: &'n str, context: &Context) -> Cow<'n, str> {
        match self.imported_file(context) {
            Some(file) if VarScope::of(name) == VarScope::Global => {
                Cow::Owned(format!("\u{1f}{}\u{1f}{name}", file.file_name))
            }
            _ => Cow::Borrowed(name),
        }
    }

    fn custom_function(&self, id: u64) -> Option<Arc<CustomFunction>> {
        self.file.custom_function(id)
    }

    fn container_data(
        &mut self,
        container: &om_calc::Container,
        _context: &Context,
    ) -> Result<om_calc::eval::StoredContainer, CalcError> {
        library_data(self.file.source, container)
    }

    fn call(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> Result<Value, CalcError> {
        match function {
            HostFunction::Builtin(builtin) => {
                if let Some(result) = self.file.design(builtin.name, args, context) {
                    return result;
                }
            }
            // The window's layout, its found set and focused field, in
            // screen points on a desktop (layout_objects).
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetLayoutObjectAttribute") => {
                self.reads.session = true;
                let (schema, session) = (self.file.schema, self.session);
                return Ok(layout_objects::answer(self, schema, session, args, context));
            }
            // Inside a layout object's calculation that object's UUID,
            // elsewhere error 10 (calc-functions.md 6.3 O5).
            HostFunction::Named(name) if name.eq_ignore_ascii_case("LayoutObjectUUID") => {
                return match &self.layout_object {
                    Some(uuid) => Ok(Value::text(uuid)),
                    None => Err(CalcError::from_code(10).expect("nonzero")),
                };
            }
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetLayoutObjectOwnerInfo") => {
                return Ok(Value::text(layout_objects::owner_info_not_found()));
            }
            _ => {}
        }
        let result = default_call(function);
        // Not FileMaker's answer: a function this host lacks.
        self.reads.unknown |= matches!(result, Err(error) if error == CalcError::NOT_SUPPORTED);
        result
    }

    fn call_reporting(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> (Value, Option<CalcError>) {
        if let Some(variant) = om_sql::calc::Variant::of(&function) {
            return sql::execute_in_context(
                self.file,
                self.session,
                self.vars.reborrow(),
                self.sql_depth,
                variant,
                args,
                context,
            );
        }
        match self.call(function, args, context) {
            Ok(v) => (v, None),
            Err(e) => (Value::invalid(), Some(e)),
        }
    }
}
