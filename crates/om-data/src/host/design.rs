//! The Design functions a host answers from the file's schema
//! (calc-functions.md 5.8, "Design functions on FileMaker Server"): lists of
//! tables, layouts, scripts and value lists, a layout's or table's fields,
//! field types, comments and serial numbers, a layout's field objects and
//! named objects, relationships, and value list items (file-metadata.md
//! 6.4). Lists are separated by `¶`; ids are the ids FileMaker shows, the
//! serial decode of each member's key (file-metadata.md 1).
//!
//! A file is named by its name, any case, with or without `.fmp12`, or by
//! `""` for the file owning the formula; another name answers empty, as for
//! a file that is not open. Another file open in the session
//! ([`super::SessionInfo::other_files`]) answers from its own schema. A
//! related file imported into the schema (calc-values.md 13) answers with
//! its own tables, occurrences, fields and value lists under its own names
//! and ids, and the root file without them; a related file's layouts and
//! scripts are not imported, so the functions listing them answer from the
//! file's open schema when the session has it open, else empty, and
//! `WindowNames` lists the windows the session gives it. Names of tables,
//! layouts, value lists and fields match in any case. Every format here is
//! FileMaker Server's own answer over the shipped templates, its sample
//! file and generated probe files.

use std::cmp::Ordering;

use om_calc::eval::Context;
use om_calc::ops::{self, Comparison};
use om_calc::{CalcError, Ctx, Language, Locale, Value, ValueType};
use om_hbam::serial::id_of_raw;
use om_layout::{Layout, ObjectKind};

use super::schema::{FieldClass, FieldDef, LayoutField, TableDef};
use super::{CalcSchema, FileCalc, FileSchemaContext};
use crate::meta::{relation_option, value_list_option, ValueList, ValueSource};

/// A catalog id as FileMaker shows it.
fn shown(raw: u64) -> String {
    id_of_raw(raw).unwrap_or(raw).to_string()
}

/// `name` equals `wanted` exactly or in any case.
fn same_name(name: &str, wanted: &str) -> bool {
    name == wanted || name.to_lowercase() == wanted.to_lowercase()
}

/// Whether `name` names the file `file`: its name in any case, with or
/// without the `.fmp12` extension in any case.
fn names_file(file: &str, name: &str) -> bool {
    same_name(file, name)
        || name
            .to_lowercase()
            .strip_suffix(".fmp12")
            .is_some_and(|stem| stem == file.to_lowercase())
}

/// Whether the Design function `lower` answers from a file's layouts or
/// scripts, which a related file's imported schema lacks.
fn reads_layouts_or_scripts(lower: &str) -> bool {
    matches!(
        lower,
        "layoutnames"
            | "layoutids"
            | "scriptnames"
            | "scriptids"
            | "fieldstyle"
            | "fieldrepetitions"
            | "fieldbounds"
            | "layoutobjectnames"
    )
}

/// The file a Design function answers for: this file or a related file
/// imported into the schema ([`CalcSchema::import_file_schema`]).
/// Each lists only its own members, under the names and ids its file gives
/// them, never another file's or a client-only id.
#[derive(Clone, Copy)]
struct DesignFile<'a> {
    schema: &'a CalcSchema,
    /// `None` for this file.
    imported: Option<&'a FileSchemaContext>,
}

impl<'a> DesignFile<'a> {
    fn is_this_file(&self) -> bool {
        self.imported.is_none()
    }

    fn file_name(&self) -> &'a str {
        self.imported
            .map_or(&self.schema.file_name, |f| &f.file_name)
    }

    fn locale(&self) -> &'a Locale {
        self.imported.map_or(&self.schema.locale, |f| &f.locale)
    }

    /// Whether a member owned by `owner` (`None`: the root file) is this
    /// file's.
    fn owns(&self, owner: Option<&FileSchemaContext>) -> bool {
        match (owner, self.imported) {
            (None, None) => true,
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }

    fn owns_occurrence(&self, occurrence: u64) -> bool {
        self.owns(self.schema.file_context(occurrence))
    }

    fn owns_table(&self, table: u64) -> bool {
        self.owns(self.schema.table_context(table))
    }

    /// Whether a formula solved in `occurrence` names this file's fields
    /// unqualified: the occurrence is this file's or shows one of its
    /// tables.
    fn reads_fields_of(&self, occurrence: u64) -> bool {
        self.owns_occurrence(occurrence)
            || self
                .schema
                .table_of(occurrence)
                .is_some_and(|t| self.owns_table(t.id))
    }

    /// The name this file gives its member the schema lists as `name`.
    fn name<'n>(&self, name: &'n str) -> &'n str {
        self.imported.map_or(name, |f| f.local_name(name))
    }

    /// The id this file stores for its member `id`.
    fn own_id(&self, id: u64) -> u64 {
        self.imported.map_or(id, |f| f.file_id(id))
    }

    /// [`Self::own_id`] as FileMaker shows it.
    fn id(&self, id: u64) -> String {
        shown(self.own_id(id))
    }

    /// Sort catalog members by name in the file's language, as FileMaker
    /// lists tables and occurrences (case and accents as the collation
    /// orders them, digits as text).
    fn sort_by_name<T>(&self, items: &mut [T], name: impl Fn(&T) -> &str) {
        let collation = super::sort::collation_of(self.locale().language);
        items.sort_by(|a, b| {
            let (a, b) = (name(a), name(b));
            collation
                .sort_key(a)
                .cmp(&collation.sort_key(b))
                .then_with(|| a.cmp(b))
        });
    }

    /// Its occurrences, of its own tables and of other files', with ids.
    fn occurrences(&self) -> Vec<(u64, &'a str)> {
        let schema = self.schema;
        schema
            .occurrences()
            .map(|o| (o.id, o.name.as_str()))
            .chain(
                schema
                    .external_occurrences()
                    .map(|o| (o.id, o.name.as_str())),
            )
            .filter(|(id, _)| self.owns_occurrence(*id))
            .map(|(id, name)| (id, self.name(name)))
            .collect()
    }

    /// Its occurrence named `name`, exactly or else in any case.
    fn occurrence_named(&self, name: &str) -> Option<u64> {
        let occurrences = self.occurrences();
        let lower = name.to_lowercase();
        occurrences
            .iter()
            .find(|(_, n)| *n == name)
            .or_else(|| occurrences.iter().find(|(_, n)| n.to_lowercase() == lower))
            .map(|(id, _)| *id)
    }

    fn tables(&self) -> Vec<&'a TableDef> {
        self.schema
            .tables()
            .filter(|t| self.owns_table(t.id))
            .collect()
    }

    /// Its value lists in display order.
    fn value_lists(&self) -> Vec<&'a ValueList> {
        let schema = self.schema;
        schema
            .value_lists()
            .iter()
            .filter(|l| self.owns(schema.value_list_context(l.id)))
            .collect()
    }

    /// Its value list named `name`, exactly or else in any case.
    fn value_list_named(&self, name: &str) -> Option<&'a ValueList> {
        let lists = self.value_lists();
        let lower = name.to_lowercase();
        lists
            .iter()
            .find(|l| self.name(&l.name) == name)
            .or_else(|| {
                lists
                    .iter()
                    .find(|l| self.name(&l.name).to_lowercase() == lower)
            })
            .copied()
    }
}

