//! Manage Database's steps the Build measure leaves out (criterion 7;
//! #430, #431, #432, #433): tables, a field of each kind, Options for
//! Field and relationships. Each goes through the operations Manage
//! Database calls (`begin_schema_edit`, `apply_schema_change`,
//! `commit_schema_changes`) on a copy of each file; the file is then
//! closed, opened again and the step read back from its schema
//! (`database_schema`). Every step makes in the file what it needs (a
//! table, fields, occurrences), so each applies to every file: the starter
//! solutions and a file File > Create New made (`om_data::new_file`).
//!
//! What the dialogs do around the operations (their controls, the graph's
//! drawing) is not exercised, and nothing here checks that FileMaker Pro
//! accepts the result: `md_step_files` writes one blank file per step for
//! that, with a README.
//!
//! ```text
//! OM_SCORECARD_FILES=a.fmp12:b.fmp12 OM_SCORECARD_OUT=<folder> \
//!   cargo test -p om-gui --lib scorecard::md_steps::md_steps_scorecard -- --ignored --exact
//! OM_MD_STEP_FILES=<folder> \
//!   cargo test -p om-gui --lib scorecard::md_steps::md_step_files -- --ignored --exact
//! ```

use std::path::{Path, PathBuf};

use om_data::calc_field::{CalcStorage, CalculationField};
use om_data::field_options::{
    AutoCalculation, AutoEnterValue, Indexing, OptionFormula, Serial, ValidationCalculation,
};
use om_data::meta::{SummaryDef, SummaryKind};
use om_data::specs::{SortEntry, SortSpec, SpecField};
use om_data::{DataProvider, DatabaseSchema, FieldKind, SchemaChange, SchemaFieldKind};
use om_data::{RelationshipMatch, RelationshipSide};
use serde::Serialize;

use super::build::{open, panic_text};

/// What a step made, for its check.
#[derive(Clone, Debug, Default)]
struct Made {
    table: u64,
    field: u64,
    /// A relationship, an occurrence or a second table.
    other: u64,
}

struct Step {
    section: &'static str,
    step: &'static str,
    act: fn(&mut dyn DataProvider) -> Result<Made, String>,
    check: fn(&DatabaseSchema, &Made) -> Result<String, String>,
}

/// One step on one file.
#[derive(Serialize)]
struct Outcome {
    file: String,
    section: &'static str,
    step: &'static str,
    done: bool,
    detail: String,
}

/// `change` inside one Manage Database session, committed; cancelled when
/// it fails.
fn edit<T>(
    p: &mut dyn DataProvider,
    change: impl FnOnce(&mut dyn DataProvider) -> Result<T, String>,
) -> Result<T, String> {
    p.begin_schema_edit()
        .map_err(|e| format!("Manage Database: {}", e.message))?;
    match change(p) {
        Ok(made) => p
            .commit_schema_changes()
            .map(|_| made)
            .map_err(|e| format!("OK: {}", e.message)),
        Err(e) => {
            let _ = p.cancel_schema_changes();
            Err(e)
        }
    }
}

fn apply(p: &mut dyn DataProvider, change: SchemaChange) -> Result<DatabaseSchema, String> {
    p.apply_schema_change(change).map_err(|e| e.message)
}

/// The file's first table and an occurrence of it.
fn first_table(p: &mut dyn DataProvider) -> Result<(u64, u64), String> {
    let schema = p.database_schema().map_err(|e| e.message)?;
    schema
        .tables
        .iter()
        .find_map(|t| {
            let occurrence = schema
                .occurrences
                .iter()
                .find(|o| o.table_id == Some(t.id))?;
            Some((t.id, occurrence.id))
        })
        .ok_or_else(|| "no table with an occurrence".into())
}

fn field_id(schema: &DatabaseSchema, table: u64, name: &str) -> Result<u64, String> {
    schema
        .table(table)
        .and_then(|t| t.fields.iter().find(|f| f.name == name))
        .map(|f| f.id)
        .ok_or_else(|| format!("no field “{name}”"))
}

