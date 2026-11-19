//! ExecuteSQL over a file's data (execute-sql.md 10.3): [`FileSql`], an
//! [`om_sql::SqlSource`] over a [`FileCalc`], which every host with a
//! calculation schema and a [`CalcSource`] queries through (the desktop
//! client's provider and windows, OpenMaker Server's script sessions), and
//! [`SqlCatalog`], what the system tables list beyond the calculation
//! schema.
//!
//! How a query reads the file:
//!
//! - Tables are the file's occurrences, those of other files without fields
//!   or records (4.1); fields come from the calculation schema.
//! - A table's records are its record list ([`CalcSource::records`]). The
//!   first value read from a record the source has not downloaded reads the
//!   rest of the found set ahead ([`CalcSource::prefetch`]): a scan
//!   downloads in batches, not record by record.
//! - `field = constant` in `WHERE` becomes a find on the host
//!   ([`CalcSource::lookup`]) where a find proposes every record holding the
//!   value (om-sql checks each proposed record again, 10.1): a stored,
//!   value-indexed text field with an English index or date field, and a
//!   constant of the field's type that is one whole index key (one line,
//!   no surrounding spaces, at most [`MAX_LOOKUP_TEXT`] characters). Number
//!   fields are scanned: OpenMaker Server's number index keys only digit
//!   text (`om_collate::number_key`), so a find for 3 would miss a stored
//!   `3.0` that SQL's `=` matches.
//! - Calculation fields evaluate on the query's cursor: `Get ( FoundCount
//!   )`, `Get ( RecordNumber )`, summaries and `GetNthRecord` see the
//!   table's found set after the `WHERE` conjuncts on that table; a
//!   calculation is empty when every field it references is (unless set to
//!   evaluate always), and one that fails reads as empty (implementation
//!   notes).
//! - A summary field's column is the summary over the cursor's found set,
//!   in its order (12, S3: `crate::summary`, read once per found set): the
//!   total of the records the `WHERE` leaves on every row, a running total
//!   so far, a fraction of the row's value. `om-sql` passes the records a
//!   query keeps after `FETCH` and `OFFSET`, in `ORDER BY` order with a
//!   `WHERE` (execute-sql.md 8.1).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use om_calc::eval::{evaluate, evaluate_formula, CalcHost, Context, Pending};
use om_calc::{CalcError, Ctx, Locale, Timestamp, Value, ValueType};
use om_formula::Expr;
use om_hbam::catalog::{Catalog, DataType, Field, FieldKind};
use om_hbam::{serial, FieldOptions, KeyTree, Node};
use om_sql::calc::Variant;
use om_sql::source::{
    BaseTableDef, Cursor, FieldClass as SqlClass, FieldDef as SqlField, FieldType, SqlSource,
    TableDef as SqlTable, ValueListDef, ValueListValues,
};
use om_sql::value::{compare, text_cmp, Val};
use om_sql::SqlError;

use super::summary::SummaryRun;
use super::{
    CalcSchema, CalcSource, FieldClass, FieldDef, FileCalc, FileHost, FileSchemaContext,
    SessionInfo, Vars,
};
use crate::meta::{self, value_list_option, ValueList, ValueSource};
use crate::rules::schema::FieldNodes;

/// Catalog keys (schema-path.md 5; file-metadata.md 0, 6).
const SCHEMA: u64 = 3;
const OCCURRENCES: u64 = 0x11;
const TABLE_FIELDS: u64 = 3;
const CATALOG_INDEX: u64 = 1;
const MEMBERS: u64 = 5;
const MOD_COUNT: u64 = 0xfc;
const VALUE_LISTS: u64 = 0x21;

/// Field option bits (schema-path.md 5.5): "Do not evaluate if all
/// referenced fields are empty" off, global storage, unstored.
const EVALUATE_ALWAYS: u32 = 0x20;
const GLOBAL: u32 = 0x1_0000;
const UNSTORED: u32 = 0x2_0000;
/// Index languages whose text keys `om_collate::text_key` builds: English
/// and English international (query-program.md 7.2).
const ENGLISH_INDEX: [u32; 2] = [21, 77];

