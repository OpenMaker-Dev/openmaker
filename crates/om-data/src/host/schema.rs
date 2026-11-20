//! What calculations need to know about a file: its tables and fields with
//! their types, repetitions and storage, table occurrences, relationships,
//! custom functions and layouts. A provider builds it from the file's
//! catalogs (schema-path.md 5, calc-encoding.md 8).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use om_calc::eval::{Context, CustomFunction, FieldInfo, Storage};
use om_calc::{Locale, ValueType};
use om_formula::{Formula, NameTable};
use om_scripts::specs::SortSpec;

use crate::meta::{FileOptions, Relationship, ScriptEntry, ValueList};
pub use crate::meta::{JoinOp, JoinPredicate, SummaryDef, SummaryKind};

use super::sql::SqlCatalog;
use super::Privileges;

mod import;
pub use import::{FileSchemaContext, ImportedFileMap};

/// Field option bit: evaluate the calculation even when every field it
/// references is empty (schema-path.md 5.5).
const EVALUATE_ALWAYS: u32 = 0x20;

/// A field's kind (definition byte 0, schema-path.md 5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldClass {
    Data,
    Calculation,
    Summary,
}

/// A field of a base table.
#[derive(Clone, Debug)]
pub struct FieldDef {
    pub id: u64,
    pub name: String,
    pub class: FieldClass,
    /// The data type, or a calculation's or summary's result type.
    pub value_type: ValueType,
    pub repetitions: u32,
    /// Global storage: one value per file (option `0x10000`).
    pub global: bool,
    /// A calculation field's formula.
    pub formula: Option<Arc<Formula>>,
    /// Whether a calculation's result is stored (option `0x20000` clear and
    /// not global); data fields are stored.
    pub stored: bool,
    /// An auto-enter calculation (option `0x8`).
    pub auto_enter: Option<Arc<Formula>>,
    /// A validation calculation (`25`, validation option `0x1`).
    pub validation: Option<Arc<Formula>>,
    pub summary: Option<SummaryDef>,
    /// The option word of the definition (schema-path.md 5.5): indexing,
    /// auto-enter and storage bits.
    pub options: u32,
    /// The index and sort language (`mIndexLang`, a `Locale::Type`); 0
    /// for the file's.
    pub language: u32,
    /// The field's comment (`03`).
    pub comment: String,
    /// An auto-enter serial number's next value (`0c`, text; option `0x2`
    /// or `0x200`), as the file was read (`GetNextSerialValue`).
    pub next_serial: Option<String>,
}

impl FieldDef {
    /// A stored data field.
    pub fn data(id: u64, name: impl Into<String>, value_type: ValueType) -> FieldDef {
        FieldDef {
            id,
            name: name.into(),
            class: FieldClass::Data,
            value_type,
            repetitions: 1,
            global: false,
            formula: None,
            stored: true,
            auto_enter: None,
            validation: None,
            summary: None,
            options: 0,
            language: 0,
            comment: String::new(),
            next_serial: None,
        }
    }

    /// A calculation field, stored or not.
    pub fn calculation(
        id: u64,
        name: impl Into<String>,
        value_type: ValueType,
        formula: Formula,
        stored: bool,
    ) -> FieldDef {
        FieldDef {
            class: FieldClass::Calculation,
            formula: Some(Arc::new(formula)),
            stored,
            ..FieldDef::data(id, name, value_type)
        }
    }

    pub fn with_repetitions(mut self, repetitions: u32) -> FieldDef {
        self.repetitions = repetitions.max(1);
        self
    }

    pub fn with_global(mut self, global: bool) -> FieldDef {
        self.global = global;
        self
    }

    /// Indexed for finds and value lists (value or word index, option
    /// bits `0x80000000` and `0x20000000`).
    pub fn indexed(&self) -> bool {
        self.options & 0xa000_0000 != 0
    }

    /// Has a value index (option bit `0x80000000`), which whole-value
    /// finds read (query-program.md 6.3, 7.1).
    pub fn value_indexed(&self) -> bool {
        self.options & 0x8000_0000 != 0
    }

    /// Whether the calculation is evaluated when every field it references
    /// is empty: "Do not evaluate if all referenced fields are empty" is
    /// off (option `0x20`, auto-entry.md).
    pub fn evaluates_always(&self) -> bool {
        self.options & EVALUATE_ALWAYS != 0
    }