fn field<'s>(schema: &'s DatabaseSchema, m: &Made) -> Result<&'s om_data::SchemaField, String> {
    schema
        .table(m.table)
        .and_then(|t| t.fields.iter().find(|f| f.id == m.field))
        .ok_or_else(|| format!("field {} of table {} is gone", m.field, m.table))
}

/// A new normal field `name` of `kind` in the first table.
fn create_field(p: &mut dyn DataProvider, name: &str, kind: FieldKind) -> Result<Made, String> {
    let (table, _) = first_table(p)?;
    let schema = edit(p, |p| {
        apply(
            p,
            SchemaChange::CreateField {
                table_id: table,
                name: name.into(),
                data_type: kind,
                comment: String::new(),
            },
        )
    })?;
    Ok(Made {
        table,
        field: field_id(&schema, table, name)?,
        other: 0,
    })
}

/// A new text field `name` of the first table given options by `set`.
fn with_options(
    p: &mut dyn DataProvider,
    name: &str,
    kind: FieldKind,
    set: impl FnOnce(&mut om_data::field_options::FieldOptions, u64),
) -> Result<Made, String> {
    let (table, occurrence) = first_table(p)?;
    let field = edit(p, |p| {
        let schema = apply(
            p,
            SchemaChange::CreateField {
                table_id: table,
                name: name.into(),
                data_type: kind,
                comment: String::new(),
            },
        )?;
        let field = schema
            .table(table)
            .and_then(|t| t.fields.iter().find(|f| f.name == name))
            .cloned()
            .ok_or("the new field is not in the schema")?;
        let mut options = field.options.ok_or("the new field has no options")?;
        set(&mut options, occurrence);
        apply(
            p,
            SchemaChange::SetFieldOptions {
                table_id: table,
                field_id: field.id,
                options: Box::new(options),
            },
        )?;
        Ok(field.id)
    })?;
    Ok(Made {
        table,
        field,
        other: 0,
    })
}

fn options(
    schema: &DatabaseSchema,
    m: &Made,
) -> Result<om_data::field_options::FieldOptions, String> {
    field(schema, m)?
        .options
        .clone()
        .ok_or_else(|| "the field has no options".into())
}

fn kind_check(expected: FieldKind) -> impl Fn(&DatabaseSchema, &Made) -> Result<String, String> {
    move |schema, m| {
        let f = field(schema, m)?;
        if f.kind == SchemaFieldKind::Normal && f.data_type == expected {
            Ok(format!("{} ({:?})", f.name, f.data_type))
        } else {
            Err(format!("{} reads {:?} {:?}", f.name, f.kind, f.data_type))
        }
    }
}

/// A relationship [`relate`] made.
struct Related {
    /// The first table, the left match field, the relationship.
    made: Made,
    /// The second match field on each side.
    left_second: u64,
    right_second: u64,
    /// The occurrences.
    left: u64,
    right: u64,
}

/// A new table `MD Related <tag>`, and a new occurrence `MD Left <tag>`
/// of the first table related to it on a new text field `MD Match <tag>`
/// of each; both have `MD Second Match <tag>` too.
fn relate(p: &mut dyn DataProvider, tag: &str) -> Result<Related, String> {
    let (table, _) = first_table(p)?;
    let (related_name, left_name) = (format!("MD Related {tag}"), format!("MD Left {tag}"));
    let (first, second) = (format!("MD Match {tag}"), format!("MD Second Match {tag}"));
    edit(p, |p| {
        let schema = apply(
            p,
            SchemaChange::CreateTable {
                name: related_name.clone(),
                comment: String::new(),
            },
        )?;
        let related = schema
            .tables
            .iter()
            .find(|t| t.name == related_name)
            .map(|t| t.id)
            .ok_or("the new table is not in the schema")?;
        for (table_id, name) in [
            (table, &first),
            (table, &second),
            (related, &first),
            (related, &second),
        ] {
            apply(
                p,
                SchemaChange::CreateField {
                    table_id,
                    name: name.clone(),
                    data_type: FieldKind::Text,
                    comment: String::new(),
                },
            )?;
        }
        // The draft as edited so far: what the last change returns.
        let schema = apply(
            p,
            SchemaChange::CreateOccurrence {
                table_id: table,
                name: left_name.clone(),
            },
        )?;
        let left = schema
            .occurrences
            .iter()
            .find(|o| o.name == left_name)
            .map(|o| o.id)
            .ok_or_else(|| format!("no occurrence “{left_name}”"))?;
        let right = schema
            .occurrences
            .iter()
            .find(|o| o.table_id == Some(related))
            .map(|o| o.id)
            .ok_or("the new table has no occurrence")?;
        let left_first = field_id(&schema, table, &first)?;
        let right_first = field_id(&schema, related, &first)?;
        let left_second = field_id(&schema, table, &second)?;
        let right_second = field_id(&schema, related, &second)?;
        let schema = apply(
            p,
            SchemaChange::CreateRelationship {
                left_occurrence: left,
                right_occurrence: right,
                matches: vec![RelationshipMatch {
                    left_field: left_first,
                    operator: 0,
                    right_field: right_first,
                }],
                left: RelationshipSide::default(),
                right: RelationshipSide::default(),
            },
        )?;
        let relationship = schema
            .relationships
            .iter()
            .find(|r| r.left_occurrence == left && r.right_occurrence == right)
            .ok_or("the new relationship is not in the schema")?;
        Ok(Related {
            made: Made {
                table,
                field: left_first,
                other: relationship.id,
            },
            left_second,
            right_second,
            left,
            right,
        })
    })
}