impl<'a> FileCalc<'a> {
    /// Whether `name` names this file: empty, or its name in any case with
    /// or without the `.fmp12` extension in any case.
    pub fn names_this_file(&self, name: &str) -> bool {
        name.is_empty() || names_file(&self.schema.file_name, name)
    }

    /// The file `name` names for a formula solved in `context`: `""` the
    /// formula's own (calc-values.md 13), else this file or an imported
    /// one by its name; `None` for a file that is not open.
    fn design_file(&self, name: &str, context: &Context) -> Option<DesignFile<'a>> {
        let schema = self.schema;
        let imported = if name.is_empty() {
            schema.context_file(context)
        } else if names_file(&schema.file_name, name) {
            None
        } else {
            let files = schema.imported_files();
            Some(files.iter().find(|f| names_file(&f.file_name, name))?)
        };
        Some(DesignFile { schema, imported })
    }

    /// The Design function `name` with `args`, `None` for one this host
    /// does not answer.
    pub fn design(
        &self,
        name: &str,
        args: &[Value],
        context: &Context,
    ) -> Option<Result<Value, CalcError>> {
        let cx = self.cx();
        let text = |i: usize| -> String {
            args.get(i)
                .map(|v| v.to_text(&cx).into_string())
                .unwrap_or_default()
        };
        let list = |items: Vec<String>| Some(Ok(Value::text(items.join("\r"))));
        let empty = || Some(Ok(Value::empty_text()));
        let schema = self.schema;
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "databasenames" => {
                return list(match self.session {
                    Some(session) => session.database_names(&schema.file_name),
                    None => vec![schema.file_name.clone()],
                })
            }
            // Every window front first, or those of the file named.
            "windownames" => {
                let Some(session) = self.session else {
                    return empty();
                };
                // `""` is the formula's own file, which may be a related
                // file; the session knows which windows each file has.
                let file = (!args.is_empty()).then(|| text(0));
                let file = file.as_deref().map(|f| {
                    self.design_file(f, context)
                        .map_or(f, |file| file.file_name())
                });
                return list(session.window_names(&schema.file_name, file));
            }
            _ => {}
        }
        let file = self.design_file(&text(0), context);
        // Another open file answers from its own schema (no records): one
        // not imported into this one, and an imported one's layouts and
        // scripts, which are not imported.
        let open_file = match file {
            None => Some(text(0)),
            Some(f) if !f.is_this_file() && reads_layouts_or_scripts(&lower) => {
                Some(f.file_name().to_owned())
            }
            Some(_) => None,
        };
        let open_file = open_file
            .zip(self.session)
            .and_then(|(named, session)| Some((session.other_file(&named)?, session)));
        if let Some((other, session)) = open_file {
            let source = super::MemorySource::new();
            return FileCalc::new(&other.schema, &source, self.today)
                .with_session(session)
                .design(name, args, &Context::new());
        }
        let Some(file) = file else {
            return empty();
        };
        match lower.as_str() {
            // A related file's layouts and scripts are not imported.
            _ if !file.is_this_file() && reads_layouts_or_scripts(&lower) => empty(),
            "tablenames" | "tableids" => {
                // Occurrences of the file's tables and of other files'.
                let mut occurrences = file.occurrences();
                file.sort_by_name(&mut occurrences, |o| o.1);
                list(
                    occurrences
                        .iter()
                        .map(|(id, name)| {
                            if lower == "tablenames" {
                                (*name).to_owned()
                            } else {
                                file.id(*id)
                            }
                        })
                        .collect(),
                )
            }
            "basetablenames" | "basetableids" => {
                let mut tables = file.tables();
                file.sort_by_name(&mut tables, |t| file.name(&t.name));
                list(
                    tables
                        .iter()
                        .map(|t| {
                            if lower == "basetablenames" {
                                file.name(&t.name).to_owned()
                            } else {
                                file.id(t.id)
                            }
                        })
                        .collect(),
                )
            }
            "layoutnames" => list(schema.listed_layouts().map(|l| l.name.clone()).collect()),
            "layoutids" => list(
                schema
                    .listed_layouts()
                    .map(|l| shown(u64::from(l.id)))
                    .collect(),
            ),
            "scriptnames" | "scriptids" => list(
                schema
                    .scripts()
                    .iter()
                    .filter(|s| {
                        !matches!(
                            s.kind,
                            om_scripts::ScriptKind::Folder | om_scripts::ScriptKind::FolderEnd
                        )
                    })
                    .map(|s| {
                        if lower == "scriptnames" {
                            s.name.clone()
                        } else {
                            s.id.to_string()
                        }
                    })
                    .collect(),
            ),
            "valuelistnames" => list(
                file.value_lists()
                    .iter()
                    .map(|l| file.name(&l.name).to_owned())
                    .collect(),
            ),
            "valuelistids" => list(file.value_lists().iter().map(|l| file.id(l.id)).collect()),
            "valuelistitems" => {
                let Some(value_list) = file.value_list_named(&text(1)) else {
                    return empty();
                };
                Some(
                    self.value_list_items(value_list, context)
                        .map(|items| Value::text(items.join("\r"))),
                )
            }
            "fieldnames" | "fieldids" => {
                list(self.field_names(file, &text(1), lower == "fieldids"))
            }
            "fieldstyle" | "fieldrepetitions" | "fieldbounds" => {
                let Some(field) = self.layout_field(file, &text(1), &text(2)) else {
                    return empty();
                };
                let answer = match lower.as_str() {
                    "fieldstyle" => self.field_style(field),
                    "fieldrepetitions" => format!(
                        "{} {}",
                        field.repetitions,
                        if field.horizontal {
                            "horizontal"
                        } else {
                            "vertical"
                        }
                    ),
                    _ => {
                        let [left, top, right, bottom] = field.bounds.map(|v| v.round() as i64);
                        format!("{left} {top} {right} {bottom} {}", field.rotation)
                    }
                };
                Some(Ok(Value::text(answer)))
            }
            "fieldtype" | "fieldcomment" | "fieldannotation" | "fielddisplaynames"
            | "getnextserialvalue" => {
                let Some(def) = self.named_field(file, &text(1), context) else {
                    return empty();
                };
                let answer = match lower.as_str() {
                    "fieldtype" => {
                        field_type(def, self.repetitions_of(file, def, &text(1), context))
                    }
                    "fieldcomment" => def.comment.clone(),
                    "getnextserialvalue" => def.next_serial.clone().unwrap_or_default(),
                    // Not stored in any file read so far: what FileMaker
                    // answers for a field without them.
                    _ => String::new(),
                };
                Some(Ok(Value::text(answer)))
            }
            "basetablecomment" => {
                let wanted = text(1);
                Some(Ok(Value::text(
                    file.tables()
                        .into_iter()
                        .find(|t| same_name(file.name(&t.name), &wanted))
                        .map(|t| t.comment.clone())
                        .unwrap_or_default(),
                )))
            }
            "relationinfo" => {
                let Some(occurrence) = file.occurrence_named(&text(1)) else {
                    return empty();
                };
                Some(Ok(Value::text(self.relation_info(file, occurrence))))
            }
            "layoutobjectnames" => {
                let names = self
                    .layout_with_objects(&text(1))
                    .and_then(|l| l.layout.as_deref())
                    .map(layout_object_names)
                    .unwrap_or_default();
                Some(Ok(Value::text(names)))
            }
            _ => None,
        }
    }

    /// Layout `name` for a Design function that reads its objects or
    /// fields: `None` while only the catalog's index was read for it (on
    /// demand, #1019), its member asked for meanwhile, so the calculation
    /// waits for it rather than finding no objects.
    fn layout_with_objects(&self, name: &str) -> Option<&'a super::LayoutEntry> {
        let entry = self.schema.layout_named(name)?;
        if self.schema.is_layout_pending(entry.id) {
            self.source.need_layout(entry.id);
            return None;
        }
        Some(entry)
    }

    /// The occurrence a `""` table or layout argument stands for: the
    /// window's layout's, else the calculation's.
    fn current_occurrence(&self, context: Option<&Context>) -> Option<u64> {
        self.session
            .and_then(|s| self.schema.layout_named(&s.layout_name))
            .and_then(|l| l.occurrence)
            .or_else(|| context.and_then(|c| c.occurrence))
    }

    /// `FieldNames` and `FieldIDs`: a layout's field objects in object
    /// order, one per object (a related field as `Occurrence::Field`, its
    /// id as `occurrenceID::fieldID`), else the fields of an occurrence's
    /// table in id order; `""` names the window's layout's occurrence. A
    /// layout wins over an occurrence of the same name. A field FileMaker
    /// cannot resolve (another file's table) lists an empty name. Only
    /// this file's layouts and window are known.
    fn field_names(&self, file: DesignFile<'a>, target: &str, ids: bool) -> Vec<String> {
        let schema = self.schema;
        if !target.is_empty() && file.is_this_file() {
            if let Some(layout) = self.layout_with_objects(target) {
                return layout
                    .fields
                    .iter()
                    .map(|f| {
                        let own = layout.occurrence == Some(f.occurrence);
                        if ids {
                            if own {
                                shown(f.field)
                            } else {
                                format!("{}::{}", shown(f.occurrence), shown(f.field))
                            }
                        } else {
                            match (
                                schema.field(f.occurrence, f.field),
                                schema.occurrence(f.occurrence),
                            ) {
                                (Some(def), _) if own => def.name.clone(),
                                (Some(def), Some(o)) => format!("{}::{}", o.name, def.name),
                                // A waiting occurrence's name comes once it
                                // is read; its need is listed.
                                _ => {
                                    self.waits(f.occurrence);
                                    String::new()
                                }
                            }
                        }
                    })
                    .collect();
            }
        }
        let occurrence = if target.is_empty() {
            self.current_occurrence(None)
                .filter(|_| file.is_this_file())
        } else {
            file.occurrence_named(target)
        };
        // A waiting occurrence lists its fields once they are read; its
        // need is listed.
        if occurrence.is_some_and(|o| self.waits(o)) {
            return Vec::new();
        }
        let Some(table) = occurrence.and_then(|o| schema.table_of(o)) else {
            return Vec::new();
        };
        table
            .fields
            .values()
            .map(|f| if ids { shown(f.id) } else { f.name.clone() })
            .collect()
    }

    /// A layout's first field object showing `name`: `Occurrence::Field`,
    /// or a field of the layout's occurrence.
    fn layout_field(
        &self,
        file: DesignFile<'a>,
        layout: &str,
        name: &str,
    ) -> Option<&'a LayoutField> {
        let schema = self.schema;
        let layout = self.layout_with_objects(layout)?;
        let (occurrence, field) = match name.split_once("::") {
            Some((occurrence, field)) => (file.occurrence_named(occurrence)?, field),
            None => (layout.occurrence?, name),
        };
        if self.waits(occurrence) {
            return None;
        }
        layout.fields.iter().find(|f| {
            f.occurrence == occurrence
                && schema
                    .field(f.occurrence, f.field)
                    .is_some_and(|d| same_name(&d.name, field))
        })
    }

    /// `FieldStyle`: the control style; a list or calendar adds a space and
    /// its value list's name, which may be empty (`Calendar `).
    fn field_style(&self, field: &LayoutField) -> String {
        let style = match field.control {
            1 => "Popuplist",
            2 => "Popupmenu",
            3 => "Checkbox",
            4 => "RadioButton",
            6 => "Calendar",
            // Edit boxes; 5 is a legacy value and 7 the concealed edit box
            // (layout-model.md 5.4), not seen in a probed file [I].
            _ if field.scrolling => return "Scrolling".into(),
            _ => return "Standard".into(),
        };
        let list = field
            .value_list
            .and_then(|id| self.schema.value_lists().iter().find(|l| l.id == id))
            .map_or("", |l| l.name.as_str());
        format!("{style} {list}")
    }

    /// The repetitions `FieldType` reports: a summary of repetitions
    /// individually has those of the field it summarizes.
    fn repetitions_of(
        &self,
        file: DesignFile<'a>,
        def: &FieldDef,
        name: &str,
        context: &Context,
    ) -> u32 {
        let summarized = def
            .summary
            .as_ref()
            .filter(|s| s.has(crate::meta::summary_flag::INDIVIDUAL_REPETITIONS))
            .and_then(|s| s.field);
        let occurrence = match name.split_once("::") {
            Some((occurrence, _)) => file.occurrence_named(occurrence),
            None => context.occurrence,
        };
        summarized
            .zip(occurrence)
            .and_then(|(f, o)| self.schema.field(o, f.field))
            .map_or(def.repetitions, |f| f.repetitions)
            .max(1)
    }

    /// A field named `Occurrence::Field` of one of the file's occurrences,
    /// or `Field` of the calculation's occurrence when that reads the
    /// file's fields.
    fn named_field(
        &self,
        file: DesignFile<'a>,
        name: &str,
        context: &Context,
    ) -> Option<&'a FieldDef> {
        let schema = self.schema;
        let (occurrence, field) = match name.split_once("::") {
            Some((occurrence, field)) => (file.occurrence_named(occurrence)?, field),
            None => (
                context.occurrence.filter(|&o| file.reads_fields_of(o))?,
                name,
            ),
        };
        if self.waits(occurrence) {
            return None;
        }
        let table = schema.table_of(occurrence)?;
        table.fields.values().find(|f| f.name == field).or_else(|| {
            table
                .fields
                .values()
                .find(|f| f.name.to_lowercase() == field.to_lowercase())
        })
    }

    /// `RelationInfo`: for each relationship of `occurrence`, ordered by the
    /// related occurrence's id, the lines `Source:file`, `Table:other`,
    /// `Options:` with the other side's checkboxes (`Delete Create
    /// Sorted`) and one line per predicate, the other side's field first
    /// with the operator read from it; relationships are separated by an
    /// empty line. Another file's occurrence has its data source as the
    /// source; its fields are unknown here unless that file is imported:
    /// empty on the other side, `<File Missing>` on the named side.
    fn relation_info(&self, file: DesignFile<'a>, occurrence: u64) -> String {
        let schema = self.schema;
        let name = |o: u64| {
            schema
                .occurrence(o)
                .map(|o| o.name.as_str())
                .or_else(|| schema.external_occurrence(o).map(|o| o.name.as_str()))
                .map_or_else(String::new, |n| file.name(n).to_owned())
        };
        let field = |o: u64, f: u64, missing: &str| {
            let field = match schema.field(o, f) {
                Some(def) => def.name.clone(),
                // Unknown here, as for a file not imported: an occurrence
                // waiting for its file or its table's fields. RelationInfo
                // names every relationship's other side, and asks for none.
                None if schema.is_pending(o) || schema.external_occurrence(o).is_some() => {
                    missing.to_owned()
                }
                None => String::new(),
            };
            format!("{}::{field}", name(o))
        };
        let source = |o: u64| schema.data_source(o).unwrap_or(file.file_name()).to_owned();
        let mut relationships: Vec<_> = schema
            .relationships()
            .iter()
            .filter(|r| r.left == occurrence || r.right == occurrence)
            .collect();
        let other_of = |r: &super::RelationshipDef| {
            if r.left == occurrence {
                r.right
            } else {
                r.left
            }
        };
        relationships.sort_by_key(|r| {
            let other = file.own_id(other_of(r));
            id_of_raw(other).unwrap_or(other)
        });
        let mut blocks = Vec::new();
        for r in relationships {
            let from_left = r.left == occurrence;
            let other = other_of(r);
            let (delete, create, sort) = if from_left {
                (
                    relation_option::CASCADE_DELETE_LEFT_TO_RIGHT,
                    relation_option::CREATE_LEFT_TO_RIGHT,
                    relation_option::SORT_LEFT_TO_RIGHT,
                )
            } else {
                (
                    relation_option::CASCADE_DELETE_RIGHT_TO_LEFT,
                    relation_option::CREATE_RIGHT_TO_LEFT,
                    relation_option::SORT_RIGHT_TO_LEFT,
                )
            };
            let options: Vec<&str> = [(delete, "Delete"), (create, "Create"), (sort, "Sorted")]
                .iter()
                .filter(|(bit, _)| r.options & bit != 0)
                .map(|(_, word)| *word)
                .collect();
            let mut lines = vec![
                format!("Source:{}", source(other)),
                format!("Table:{}", name(other)),
                format!("Options:{}", options.join(" ")),
            ];
            for p in &r.predicates {
                let (theirs, op, mine) = if from_left {
                    (
                        field(other, p.right, ""),
                        p.op.reversed(),
                        field(occurrence, p.left, "<File Missing>"),
                    )
                } else {
                    (
                        field(other, p.left, ""),
                        p.op,
                        field(occurrence, p.right, "<File Missing>"),
                    )
                };
                lines.push(format!("{theirs} {} {mine}", operator_text(op)));
            }
            blocks.push(lines.join("\r"));
        }
        blocks.join("\r\r")
    }

    /// `ValueListItems` (file-metadata.md 6.4): custom values in order,
    /// dividers included; a field's values unique and in index order, or
    /// one per distinct second-field value ordered by it; related values
    /// from the context's record of the start occurrence. These are the
    /// stored (first field's) values whatever the list displays.
    pub fn value_list_items(
        &self,
        list: &ValueList,
        context: &Context,
    ) -> Result<Vec<String>, CalcError> {
        Ok(self
            .list_entries(list, context, false, usize::MAX)?
            .into_iter()
            .flatten()
            .map(|entry| entry.value)
            .collect())
    }

    /// The items of [`Self::value_list_items`], in the same order, each
    /// with the text a checkbox or radio button set shows for it: the
    /// second field's value of a list set to show values only from the
    /// second field (FileMaker Pro Help, "Setting up a field control":
    /// the set displays the second field's value and the first field's is
    /// stored), else the stored value itself. `None` as soon as the list
    /// has more than `limit` items: the rest is not read or sorted.
    pub fn value_list_entries(
        &self,
        list: &ValueList,
        context: &Context,
        limit: usize,
    ) -> Result<Option<Vec<ValueListEntry>>, CalcError> {
        self.list_entries(list, context, true, limit)
    }

    /// The list's items, or `None` past `limit` of them; with `labels`, a
    /// second-field display reads the second field too (`ValueListItems`
    /// never needs it).
    fn list_entries(
        &self,
        list: &ValueList,
        context: &Context,
        labels: bool,
        limit: usize,
    ) -> Result<Option<Vec<ValueListEntry>>, CalcError> {
        // In the formats of the file owning the list.
        let schema = self.schema;
        // A list the account's custom set gives no access has no items to
        // read (session-identity.md 2.2).
        if !schema.shows_value_list(list.id) {
            return Ok(Some(Vec::new()));
        }
        let locale = schema
            .value_list_context(list.id)
            .map_or(&schema.locale, |file| &file.locale);
        let cx = Ctx::new(locale, self.today);
        let (primary, secondary, start) = match &list.source {
            ValueSource::Custom(values) => {
                let items: Vec<ValueListEntry> = values
                    .iter()
                    .flat_map(|v| v.split(is_value_break))
                    .filter(|v| !v.is_empty())
                    .map(ValueListEntry::same)
                    .collect();
                return Ok((items.len() <= limit).then_some(items));
            }
            ValueSource::External { .. } => return Ok(Some(Vec::new())),
            ValueSource::Calculation(_) => return Err(CalcError::NOT_SUPPORTED),
            ValueSource::Field {
                primary,
                secondary,
                start,
            } => (primary, secondary, start),
        };
        let occurrence = primary.occurrence.ok_or(CalcError::TABLE_MISSING)?;
        // A waiting occurrence's values come once it is read: 1224 with
        // its need listed, not a missing table.
        if self.waits(occurrence) {
            return Err(CalcError::from_code(1224).expect("nonzero"));
        }
        let table = self
            .schema
            .table_of(occurrence)
            .ok_or(CalcError::TABLE_MISSING)?;
        let records: Vec<u64> = match start {
            Some(start) => {
                let same_table = context
                    .occurrence
                    .and_then(|o| self.schema.table_of(o))
                    .zip(self.schema.table_of(*start))
                    .is_some_and(|(a, b)| a.id == b.id);
                match context.record {
                    Some(record) if same_table => {
                        self.related_records(*start, record, occurrence)?
                    }
                    _ => return Ok(Some(Vec::new())),
                }
            }
            None => self.source.records(table.id)?.to_vec(),
        };
        self.source.prefetch_records(table.id, &records);
        // As a calculation reads them: an unstored calculation evaluated
        // on the record, FileMaker lists its values too (#1058). A record
        // whose value OpenMaker could not compute, or whose calculation
        // read data still being fetched, fails the list rather than leave
        // that record's values out.
        let read = |record: u64, field: u64, repetition: u16| {
            self.read_field_computed(occurrence, record, field, u32::from(repetition.max(1)))
        };
        let def = table.fields.get(&primary.field);
        let language = match (list.sort_locale, def.map_or(0, |d| d.language)) {
            (0, 0) => cx.locale.language,
            (0, l) | (l, _) => Language::from_id(u16::try_from(l).unwrap_or(0)),
        };
        let collation = super::sort::collation_of(language);
        let same = |a: &Value, b: &Value| {
            matches!(
                ops::compare(a, b, &cx),
                Comparison::Ordered(Ordering::Equal)
            )
        };
        let by_second = list.options & value_list_option::SORT_BY_SECOND != 0;
        let second_only = labels && list.options & value_list_option::SECOND_ONLY != 0;
        // The sort key, the stored value and, for a second-field display,
        // the second field's value of the record that gave the item.
        let mut items: Vec<(Value, Value, Option<Value>)> = Vec::new();
        // An item is new when `same` holds for none of the items before it.
        // Among items of one kind that is found by key, built once per
        // value (`eq_key`). `=` can also hold between kinds (text and a
        // number by its text, numbers of two precisions), so a value is
        // compared one by one, as before, with the items of other kinds
        // and with containers, which have no key (`unkeyed`).
        let mut kinds: std::collections::HashMap<
            KeyKind,
            (std::collections::HashSet<EqKey>, Vec<usize>),
        > = std::collections::HashMap::new();
        let mut unkeyed: Vec<usize> = Vec::new();
        for record in records {
            let value = read(record, primary.field, primary.repetition)?;
            if value.is_empty() {
                continue;
            }
            // A text field's index holds each of its values (lines), and
            // the list shows them one by one.
            let values: Vec<Value> = match (&value, secondary) {
                (Value::Text(text), None) => text
                    .as_str()
                    .split(is_value_break)
                    .filter(|v| !v.is_empty())
                    .map(Value::text)
                    .collect(),
                _ => vec![value],
            };
            for value in values {
                let second = match secondary {
                    Some(second) if by_second || second_only => {
                        Some(read(record, second.field, second.repetition)?)
                    }
                    _ => None,
                };
                let key = match &second {
                    Some(second) if by_second => second.clone(),
                    _ => value.clone(),
                };
                if key.is_empty() {
                    continue;
                }
                let keyed = eq_key(&key, &cx);
                let any_same = |at: &[usize]| at.iter().any(|&i| same(&items[i].0, &key));
                let known = match &keyed {
                    Some((kind, k)) => {
                        kinds.get(kind).is_some_and(|(seen, _)| seen.contains(k))
                            || any_same(&unkeyed)
                            || kinds
                                .iter()
                                .any(|(other, (_, at))| other != kind && any_same(at))
                    }
                    None => items.iter().any(|(k, _, _)| same(k, &key)),
                };
                if known {
                    continue;
                }
                match keyed {
                    Some((kind, k)) => {
                        let (seen, at) = kinds.entry(kind).or_default();
                        seen.insert(k);
                        at.push(items.len());
                    }
                    None => unkeyed.push(items.len()),
                }
                items.push((key, value, second.filter(|_| second_only)));
                if items.len() > limit {
                    return Ok(None);
                }
            }
        }
        // In the list's collation when an item is text (a number then by
        // its text), else as `<` orders them; the sort keys are built once
        // per item.
        let text = items
            .iter()
            .any(|(k, _, _)| k.value_type() == ValueType::Text);
        let items: Vec<_> = items
            .into_iter()
            .map(|item| {
                let key = text.then(|| collation.sort_key(item.0.to_text(&cx).as_str()));
                (key, item)
            })
            .collect();
        let items = stable_sort_by(items, |(x, a), (y, b)| {
            if a.0.value_type() == ValueType::Text || b.0.value_type() == ValueType::Text {
                x.cmp(y)
            } else {
                match ops::compare(&a.0, &b.0, &cx) {
                    Comparison::Ordered(o) => o,
                    _ => Ordering::Equal,
                }
            }
        });
        Ok(Some(
            items
                .into_iter()
                .map(|(_, (_, value, second))| {
                    let value = value.to_text(&cx).into_string();
                    let label =
                        second.map_or_else(|| value.clone(), |s| s.to_text(&cx).into_string());
                    ValueListEntry { value, label }
                })
                .collect(),
        ))
    }
}