    /// Whether reading the field evaluates its formula: an unstored
    /// calculation (calc-values.md 8.8). A global calculation is computed
    /// when read too.
    pub fn is_unstored_calculation(&self) -> bool {
        self.class == FieldClass::Calculation && (!self.stored || self.global)
    }

    /// Whether the field auto-enters a serial number, generated on
    /// creation (`0x2`) or on commit (`0x200`; schema-path.md 5.5, 5.9).
    pub fn auto_enters_serial(&self) -> bool {
        self.class == FieldClass::Data && self.options & 0x202 != 0
    }

    /// Whether Duplicate Record copies the field's value into the copy: a
    /// stored data field's, unless the field auto-enters a value, which
    /// the copy gets anew. A lookup and a calculation that does not
    /// replace an existing value are copied (auto-entry.md, "Serial
    /// numbers"; script oracle s44-record-number-ae for a replacing
    /// calculation). The auto-entry bits (schema-path.md 5.5): a selected
    /// variable `0x1`, a serial `0x2` or `0x200`, a constant `0x4`, the
    /// last visited record's value `0x10`, a calculation `0x8` replacing
    /// with `0x80`.
    pub fn duplicate_copies(&self) -> bool {
        let replacing_calculation = self.options & 0x88 == 0x88;
        self.class == FieldClass::Data
            && self.stored
            && !self.global
            && self.options & (0x1 | 0x2 | 0x4 | 0x10 | 0x200) == 0
            && !replacing_calculation
    }

    /// The evaluator's view of the field.
    pub fn info(&self) -> FieldInfo {
        let storage = match (&self.class, &self.formula) {
            (FieldClass::Summary, _) => Storage::Summary,
            (FieldClass::Calculation, Some(formula)) if self.is_unstored_calculation() => {
                Storage::Unstored(Arc::clone(formula))
            }
            _ if self.global => Storage::Global,
            _ => Storage::Stored,
        };
        FieldInfo {
            value_type: self.value_type,
            repetitions: self.repetitions.max(1),
            storage,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TableDef {
    pub id: u64,
    pub name: String,
    /// The base table's comment (`[3, 16, 5, T, 03]`).
    pub comment: String,
    /// Fields by id.
    pub fields: BTreeMap<u64, FieldDef>,
}

#[derive(Clone, Debug)]
pub struct OccurrenceDef {
    pub id: u64,
    pub name: String,
    /// The base table it shows.
    pub table: u64,
}

/// An occurrence of another file's table (an external data source): no
/// fields or records here, but a name the Design functions list.
#[derive(Clone, Debug)]
pub struct ExternalOccurrence {
    pub id: u64,
    pub name: String,
    /// The data source's name (`[32, 5, D, 10]`).
    pub source: String,
}

/// A relationship between two occurrences (`[3, fb, 5, R]`,
/// file-metadata.md 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationshipDef {
    pub id: u64,
    pub left: u64,
    pub right: u64,
    /// `left op right`, all of which must hold.
    pub predicates: Vec<JoinPredicate>,
    /// The order of the left occurrence's records seen from the right
    /// (option bit `0x80`), and of the right's seen from the left (`0x40`);
    /// `None` when that side is unsorted (creation order).
    pub sort_left: Option<SortSpec>,
    pub sort_right: Option<SortSpec>,
    /// The option word (cascade delete and create, sorting).
    pub options: u32,
}

impl RelationshipDef {
    /// An unsorted relationship.
    pub fn new(id: u64, left: u64, right: u64, predicates: Vec<JoinPredicate>) -> RelationshipDef {
        RelationshipDef {
            id,
            left,
            right,
            predicates,
            sort_left: None,
            sort_right: None,
            options: 0,
        }
    }
}

impl From<&Relationship> for RelationshipDef {
    fn from(r: &Relationship) -> RelationshipDef {
        RelationshipDef {
            id: r.id,
            left: r.left,
            right: r.right,
            predicates: r.predicates.clone(),
            sort_left: r.order_of(r.left).cloned(),
            sort_right: r.order_of(r.right).cloned(),
            options: r.options,
        }
    }
}

/// One step of a relationship path: from the previous occurrence to `to`,
/// with the predicates oriented `from op to`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    pub from: u64,
    pub to: u64,
    pub predicates: Vec<JoinPredicate>,
    /// The order of `to`'s records, when the relationship sorts them.
    pub sort: Option<SortSpec>,
}

impl Hop {
    /// The relationship sorts `to`'s records.
    pub fn sorted(&self) -> bool {
        self.sort.is_some()
    }
}

/// What a layout catalog member is (its flags `[4, 5, L, f0]`,
/// file-metadata.md 1.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayoutKind {
    /// A layout, also a separator (a layout named `-`, flag `0x10`).
    #[default]
    Layout,
    /// A folder of the layout list (flag `0x01`).
    Folder,
    /// The end of a folder, named `--` (flag `0x02`).
    FolderEnd,
}