fn relationship<'s>(
    schema: &'s DatabaseSchema,
    m: &Made,
) -> Result<&'s om_data::SchemaRelationship, String> {
    schema
        .relationships
        .iter()
        .find(|r| r.id == m.other)
        .ok_or_else(|| format!("relationship {} is gone", m.other))
}

fn steps() -> Vec<Step> {
    vec![
        Step {
            section: "Tables",
            step: "Create a table",
            act: |p| {
                let schema = edit(p, |p| {
                    apply(
                        p,
                        SchemaChange::CreateTable {
                            name: "MD Table".into(),
                            comment: String::new(),
                        },
                    )
                })?;
                let table = schema
                    .tables
                    .iter()
                    .find(|t| t.name == "MD Table")
                    .ok_or("the new table is not in the schema")?;
                Ok(Made {
                    table: table.id,
                    ..Made::default()
                })
            },
            check: |schema, m| {
                let table = schema.table(m.table).ok_or("the table is gone")?;
                let shown = schema
                    .occurrences
                    .iter()
                    .any(|o| o.table_id == Some(m.table));
                if table.name == "MD Table" && shown {
                    Ok("MD Table, with an occurrence".into())
                } else {
                    Err(format!("“{}”, an occurrence: {shown}", table.name))
                }
            },
        },
        Step {
            section: "Tables",
            step: "Rename a table",
            act: |p| {
                let table = edit(p, |p| {
                    let schema = apply(
                        p,
                        SchemaChange::CreateTable {
                            name: "MD Rename".into(),
                            comment: String::new(),
                        },
                    )?;
                    let id = schema
                        .tables
                        .iter()
                        .find(|t| t.name == "MD Rename")
                        .map(|t| t.id)
                        .ok_or("the new table is not in the schema")?;
                    Ok(id)
                })?;
                edit(p, |p| {
                    apply(
                        p,
                        SchemaChange::ChangeTable {
                            table_id: table,
                            name: "MD Renamed".into(),
                            comment: String::new(),
                        },
                    )
                })?;
                Ok(Made {
                    table,
                    ..Made::default()
                })
            },
            check: |schema, m| {
                let table = schema.table(m.table).ok_or("the table is gone")?;
                let occurrence = schema
                    .occurrences
                    .iter()
                    .any(|o| o.table_id == Some(m.table) && o.name == "MD Renamed");
                match (table.name.as_str(), occurrence) {
                    ("MD Renamed", true) => Ok("MD Renamed, its occurrence too".into()),
                    (name, occurrence) => {
                        Err(format!("“{name}”, occurrence renamed: {occurrence}"))
                    }
                }
            },
        },
        Step {
            section: "Tables",
            step: "Delete a table",
            act: |p| {
                let table = edit(p, |p| {
                    let schema = apply(
                        p,
                        SchemaChange::CreateTable {
                            name: "MD Delete".into(),
                            comment: String::new(),
                        },
                    )?;
                    schema
                        .tables
                        .iter()
                        .find(|t| t.name == "MD Delete")
                        .map(|t| t.id)
                        .ok_or_else(|| "the new table is not in the schema".to_string())
                })?;
                edit(p, |p| {
                    apply(
                        p,
                        SchemaChange::DeleteTable {
                            table_id: table,
                            remove_occurrences: true,
                        },
                    )
                })?;
                Ok(Made {
                    table,
                    ..Made::default()
                })
            },
            check: |schema, m| {
                let table = schema.table(m.table).is_some();
                let occurrence = schema
                    .occurrences
                    .iter()
                    .any(|o| o.table_id == Some(m.table));
                if !table && !occurrence {
                    Ok("gone, with its occurrence".into())
                } else {
                    Err(format!(
                        "table there: {table}, occurrence there: {occurrence}"
                    ))
                }
            },
        },
        Step {
            section: "Fields",
            step: "Add a text field",
            act: |p| create_field(p, "MD Text", FieldKind::Text),
            check: |s, m| kind_check(FieldKind::Text)(s, m),
        },
        Step {
            section: "Fields",
            step: "Add a number field",
            act: |p| create_field(p, "MD Number", FieldKind::Number),
            check: |s, m| kind_check(FieldKind::Number)(s, m),
        },
        Step {
            section: "Fields",
            step: "Add a date field",
            act: |p| create_field(p, "MD Date", FieldKind::Date),
            check: |s, m| kind_check(FieldKind::Date)(s, m),
        },
        Step {
            section: "Fields",
            step: "Add a time field",
            act: |p| create_field(p, "MD Time", FieldKind::Time),
            check: |s, m| kind_check(FieldKind::Time)(s, m),
        },
        Step {
            section: "Fields",
            step: "Add a timestamp field",
            act: |p| create_field(p, "MD Timestamp", FieldKind::Timestamp),
            check: |s, m| kind_check(FieldKind::Timestamp)(s, m),
        },
        Step {
            section: "Fields",
            step: "Add a container field",
            act: |p| create_field(p, "MD Container", FieldKind::Container),
            check: |s, m| kind_check(FieldKind::Container)(s, m),
        },
        Step {
            section: "Fields",
            step: "Add a stored calculation",
            act: |p| calculation(p, "MD Stored Calculation", false),
            check: |s, m| calculation_check(s, m, false),
        },
        Step {
            section: "Fields",
            step: "Add an unstored calculation",
            act: |p| calculation(p, "MD Unstored Calculation", true),
            check: |s, m| calculation_check(s, m, true),
        },
        Step {
            section: "Fields",
            step: "Add a summary field (Count of a number field)",
            act: |p| {
                let (table, _) = first_table(p)?;
                let field = edit(p, |p| {
                    let schema = apply(
                        p,
                        SchemaChange::CreateField {
                            table_id: table,
                            name: "MD Counted".into(),
                            data_type: FieldKind::Number,
                            comment: String::new(),
                        },
                    )?;
                    let counted = field_id(&schema, table, "MD Counted")?;
                    let schema = apply(
                        p,
                        SchemaChange::CreateSummaryField {
                            table_id: table,
                            name: "MD Summary".into(),
                            comment: String::new(),
                            summary: SummaryDef::new(SummaryKind::Count, counted),
                        },
                    )?;
                    field_id(&schema, table, "MD Summary")
                })?;
                Ok(Made {
                    table,
                    field,
                    other: 0,
                })
            },
            check: |s, m| {
                let f = field(s, m)?;
                match (&f.kind, &f.summary) {
                    (SchemaFieldKind::Summary, Some(summary))
                        if summary.kind == SummaryKind::Count =>
                    {
                        Ok(format!("{}: Count of", f.name))
                    }
                    (kind, summary) => Err(format!("{kind:?} {summary:?}")),
                }
            },
        },
        Step {
            section: "Fields",
            step: "Add a global field",
            act: |p| {
                with_options(p, "MD Global", FieldKind::Text, |o, _| {
                    o.storage.global = true
                })
            },
            check: |s, m| {
                if options(s, m)?.storage.global {
                    Ok("global storage".into())
                } else {
                    Err("not global".into())
                }
            },
        },
        Step {
            section: "Fields",
            step: "Rename a field",
            act: |p| {
                let made = create_field(p, "MD Rename Field", FieldKind::Text)?;
                edit(p, |p| {
                    apply(
                        p,
                        SchemaChange::ChangeField {
                            table_id: made.table,
                            field_id: made.field,
                            name: "MD Renamed Field".into(),
                            data_type: FieldKind::Text,
                            comment: String::new(),
                        },
                    )
                })?;
                Ok(made)
            },
            check: |s, m| {
                let f = field(s, m)?;
                if f.name == "MD Renamed Field" {
                    Ok("MD Renamed Field".into())
                } else {
                    Err(format!("“{}”", f.name))
                }
            },
        },
        Step {
            section: "Fields",
            step: "Change a field's type (text to number)",
            act: |p| {
                let made = create_field(p, "MD Type", FieldKind::Text)?;
                edit(p, |p| {
                    apply(
                        p,
                        SchemaChange::ChangeField {
                            table_id: made.table,
                            field_id: made.field,
                            name: "MD Type".into(),
                            data_type: FieldKind::Number,
                            comment: String::new(),
                        },
                    )
                })?;
                Ok(made)
            },
            check: |s, m| kind_check(FieldKind::Number)(s, m),
        },
        Step {
            section: "Fields",
            step: "Delete a field",
            act: |p| {
                let made = create_field(p, "MD Delete Field", FieldKind::Text)?;
                edit(p, |p| {
                    apply(
                        p,
                        SchemaChange::DeleteField {
                            table_id: made.table,
                            field_id: made.field,
                        },
                    )
                })?;
                Ok(made)
            },
            check: |s, m| match field(s, m) {
                Err(_) => Ok("gone".into()),
                Ok(f) => Err(format!("“{}” is still there", f.name)),
            },
        },
        Step {
            section: "Options for Field",
            step: "Auto-enter a serial number",
            act: |p| {
                with_options(p, "MD Serial", FieldKind::Number, |o, _| {
                    o.auto_enter.value = AutoEnterValue::Serial(Serial::default());
                })
            },
            check: |s, m| match options(s, m)?.auto_enter.value {
                AutoEnterValue::Serial(serial) => Ok(format!(
                    "serial, next {}, increment {}",
                    serial.next, serial.increment
                )),
                other => Err(format!("{other:?}")),
            },
        },
        Step {
            section: "Options for Field",
            step: "Auto-enter a calculated value",
            act: |p| {
                with_options(p, "MD Auto Calculated", FieldKind::Text, |o, occurrence| {
                    o.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation::new(
                        OptionFormula::new("\"made\"", occurrence),
                    ));
                })
            },
            check: |s, m| match options(s, m)?.auto_enter.value {
                AutoEnterValue::Calculated(c) if c.formula.text.as_deref() == Some("\"made\"") => {
                    Ok("calculated value \"made\"".into())
                }
                other => Err(format!("{other:?}")),
            },
        },
        Step {
            section: "Options for Field",
            step: "Auto-enter a looked-up value",
            act: |p| {
                let related = relate(p, "Lookup")?;
                let made = related.made.clone();
                let field = edit(p, |p| {
                    let schema = apply(
                        p,
                        SchemaChange::CreateField {
                            table_id: made.table,
                            name: "MD Looked Up".into(),
                            data_type: FieldKind::Text,
                            comment: String::new(),
                        },
                    )?;
                    let field = schema
                        .table(made.table)
                        .and_then(|t| t.fields.iter().find(|f| f.name == "MD Looked Up"))
                        .cloned()
                        .ok_or("the new field is not in the schema")?;
                    let mut options = field.options.ok_or("the new field has no options")?;
                    options.auto_enter.value =
                        AutoEnterValue::LookedUp(Some(om_data::field_options::Lookup {
                            from: related.left,
                            to: related.right,
                            copied: related.right_second,
                            copy_empty: false,
                            no_match: om_data::field_options::NoMatch::DoNotCopy,
                        }));
                    apply(
                        p,
                        SchemaChange::SetFieldOptions {
                            table_id: made.table,
                            field_id: field.id,
                            options: Box::new(options),
                        },
                    )?;
                    Ok(field.id)
                })?;
                Ok(Made { field, ..made })
            },
            check: |s, m| match options(s, m)?.auto_enter.value {
                AutoEnterValue::LookedUp(Some(lookup)) => Ok(format!(
                    "looks up field {} through occurrence {}",
                    lookup.copied, lookup.to
                )),
                other => Err(format!("{other:?}")),
            },
        },
        Step {
            section: "Options for Field",
            step: "Validation: not empty",
            act: |p| {
                with_options(p, "MD Required", FieldKind::Text, |o, _| {
                    o.validation.not_empty = true
                })
            },
            check: |s, m| {
                if options(s, m)?.validation.not_empty {
                    Ok("not empty".into())
                } else {
                    Err("not set".into())
                }
            },
        },
        Step {
            section: "Options for Field",
            step: "Validation: unique",
            act: |p| {
                with_options(p, "MD Unique", FieldKind::Text, |o, _| {
                    o.validation.unique = true
                })
            },
            check: |s, m| {
                if options(s, m)?.validation.unique {
                    Ok("unique".into())
                } else {
                    Err("not set".into())
                }
            },
        },
        Step {
            section: "Options for Field",
            step: "Validation: by calculation",
            act: |p| {
                with_options(p, "MD Validated", FieldKind::Text, |o, occurrence| {
                    o.validation.calculation = Some(ValidationCalculation {
                        formula: OptionFormula::new("Length ( Self ) < 10", occurrence),
                        only_if_modified: false,
                    });
                })
            },
            check: |s, m| match options(s, m)?.validation.calculation {
                Some(c) if c.formula.text.as_deref() == Some("Length ( Self ) < 10") => {
                    Ok("Length ( Self ) < 10".into())
                }
                other => Err(format!("{other:?}")),
            },
        },
        Step {
            section: "Options for Field",
            step: "Storage: indexing All",
            act: |p| {
                with_options(p, "MD Indexed", FieldKind::Text, |o, _| {
                    o.storage.indexing = Indexing::All;
                })
            },
            check: |s, m| match options(s, m)?.storage.indexing {
                Indexing::All => Ok("indexing All".into()),
                other => Err(format!("{other:?}")),
            },
        },
        Step {
            section: "Relationships",
            step: "Add a table occurrence",
            act: |p| {
                let (table, _) = first_table(p)?;
                let schema = edit(p, |p| {
                    apply(
                        p,
                        SchemaChange::CreateOccurrence {
                            table_id: table,
                            name: "MD Occurrence".into(),
                        },
                    )
                })?;
                let occurrence = schema
                    .occurrences
                    .iter()
                    .find(|o| o.name == "MD Occurrence")
                    .ok_or("the new occurrence is not in the schema")?;
                Ok(Made {
                    table,
                    field: 0,
                    other: occurrence.id,
                })
            },
            check: |s, m| match s.occurrences.iter().find(|o| o.id == m.other) {
                Some(o) if o.table_id == Some(m.table) && o.name == "MD Occurrence" => {
                    Ok("MD Occurrence".into())
                }
                other => Err(format!("{other:?}")),
            },
        },
        Step {
            section: "Relationships",
            step: "Add a relationship in the graph",
            act: |p| relate(p, "Add").map(|r| r.made),
            check: |s, m| {
                let r = relationship(s, m)?;
                if r.matches.len() == 1 && r.matches[0].left_field == m.field {
                    Ok("MD Left Add::MD Match Add = MD Related Add::MD Match Add".into())
                } else {
                    Err(format!("{:?}", r.matches))
                }
            },
        },
        Step {
            section: "Relationships",
            step: "Edit it: a second predicate, a sort, allow creation, delete related records",
            act: |p| {
                let related = relate(p, "Edit")?;
                let made = related.made.clone();
                edit(p, |p| {
                    let schema = p.database_schema().map_err(|e| e.message)?;
                    let r = schema
                        .relationships
                        .iter()
                        .find(|r| r.id == made.other)
                        .ok_or("the relationship is gone")?;
                    let mut matches: Vec<RelationshipMatch> = r
                        .matches
                        .iter()
                        .map(|m| RelationshipMatch {
                            left_field: m.left_field,
                            operator: m.operator,
                            right_field: m.right_field,
                        })
                        .collect();
                    matches.push(RelationshipMatch {
                        left_field: related.left_second,
                        operator: 0,
                        right_field: related.right_second,
                    });
                    let right_side = RelationshipSide {
                        allow_creation: true,
                        delete_related: true,
                        sort: true,
                        order: Some(SortSpec {
                            keep_sorted: true,
                            blanks_last: false,
                            fields: vec![SortEntry {
                                field: SpecField {
                                    occurrence: Some(related.right),
                                    field: related.right_second,
                                    repetition: 1,
                                },
                                summary: None,
                                value_list: None,
                                descending: false,
                                locale: 0,
                            }],
                        }),
                    };
                    apply(
                        p,
                        SchemaChange::ChangeRelationship {
                            relationship_id: made.other,
                            matches,
                            left: r.left(),
                            right: right_side,
                        },
                    )
                })?;
                Ok(made)
            },
            check: |s, m| {
                let r = relationship(s, m)?;
                let right = RelationshipSide::from_options(r.options, true);
                let sorted = r.orders[1].as_ref().is_some_and(|o| o.fields.len() == 1);
                if r.matches.len() == 2
                    && right.allow_creation
                    && right.delete_related
                    && right.sort
                    && sorted
                {
                    Ok("two predicates; the related side sorted, creation allowed, related records deleted".into())
                } else {
                    Err(format!(
                        "{} predicates, right side {right:?}, sort order kept: {sorted}",
                        r.matches.len()
                    ))
                }
            },
        },
        Step {
            section: "Relationships",
            step: "Delete a relationship",
            act: |p| {
                let made = relate(p, "Delete")?.made;
                edit(p, |p| {
                    apply(
                        p,
                        SchemaChange::DeleteRelationship {
                            relationship_id: made.other,
                        },
                    )
                })?;
                Ok(made)
            },
            check: |s, m| match relationship(s, m) {
                Err(_) => Ok("gone".into()),
                Ok(r) => Err(format!("relationship {} is still there", r.id)),
            },
        },
    ]
}

