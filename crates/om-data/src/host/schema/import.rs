//! Related hosted files in one calculation schema (calc-values.md 13,
//! "Related files' formulas"). A client evaluating another hosted file's
//! formulas imports that file's base tables, fields, occurrences, custom
//! functions, relationships and value lists into the root file's
//! [`CalcSchema`] under client-only ids. They are handed out downward from
//! `u64::MAX - 1`, past every id the schema uses and every id an earlier
//! import handed out, so an imported member never shares an id with a root
//! member or another file's even where the files' own ids coincide. The
//! files' own ids are never changed: [`ImportedFileMap`] maps each back, the
//! tables for record reads on the owning file's connection. Field ids are
//! kept, as they are per base table.
//!
//! The root file lists imported members as `File::Name` so that its own
//! name lookups never find them; their file's formulas see them under their
//! own names through [`FileSchemaContext`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use om_calc::eval::Context;
use om_calc::{Locale, Value};
use om_formula::{Expr, Formula, Key, NameTable};
use om_scripts::specs::SortSpec;

use super::{CalcSchema, ExternalOccurrence, FieldClass, FieldDef, Privileges};
use crate::host::sql::{SqlCatalog, ValueListSource};
use crate::meta::ValueSource;

/// What a formula of an imported file resolves in that file: its names
/// (by client-only ids), formats, fonts and ExecuteSQL catalog.
#[derive(Clone, Debug)]
pub struct FileSchemaContext {
    /// The file's name without its extension (`Get ( FileName )`).
    pub file_name: String,
    /// The resolved owning path when known. Hosted imports without an
    /// explicit path retain the relative same-host derivation.
    pub file_path: Option<String>,
    /// Which file it is, when the importer knows: its host, port and name,
    /// or its path on this computer
    /// ([`crate::file_refs::FileIdentity`]).
    pub identity: Option<crate::file_refs::FileIdentity>,
    /// Its formats (file-metadata.md 2).
    pub locale: Locale,
    /// The owning file's authenticated account, independent of the root
    /// session's account or script elevation. `None` means unavailable.
    pub authenticated_privileges: Option<Privileges>,
    pub account_info: crate::host::AccountInfo,
    /// Its font ids (`[25, 5]`), for its stored character styles.
    pub font_families: BTreeMap<u64, String>,
    /// Its occurrences, fields and custom functions under their own names.
    pub names: NameTable,
    /// What its ExecuteSQL lists.
    pub sql: SqlCatalog,
    /// The file's own id of each of its members, by client-only id.
    ids: HashMap<u64, u64>,
}

impl FileSchemaContext {
    /// Privilege selectors for a formula owned by this file, independent of
    /// root script elevation. Missing metadata answers empty text; `None`
    /// means this is not a privilege selector.
    pub fn privilege_selector(&self, selector: u8) -> Option<Value> {
        use crate::host::session::selector::*;
        let privileges = self.authenticated_privileges.as_ref();
        Some(match selector {
            ACCOUNT_PRIVILEGE_SET_NAME | CURRENT_PRIVILEGE_SET_NAME => {
                Value::text(privileges.map_or("", |p| &p.account_set_name))
            }
            ACCOUNT_EXTENDED_PRIVILEGES | CURRENT_EXTENDED_PRIVILEGES => {
                Value::text(privileges.map_or_else(String::new, |p| p.extended.join("\r")))
            }
            LAYOUT_ACCESS | RECORD_ACCESS => {
                let access = privileges.and_then(|p| {
                    if selector == LAYOUT_ACCESS {
                        p.layout_access
                    } else {
                        p.record_access
                    }
                });
                access.map_or_else(Value::empty_text, Value::number)
            }
            _ => return None,
        })
    }

    /// Whether [`Self::privilege_selector`] answers `selector` empty because
    /// this file's security metadata could not be read, rather than with the
    /// account's own value.
    pub fn unknown_privilege(&self, selector: u8) -> bool {
        use crate::host::session::selector::*;
        let privileges = self.authenticated_privileges.as_ref();
        match selector {
            ACCOUNT_PRIVILEGE_SET_NAME
            | CURRENT_PRIVILEGE_SET_NAME
            | ACCOUNT_EXTENDED_PRIVILEGES
            | CURRENT_EXTENDED_PRIVILEGES => privileges.is_none(),
            LAYOUT_ACCESS => privileges.and_then(|p| p.layout_access).is_none(),
            RECORD_ACCESS => privileges.and_then(|p| p.record_access).is_none(),
            _ => false,
        }
    }

    /// `name` without the `File::` prefix the root file lists this file's
    /// members under.
    pub fn local_name<'n>(&self, name: &'n str) -> &'n str {
        name.strip_prefix(self.file_name.as_str())
            .and_then(|rest| rest.strip_prefix("::"))
            .unwrap_or(name)
    }

    /// The id this file stores for its member with client-only id `id`
    /// (what its ExecuteSQL system tables show); `id` itself when it is not
    /// this file's.
    pub fn file_id(&self, id: u64) -> u64 {
        self.ids.get(&id).copied().unwrap_or(id)
    }
}