#[derive(Clone, Debug)]
pub struct LayoutEntry {
    pub id: u32,
    pub name: String,
    pub kind: LayoutKind,
    /// The layout's table occurrence.
    pub occurrence: Option<u64>,
    /// Its field objects in object order, one per object, the order
    /// `FieldNames` lists them in (`FieldStyle`, `FieldBounds`).
    pub fields: Vec<LayoutField>,
    /// The decoded layout with its objects' names, bounds and contents
    /// (`LayoutObjectNames`, `GetLayoutObjectAttribute`), when read.
    pub layout: Option<Arc<om_layout::Layout>>,
}

impl LayoutEntry {
    /// A layout showing `occurrence`, without objects.
    pub fn new(id: u32, name: impl Into<String>, occurrence: Option<u64>) -> LayoutEntry {
        LayoutEntry {
            id,
            name: name.into(),
            kind: LayoutKind::Layout,
            occurrence,
            fields: Vec::new(),
            layout: None,
        }
    }

    /// A layout of the list, not a folder or a folder's end: what
    /// `LayoutNames` lists.
    pub fn listed(&self) -> bool {
        self.kind == LayoutKind::Layout
    }
}

/// A field object of a layout (layout-model.md 5.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutField {
    /// The object's id within the layout.
    pub object: u64,
    pub occurrence: u64,
    pub field: u64,
    /// `displayType`: the control style.
    pub control: u32,
    /// The value list it shows (catalog id), `None` without one.
    pub value_list: Option<u64>,
    /// `numOfReps`: the repetitions shown.
    pub repetitions: u32,
    /// The repetitions run across (`numOfReps` stored negative).
    pub horizontal: bool,
    /// An edit box with a vertical scroll bar (field flag `0x100`).
    pub scrolling: bool,
    /// Left, top, right and bottom in layout coordinates, a portal's
    /// fields in its first row (`FieldBounds`).
    pub bounds: [f64; 4],
    /// Rotation in degrees.
    pub rotation: u32,
}

impl LayoutField {
    /// An edit box showing one repetition of `field`.
    pub fn new(occurrence: u64, field: u64) -> LayoutField {
        LayoutField {
            object: 0,
            occurrence,
            field,
            control: 0,
            value_list: None,
            repetitions: 1,
            horizontal: false,
            scrolling: false,
            bounds: [0.0; 4],
            rotation: 0,
        }
    }
}

/// A file's schema for calculations.
#[derive(Clone, Debug)]
pub struct CalcSchema {
    pub file_name: String,
    pub locale: Locale,
    /// This file's authenticated account metadata, retained when its schema
    /// is imported. `None` stays unavailable; script elevation is session-local.
    pub authenticated_privileges: Option<Privileges>,
    /// Account facts for calculations owned by this file.
    pub authenticated_account: super::AccountInfo,
    /// File-local font IDs from catalog `[25, 5]`, used by stored text.
    pub font_families: BTreeMap<u64, String>,
    tables: BTreeMap<u64, TableDef>,
    occurrences: BTreeMap<u64, OccurrenceDef>,
    external: BTreeMap<u64, ExternalOccurrence>,
    /// Occurrences of [`Self::external`] whose file has not been tried yet
    /// ([`Self::is_pending`]).
    pending: BTreeSet<u64>,
    /// Tables known from the schema catalog `[3]` whose field catalog
    /// `[T, 3]` has not been read yet ([`Self::set_fields_pending`]): their
    /// occurrences wait, bound or not.
    fields_pending: BTreeSet<u64>,
    /// Layouts listed from the layout catalog's index only, whose members
    /// (objects, parts, fields) are read when first needed
    /// ([`Self::is_layout_pending`]).
    pending_layouts: BTreeSet<u32>,
    relationships: Vec<RelationshipDef>,
    custom_functions: HashMap<u64, Arc<CustomFunction>>,
    layouts: Vec<LayoutEntry>,
    names: NameTable,
    /// What ExecuteSQL lists beyond the above.
    sql: SqlCatalog,
    /// The document preferences: default layout and file triggers.
    pub file_options: Option<FileOptions>,
    /// Value lists in display order.
    value_lists: Vec<ValueList>,
    /// The script catalog's members in display order.
    scripts: Vec<ScriptEntry>,
    /// The display order of base tables and of occurrences (raw ids);
    /// `None` for id order.
    table_order: Option<Vec<u64>>,
    occurrence_order: Option<Vec<u64>>,
    /// Related files imported under client-only ids (`import`), and the
    /// file owning each of their occurrences and base tables.
    imported: Vec<FileSchemaContext>,
    occurrence_files: HashMap<u64, usize>,
    table_files: HashMap<u64, usize>,
    /// The data source of each occurrence of another file's table that
    /// was bound to an imported table.
    data_sources: HashMap<u64, String>,
    /// The lowest client-only id handed out; `u64::MAX` before any import.
    runtime_floor: u64,
}