/// A new calculation field `1 + 1`, a number, stored or not.
fn calculation(p: &mut dyn DataProvider, name: &str, unstored: bool) -> Result<Made, String> {
    let (table, occurrence) = first_table(p)?;
    let field = edit(p, |p| {
        let schema = apply(
            p,
            SchemaChange::CreateCalculationField {
                table_id: table,
                name: name.into(),
                comment: String::new(),
                calculation: CalculationField {
                    formula: "1 + 1".into(),
                    context: occurrence,
                    result: FieldKind::Number,
                    repetitions: 1,
                    skip_when_empty: true,
                    storage: CalcStorage {
                        unstored,
                        ..CalcStorage::default()
                    },
                },
            },
        )?;
        field_id(&schema, table, name)
    })?;
    Ok(Made {
        table,
        field,
        other: 0,
    })
}

fn calculation_check(s: &DatabaseSchema, m: &Made, unstored: bool) -> Result<String, String> {
    let f = field(s, m)?;
    match (&f.kind, &f.calculation) {
        (SchemaFieldKind::Calculation, Some(c))
            if c.formula.trim() == "1 + 1" && c.storage.unstored == unstored =>
        {
            Ok(format!(
                "{}: 1 + 1, {}",
                f.name,
                if unstored { "unstored" } else { "stored" }
            ))
        }
        (kind, calculation) => Err(format!("{kind:?} {calculation:?}")),
    }
}