/// `items` in the order of `cmp`, equal items in their first order: what
/// `sort_by` gives, without its panic when `cmp` is not a total order. A
/// value list mixing text and numbers has such an order (a number and a
/// text compare as text, two numbers as numbers: 2 < 10, "10" < "2"); it is
/// then sorted some way rather than taking the window down.
fn stable_sort_by<T>(items: Vec<T>, cmp: impl Fn(&T, &T) -> Ordering) -> Vec<T> {
    let count = items.len();
    let mut order: Vec<usize> = (0..count).collect();
    let mut merged = vec![0; count];
    let mut width = 1;
    while width < count {
        for start in (0..count).step_by(2 * width) {
            let middle = (start + width).min(count);
            let end = (start + 2 * width).min(count);
            let (mut left, mut right, mut at) = (start, middle, start);
            while left < middle && right < end {
                // The right run's item goes first only when it is less.
                if cmp(&items[order[right]], &items[order[left]]) == Ordering::Less {
                    merged[at] = order[right];
                    right += 1;
                } else {
                    merged[at] = order[left];
                    left += 1;
                }
                at += 1;
            }
            let rest = middle - left;
            merged[at..at + rest].copy_from_slice(&order[left..middle]);
            merged[at + rest..end].copy_from_slice(&order[right..end]);
        }
        std::mem::swap(&mut order, &mut merged);
        width *= 2;
    }
    let mut items: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|at| items[at].take())
        .collect()
}