/// An imported file's ids, as stored in that file, and the client-only ids
/// its members got. Members a reference names but the file does not define
/// (or the account cannot read) get ids too, so they stay missing rather
/// than naming a root member.
#[derive(Clone, Debug, Default)]
pub struct ImportedFileMap {
    /// Base tables; a provider reads a client-only table's records as the
    /// owning file's table.
    pub tables: BTreeMap<u64, u64>,
    pub occurrences: BTreeMap<u64, u64>,
    pub functions: BTreeMap<u64, u64>,
    pub value_lists: BTreeMap<u64, u64>,
    /// Relationship IDs also survive a rebuild when new formula references appear.
    pub relationships: BTreeMap<u64, u64>,
}

/// Client-only ids, handed out downward past every id in use.
struct Ids {
    used: BTreeSet<u64>,
    next: u64,
}

impl Ids {
    fn take(&mut self) -> u64 {
        while !self.used.insert(self.next) {
            self.next -= 1;
        }
        self.next
    }
}

/// One import's id assignment.
struct Importer {
    ids: Ids,
    map: ImportedFileMap,
}

impl Importer {
    fn table(&mut self, id: u64) -> u64 {
        *self.map.tables.entry(id).or_insert_with(|| self.ids.take())
    }

    fn occurrence(&mut self, id: u64) -> u64 {
        *self
            .map
            .occurrences
            .entry(id)
            .or_insert_with(|| self.ids.take())
    }

    fn function(&mut self, id: u64) -> u64 {
        *self
            .map
            .functions
            .entry(id)
            .or_insert_with(|| self.ids.take())
    }

    fn value_list(&mut self, id: u64) -> u64 {
        *self
            .map
            .value_lists
            .entry(id)
            .or_insert_with(|| self.ids.take())
    }

    /// `formula` with client-only ids. Its formatted stream, which names
    /// the old ids, is dropped: only the Polish stream is evaluated
    /// (calc-encoding.md 0).
    fn formula(&mut self, formula: &Formula) -> Formula {
        let mut polish = formula.polish.clone();
        self.expr(&mut polish);
        Formula {
            // A zero key means no table occurrence, not a member.
            context: formula
                .occurrence()
                .map(|o| Key::from_id(self.occurrence(o))),
            formatted: None,
            polish,
        }
    }

    fn expr(&mut self, expr: &mut Expr) {
        match expr {
            // Occurrence 0 is the context's own (layout calculations).
            Expr::Field(f) | Expr::FieldRef(f) => {
                if let Some(id) = f.occurrence.id().filter(|&id| id != 0) {
                    f.occurrence = Key::from_id(self.occurrence(id));
                }
            }
            Expr::CustomCall { function, .. } => {
                if let Some(id) = function.id() {
                    *function = Key::from_id(self.function(id));
                }
            }
            _ => {}
        }
        for_each_child(expr, |child| self.expr(child));
    }

    fn field(&mut self, field: &FieldDef) -> FieldDef {
        let mut def = field.clone();
        for formula in [&mut def.formula, &mut def.auto_enter, &mut def.validation]
            .into_iter()
            .flatten()
        {
            *formula = self.formula(formula).into();
        }
        if let Some(summary) = &mut def.summary {
            for spec in summary.field.iter_mut().chain(&mut summary.additional) {
                spec.occurrence = spec.occurrence.map(|o| self.occurrence(o));
            }
        }
        def
    }

    fn sort(&mut self, sort: &mut SortSpec) {
        for entry in &mut sort.fields {
            for field in std::iter::once(&mut entry.field).chain(&mut entry.summary) {
                field.occurrence = field.occurrence.map(|o| self.occurrence(o));
            }
            entry.value_list = entry.value_list.map(|v| self.value_list(v));
        }
    }

    fn value_source(&mut self, source: &mut ValueSource) {
        match source {
            ValueSource::Field {
                primary,
                secondary,
                start,
            } => {
                for field in std::iter::once(primary).chain(secondary) {
                    field.occurrence = field.occurrence.map(|o| self.occurrence(o));
                }
                *start = start.map(|o| self.occurrence(o));
            }
            ValueSource::Calculation(stored) => {
                // A formula that does not decode cannot be given new ids;
                // it is dropped rather than left naming the old ones.
                *stored = stored.take().and_then(|mut stored| {
                    let formula =
                        Formula::decode(stored.context.as_deref(), None, &stored.polish).ok()?;
                    let formula = self.formula(&formula);
                    stored.context = formula.encode_context();
                    stored.polish = om_formula::polish::encode(&formula.polish);
                    Some(stored)
                });
            }
            ValueSource::Custom(_) | ValueSource::External { .. } => {}
        }
    }

    /// ExecuteSQL's catalog with client-only ids.
    fn sql(&mut self, source: &SqlCatalog) -> SqlCatalog {
        let mut sql = source.clone();
        for occurrence in &mut sql.occurrences {
            // An occurrence whose definition did not decode is listed too.
            occurrence.id = self.occurrence(occurrence.id);
            occurrence.table = occurrence.table.map(|t| self.table(t));
        }
        // Only the file's own tables have these entries (all assigned).
        let tables = &self.map.tables;
        sql.tables = source
            .tables
            .iter()
            .filter_map(|(t, count)| Some((*tables.get(t)?, *count)))
            .collect();
        sql.fields = source
            .fields
            .iter()
            .filter_map(|((t, f), info)| Some(((*tables.get(t)?, *f), *info)))
            .collect();
        for list in &mut sql.value_lists {
            list.id = self.value_list(list.id);
            if let ValueListSource::Field { occurrence, .. } = &mut list.source {
                *occurrence = self.occurrence(*occurrence);
            }
        }
        sql
    }
}