/// `step` on the file at `copy`: done, then the file closed, opened again
/// and the step read back.
fn run(copy: &Path, step: &Step) -> (bool, String) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut p = open(copy).map_err(|e| format!("opening: {e}"))?;
        let made = (step.act)(&mut *p);
        p.close();
        drop(p);
        let made = made?;
        let mut p = open(copy).map_err(|e| format!("reopening: {e}"))?;
        let schema = p.database_schema().map_err(|e| e.message);
        p.close();
        (step.check)(&schema?, &made)
    }))
    .map_err(|p| format!("panicked: {}", panic_text(p)))
    .and_then(|r| r);
    match result {
        Ok(detail) => (true, detail),
        Err(detail) => (false, detail),
    }
}

/// The table, ranked by section: each step, the files it held in, the
/// first file's failure.
fn summary(outcomes: &[Outcome], files: usize) -> String {
    let mut md = String::from("| Section | Step | Files | First failure |\n|---|---|---:|---|\n");
    for step in steps() {
        let of: Vec<&Outcome> = outcomes
            .iter()
            .filter(|o| o.section == step.section && o.step == step.step)
            .collect();
        let done = of.iter().filter(|o| o.done).count();
        let failure = of
            .iter()
            .find(|o| !o.done)
            .map(|o| format!("{}: {}", o.file, o.detail.replace('|', "/")))
            .unwrap_or_default();
        md.push_str(&format!(
            "| {} | {} | {done} of {files} | {failure} |\n",
            step.section, step.step
        ));
    }
    md
}