/// The kind of value an [`EqKey`] may be compared with: text, or numbers,
/// dates, times or timestamps of one type and display precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum KeyKind {
    Text,
    Number(ValueType, u16),
}

/// What `=` compares, built once per value (`ops::compare`): text by its
/// characters' keys in the file's language (`text_eq_key`), a number by
/// its value rounded to its precision (`Number::eq_key`), a date, time or
/// timestamp by its number. Two values of one [`KeyKind`] are `=` exactly
/// when their keys are equal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum EqKey {
    Text(Vec<u32>),
    Number(om_calc::number::NumberKey),
}

/// [`EqKey`] of `value` and its kind; `None` for a container, which `=`
/// compares by its data.
fn eq_key(value: &Value, cx: &Ctx) -> Option<(KeyKind, EqKey)> {
    match value.value_type() {
        ValueType::Text => {
            let text = value.to_text(cx);
            let key = om_calc::text::text_eq_key(text.as_str(), cx.locale.language);
            Some((KeyKind::Text, EqKey::Text(key)))
        }
        ValueType::Container => None,
        kind => {
            let number = value.to_number(cx);
            let precision = number.precision();
            Some((
                KeyKind::Number(kind, precision),
                EqKey::Number(number.eq_key()),
            ))
        }
    }
}