impl CalcSchema {
    pub fn new(file_name: impl Into<String>, locale: Locale) -> CalcSchema {
        CalcSchema {
            file_name: file_name.into(),
            locale,
            authenticated_privileges: None,
            authenticated_account: super::AccountInfo::default(),
            font_families: BTreeMap::new(),
            tables: BTreeMap::new(),
            occurrences: BTreeMap::new(),
            external: BTreeMap::new(),
            pending: BTreeSet::new(),
            fields_pending: BTreeSet::new(),
            pending_layouts: BTreeSet::new(),
            relationships: Vec::new(),
            custom_functions: HashMap::new(),
            layouts: Vec::new(),
            names: NameTable::default(),
            sql: SqlCatalog::default(),
            file_options: None,
            value_lists: Vec::new(),
            scripts: Vec::new(),
            table_order: None,
            occurrence_order: None,
            imported: Vec::new(),
            occurrence_files: HashMap::new(),
            table_files: HashMap::new(),
            data_sources: HashMap::new(),
            runtime_floor: u64::MAX,
        }
    }

    pub fn add_table(&mut self, id: u64, name: impl Into<String>) {
        let name = name.into();
        self.tables
            .entry(id)
            .and_modify(|t| t.name.clone_from(&name))
            .or_insert_with(|| TableDef {
                id,
                name,
                comment: String::new(),
                fields: BTreeMap::new(),
            });
    }

    /// A base table's comment (`BaseTableComment`).
    pub fn set_table_comment(&mut self, table: u64, comment: impl Into<String>) {
        if let Some(t) = self.tables.get_mut(&table) {
            t.comment = comment.into();
        }
    }

    /// Forget a table's fields, to add its current definitions again.
    pub fn clear_fields(&mut self, table: u64) {
        if let Some(t) = self.tables.get_mut(&table) {
            t.fields.clear();
        }
    }

    /// A field of base table `table` (added if missing).
    pub fn add_field(&mut self, table: u64, field: FieldDef) {
        self.names.add_field(table, field.id, &field.name);
        self.names
            .set_summary_field(table, field.id, field.class == FieldClass::Summary);
        let entry = self.tables.entry(table).or_insert_with(|| TableDef {
            id: table,
            name: String::new(),
            comment: String::new(),
            fields: BTreeMap::new(),
        });
        entry.fields.insert(field.id, field);
    }

    pub fn add_occurrence(&mut self, id: u64, name: impl Into<String>, table: u64) {
        let name = name.into();
        self.names.add_occurrence(id, &name, table);
        self.occurrences
            .insert(id, OccurrenceDef { id, name, table });
    }

    /// An occurrence of another file's table.
    pub fn add_external_occurrence(&mut self, occurrence: ExternalOccurrence) {
        self.external.insert(occurrence.id, occurrence);
    }

    /// The occurrences of other files' tables.
    pub fn external_occurrences(&self) -> impl Iterator<Item = &ExternalOccurrence> {
        self.external.values()
    }

    pub fn external_occurrence(&self, id: u64) -> Option<&ExternalOccurrence> {
        self.external.get(&id)
    }