impl CalcSchema {
    /// Import `source`, another hosted file's schema, under client-only ids
    /// (module docs) and return how its ids map. This file's members,
    /// layouts and scripts are unchanged. Occurrences of other files stay
    /// unresolved until [`Self::bind_external_occurrence`].
    pub fn import_file_schema(&mut self, source: &CalcSchema) -> ImportedFileMap {
        self.import_file_schema_with_path(source, None)
    }

    /// Import a file with its resolved owning path, for example a local
    /// file reached through a symlink. Its formulas keep that path rather
    /// than deriving one from the importing file's folder.
    pub fn import_file_schema_with_path(
        &mut self,
        source: &CalcSchema,
        file_path: Option<String>,
    ) -> ImportedFileMap {
        self.import_owned_file_schema(source, file_path, None)
    }

    /// [`Self::import_file_schema_with_path`], naming which file it is
    /// ([`FileSchemaContext::identity`]).
    pub fn import_owned_file_schema(
        &mut self,
        source: &CalcSchema,
        file_path: Option<String>,
        identity: Option<crate::file_refs::FileIdentity>,
    ) -> ImportedFileMap {
        self.import_owned_file_schema_with_map(
            source,
            file_path,
            identity,
            ImportedFileMap::default(),
        )
    }

    /// Reserve all IDs from earlier imports before rebuilding a schema.
    /// This includes unresolved formula references, which have no definition.
    pub fn reserve_imported_ids(&mut self, map: &ImportedFileMap) {
        for id in map
            .tables
            .values()
            .chain(map.occurrences.values())
            .chain(map.functions.values())
            .chain(map.value_lists.values())
            .chain(map.relationships.values())
        {
            self.runtime_floor = self.runtime_floor.min(*id);
        }
    }

    /// Import a grown file using its previously published mapping. Reserve
    /// every file's earlier mapping first with [`Self::reserve_imported_ids`]
    /// so newly discovered references cannot take another file's IDs.
    pub fn import_owned_file_schema_with_map(
        &mut self,
        source: &CalcSchema,
        file_path: Option<String>,
        identity: Option<crate::file_refs::FileIdentity>,
        previous: ImportedFileMap,
    ) -> ImportedFileMap {
        self.reserve_imported_ids(&previous);
        let mut used: BTreeSet<u64> = self.tables.keys().copied().collect();
        used.extend(self.occurrences.keys());
        used.extend(self.external.keys());
        used.extend(self.custom_functions.keys());
        used.extend(self.relationships.iter().map(|r| r.id));
        used.extend(self.value_lists.iter().map(|l| l.id));
        used.extend(self.sql.occurrences.iter().map(|o| o.id));
        used.extend(self.sql.value_lists.iter().map(|l| l.id));
        let mut import = Importer {
            ids: Ids {
                used,
                // Below every earlier import's ids, assigned or reserved;
                // `u64::MAX` stays free as an inclusive range bound.
                next: self.runtime_floor - 1,
            },
            map: previous,
        };
        // Members first, in id order: the same files import to the same ids.
        let mut functions: Vec<u64> = source.custom_functions.keys().copied().collect();
        functions.sort_unstable();
        for id in source.tables.keys() {
            import.table(*id);
        }
        for id in source.occurrences.keys().chain(source.external.keys()) {
            import.occurrence(*id);
        }
        for id in &functions {
            import.function(*id);
        }
        for list in &source.value_lists {
            import.value_list(list.id);
        }

        let file = self.imported.len();
        let prefixed = |name: &str| format!("{}::{name}", source.file_name);
        let mut names = NameTable::default();
        for table in source.tables.values() {
            let id = import.table(table.id);
            self.add_table(id, prefixed(&table.name));
            self.set_table_comment(id, &table.comment);
            // Its fields not read yet: its occurrences here wait too.
            if source.are_fields_pending(table.id) {
                self.set_fields_pending(id);
            }
            for field in table.fields.values() {
                let def = import.field(field);
                names.add_field(id, def.id, &def.name);
                names.set_summary_field(id, def.id, def.class == FieldClass::Summary);
                self.add_field(id, def);
            }
        }
        for occurrence in source.occurrences.values() {
            let id = import.occurrence(occurrence.id);
            // A base table the account cannot read gets an id without a
            // definition: the occurrence stays unreadable.
            let table = import.table(occurrence.table);
            names.add_occurrence(id, &occurrence.name, table);
            self.add_occurrence(id, prefixed(&occurrence.name), table);
        }
        for occurrence in source.external.values() {
            self.add_external_occurrence(ExternalOccurrence {
                id: import.occurrence(occurrence.id),
                name: prefixed(&occurrence.name),
                source: occurrence.source.clone(),
            });
        }
        for id in functions {
            let mut function = source.custom_functions[&id].as_ref().clone();
            import.expr(&mut function.body);
            let id = import.function(id);
            names.add_custom_function(id, &function.name, function.parameters.len() as u16);
            function.name = prefixed(&function.name);
            self.add_custom_function(id, function);
        }
        for relationship in &source.relationships {
            let mut relationship = relationship.clone();
            relationship.id = *import
                .map
                .relationships
                .entry(relationship.id)
                .or_insert_with(|| import.ids.take());
            relationship.left = import.occurrence(relationship.left);
            relationship.right = import.occurrence(relationship.right);
            for sort in [&mut relationship.sort_left, &mut relationship.sort_right]
                .into_iter()
                .flatten()
            {
                import.sort(sort);
            }
            self.add_relationship(relationship);
        }
        for list in &source.value_lists {
            let mut list = list.clone();
            list.id = import.value_list(list.id);
            list.name = prefixed(&list.name);
            import.value_source(&mut list.source);
            self.add_value_list(list);
        }
        let sql = import.sql(&source.sql);

        let map = import.map;
        for id in map.occurrences.values() {
            self.occurrence_files.insert(*id, file);
        }
        for id in map.tables.values() {
            self.table_files.insert(*id, file);
        }
        self.runtime_floor = import.ids.next;
        self.imported.push(FileSchemaContext {
            file_name: source.file_name.clone(),
            file_path,
            identity,
            locale: source.locale.clone(),
            authenticated_privileges: source.authenticated_privileges.clone(),
            account_info: source.authenticated_account.clone(),
            font_families: source.font_families.clone(),
            names,
            sql,
            ids: [
                &map.tables,
                &map.occurrences,
                &map.functions,
                &map.value_lists,
            ]
            .into_iter()
            .flatten()
            .map(|(own, client)| (*client, *own))
            .collect(),
        });
        map
    }