/// The longest text constant a lookup is asked for; longer values may be
/// cut in the host's index keys.
pub const MAX_LOOKUP_TEXT: usize = 100;

/// Nested `ExecuteSQL` calls (a calculation field a query reads that
/// queries again) beyond this fail with error 2, as a runaway recursion
/// would exhaust FileMaker's memory [I].
const MAX_DEPTH: u8 = 16;

/// What ExecuteSQL reads from a file's catalogs beyond the calculation
/// schema (execute-sql.md 4.2, 8.1; file-metadata.md 1, 6): every table
/// occurrence (those of other files too), modification counts, the
/// calculation option "do not evaluate if all referenced fields are
/// empty", the fields a find can look values up in, and the value lists.
/// An empty catalog (a schema built by hand) lists the schema's own
/// occurrences.
#[derive(Clone, Debug, Default)]
pub struct SqlCatalog {
    /// Every table occurrence, in name order.
    pub occurrences: Vec<SqlOccurrence>,
    /// Base table modification counts.
    pub tables: HashMap<u64, u64>,
    /// By (base table, field).
    pub fields: HashMap<(u64, u64), SqlFieldInfo>,
    /// In name order.
    pub value_lists: Vec<SqlValueList>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlOccurrence {
    pub id: u64,
    pub name: String,
    /// Its base table; `None` for an occurrence of another file.
    pub table: Option<u64>,
    pub mod_count: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SqlFieldInfo {
    /// A calculation evaluated even when every field it references is
    /// empty (option `0x20`).
    pub always: bool,
    pub mod_count: u64,
    /// A find on the host's value index proposes every record that holds a
    /// value: a stored, value-indexed text field with an English index, or
    /// date field.
    pub findable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlValueList {
    pub id: u64,
    pub name: String,
    pub mod_count: u64,
    pub source: ValueListSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueListSource {
    /// Custom values, in order (dividers `-` included).
    Custom(Vec<String>),
    /// The values of a field: occurrence and field.
    Field { occurrence: u64, field: u64 },
    /// Related values only (option `0x1`): the one row `<Valuelist
    /// Unsupported>`.
    Related,
    /// Another file's list (type 2, not in the corpus): the same row [I].
    External,
}

impl ValueListSource {
    /// `FileMaker_ValueLists.Source`.
    pub fn name(&self) -> &'static str {
        match self {
            ValueListSource::Custom(_) => "Custom",
            ValueListSource::Field { .. } => "Field",
            ValueListSource::Related => "Related",
            ValueListSource::External => "External",
        }
    }
}

/// A modification count as stored: a length byte and a serial key.
fn mod_count(node: Option<&Node<'_>>) -> u64 {
    match node.and_then(|n| n.value).and_then(|v| v.split_first()) {
        Some((&len, key)) if usize::from(len) == key.len() => serial::decode(key).unwrap_or(0),
        _ => 0,
    }
}

/// A catalog id's serial-key value, the id FileMaker shows
/// (file-metadata.md 1).
fn serial_value(id: u64) -> u64 {
    serial::id_of_raw(id).unwrap_or(id)
}

/// A value list (file-metadata.md 6) as ExecuteSQL's system table lists it,
/// from the shared decode; `mod_count` is the member's `fc`.
fn sql_value_list(list: &ValueList, mod_count: u64) -> SqlValueList {
    let source = match &list.source {
        ValueSource::Custom(values) => ValueListSource::Custom(values.clone()),
        _ if list.options & value_list_option::SHOW_RELATED != 0 => ValueListSource::Related,
        ValueSource::Field { primary, .. } => match primary.occurrence {
            Some(occurrence) => ValueListSource::Field {
                occurrence,
                field: primary.field,
            },
            None => ValueListSource::External,
        },
        ValueSource::External { .. } | ValueSource::Calculation(_) => ValueListSource::External,
    };
    SqlValueList {
        id: list.id,
        name: list.name.clone(),
        mod_count,
        source,
    }
}

impl SqlFieldInfo {
    /// SQL keeps metadata even when the calculation schema omits this field.
    pub(crate) fn read(
        node: Option<&Node<'_>>,
        field: &Field,
        options: Option<&FieldOptions>,
    ) -> Self {
        let word = options.map_or(0, |o| o.options);
        let stored = match field.kind {
            FieldKind::Normal => word & GLOBAL == 0,
            FieldKind::Calculation => word & (GLOBAL | UNSTORED) == 0,
            _ => false,
        };
        let findable = stored
            && options.is_some_and(|o| {
                o.value_indexed()
                    && match field.data_type {
                        DataType::Text => ENGLISH_INDEX.contains(&o.index_language()),
                        DataType::Date => true,
                        _ => false,
                    }
            });
        Self {
            always: word & EVALUATE_ALWAYS != 0,
            mod_count: mod_count(node.and_then(|node| node.get(MOD_COUNT))),
            findable,
        }
    }
}

impl SqlCatalog {
    /// From the file's catalogs: `tree` holds `[3]`, every `[T, 3]` and,
    /// when the file has any, the value lists `[21]`.
    pub fn read(tree: &KeyTree<'_>, catalog: &Catalog) -> SqlCatalog {
        let mut fields = HashMap::new();
        for table in &catalog.tables {
            let nodes = FieldNodes::new(tree, table.id);
            for field in &table.fields {
                let options = field.decoded_options();
                fields.insert(
                    (table.id, field.id),
                    SqlFieldInfo::read(nodes.get(field.id), field, options.as_ref()),
                );
            }
        }
        Self::read_with_fields(tree, catalog, fields)
    }

    /// Finish the SQL catalog from metadata already read by the calculation
    /// schema's field pass, without building another index or decoding options.
    pub(crate) fn read_with_fields(
        tree: &KeyTree<'_>,
        catalog: &Catalog,
        fields: HashMap<(u64, u64), SqlFieldInfo>,
    ) -> SqlCatalog {
        let mut sql = SqlCatalog {
            fields,
            ..SqlCatalog::default()
        };
        for o in &catalog.table_occurrences {
            sql.occurrences.push(SqlOccurrence {
                id: o.id,
                name: o.name.clone(),
                table: o.base_table().filter(|t| catalog.table(*t).is_some()),
                mod_count: mod_count(tree.at(&[SCHEMA, OCCURRENCES, MEMBERS, o.id, MOD_COUNT])),
            });
        }
        sql.occurrences.sort_by_key(|o| o.name.to_lowercase());
        for t in &catalog.tables {
            sql.tables.insert(
                t.id,
                mod_count(tree.at(&[t.id, TABLE_FIELDS, CATALOG_INDEX, MOD_COUNT])),
            );
        }
        sql.value_lists = meta::value_lists(tree)
            .iter()
            .map(|list| {
                let fc = tree.at(&[VALUE_LISTS, MEMBERS, list.id, MOD_COUNT]);
                sql_value_list(list, mod_count(fc))
            })
            .collect();
        sql.value_lists.sort_by_key(|l| l.name.to_lowercase());
        sql
    }
}

/// `ExecuteSQL` or `ExecuteSQLe` over `file`, what [`FileHost`] answers:
/// the value and the error to report beside it.
pub fn execute(
    file: FileCalc<'_>,
    session: &SessionInfo,
    vars: Vars<'_>,
    depth: u8,
    variant: Variant,
    args: &[Value],
) -> (Value, Option<CalcError>) {
    execute_in_context(
        file,
        session,
        vars,
        depth,
        variant,
        args,
        &Context::default(),
    )
}

/// [`execute`] from a formula solved in `context`: a formula of an
/// imported related file queries that file's tables, in its locale.
pub fn execute_in_context(
    file: FileCalc<'_>,
    session: &SessionInfo,
    vars: Vars<'_>,
    depth: u8,
    variant: Variant,
    args: &[Value],
    context: &Context,
) -> (Value, Option<CalcError>) {
    if depth >= MAX_DEPTH {
        return om_sql::calc::error_result(variant, &SqlError::Engine(2));
    }
    let mut sql = FileSql::new(file, session, vars);
    sql.file_context = file.schema.context_file(context);
    sql.depth = depth;
    om_sql::calc::execute_sql(&mut sql, variant, args)
}

/// A query's found set (om-sql's) and the same records as a calculation
/// source's found set.
type SharedFound = (Rc<Vec<u64>>, Arc<[u64]>);

/// A summary field read over a query's found set (`None`: a total of
/// nothing), with that found set.
type FoundSummary = (Rc<Vec<u64>>, Rc<Option<SummaryRun>>);

/// The file as ExecuteSQL reads it.
pub struct FileSql<'a> {
    file: FileCalc<'a>,
    /// The imported related file whose tables the query reads; `None` for
    /// this file's.
    file_context: Option<&'a FileSchemaContext>,
    /// The session as the query's calculation fields see it: without a
    /// window, so `Get ( FoundCount )` is the query's found set, and with
    /// one clock for the whole query.
    session: SessionInfo,
    vars: Vars<'a>,
    depth: u8,
    now: Timestamp,
    /// The found set last handed to calculations, shared rather than
    /// copied for every value.
    found: Option<SharedFound>,
    /// Each calculation field's references, by (base table, field).
    references: HashMap<(u64, u64), Rc<Vec<om_formula::Field>>>,
    /// Summary fields over the found set they were read for, by
    /// (occurrence, field, repetition).
    summaries: HashMap<(u64, u64, u32), FoundSummary>,
}

impl<'a> FileSql<'a> {
    pub fn new(file: FileCalc<'a>, session: &SessionInfo, vars: Vars<'a>) -> FileSql<'a> {
        let now = session
            .now
            .or_else(|| super::session::now(session.utc_offset).ok())
            .unwrap_or_else(|| file.today.to_timestamp());
        FileSql {
            file,
            file_context: None,
            session: SessionInfo {
                found_count: None,
                active_record_number: None,
                record_open_state: None,
                modified_fields: Vec::new(),
                now: Some(now),
                ..session.clone()
            },
            vars,
            depth: 0,
            now,
            found: None,
            references: HashMap::new(),
            summaries: HashMap::new(),
        }
    }

    fn schema(&self) -> &'a CalcSchema {
        self.file.schema
    }