    /// Mark unresolved occurrence `id` (one of [`Self::external_occurrences`])
    /// as waiting for its file, which has not been tried yet: what reads it
    /// waits ([`crate::host::CalcNeed::Occurrence`]) rather than showing
    /// the file missing. `false` when it is not unresolved.
    pub fn set_pending(&mut self, id: u64) -> bool {
        self.external.contains_key(&id) && self.pending.insert(id)
    }

    /// Whether occurrence `id` waits: it shows a table of a file not tried
    /// yet ([`Self::set_pending`], which binding it ends), or a table whose
    /// field catalog has not been read yet ([`Self::set_fields_pending`],
    /// which binding does not end). What reads it asks for it
    /// ([`crate::host::CalcNeed::Occurrence`]) rather than reading its
    /// fields missing.
    pub fn is_pending(&self, id: u64) -> bool {
        self.pending.contains(&id)
            || self
                .occurrences
                .get(&id)
                .is_some_and(|o| self.fields_pending.contains(&o.table))
    }

    /// Mark table `table` as known from the schema catalog `[3]` with its
    /// field catalog `[T, 3]` not read yet: it lists no fields, and its
    /// occurrences wait ([`Self::is_pending`]) until a schema with its fields
    /// replaces this one. An import keeps the mark on the table's client id.
    pub fn set_fields_pending(&mut self, table: u64) {
        self.fields_pending.insert(table);
    }

    /// Whether table `table`'s field catalog has not been read yet
    /// ([`Self::set_fields_pending`]).
    pub fn are_fields_pending(&self, table: u64) -> bool {
        self.fields_pending.contains(&table)
    }

    /// Whether any occurrence waits ([`Self::is_pending`]): one of a file
    /// not tried yet, which [`Self::pending_occurrences`] lists, or one,
    /// bound or not, whose table's field catalog is not read yet, which it
    /// does not. A reader that grows the schema for what waits checks this
    /// before looking further.
    pub fn waits(&self) -> bool {
        !self.pending.is_empty()
            || (!self.fields_pending.is_empty()
                && self
                    .occurrences
                    .values()
                    .any(|o| self.fields_pending.contains(&o.table)))
    }