    /// Resolve `occurrence`, an occurrence of another file's table (this
    /// file's or an imported file's), to `table`, that table's client-only
    /// id once its file is imported. The occurrence keeps its id and name;
    /// the formulas of its own file see it and the table's fields under
    /// that name. `false` when `table` is not a base table here or the
    /// occurrence is not unresolved.
    pub fn bind_external_occurrence(&mut self, occurrence: u64, table: u64) -> bool {
        let Some(fields) = self.tables.get(&table).map(|t| &t.fields) else {
            return false;
        };
        let Some(external) = self.external.get(&occurrence) else {
            return false;
        };
        if let Some(&file) = self.occurrence_files.get(&occurrence) {
            let context = &mut self.imported[file];
            let name = context.local_name(&external.name).to_owned();
            context.names.add_occurrence(occurrence, &name, table);
            for field in fields.values() {
                context.names.add_field(table, field.id, &field.name);
                context.names.set_summary_field(
                    table,
                    field.id,
                    field.class == FieldClass::Summary,
                );
            }
            if let Some(entry) = context
                .sql
                .occurrences
                .iter_mut()
                .find(|o| o.id == occurrence)
            {
                entry.table = Some(table);
            }
        }
        // A formula of the table's file without an occurrence of its own
        // is solved here when this occurrence reads its field
        // (`Context::file`): that file's fields resolve here unqualified,
        // but none of its names finds the occurrence.
        let owner = self.occurrence_files.get(&occurrence).copied();
        if let Some(&file) = self.table_files.get(&table).filter(|&&f| Some(f) != owner) {
            let name = owner.map_or(external.name.as_str(), |o| {
                self.imported[o].local_name(&external.name)
            });
            let name = name.to_owned();
            self.imported[file]
                .names
                .add_foreign_occurrence(occurrence, &name, table);
        }
        if let Some(entry) = self.sql.occurrences.iter_mut().find(|o| o.id == occurrence) {
            entry.table = Some(table);
        }
        let external = self.external.remove(&occurrence).expect("checked above");
        // Its file is open: it waits no longer for that, but still for its
        // table's fields when they are not read yet (`fields_pending`).
        self.pending.remove(&occurrence);
        self.data_sources.insert(occurrence, external.source);
        self.add_occurrence(external.id, external.name, table);
        true
    }

    /// The data source an occurrence of another file's table names, bound
    /// ([`Self::bind_external_occurrence`]) or not.
    pub fn data_source(&self, occurrence: u64) -> Option<&str> {
        self.external
            .get(&occurrence)
            .map(|o| o.source.as_str())
            .or_else(|| self.data_sources.get(&occurrence).map(String::as_str))
    }

    /// The file owning base table `table` as [`Context::file`] numbers the
    /// files: 0 for this file, n for the nth imported. `None` when nothing
    /// is imported, where every formula is this file's.
    pub fn table_file(&self, table: u64) -> Option<u64> {
        if self.imported.is_empty() {
            return None;
        }
        Some(
            self.table_files
                .get(&table)
                .map_or(0, |&file| file as u64 + 1),
        )
    }

    /// [`Self::table_file`] of the base table `occurrence` shows.
    pub fn occurrence_table_file(&self, occurrence: u64) -> Option<u64> {
        if self.imported.is_empty() {
            return None;
        }
        self.table_file(self.occurrence(occurrence)?.table)
    }

    /// The imported file owning a formula solved in `context`: the file
    /// [`Context::file`] names, else the one owning the context's
    /// occurrence; `None` for this file's formulas.
    pub fn context_file(&self, context: &Context) -> Option<&FileSchemaContext> {
        match context.file {
            Some(file) => self
                .imported
                .get(usize::try_from(file.checked_sub(1)?).ok()?),
            None => self.file_context(context.occurrence?),
        }
    }