#[test]
#[ignore = "requires OM_SCORECARD_FILES (.fmp12 paths) and OM_SCORECARD_OUT (a folder)"]
fn md_steps_scorecard() {
    let files: Vec<PathBuf> = std::env::split_paths(
        &std::env::var_os("OM_SCORECARD_FILES").expect("set OM_SCORECARD_FILES"),
    )
    .collect();
    let out = PathBuf::from(std::env::var_os("OM_SCORECARD_OUT").expect("set OM_SCORECARD_OUT"));
    let temp = tempfile::tempdir().expect("temporary folder");
    // Each starter's copy, and a file File > Create New makes.
    let mut copies: Vec<(String, PathBuf)> = files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let copy = temp.path().join(format!("{i}.fmp12"));
            std::fs::copy(path, &copy).expect("copy the file");
            (super::label(path), copy)
        })
        .collect();
    let blank = temp.path().join("Create New.fmp12");
    om_data::new_file::create(&blank).expect("Create New");
    copies.push(("Create New".into(), blank));
    let mut outcomes = Vec::new();
    for (file, copy) in &copies {
        for step in steps() {
            let (done, detail) = run(copy, &step);
            outcomes.push(Outcome {
                file: file.clone(),
                section: step.section,
                step: step.step,
                done,
                detail,
            });
        }
    }
    std::fs::create_dir_all(&out).expect("output folder");
    let json = serde_json::to_string_pretty(&outcomes).expect("JSON");
    std::fs::write(out.join("md-steps.json"), json).expect("write md-steps.json");
    let md = summary(&outcomes, copies.len());
    std::fs::write(out.join("md-steps.md"), &md).expect("write md-steps.md");
    println!("{md}");
}