    /// The locale of the file the query reads.
    fn file_locale(&self) -> &'a Locale {
        self.file_context
            .map_or(&self.file.schema.locale, |file| &file.locale)
    }

    /// The catalog of the file the query reads.
    fn catalog(&self) -> &'a SqlCatalog {
        self.file_context
            .map_or_else(|| self.schema().sql(), |file| &file.sql)
    }

    /// Whether a member owned by `file` (`None`: this file) belongs to the
    /// file the query reads.
    fn reads(&self, file: Option<&FileSchemaContext>) -> bool {
        match (file, self.file_context) {
            (None, None) => true,
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }

    /// The id a member shows: its own file's.
    fn shown_id(&self, id: u64) -> u64 {
        self.file_context.map_or(id, |file| file.file_id(id))
    }

    /// A field's options, from the catalog of the file owning its table.
    fn info(&self, table: u64, field: u64) -> SqlFieldInfo {
        let schema = self.schema();
        schema
            .table_context(table)
            .map_or(schema.sql(), |file| &file.sql)
            .fields
            .get(&(table, field))
            .copied()
            .unwrap_or_default()
    }

    /// A field of base table `table` as FQL sees it.
    fn sql_field(&self, table: u64, f: &FieldDef) -> SqlField {
        SqlField {
            id: f.id,
            name: f.name.clone(),
            ty: field_type(f.value_type),
            class: match f.class {
                FieldClass::Data => SqlClass::Normal,
                FieldClass::Calculation => SqlClass::Calculated,
                FieldClass::Summary => SqlClass::Summary,
            },
            global: f.global,
            repetitions: f.repetitions.max(1),
            mod_count: self.info(table, f.id).mod_count,
        }
    }

    /// Read the found set ahead from `record` when the source would
    /// download it.
    fn read_ahead(&self, table: u64, record: u64, cursor: &Cursor) -> Result<(), CalcError> {
        let source = self.file.source;
        if source.has_record(table, record) {
            return Ok(());
        }
        let ahead = cursor.found.as_ref().and_then(|found| {
            let at = cursor.record_number.checked_sub(1)?;
            (found.get(at) == Some(&record)).then(|| &found[at..])
        });
        source.prefetch(table, ahead.unwrap_or(std::slice::from_ref(&record)))
    }

    /// The cursor's found set for calculations (every record of the table
    /// without one).
    fn cursor_found(&mut self, table: u64, cursor: &Cursor) -> Result<Arc<[u64]>, CalcError> {
        let Some(found) = &cursor.found else {
            return self.file.source.records(table);
        };
        if let Some((rc, shared)) = &self.found {
            if Rc::ptr_eq(rc, found) {
                return Ok(Arc::clone(shared));
            }
        }
        let shared: Arc<[u64]> = found.iter().copied().collect();
        self.found = Some((Rc::clone(found), Arc::clone(&shared)));
        Ok(shared)
    }

    fn references_of(&mut self, table: u64, def: &FieldDef) -> Rc<Vec<om_formula::Field>> {
        let formula = def.formula.as_deref();
        Rc::clone(self.references.entry((table, def.id)).or_insert_with(|| {
            Rc::new(
                formula
                    .map(|f| f.polish.field_references())
                    .unwrap_or_default(),
            )
        }))
    }

    /// A summary field's value on `record`: the summary over the cursor's
    /// found set (every record of the table without one), read once per
    /// found set. A summary that fails reads as empty, as a calculation.
    fn summarize(
        &mut self,
        occurrence: u64,
        table: u64,
        record: u64,
        def: &FieldDef,
        repetition: u32,
        cursor: &Cursor,
    ) -> Result<Value, CalcError> {
        let empty = Value::empty(def.value_type);
        let key = (occurrence, def.id, repetition);
        let cached = match (self.summaries.get(&key), &cursor.found) {
            (Some((found, run)), Some(current)) if Rc::ptr_eq(found, current) => {
                Some(Rc::clone(run))
            }
            _ => None,
        };
        let run = match cached {
            Some(run) => run,
            None => {
                let found = self.cursor_found(table, cursor)?;
                let source = CursorSource {
                    inner: self.file.source,
                    table,
                    found,
                };
                let file = FileCalc::new(self.file.schema, &source, self.file.today)
                    .with_session(&self.session);
                let run = match file.summary_run(occurrence, def, repetition) {
                    Ok(run) => Rc::new(run),
                    Err(_) => return Ok(empty),
                };
                if let Some(found) = &cursor.found {
                    self.summaries
                        .insert(key, (Rc::clone(found), Rc::clone(&run)));
                }
                run
            }
        };
        let Some(run) = run.as_ref() else {
            return Ok(empty);
        };
        let value = run
            .value(record, &self.file.table_cx(occurrence))
            .unwrap_or(empty.clone());
        Ok(if value.is_invalid() { empty } else { value })
    }

    /// An unstored calculation's value on `record` through `cursor`.
    fn calculate(
        &mut self,
        occurrence: u64,
        table: u64,
        record: u64,
        def: &FieldDef,
        repetition: u32,
        cursor: &Cursor,
    ) -> Result<Value, CalcError> {
        let empty = Value::empty(def.value_type);
        let Some(formula) = def.formula.clone() else {
            return Ok(empty);
        };
        let always = self.info(table, def.id).always;
        let references = self.references_of(table, def);
        let found = self.cursor_found(table, cursor)?;
        let source = CursorSource {
            inner: self.file.source,
            table,
            found,
        };
        let file = FileCalc::new(self.file.schema, &source, self.file.today);
        // The formula is its table's file's, whichever occurrence reads it.
        let owner = file.schema.table_file(table);
        let mut host = FileHost::with_vars(file, &self.session, self.vars.reborrow());
        host.sql_depth = self.depth + 1;
        let context = Context {
            occurrence: Some(formula.occurrence().unwrap_or(occurrence)),
            file: owner,
            record: Some(record),
            repetition,
            result_type: Some(def.value_type),
            ..Context::default()
        };
        // Containers do not count as empty references (a formula over an
        // empty photo still evaluates).
        let skip = !always
            && !references.is_empty()
            && references.iter().all(|r| {
                let occurrence = r.occurrence.id().or(context.occurrence).unwrap_or(0);
                let container = r
                    .field
                    .id()
                    .and_then(|f| host.field(occurrence, f))
                    .is_some_and(|i| i.value_type == ValueType::Container);
                !container
                    && evaluate(&Expr::Field(r.clone()), &mut host, &context)
                        .value
                        .is_empty()
            });
        if skip {
            return Ok(empty);
        }
        let result = evaluate_formula(&formula, &mut host, &context);
        if result.pending_execution {
            return Err(CalcError::from_code(1224).expect("nonzero"));
        }
        let value = result.value;
        Ok(if value.is_invalid() { empty } else { value })
    }
}