    /// The related files imported into this schema, in import order.
    pub fn imported_files(&self) -> &[FileSchemaContext] {
        &self.imported
    }

    /// The imported file owning value list `id`; `None` for this file's
    /// own value lists.
    pub fn value_list_context(&self, id: u64) -> Option<&FileSchemaContext> {
        if id < self.runtime_floor {
            return None;
        }
        self.imported.iter().find(|file| file.ids.contains_key(&id))
    }

    /// The imported file owning occurrence `occurrence`; `None` for this
    /// file's own occurrences.
    pub fn file_context(&self, occurrence: u64) -> Option<&FileSchemaContext> {
        if occurrence < self.runtime_floor {
            return None;
        }
        let file = *self.occurrence_files.get(&occurrence)?;
        self.imported.get(file)
    }

    /// The imported file owning base table `table`; `None` for this file's
    /// own tables.
    pub fn table_context(&self, table: u64) -> Option<&FileSchemaContext> {
        if table < self.runtime_floor {
            return None;
        }
        let file = *self.table_files.get(&table)?;
        self.imported.get(file)
    }

    /// The name of base table `table` in the file that owns it.
    pub fn local_table_name(&self, table: u64) -> Option<&str> {
        let name = &self.tables.get(&table)?.name;
        Some(
            self.table_context(table)
                .map_or(name, |file| file.local_name(name)),
        )
    }

    /// The font ids of the file owning base table `table`.
    pub fn fonts_for_table(&self, table: u64) -> &BTreeMap<u64, String> {
        self.table_context(table)
            .map_or(&self.font_families, |file| &file.font_families)
    }
}