/// A value list item: the value a field stores when the item is chosen,
/// and the text a checkbox or radio button set shows for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueListEntry {
    pub value: String,
    pub label: String,
}

impl ValueListEntry {
    fn same(value: &str) -> Self {
        Self {
            value: value.to_owned(),
            label: value.to_owned(),
        }
    }
}

/// A character that ends a value in a list (calc-functions.md 3.4): CR,
/// LF, U+2028 and U+2029. FileMaker's XML importer writes custom values
/// with LF between them.
fn is_value_break(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}')
}

/// The comparison operator as `RelationInfo` writes it.
fn operator_text(op: crate::meta::JoinOp) -> &'static str {
    use crate::meta::JoinOp;
    match op {
        JoinOp::Equal => "=",
        JoinOp::NotEqual => "<>",
        JoinOp::Less => "<",
        JoinOp::LessEqual => "<=",
        JoinOp::Greater => ">",
        JoinOp::GreaterEqual => ">=",
        JoinOp::Cartesian => "x",
    }
}

/// `LayoutObjectNames`: the layout's objects depth first in object order.
/// A named object adds its name; a portal, tab panel, slide panel, popover
/// or button bar adds `<` before its contents and `>` after; a group adds an
/// empty item before its contents. Items are joined so that each starts
/// on a new line unless the text already ends with one, which makes empty
/// items invisible except at the very end (`<¶>¶` when a group follows the
/// last named object or bracket), as FileMaker Server answers.
pub fn layout_object_names(layout: &Layout) -> String {
    fn add(out: &mut String, item: &str) {
        if !out.is_empty() && !out.ends_with('\r') {
            out.push('\r');
        }
        out.push_str(item);
    }
    fn walk(layout: &Layout, id: u64, depth: usize, out: &mut String) {
        let Some(object) = layout.objects.get(id) else {
            return;
        };
        if depth > 64 {
            return;
        }
        if let Some(name) = &object.name {
            add(out, name);
        }
        let bracket = matches!(
            object.kind(),
            ObjectKind::Portal
                | ObjectKind::TabPanel
                | ObjectKind::SlidePanel
                | ObjectKind::Popover
                | ObjectKind::ButtonBar
        );
        if bracket {
            add(out, "<");
        } else if object.kind() == ObjectKind::Group {
            add(out, "");
        }
        for child in &object.children {
            walk(layout, *child, depth + 1, out);
        }
        if bracket {
            add(out, ">");
        }
    }
    let mut out = String::new();
    for id in &layout.objects.root {
        walk(layout, *id, 0, &mut out);
    }
    out
}