/// The text a find looks `value` up by in a field of type `ty`, when a find
/// for the whole value proposes every record that holds it.
fn lookup_text(ty: ValueType, value: &Value, cx: &Ctx) -> Option<String> {
    match (ty, value) {
        (ValueType::Text, Value::Text(text)) => {
            let text = text.as_str();
            let whole = !text.is_empty()
                && text.chars().count() <= MAX_LOOKUP_TEXT
                && text.trim() == text
                && !text.contains([
                    '\r', '\n', '\u{2028}', '\u{2029}', '*', '@', '#', '"', '~', '?',
                ]);
            whole.then(|| text.to_owned())
        }
        (ValueType::Date, Value::Date(date) | Value::DateText(date, _)) => {
            Some(date.format(cx.locale))
        }
        _ => None,
    }
}

fn field_type(ty: ValueType) -> FieldType {
    match ty {
        ValueType::Number => FieldType::Number,
        ValueType::Date => FieldType::Date,
        ValueType::Time => FieldType::Time,
        ValueType::Timestamp => FieldType::Timestamp,
        ValueType::Container => FieldType::Container,
        ValueType::Text => FieldType::Text,
    }
}

impl SqlSource for FileSql<'_> {
    fn locale(&self) -> &Locale {
        self.file_locale()
    }

    fn now(&self) -> Timestamp {
        self.now
    }

    fn user_name(&self) -> String {
        self.session.account_name.clone()
    }

    fn tables(&self) -> Vec<SqlTable> {
        let schema = self.schema();
        // The base table's name and file, each its own file's.
        let base = |table: Option<u64>| {
            let table = table?;
            let name = schema.local_table_name(table)?;
            let file = schema
                .table_context(table)
                .map_or(&schema.file_name, |f| &f.file_name);
            Some((name.to_owned(), file.clone()))
        };
        let catalog = &self.catalog().occurrences;
        if catalog.is_empty() {
            let mut tables: Vec<SqlTable> = schema
                .occurrences()
                .filter(|o| self.reads(schema.file_context(o.id)))
                .map(|o| SqlTable {
                    id: o.id,
                    name: self
                        .file_context
                        .map_or(&*o.name, |file| file.local_name(&o.name))
                        .to_owned(),
                    table_id: serial_value(self.shown_id(o.id)),
                    base: base(Some(o.table)),
                    mod_count: 0,
                })
                .collect();
            tables.sort_by_key(|t| t.name.to_lowercase());
            return tables;
        }
        catalog
            .iter()
            .map(|o| SqlTable {
                id: o.id,
                name: o.name.clone(),
                table_id: serial_value(self.shown_id(o.id)),
                base: base(o.table),
                mod_count: o.mod_count,
            })
            .collect()
    }

    fn fields(&self, table: &SqlTable) -> Vec<SqlField> {
        // A waiting occurrence's fields come once read; its need is listed.
        if self.file.waits(table.id) {
            return Vec::new();
        }
        let Some(t) = self.schema().table_of(table.id) else {
            return Vec::new();
        };
        t.fields.values().map(|f| self.sql_field(t.id, f)).collect()
    }

    fn records(&mut self, table: &SqlTable) -> Result<Vec<u64>, SqlError> {
        // A waiting occurrence's records are not read empty: 1224, its need
        // listed.
        if self.file.waits(table.id) {
            return Err(CalcError::from_code(1224).expect("nonzero").into());
        }
        let Some(t) = self.schema().table_of(table.id) else {
            return Ok(Vec::new());
        };
        Ok(self.file.source.records(t.id)?.to_vec())
    }

    fn value(
        &mut self,
        table: &SqlTable,
        record: u64,
        field: &SqlField,
        repetition: u32,
        cursor: Cursor,
    ) -> Result<Value, CalcError> {
        if self.file.waits(table.id) {
            return Err(CalcError::from_code(1224).expect("nonzero"));
        }
        let schema = self.schema();
        let base = schema
            .table_of(table.id)
            .ok_or(CalcError::TABLE_MISSING)?
            .id;
        let def = schema
            .field(table.id, field.id)
            .ok_or(CalcError::FIELD_MISSING)?;
        if def.class == FieldClass::Summary {
            return self.summarize(table.id, base, record, def, repetition, &cursor);
        }
        if !def.global {
            self.read_ahead(base, record, &cursor)?;
        }
        if def.is_unstored_calculation() {
            return self.calculate(table.id, base, record, def, repetition, &cursor);
        }
        self.file
            .field_value(table.id, record, field.id, repetition)
    }

    fn mod_count(&mut self, table: &SqlTable, record: u64) -> u64 {
        if self.file.waits(table.id) {
            return 0;
        }
        let Some(t) = self.schema().table_of(table.id) else {
            return 0;
        };
        self.file
            .source
            .record(t.id, record)
            .ok()
            .flatten()
            .and_then(|r| r.mod_count)
            .unwrap_or(0)
    }

    fn lookup(
        &mut self,
        table: &SqlTable,
        field: &SqlField,
        value: &Value,
    ) -> Result<Option<Vec<u64>>, SqlError> {
        // A waiting occurrence has no index to look up: its records are
        // read, and wait ([`Self::records`]).
        if self.file.waits(table.id) {
            return Ok(None);
        }
        let schema = self.schema();
        let (Some(t), Some(def)) = (schema.table_of(table.id), schema.field(table.id, field.id))
        else {
            return Ok(None);
        };
        if !self.info(t.id, def.id).findable
            || def.global
            || def.class == FieldClass::Summary
            || def.is_unstored_calculation()
        {
            return Ok(None);
        }
        // In the formats of the file holding the table (`CalcSource::lookup`).
        let Some(text) = lookup_text(def.value_type, value, &self.file.table_cx(table.id)) else {
            return Ok(None);
        };
        Ok(self
            .file
            .source
            .lookup(t.id, def.id, &text)?
            .map(|ids| ids.to_vec()))
    }

    fn base_tables(&self) -> Vec<BaseTableDef> {
        let schema = self.schema();
        let catalog = self.catalog();
        let mut tables: Vec<BaseTableDef> = schema
            .tables()
            .filter(|t| self.reads(schema.table_context(t.id)))
            .map(|t| BaseTableDef {
                name: schema.local_table_name(t.id).unwrap_or(&t.name).to_owned(),
                id: serial_value(self.shown_id(t.id)),
                source: "<Internal>".to_string(),
                mod_count: catalog.tables.get(&t.id).copied().unwrap_or(0),
                fields: t.fields.values().map(|f| self.sql_field(t.id, f)).collect(),
            })
            .collect();
        tables.sort_by_key(|b| b.name.to_lowercase());
        tables
    }

    fn value_lists(&self) -> Vec<ValueListDef> {
        self.catalog()
            .value_lists
            .iter()
            .map(|l| ValueListDef {
                name: l.name.clone(),
                id: self.shown_id(l.id),
                source: l.source.name().to_string(),
                mod_count: l.mod_count,
            })
            .collect()
    }

    /// A field's list: its non-empty values, once each, in the field's
    /// order (text in UTF-16 order, as the SQL engine's samples show;
    /// numbers, dates and times by value, file-metadata.md 6.4).
    fn value_list_values(&mut self, name: &str) -> Option<ValueListValues> {
        let schema = self.schema();
        let list = self
            .catalog()
            .value_lists
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case(name))?;
        // A list the account's custom set gives no access reads no values.
        if !schema.shows_value_list(list.id) {
            return Some(ValueListValues::Values(Vec::new()));
        }
        let (occurrence, field) = match &list.source {
            ValueListSource::Custom(values) => {
                return Some(ValueListValues::Values(values.clone()))
            }
            ValueListSource::Related | ValueListSource::External => {
                return Some(ValueListValues::Unsupported)
            }
            ValueListSource::Field { occurrence, field } => (*occurrence, *field),
        };
        let table = self.tables().into_iter().find(|t| t.id == occurrence)?;
        let def = self.fields(&table).into_iter().find(|f| f.id == field)?;
        let base = schema.table_of(occurrence)?.id;
        let records = Rc::new(self.records(&table).ok()?);
        let _ = self.file.source.prefetch(base, &records);
        let cx = Ctx::new(self.file_locale(), self.file.today);
        let mut values: Vec<(String, Val)> = Vec::new();
        for (i, &record) in records.iter().enumerate() {
            let cursor = Cursor {
                found_count: records.len(),
                record_number: i + 1,
                found: Some(Rc::clone(&records)),
            };
            let Ok(value) = self.value(&table, record, &def, 1, cursor) else {
                continue;
            };
            let text = value.to_text(&cx).into_string();
            if !text.is_empty() && !values.iter().any(|(t, _)| *t == text) {
                values.push((text, Val::from_calc(value)));
            }
        }
        if def.ty == FieldType::Text {
            values.sort_by(|a, b| text_cmp(&a.0, &b.0));
        } else {
            values.sort_by(|a, b| compare(&a.1, &b.1).unwrap_or_else(|| text_cmp(&a.0, &b.0)));
        }
        Some(ValueListValues::Values(
            values.into_iter().map(|(t, _)| t).collect(),
        ))
    }

    fn interrupted(&self) -> bool {
        self.file.source.interrupted()
    }
}