/// Call `f` on each direct operand of `expr`.
fn for_each_child(expr: &mut Expr, mut f: impl FnMut(&mut Expr)) {
    match expr {
        Expr::List(args)
        | Expr::Call { args, .. }
        | Expr::Evaluate { args, .. }
        | Expr::CustomCall { args, .. }
        | Expr::PluginCall { args, .. } => args.iter_mut().for_each(f),
        Expr::Case(pairs) => {
            for (test, result) in pairs {
                f(test);
                f(result);
            }
        }
        Expr::Choose { test, results } => {
            f(test);
            results.iter_mut().for_each(f);
        }
        Expr::Let { bindings, body } => {
            bindings.iter_mut().for_each(&mut f);
            f(body);
        }
        Expr::While {
            initial,
            condition,
            logic,
            result,
        } => {
            initial.iter_mut().for_each(&mut f);
            f(condition);
            logic.iter_mut().for_each(&mut f);
            f(result);
        }
        Expr::GetField { arg, .. } => f(arg),
        Expr::Empty
        | Expr::Number(_)
        | Expr::Boolean(_)
        | Expr::Text(_)
        | Expr::Field(_)
        | Expr::FieldRef(_)
        | Expr::Symbol(_)
        | Expr::Local(_)
        | Expr::Variable(_)
        | Expr::Get(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::sql::{SqlOccurrence, SqlValueList};
    use crate::host::{JoinOp, JoinPredicate, RelationshipDef, SummaryDef, SummaryKind};
    use crate::meta::ValueList;
    use om_calc::eval::CustomFunction;
    use om_calc::ValueType;
    use om_formula::{Field, Names};
    use om_scripts::specs::{SortEntry, SpecField};

    fn file(name: &str) -> CalcSchema {
        let mut schema = CalcSchema::new(name, Locale::en_us());
        schema.add_table(1, "Rows");
        schema.add_occurrence(2, "Rows", 1);
        schema.add_field(1, FieldDef::data(1, "Value", ValueType::Text));
        schema
    }

    fn formula(context: u64, polish: Expr) -> Formula {
        Formula {
            context: Some(Key::from_id(context)),
            formatted: None,
            polish,
        }
    }

    fn call(function: u64) -> Expr {
        Expr::CustomCall {
            tail: false,
            function: Key::from_id(function),
            args: vec![],
        }
    }

    fn spec(occurrence: u64) -> SpecField {
        SpecField {
            occurrence: Some(occurrence),
            field: 1,
            repetition: 1,
        }
    }

    /// Every id a schema's members use.
    fn ids(schema: &CalcSchema) -> BTreeSet<u64> {
        let mut ids: BTreeSet<u64> = schema.tables.keys().copied().collect();
        ids.extend(schema.occurrences.keys());
        ids.extend(schema.external.keys());
        ids.extend(schema.custom_functions.keys());
        ids.extend(schema.relationships.iter().map(|r| r.id));
        ids.extend(schema.value_lists.iter().map(|l| l.id));
        ids
    }

    /// A file whose ids are the same as [`file`]'s and a root's more.
    fn same_ids(name: &str) -> CalcSchema {
        let mut schema = file(name);
        schema.add_occurrence(3, "More rows", 1);
        schema.add_custom_function(
            4,
            CustomFunction {
                name: "Shout".into(),
                parameters: vec![],
                body: Expr::Empty,
            },
        );
        schema.add_relationship(RelationshipDef::new(
            5,
            2,
            3,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 1,
                right: 1,
            }],
        ));
        schema.add_value_list(ValueList {
            id: 6,
            name: "Values".into(),
            source: ValueSource::Custom(vec!["a".into()]),
            options: 0,
            sort_locale: 0,
            separator: " ".into(),
        });
        schema
    }

    #[test]
    fn imported_ids_never_collide_and_map_back_to_the_files_own() {
        let mut root = same_ids("Root");
        let before = ids(&root);
        let first = root.import_file_schema(&same_ids("First"));
        let second = root.import_file_schema(&same_ids("Second"));
        let mut seen = before.clone();
        for (map, name) in [(&first, "First"), (&second, "Second")] {
            let owner = root.file_context(map.occurrences[&2]).unwrap();
            assert_eq!(owner.file_name, name);
            assert!(std::ptr::eq(
                owner,
                root.table_context(map.tables[&1]).unwrap()
            ));
            for (own, client) in map
                .tables
                .iter()
                .chain(&map.occurrences)
                .chain(&map.functions)
                .chain(&map.value_lists)
            {
                assert!(seen.insert(*client), "{own} -> {client} reused");
                assert_eq!(owner.file_id(*client), *own);
            }
        }
        // The root's own members are untouched and stay the root's.
        assert_eq!(root.table(1).unwrap().name, "Rows");
        assert_eq!(root.occurrence(2).unwrap().table, 1);
        assert_eq!(root.custom_function(4).unwrap().name, "Shout");
        assert!(root.file_context(2).is_none());
        assert!(root.table_context(1).is_none());
        assert_eq!(root.names().find_occurrence("Rows"), Some(2));
        assert_eq!(root.value_lists()[0].id, 6);
        // Imported members are listed under their file's name.
        let table = root.table(first.tables[&1]).unwrap();
        assert_eq!(table.name, "First::Rows");
        assert_eq!(root.local_table_name(table.id), Some("Rows"));
        assert_eq!(
            root.table_of(second.occurrences[&3]).unwrap().id,
            second.tables[&1]
        );
        // Relationships connect the importing file's occurrences only.
        let relationship = root.relationships().last().unwrap();
        assert_eq!(
            (relationship.left, relationship.right),
            (second.occurrences[&2], second.occurrences[&3])
        );
        assert!(!before.contains(&relationship.id));
        // The same files import to the same ids.
        let mut again = same_ids("Root");
        assert_eq!(
            again.import_file_schema(&same_ids("First")).occurrences,
            first.occurrences
        );
    }

    #[test]
    fn imported_formulas_name_their_own_files_members() {
        let mut root = same_ids("Root");
        let mut source = same_ids("Child");
        let reference = |occurrence| Expr::Field(Field::new(occurrence, 1, true));
        source.add_field(
            1,
            FieldDef::calculation(
                2,
                "Both",
                ValueType::Text,
                formula(2, Expr::List(vec![reference(3), call(4)])),
                false,
            ),
        );
        source.add_field(
            1,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(SummaryDef {
                    additional: Some(crate::meta::SummaryField {
                        occurrence: Some(3),
                        field: 1,
                        repetition: 1,
                    }),
                    ..SummaryDef::new(SummaryKind::Total, 1)
                }),
                ..FieldDef::data(3, "Total", ValueType::Number)
            },
        );
        source.relationships[0].sort_right = Some(SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: spec(3),
                summary: None,
                value_list: Some(6),
                descending: false,
                locale: 0,
            }],
        });
        source.add_value_list(ValueList {
            id: 7,
            name: "From field".into(),
            source: ValueSource::Field {
                primary: spec(3),
                secondary: None,
                start: Some(2),
            },
            options: 0,
            sort_locale: 0,
            separator: " ".into(),
        });
        let map = root.import_file_schema(&source);
        let occurrence = |id: u64| Key::from_id(map.occurrences[&id]);
        let both = root.field(map.occurrences[&2], 2).unwrap();
        let both = both.formula.as_deref().unwrap();
        assert_eq!(both.context, Some(occurrence(2)));
        assert_eq!(
            both.polish,
            Expr::List(vec![
                Expr::Field(Field {
                    occurrence: occurrence(3),
                    ..Field::new(3, 1, true)
                }),
                call(map.functions[&4]),
            ])
        );
        let total = root.field(map.occurrences[&2], 3).unwrap().summary.unwrap();
        assert_eq!(
            total.additional.unwrap().occurrence,
            Some(map.occurrences[&3])
        );
        let sort = root
            .relationships()
            .last()
            .unwrap()
            .sort_right
            .clone()
            .unwrap();
        assert_eq!(sort.fields[0].field.occurrence, Some(map.occurrences[&3]));
        assert_eq!(sort.fields[0].value_list, Some(map.value_lists[&6]));
        let list = root.value_list_named("Child::From field").unwrap();
        assert_eq!(
            list.source,
            ValueSource::Field {
                primary: SpecField {
                    occurrence: Some(map.occurrences[&3]),
                    ..spec(3)
                },
                secondary: None,
                start: Some(map.occurrences[&2]),
            }
        );
        // Names in the file resolve to the file's members by client ids.
        let names = &root.file_context(map.occurrences[&2]).unwrap().names;
        assert_eq!(
            names.find_occurrence("More rows"),
            Some(map.occurrences[&3])
        );
        assert_eq!(names.find_custom_function("Shout"), Some(map.functions[&4]));
        assert_eq!(names.find_occurrence("Child::Rows"), None);
    }

    #[test]
    fn catalog_growth_preserves_published_ids_and_unresolved_reservations() {
        let mut first = file("First");
        let second = same_ids("Second");
        let mut root = file("Root");
        let initial = root.import_file_schema(&first);
        let later = root.import_file_schema(&second);
        // A deferred catalog introduces references with no readable definitions.
        first.add_field(
            1,
            FieldDef::calculation(
                2,
                "Late",
                ValueType::Text,
                formula(
                    2,
                    Expr::List(vec![Expr::Field(Field::new(90, 1, true)), call(91)]),
                ),
                false,
            ),
        );
        let mut grown = file("Root");
        grown.reserve_imported_ids(&initial);
        grown.reserve_imported_ids(&later);
        let first_map =
            grown.import_owned_file_schema_with_map(&first, None, None, initial.clone());
        let later_map = grown.import_owned_file_schema_with_map(&second, None, None, later.clone());
        assert_eq!(first_map.tables, initial.tables);
        assert_eq!(first_map.occurrences[&2], initial.occurrences[&2]);
        assert_eq!(later_map.tables, later.tables);
        assert_eq!(later_map.occurrences, later.occurrences);
        assert_eq!(later_map.functions, later.functions);
        assert_eq!(later_map.value_lists, later.value_lists);
        let reserved = [first_map.occurrences[&90], first_map.functions[&91]];
        assert!(reserved.iter().all(|id| !ids(&root).contains(id)));
        // Another growth retains those reservations, including absent members.
        let mut again = file("Root");
        again.reserve_imported_ids(&first_map);
        again.reserve_imported_ids(&later_map);
        let next = again.import_owned_file_schema_with_map(&first, None, None, first_map.clone());
        assert_eq!(next.occurrences, first_map.occurrences);
        assert_eq!(next.functions, first_map.functions);
        assert!(again.table_of(next.occurrences[&90]).is_none());
    }

    #[test]
    fn missing_members_keep_reserved_ids_across_further_imports() {
        let mut target = file("Root");
        let mut source = file("First");
        source.add_field(
            1,
            FieldDef::calculation(
                2,
                "Missing",
                ValueType::Text,
                formula(
                    2,
                    Expr::List(vec![Expr::Field(Field::new(90, 1, true)), call(91)]),
                ),
                false,
            ),
        );
        let first = target.import_file_schema(&source);
        let mut other = file("Second");
        other.add_occurrence(90, "Other", 1);
        other.add_custom_function(
            91,
            CustomFunction {
                name: "Wrong function".into(),
                parameters: vec![],
                body: Expr::Empty,
            },
        );
        let second = target.import_file_schema(&other);
        let all = |map: &ImportedFileMap| -> BTreeSet<u64> {
            [
                &map.tables,
                &map.occurrences,
                &map.functions,
                &map.value_lists,
            ]
            .into_iter()
            .flat_map(|ids| ids.values().copied())
            .collect()
        };
        assert!(all(&first).is_disjoint(&all(&second)));
        assert_ne!(first.occurrences[&90], second.occurrences[&90]);
        assert_ne!(first.functions[&91], second.functions[&91]);
        assert!(target.occurrence(first.occurrences[&90]).is_none());
        assert!(target.custom_function(first.functions[&91]).is_none());
        assert_eq!(target.names().find_occurrence("Rows"), Some(2));
        assert_eq!(
            target
                .file_context(first.occurrences[&2])
                .unwrap()
                .names
                .find_occurrence("Rows"),
            Some(first.occurrences[&2])
        );
    }

    #[test]
    fn binding_an_external_occurrence_names_it_in_its_own_file() {
        let mut target = file("Root");
        let mut source = file("Child");
        source.add_external_occurrence(ExternalOccurrence {
            id: 3,
            name: "Parent".into(),
            source: "Root".into(),
        });
        let map = target.import_file_schema(&source);
        assert!(target.table_of(map.occurrences[&3]).is_none());
        assert!(target.bind_external_occurrence(map.occurrences[&3], 1));
        let names = &target.file_context(map.occurrences[&2]).unwrap().names;
        assert_eq!(names.find_occurrence("Parent"), Some(map.occurrences[&3]));
        assert_eq!(names.find_field(map.occurrences[&3], "Value"), Some(1));
        assert_eq!(target.table_of(map.occurrences[&3]).unwrap().id, 1);
        // Once only, and only to a table this schema has.
        assert!(!target.bind_external_occurrence(map.occurrences[&3], 1));
        target.add_external_occurrence(ExternalOccurrence {
            id: 4,
            name: "Elsewhere".into(),
            source: "Other".into(),
        });
        assert!(!target.bind_external_occurrence(4, 99));
        assert!(target.external_occurrence(4).is_some());
    }

    /// A table whose field catalog is not read yet (#772's lazy field
    /// catalogs) keeps that mark through an import, and its occurrences
    /// wait: the file's own, and the importing file's once bound to it.
    /// Binding ends only the wait for the file.
    #[test]
    fn a_table_without_its_fields_keeps_its_occurrences_waiting() {
        let mut target = file("Root");
        target.add_external_occurrence(ExternalOccurrence {
            id: 9,
            name: "Child Rows".into(),
            source: "Child".into(),
        });
        assert!(target.set_pending(9));
        let mut source = CalcSchema::new("Child", Locale::en_us());
        source.add_table(1, "Rows");
        source.add_occurrence(2, "Rows", 1);
        source.set_fields_pending(1);
        assert!(source.is_pending(2), "the file's own occurrence");
        let map = target.import_file_schema(&source);
        let table = map.tables[&1];
        assert!(target.are_fields_pending(table));
        assert!(target.is_pending(map.occurrences[&2]));
        assert!(target.bind_external_occurrence(9, table));
        assert_eq!(target.table_of(9).map(|t| t.id), Some(table));
        assert!(target.is_pending(9), "bound, its fields still to read");
        assert_eq!(target.pending_occurrences().count(), 0, "its file is open");
        assert!(target.waits(), "an occurrence still waits, for its table");
        // With its fields read, the same import waits for nothing.
        let mut target = file("Root");
        let map = target.import_file_schema(&file("Child"));
        assert!(!target.is_pending(map.occurrences[&2]));
        assert!(!target.waits());
    }

    #[test]
    fn font_ids_are_resolved_in_the_owning_file() {
        let mut target = file("Root");
        target.font_families.insert(0, "Helvetica".into());
        let mut child = file("Child");
        child.font_families.insert(0, "Courier".into());
        let map = target.import_file_schema(&child);
        assert_eq!(target.fonts_for_table(1)[&0], "Helvetica");
        assert_eq!(target.fonts_for_table(map.tables[&1])[&0], "Courier");
    }

    #[test]
    fn zero_formula_context_and_inaccessible_base_tables_stay_unresolved() {
        let mut target = file("Root");
        target.add_occurrence(77, "Root only", 1);
        let mut source = file("Child");
        source.add_occurrence(3, "Inaccessible", 99);
        source.set_sql(SqlCatalog {
            occurrences: vec![SqlOccurrence {
                id: 77,
                name: "Opaque definition".into(),
                table: None,
                mod_count: 0,
            }],
            value_lists: vec![SqlValueList {
                id: 8,
                name: "Listed".into(),
                mod_count: 0,
                source: ValueListSource::Field {
                    occurrence: 2,
                    field: 1,
                },
            }],
            ..SqlCatalog::default()
        });
        source.add_field(
            1,
            FieldDef::calculation(
                2,
                "No context",
                ValueType::Text,
                formula(0, Expr::Empty),
                false,
            ),
        );
        let map = target.import_file_schema(&source);
        let no_context = target.field(map.occurrences[&2], 2).unwrap();
        assert_eq!(no_context.formula.as_ref().unwrap().occurrence(), None);
        assert!(target.table(map.tables[&99]).is_none());
        assert!(target.table_of(map.occurrences[&3]).is_none());
        let sql = &target.file_context(map.occurrences[&2]).unwrap().sql;
        assert_eq!(sql.occurrences[0].id, map.occurrences[&77]);
        assert_ne!(sql.occurrences[0].id, 77);
        assert!(target.table_of(sql.occurrences[0].id).is_none());
        assert_eq!(
            sql.value_lists[0].source,
            ValueListSource::Field {
                occurrence: map.occurrences[&2],
                field: 1
            }
        );
    }

    #[test]
    fn imported_formulas_keep_their_decimal_separator_in_nested_evaluations() {
        use crate::host::{FileCalc, FileHost, MemorySource, RecordValues, SessionInfo};
        use om_calc::eval::{evaluate_text, Context, Variables};
        use om_calc::Date;
        let mut target = file("Root");
        let mut child = file("Child");
        child.locale.decimal_separator = ',';
        child.locale.thousands_separator = '.';
        let mut context = om_formula::Context::new(child.names());
        context.occurrence = Some(2);
        let expr = om_formula::compile(
            "GetAsNumber ( \"1,5\" ) & \"|\" & Evaluate ( \"1,5+1,5\" )",
            &context,
        )
        .unwrap()
        .expr;
        let mut field =
            FieldDef::calculation(2, "Decimal", ValueType::Text, formula(2, expr), false);
        field.options = 0x20;
        child.add_field(1, field);
        target.add_external_occurrence(ExternalOccurrence {
            id: 3,
            name: "Child Rows".into(),
            source: "Child".into(),
        });
        let map = target.import_file_schema(&child);
        assert!(target.bind_external_occurrence(3, map.tables[&1]));
        let mut source = MemorySource::new();
        source.put_record(1, 1, RecordValues::default());
        source.put_record(map.tables[&1], 1, RecordValues::default());
        let session = SessionInfo::default();
        let (mut locals, mut globals) = (Variables::new(), Variables::new());
        let mut host = FileHost::new(
            FileCalc::new(&target, &source, Date::from_ymd(2026, 10, 29).unwrap()),
            &session,
            &mut locals,
            &mut globals,
        );
        let result = evaluate_text(
            "Decimal",
            &mut host,
            &Context::record(map.occurrences[&2], 1),
        );
        assert_eq!(result.code(), 0);
        assert_eq!(om_calc::eval::display(&result.value, &host), "1,5|3");
    }
}