    /// The occurrences waiting for their files, in id order.
    pub fn pending_occurrences(&self) -> impl Iterator<Item = u64> + '_ {
        self.pending.iter().copied()
    }

    /// Mark layout `id` as listed from the layout catalog's index only: its
    /// name, kind and table occurrence are known, its objects, parts and
    /// fields come with its member, which what reads them waits for
    /// ([`crate::host::CalcNeed::Layout`]) rather than finding none.
    pub fn set_layout_pending(&mut self, id: u32) {
        self.pending_layouts.insert(id);
    }

    /// Layout `id` waits no longer although its member was not read: the
    /// host has none, so it reads as a layout with no objects rather than
    /// being asked for again.
    pub fn settle_layout(&mut self, id: u32) {
        self.pending_layouts.remove(&id);
    }

    /// Whether layout `id`'s member has not been read yet
    /// ([`Self::set_layout_pending`]).
    pub fn is_layout_pending(&self, id: u32) -> bool {
        self.pending_layouts.contains(&id)
    }

    /// The layouts waiting for their members, in id order.
    pub fn pending_layouts(&self) -> impl Iterator<Item = u32> + '_ {
        self.pending_layouts.iter().copied()
    }

    pub fn add_relationship(&mut self, relationship: RelationshipDef) {
        self.relationships.push(relationship);
    }

    pub fn add_custom_function(&mut self, id: u64, function: CustomFunction) {
        self.names
            .add_custom_function(id, &function.name, function.parameters.len() as u16);
        self.custom_functions.insert(id, Arc::new(function));
    }

    pub fn add_layout(&mut self, id: u32, name: impl Into<String>, occurrence: Option<u64>) {
        self.add_layout_entry(LayoutEntry::new(id, name, occurrence));
    }

    /// Add a layout, in display order.
    pub fn add_layout_entry(&mut self, layout: LayoutEntry) {
        self.layouts.push(layout);
    }

    /// Add a value list, in display order.
    pub fn add_value_list(&mut self, list: ValueList) {
        self.value_lists.push(list);
    }

    /// The value lists in display order.
    pub fn value_lists(&self) -> &[ValueList] {
        &self.value_lists
    }

    /// The value list named `name` (case-insensitively, as FileMaker
    /// matches names).
    pub fn value_list_named(&self, name: &str) -> Option<&ValueList> {
        self.value_lists
            .iter()
            .find(|l| l.name == name)
            .or_else(|| {
                self.value_lists
                    .iter()
                    .find(|l| l.name.to_lowercase() == name.to_lowercase())
            })
    }

    /// The script catalog's members (scripts, folders, separators) in
    /// display order.
    pub fn set_scripts(&mut self, scripts: Vec<ScriptEntry>) {
        self.scripts = scripts;
    }

    pub fn scripts(&self) -> &[ScriptEntry] {
        &self.scripts
    }

    /// The display order of base tables (`[3, 16, 1, 03]`) and of
    /// occurrences (`[3, 17, 1, 03]`).
    pub fn set_display_order(&mut self, tables: Option<Vec<u64>>, occurrences: Option<Vec<u64>>) {
        self.table_order = tables;
        self.occurrence_order = occurrences;
    }

    /// Base tables in display order.
    pub fn tables_in_order(&self) -> Vec<&TableDef> {
        ordered(&self.tables, self.table_order.as_deref())
    }

    /// This file's calculation schema with its base tables, fields and
    /// occurrences replaced by `schema`'s, in `schema`'s order: what
    /// Specify Calculation names while Manage Database edits the schema.
    /// Functions, the locale and everything else are kept.
    pub fn with_database_schema(&self, schema: &crate::DatabaseSchema) -> CalcSchema {
        let mut out = self.clone();
        out.tables.clear();
        out.occurrences.clear();
        out.names = self.names.functions_only();
        out.table_order = Some(schema.tables.iter().map(|t| t.id).collect());
        out.occurrence_order = Some(schema.occurrences.iter().map(|o| o.id).collect());
        for table in &schema.tables {
            out.add_table(table.id, &table.name);
            out.set_table_comment(table.id, &table.comment);
            for field in &table.fields {
                let mut def = FieldDef::data(
                    field.id,
                    &field.name,
                    crate::mock_calc::value_type(field.data_type),
                );
                def.class = match field.kind {
                    crate::SchemaFieldKind::Calculation => FieldClass::Calculation,
                    crate::SchemaFieldKind::Summary => FieldClass::Summary,
                    _ => FieldClass::Data,
                };
                def.stored = !field.unstored;
                def.comment.clone_from(&field.comment);
                out.add_field(table.id, def);
            }
        }
        for occurrence in &schema.occurrences {
            if let Some(table) = occurrence.table_id {
                out.add_occurrence(occurrence.id, &occurrence.name, table);
            }
        }
        out
    }

    /// Occurrences in display order.
    pub fn occurrences_in_order(&self) -> Vec<&OccurrenceDef> {
        ordered(&self.occurrences, self.occurrence_order.as_deref())
    }

    /// The occurrence named `name` (case-insensitively).
    pub fn occurrence_named(&self, name: &str) -> Option<&OccurrenceDef> {
        self.occurrences
            .values()
            .find(|o| o.name == name)
            .or_else(|| {
                self.occurrences
                    .values()
                    .find(|o| o.name.to_lowercase() == name.to_lowercase())
            })
    }

    /// The field of `table` a found-set sort key names (`Occurrence::Field`
    /// or a bare field name), as [`super::CalcSource::found_sort`] lists
    /// it: [`super::UNRESOLVED_SORT_FIELD`] for a field of another table's
    /// occurrence or no such field. A qualifier this graph does not name
    /// (another file's occurrence of the table) reads as the table's own.
    pub fn sort_key_field(&self, table: u64, key: &str) -> u64 {
        let (qualifier, name) = match key.rsplit_once("::") {
            Some((qualifier, name)) => (Some(qualifier), name),
            None => (None, key),
        };
        qualifier
            .and_then(|q| self.occurrence_named(q))
            .map_or(true, |occurrence| occurrence.table == table)
            .then(|| self.tables.get(&table))
            .flatten()
            .and_then(|def| def.fields.values().find(|f| f.name == name))
            .map_or(super::UNRESOLVED_SORT_FIELD, |f| f.id)
    }

    pub fn names(&self) -> &NameTable {
        &self.names
    }

    pub fn tables(&self) -> impl Iterator<Item = &TableDef> {
        self.tables.values()
    }

    pub fn table(&self, id: u64) -> Option<&TableDef> {
        self.tables.get(&id)
    }

    pub fn occurrences(&self) -> impl Iterator<Item = &OccurrenceDef> {
        self.occurrences.values()
    }

    pub fn occurrence(&self, id: u64) -> Option<&OccurrenceDef> {
        self.occurrences.get(&id)
    }

    /// The base table an occurrence shows.
    pub fn table_of(&self, occurrence: u64) -> Option<&TableDef> {
        self.tables.get(&self.occurrences.get(&occurrence)?.table)
    }

    /// `field` of the table `occurrence` shows.
    pub fn field(&self, occurrence: u64, field: u64) -> Option<&FieldDef> {
        self.table_of(occurrence)?.fields.get(&field)
    }

    pub fn relationships(&self) -> &[RelationshipDef] {
        &self.relationships
    }

    pub fn custom_function(&self, id: u64) -> Option<Arc<CustomFunction>> {
        self.custom_functions.get(&id).cloned()
    }

    pub fn custom_functions(&self) -> impl Iterator<Item = &CustomFunction> {
        self.custom_functions.values().map(Arc::as_ref)
    }

    /// Whether value list `id`'s items may be read: not when the custom set
    /// of the account the owning file was opened with gives the list no
    /// access (session-identity.md 2.2). A related file's list is judged by
    /// that file's account under its own id. `ValueListItems`, layout
    /// controls, sorts by the list and ExecuteSQL then read no items; its
    /// name and id stay.
    pub fn shows_value_list(&self, id: u64) -> bool {
        let (account, id) = match self.value_list_context(id) {
            Some(file) => (&file.account_info, file.file_id(id)),
            None => (&self.authenticated_account, id),
        };
        account
            .set_access
            .as_ref()
            .is_none_or(|access| access.shows_value_list(id))
    }

    /// Drop the fields and objects of the layouts the authenticated
    /// account's custom set gives no access (session-identity.md 2.2), so
    /// that `FieldNames`, `FieldBounds`, `LayoutObjectNames`,
    /// `GetLayoutObjectAttribute` and the like read nothing in them. Their
    /// ids, names and occurrences stay, as the file's layout list.
    pub fn withhold_hidden_layouts(&mut self) {
        let Some(access) = self.authenticated_account.set_access.as_ref() else {
            return;
        };
        for layout in &mut self.layouts {
            if layout.kind == LayoutKind::Layout && !access.shows_layout(u64::from(layout.id)) {
                layout.fields.clear();
                layout.layout = None;
            }
        }
    }

    /// Every layout catalog member in display order, folders included.
    pub fn layouts(&self) -> &[LayoutEntry] {
        &self.layouts
    }

    /// The layouts of the list in display order: no folders or folder
    /// ends (`LayoutNames`, `Get ( LayoutCount )`).
    pub fn listed_layouts(&self) -> impl Iterator<Item = &LayoutEntry> {
        self.layouts.iter().filter(|l| l.listed())
    }

    /// The listed layout named `name` (case-insensitively).
    pub fn layout_named(&self, name: &str) -> Option<&LayoutEntry> {
        let lower = name.to_lowercase();
        self.listed_layouts().find(|l| l.name == name).or_else(|| {
            self.listed_layouts()
                .find(|l| l.name.to_lowercase() == lower)
        })
    }

    /// The catalog ExecuteSQL reads (system tables, value lists, options
    /// of calculations and indexes).
    pub fn sql(&self) -> &SqlCatalog {
        &self.sql
    }

    pub fn set_sql(&mut self, sql: SqlCatalog) {
        self.sql = sql;
    }

    /// The layout's parent record, except when its path to `related`
    /// passes through a displayed portal row. Pin that intermediate hop
    /// to the row. A reference to the portal occurrence itself retains the
    /// full parent-related set for aggregate/GetNthRecord semantics.
    pub fn relationship_origin(&self, context: &Context, related: u64) -> Option<(u64, u64)> {
        let parent = context.occurrence.zip(context.record)?;
        if let Some(portal) = context.portal {
            if portal.0 != related
                && self
                    .path(parent.0, related)
                    .is_some_and(|path| path.iter().any(|hop| hop.to == portal.0))
            {
                return Some(portal);
            }
        }
        Some(parent)
    }

    /// The relationship path from `from` to `to`, shortest first. `None`
    /// when the occurrences are not related; an empty path from an
    /// occurrence to itself.
    pub fn path(&self, from: u64, to: u64) -> Option<Vec<Hop>> {
        let mut previous: HashMap<u64, Hop> = HashMap::new();
        let mut queue = VecDeque::from([from]);
        while let Some(at) = queue.pop_front() {
            if at == to {
                let mut path = Vec::new();
                let mut node = to;
                while node != from {
                    let hop = previous.remove(&node)?;
                    node = hop.from;
                    path.push(hop);
                }
                path.reverse();
                return Some(path);
            }
            for r in &self.relationships {
                let hop = if r.left == at {
                    Hop {
                        from: at,
                        to: r.right,
                        predicates: r.predicates.clone(),
                        sort: r.sort_right.clone(),
                    }
                } else if r.right == at {
                    Hop {
                        from: at,
                        to: r.left,
                        predicates: r
                            .predicates
                            .iter()
                            .map(|p| JoinPredicate {
                                op: p.op.reversed(),
                                left: p.right,
                                right: p.left,
                            })
                            .collect(),
                        sort: r.sort_left.clone(),
                    }
                } else {
                    continue;
                };
                if hop.to != from && !previous.contains_key(&hop.to) {
                    queue.push_back(hop.to);
                    previous.insert(hop.to, hop);
                }
            }
        }
        None
    }
}