/// A source whose found set of one table is a query's cursor.
struct CursorSource<'s> {
    inner: &'s dyn CalcSource,
    table: u64,
    found: Arc<[u64]>,
}

impl CalcSource for CursorSource<'_> {
    fn library_member(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<crate::containers::MemberInfo>>, CalcError> {
        self.inner.library_member(table, member)
    }

    fn library_data(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<om_calc::eval::StoredContainer>>, CalcError> {
        self.inner.library_data(table, member)
    }

    fn record(
        &self,
        table: u64,
        record: u64,
    ) -> Result<Option<Arc<super::RecordValues>>, CalcError> {
        self.inner.record(table, record)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.inner.records(table)
    }

    fn globals(&self, table: u64) -> Result<Arc<super::RecordValues>, CalcError> {
        self.inner.globals(table)
    }

    fn found_set(&self, occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        if table == self.table {
            return Ok(Arc::clone(&self.found));
        }
        self.inner.found_set(occurrence, table)
    }

    fn prefetch_records(&self, table: u64, records: &[u64]) {
        self.inner.prefetch_records(table, records)
    }

    fn source_match_values(
        &self,
        schema: &CalcSchema,
        occurrence: u64,
        record: u64,
        field: u64,
        cx: &Ctx,
    ) -> Result<Vec<Value>, CalcError> {
        self.inner
            .source_match_values(schema, occurrence, record, field, cx)
    }

    fn related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        self.inner.related(schema, from, record, to, cx)
    }

    fn related_ids(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        self.inner.related_ids(schema, from, record, to, cx)
    }

    fn has_record(&self, table: u64, record: u64) -> bool {
        self.inner.has_record(table, record)
    }

    fn prefetch(&self, table: u64, ids: &[u64]) -> Result<(), CalcError> {
        self.inner.prefetch(table, ids)
    }

    fn lookup(&self, table: u64, field: u64, text: &str) -> Result<Option<Arc<[u64]>>, CalcError> {
        self.inner.lookup(table, field, text)
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