/// One file per step, for FileMaker's check: each a blank file File >
/// Create New makes, with that one step (and what it needs) made through
/// Manage Database's operations, left closed in `OM_MD_STEP_FILES` with a
/// README saying what to look for.
#[test]
#[ignore = "requires OM_MD_STEP_FILES (an output folder)"]
fn md_step_files() {
    let Some(out) = std::env::var_os("OM_MD_STEP_FILES").map(PathBuf::from) else {
        eprintln!("md_step_files: set OM_MD_STEP_FILES");
        return;
    };
    std::fs::create_dir_all(&out).expect("output folder");
    let mut readme = String::from(
        "One file per Manage Database step, each a blank file made by File > Create New's \
         operation (om_data::new_file) with that step made through Manage Database's \
         operations. Open each in FileMaker Pro, then File > Manage > Database, and look \
         for what its line says; then run FMDeveloperTool --checkConsistency on it.\n\n",
    );
    for (i, step) in steps().iter().enumerate() {
        // The file's name names its table too, so it is plain: the README
        // says which step each holds.
        let name = format!("MD{:02}.fmp12", i + 1);
        let path = out.join(&name);
        let _ = std::fs::remove_file(&path);
        om_data::new_file::create(&path).expect("Create New");
        let (done, detail) = run(&path, step);
        readme.push_str(&format!(
            "- {name}: {} > {}. OpenMaker read back: {} {detail}\n",
            step.section,
            step.step,
            if done { "yes," } else { "NO," }
        ));
    }
    std::fs::write(out.join("README.md"), readme).expect("write README");
}