/// `map`'s values in `order` (raw ids), the rest after them in id order.
fn ordered<'a, T>(map: &'a BTreeMap<u64, T>, order: Option<&[u64]>) -> Vec<&'a T> {
    let mut items: Vec<(u64, &T)> = map.iter().map(|(id, item)| (*id, item)).collect();
    if let Some(order) = order {
        items.sort_by_key(|(id, _)| {
            (
                order.iter().position(|o| o == id).unwrap_or(usize::MAX),
                *id,
            )
        });
    }
    items.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_keys_of_another_table_stay_unresolved() {
        let mut schema = CalcSchema::new("f", Locale::en_us());
        schema.add_table(10, "Rows");
        schema.add_table(20, "Other");
        schema.add_occurrence(1, "Rows", 10);
        schema.add_occurrence(2, "Rows Again", 10);
        schema.add_occurrence(3, "Other", 20);
        schema.add_field(10, FieldDef::data(4, "Region", ValueType::Text));
        schema.add_field(20, FieldDef::data(4, "Region", ValueType::Text));
        let key = |name| schema.sort_key_field(10, name);
        assert_eq!(key("Region"), 4);
        assert_eq!(key("Rows Again::Region"), 4);
        // Another file's name for the table is still the table's own field.
        assert_eq!(key("Remote Rows::Region"), 4);
        // Never the same-named field of the sorted table.
        assert_eq!(key("Other::Region"), super::super::UNRESOLVED_SORT_FIELD);
        assert_eq!(key("Missing"), super::super::UNRESOLVED_SORT_FIELD);
    }

    #[test]
    fn paths_follow_relationships_both_ways() {
        let mut schema = CalcSchema::new("f", Locale::en_us());
        for (id, table) in [(1, 10), (2, 20), (3, 30)] {
            schema.add_occurrence(id, format!("O{id}"), table);
        }
        let sorted = SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: Vec::new(),
        };
        schema.add_relationship(RelationshipDef {
            sort_right: Some(sorted),
            ..RelationshipDef::new(
                1,
                1,
                2,
                vec![JoinPredicate {
                    op: JoinOp::Less,
                    left: 5,
                    right: 6,
                }],
            )
        });
        schema.add_relationship(RelationshipDef::new(
            2,
            3,
            2,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 7,
                right: 8,
            }],
        ));
        let path = schema.path(1, 3).unwrap();
        assert_eq!(path.len(), 2);
        assert_eq!((path[0].from, path[0].to, path[0].sorted()), (1, 2, true));
        assert!(!path[1].sorted());
        assert_eq!(
            path[1].predicates,
            [JoinPredicate {
                op: JoinOp::Equal,
                left: 8,
                right: 7
            }]
        );
        let back = schema.path(2, 1).unwrap();
        assert_eq!(back[0].predicates[0].op, JoinOp::Greater);
        assert!(schema.path(1, 4).is_none());
        assert_eq!(schema.path(1, 1), Some(vec![]));
    }
}