/// `FieldType`: storage, data type, indexing and repetitions, separated by
/// spaces: `Standard Text Indexed 1`. A global field is `Global`, a global
/// calculation `StoredCalc`; container fields stored externally
/// (`External(Secure)`, `External(Open)` in the Claris reference) are not
/// told apart, their option bits are not established.
fn field_type(def: &FieldDef, repetitions: u32) -> String {
    let storage = match def.class {
        FieldClass::Data if def.global => "Global",
        FieldClass::Data => "Standard",
        FieldClass::Summary => "Summary",
        FieldClass::Calculation if !def.stored && !def.global => "UnstoredCalc",
        FieldClass::Calculation => "StoredCalc",
    };
    let data_type = match def.value_type {
        ValueType::Text => "Text",
        ValueType::Number => "Number",
        ValueType::Date => "Date",
        ValueType::Time => "Time",
        ValueType::Timestamp => "Timestamp",
        ValueType::Container => "Container",
    };
    let stored = def.class == FieldClass::Data || def.stored;
    let indexed = if stored && !def.global && def.class != FieldClass::Summary && def.indexed() {
        "Indexed"
    } else {
        "Unindexed"
    };
    format!("{storage} {data_type} {indexed} {repetitions}")
}

#[cfg(test)]
mod tests {
    use om_layout::{Layout, LayoutObject, ObjectStore};

    use super::layout_object_names;

    /// An object of wire type `kind` (layout-model.md 5.3).
    fn object(id: u64, kind: u32, name: Option<&str>, children: &[u64]) -> LayoutObject {
        LayoutObject {
            id,
            object_type: kind,
            name: name.map(str::to_owned),
            children: children.to_vec(),
            ..LayoutObject::default()
        }
    }

    fn layout(objects: Vec<LayoutObject>, root: &[u64]) -> Layout {
        let mut objects = objects;
        let parents: Vec<(u64, u64)> = objects
            .iter()
            .flat_map(|o| o.children.iter().map(move |c| (*c, o.id)))
            .collect();
        for (child, parent) in parents {
            if let Some(o) = objects.iter_mut().find(|o| o.id == child) {
                o.parent = Some(parent);
            }
        }
        Layout {
            objects: ObjectStore {
                root: root.to_vec(),
                objects,
                ..ObjectStore::default()
            },
            ..Layout::default()
        }
    }

    #[test]
    fn layout_object_names_bracket_panels_portals_and_button_bars() {
        // A button bar with a named segment, a named button, a tab control
        // whose panel holds a named field and an unnamed portal, a named
        // portal, a group holding a named field.
        let l = layout(
            vec![
                object(1, 18, None, &[2]),
                object(2, 10, Some("Seg"), &[]),
                object(3, 10, Some("Back"), &[]),
                object(4, 11, None, &[5]),
                object(5, 12, None, &[6, 7]),
                object(6, 1, Some("Company"), &[]),
                object(7, 9, None, &[8]),
                object(8, 1, None, &[]),
                object(9, 9, Some("Lines"), &[]),
                object(10, 8, None, &[11]),
                object(11, 1, Some("Inside"), &[]),
            ],
            &[1, 3, 4, 9, 10],
        );
        assert_eq!(
            layout_object_names(&l),
            "<\rSeg\r>\rBack\r<\rCompany\r<\r>\r>\rLines\r<\r>\rInside"
        );
    }

    #[test]
    fn a_group_after_the_last_name_leaves_an_empty_line() {
        // FileMaker Server's answer for the FMS sample's Task Details:
        // brackets, then groups with nothing named.
        let l = layout(
            vec![
                object(1, 9, None, &[2]),
                object(2, 8, None, &[3]),
                object(3, 3, None, &[]),
                object(4, 8, None, &[5]),
                object(5, 5, None, &[]),
            ],
            &[1, 4],
        );
        assert_eq!(layout_object_names(&l), "<\r>\r");
        assert_eq!(layout_object_names(&layout(Vec::new(), &[])), "");
    }

    #[test]
    fn a_text_fields_value_list_lists_each_line_once_in_order() {
        use om_calc::eval::Context;
        use om_calc::{Date, Locale, Value, ValueType};
        use om_scripts::specs::SpecField;

        use crate::host::{CalcSchema, FieldDef, FileCalc, MemorySource, RecordValues};
        use crate::meta::{ValueList, ValueSource};

        let mut schema = CalcSchema::new("f", Locale::en_us());
        schema.add_table(130, "T");
        schema.add_field(130, FieldDef::data(1, "Names", ValueType::Text));
        schema.add_field(130, FieldDef::data(2, "Sizes", ValueType::Number));
        schema.add_occurrence(7, "T", 130);
        let list = ValueList {
            id: 1,
            name: "Names".into(),
            source: ValueSource::Field {
                primary: SpecField {
                    occurrence: Some(7),
                    field: 1,
                    repetition: 1,
                },
                secondary: None,
                start: None,
            },
            options: 0,
            sort_locale: 0,
            separator: " ".into(),
        };
        let mut source = MemorySource::new();
        let number = |n: &str| {
            let parsed = |n| om_calc::Number::from_literal(n, '.').unwrap();
            Value::Number(
                n.strip_prefix('-')
                    .map_or_else(|| parsed(n), |n| parsed(n).neg()),
            )
        };
        for (id, text, size) in [
            (1, "home\rDelete", "10"),
            (2, "Info\rHome\rapple", "1.50"),
            (3, "", "2"),
            (4, "Résumé\rss\rＩｎｆｏ", "1.5"),
            (5, "resume\rß\rAPPLE", "-0"),
            (6, "", "0"),
        ] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(text));
            values.set(2, 1, number(size));
            source.put_record(130, id, values);
        }
        let today = Date::from_ymd(2026, 10, 29).unwrap();
        let file = FileCalc::new(&schema, &source, today);
        // Unique as `=` compares them, in any case, accent or width (the
        // first wins; "ß" is not "ss"), in the collation's order.
        assert_eq!(
            file.value_list_items(&list, &Context::new()).unwrap(),
            ["apple", "Delete", "home", "Info", "Résumé", "ss", "ß"]
        );
        // A number field's values: unique and in order as numbers (1.5 is
        // 1.50, 0 is -0; each shows as the first of them does).
        let sizes = ValueList {
            source: ValueSource::Field {
                primary: SpecField {
                    occurrence: Some(7),
                    field: 2,
                    repetition: 1,
                },
                secondary: None,
                start: None,
            },
            ..list.clone()
        };
        assert_eq!(
            file.value_list_items(&sizes, &Context::new()).unwrap(),
            ["0", "1.5", "2", "10"]
        );
        // Custom values from FileMaker's XML importer have LF between them.
        let custom = ValueList {
            source: ValueSource::Custom(vec!["Alpha\n-\nBeta".into(), "Gamma".into()]),
            ..list
        };
        assert_eq!(
            file.value_list_items(&custom, &Context::new()).unwrap(),
            ["Alpha", "-", "Beta", "Gamma"]
        );
    }

    /// A value list of an unstored calculation's values lists what the
    /// calculation gives on each record (#1058). While that reads data the
    /// window is still fetching (a global field's value, here) it is error
    /// 1224 with the need listed, not an empty list. Data that stays
    /// missing, or a calculation that is `?` on a record, fails the list
    /// rather than give `?` items.
    #[test]
    fn an_unstored_calculations_value_list_lists_each_records_value() {
        use std::cell::RefCell;
        use std::sync::Arc;

        use om_calc::eval::Context;
        use om_calc::{Date, Locale, Value, ValueType};
        use om_formula::Key;
        use om_scripts::specs::SpecField;

        use crate::host::{
            CachedSource, CalcData, CalcNeed, CalcSchema, CalcSource, FieldDef, FileCalc,
            RecordValues,
        };
        use crate::meta::{ValueList, ValueSource};

        let mut schema = CalcSchema::new("f", Locale::en_us());
        schema.add_table(130, "T");
        schema.add_field(130, FieldDef::data(1, "Names", ValueType::Text));
        schema.add_field(
            130,
            FieldDef::data(2, "Prefix", ValueType::Text).with_global(true),
        );
        schema.add_occurrence(7, "T", 130);
        let compile = |text: &str| {
            let mut ctx = om_formula::Context::new(schema.names());
            ctx.occurrence = Some(7);
            om_formula::Formula {
                context: Some(Key::from_id(7)),
                formatted: None,
                polish: om_formula::compile(text, &ctx).unwrap().expr,
            }
        };
        let initial = compile("Prefix & Left ( Names ; 1 )");
        let ratio = compile("If ( Names = \"Info\" ; 1 / 0 ; Names )");
        // An occurrence the file names but the schema does not have (a
        // data source's table that is not there).
        let gone = {
            let mut names = schema.names().clone();
            names.add_occurrence(9, "Gone", 130);
            let mut ctx = om_formula::Context::new(&names);
            ctx.occurrence = Some(7);
            om_formula::Formula {
                context: Some(Key::from_id(7)),
                formatted: None,
                polish: om_formula::compile("Gone::Names & \"h\"", &ctx)
                    .unwrap()
                    .expr,
            }
        };
        schema.add_field(
            130,
            FieldDef::calculation(3, "Initial", ValueType::Text, initial, false),
        );
        schema.add_field(
            130,
            FieldDef::calculation(4, "Ratio", ValueType::Text, ratio, false),
        );
        schema.add_field(
            130,
            FieldDef::calculation(5, "Gone", ValueType::Text, gone, false),
        );
        let list = ValueList {
            id: 1,
            name: "Initials".into(),
            source: ValueSource::Field {
                primary: SpecField {
                    occurrence: Some(7),
                    field: 3,
                    repetition: 1,
                },
                secondary: None,
                start: None,
            },
            options: 0,
            sort_locale: 0,
            separator: " ".into(),
        };
        let mut data = CalcData::default();
        data.tables.insert(130, Arc::from([1, 2, 3, 4]));
        for (id, names) in [(1, "home\rDelete"), (2, "Info"), (3, ""), (4, "info")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(names));
            data.records.insert((130, id), Some(Arc::new(values)));
        }
        let cache = RefCell::new(data);
        let today = Date::from_ymd(2026, 11, 17).unwrap();
        // What stays missing: the calculation reads `?` for the global
        // ("?h"), which is no item.
        let source = CachedSource::new(&cache);
        let file = FileCalc::new(&schema, &source, today);
        let missing = file.value_list_items(&list, &Context::new()).unwrap_err();
        assert_eq!(missing.code(), 1224);
        assert_eq!(source.pending().reads, 0);

        // As a window reads: what is missing is fetched, then read again.
        let failed = std::collections::BTreeSet::new();

        // The global is not read yet: the list waits for it.
        let source = CachedSource::new(&cache).retrying(&failed);
        let file = FileCalc::new(&schema, &source, today);
        let pending = file.value_list_items(&list, &Context::new()).unwrap_err();
        assert_eq!(pending.code(), 1224);
        assert!(source.pending().reads > 0);
        assert_eq!(source.missing(), [CalcNeed::Globals { table: 130 }]);

        // Once it is, each record's value: "-" and its first letter, the
        // records whose calculation is empty left out, one item per value.
        let mut globals = RecordValues::default();
        globals.set(2, 1, Value::text("-"));
        cache.borrow_mut().globals.insert(130, Arc::new(globals));
        let source = CachedSource::new(&cache).retrying(&failed);
        let file = FileCalc::new(&schema, &source, today);
        assert_eq!(
            file.value_list_items(&list, &Context::new()).unwrap(),
            ["-", "-h", "-I"]
        );
        assert!(!source.has_missing());

        // `1 / 0` on one record: `?` there, which fails the list. So does
        // a field the schema does not have ("?h" otherwise).
        let of_field = |field: u64| ValueList {
            source: ValueSource::Field {
                primary: SpecField {
                    occurrence: Some(7),
                    field,
                    repetition: 1,
                },
                secondary: None,
                start: None,
            },
            ..list.clone()
        };
        assert!(file
            .value_list_items(&of_field(4), &Context::new())
            .is_err());
        let gone = file.value_list_items(&of_field(5), &Context::new());
        assert_eq!(gone.unwrap_err().code(), 102);
    }

    /// A value list of a field with many distinct values is built in
    /// linear time: each value is keyed once, not compared with every item
    /// before it (a production solution's 75,000-record table froze the
    /// window for hours in a debug build). A first number of another
    /// precision leaves the others keyed.
    #[test]
    fn a_field_value_list_of_many_distinct_values_is_built_in_linear_time() {
        use om_calc::eval::Context;
        use om_calc::{Date, Locale, Value, ValueType};
        use om_scripts::specs::SpecField;

        use crate::host::{CalcSchema, FieldDef, FileCalc, MemorySource, RecordValues};
        use crate::meta::{ValueList, ValueSource};

        const COUNT: u64 = 50_000;
        let mut schema = CalcSchema::new("f", Locale::en_us());
        schema.add_table(130, "T");
        schema.add_field(130, FieldDef::data(1, "Code", ValueType::Text));
        schema.add_field(130, FieldDef::data(2, "Size", ValueType::Number));
        schema.add_occurrence(7, "T", 130);
        let list = ValueList {
            id: 1,
            name: "Codes".into(),
            source: ValueSource::Field {
                primary: SpecField {
                    occurrence: Some(7),
                    field: 1,
                    repetition: 1,
                },
                secondary: None,
                start: None,
            },
            options: 0,
            sort_locale: 0,
            separator: " ".into(),
        };
        let odd = om_calc::Number::from_literal("0.00000000000000001", '.').unwrap();
        assert_ne!(odd.precision(), om_calc::Number::from_i64(2).precision());
        let mut source = MemorySource::new();
        for id in 1..=COUNT {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(format!("Code {:05}", COUNT - id)));
            let size = match id {
                1 => odd.clone(),
                _ => om_calc::Number::from_i64(id as i64),
            };
            values.set(2, 1, Value::Number(size));
            source.put_record(130, id, values);
        }
        let file = FileCalc::new(&schema, &source, Date::from_ymd(2026, 11, 14).unwrap());
        let entries = file
            .value_list_entries(&list, &Context::new(), usize::MAX)
            .unwrap()
            .unwrap();
        assert_eq!(entries.len(), COUNT as usize);
        assert_eq!(entries[0].value, "Code 00000");
        assert_eq!(entries[COUNT as usize - 1].value, "Code 49999");
        // Past the limit the build stops: no list, not its first items.
        assert!(file
            .value_list_entries(&list, &Context::new(), COUNT as usize - 1)
            .unwrap()
            .is_none());
        let sizes = ValueList {
            source: ValueSource::Field {
                primary: SpecField {
                    occurrence: Some(7),
                    field: 2,
                    repetition: 1,
                },
                secondary: None,
                start: None,
            },
            ..list
        };
        let entries = file
            .value_list_entries(&sizes, &Context::new(), usize::MAX)
            .unwrap()
            .unwrap();
        assert_eq!(entries.len(), COUNT as usize);
    }

    /// A list mixing text and numbers is not totally ordered (2 < 10 as
    /// numbers, "10" < "2" as text), and `sort_by` may panic on it: the
    /// list's sort gives every item back instead, and with a total order
    /// what `sort_by` gives.
    #[test]
    fn a_value_list_sort_survives_an_order_that_is_not_total() {
        use super::stable_sort_by;
        use std::cmp::Ordering;

        let cyclic = |a: &u32, b: &u32| match (a % 3, b % 3) {
            (x, y) if x == y => a.cmp(b),
            (0, 1) | (1, 2) | (2, 0) => Ordering::Less,
            _ => Ordering::Greater,
        };
        let items: Vec<u32> = (0..200).rev().collect();
        let mut sorted = stable_sort_by(items.clone(), cyclic);
        sorted.sort_unstable();
        assert_eq!(sorted, (0..200).collect::<Vec<_>>());
        let by_tens = |a: &u32, b: &u32| (a / 10).cmp(&(b / 10));
        let mut expected = items.clone();
        expected.sort_by(by_tens);
        assert_eq!(stable_sort_by(items, by_tens), expected);
    }

    #[test]
    fn a_second_field_display_labels_items_that_store_the_first_field() {
        use om_calc::eval::Context;
        use om_calc::{Date, Locale, Value, ValueType};
        use om_scripts::specs::SpecField;

        use crate::host::{CalcSchema, FieldDef, FileCalc, MemorySource, RecordValues};
        use crate::meta::{value_list_option, ValueList, ValueSource};

        let mut schema = CalcSchema::new("f", Locale::en_us());
        schema.add_table(130, "Codes");
        schema.add_field(130, FieldDef::data(1, "Id", ValueType::Number));
        schema.add_field(130, FieldDef::data(2, "Name", ValueType::Text));
        schema.add_occurrence(7, "Codes", 130);
        let field = |field| SpecField {
            occurrence: Some(7),
            field,
            repetition: 1,
        };
        let list = ValueList {
            id: 1,
            name: "Codes".into(),
            source: ValueSource::Field {
                primary: field(1),
                secondary: Some(field(2)),
                start: None,
            },
            options: value_list_option::SECOND_ONLY,
            sort_locale: 0,
            separator: " ".into(),
        };
        let mut source = MemorySource::new();
        for (id, name) in [(3, "Gamma"), (1, "Alpha"), (2, "Beta")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::number(id));
            values.set(2, 1, Value::text(name));
            source.put_record(130, id as u64, values);
        }
        let today = Date::from_ymd(2026, 11, 2).unwrap();
        let file = FileCalc::new(&schema, &source, today);
        let entries = |list: &ValueList| -> Vec<(String, String)> {
            file.value_list_entries(list, &Context::new(), usize::MAX)
                .unwrap()
                .into_iter()
                .flatten()
                .map(|e| (e.value, e.label))
                .collect()
        };
        let pair = |value: &str, label: &str| (value.to_owned(), label.to_owned());
        // The set shows the names and stores the ids, in the first
        // field's order; ValueListItems still answers the ids.
        assert_eq!(
            entries(&list),
            [pair("1", "Alpha"), pair("2", "Beta"), pair("3", "Gamma")]
        );
        assert_eq!(
            file.value_list_items(&list, &Context::new()).unwrap(),
            ["1", "2", "3"]
        );
        // Without "show values only from second field" the item is its
        // stored value.
        let both = ValueList {
            options: 0,
            ..list.clone()
        };
        assert_eq!(
            entries(&both),
            [pair("1", "1"), pair("2", "2"), pair("3", "3")]
        );
        // Sorted by the second field, labels follow their own values.
        let sorted = ValueList {
            options: value_list_option::SECOND_ONLY | value_list_option::SORT_BY_SECOND,
            ..list
        };
        assert_eq!(
            entries(&sorted),
            [pair("1", "Alpha"), pair("2", "Beta"), pair("3", "Gamma")]
        );
    }
}
