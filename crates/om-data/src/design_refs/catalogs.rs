//! The schema's catalogs (tables, fields, occurrences, relationships),
//! custom functions, value lists, data sources, the file's options, custom
//! menus and privilege sets, read through the access layer
//! ([`super::access`]): what a reader does not take is reported by the
//! sweep.

use om_hbam::catalog::FieldKind;
use om_hbam::slot::be_uint;
use om_hbam::{serial, FieldOptions};

use super::access::{At, Declared, Layer, Read};
use super::read::{data_source_id, stamps, Builder, Structure};
use super::steps;
use super::{Base, Kind, NameUse, ObjectId, Place, Target, UnreadReason};
use crate::field_options::Lookup;
use crate::meta::{self, relation_option, SummaryDef, ValueSource};

/// Keys under a field definition `[T, 3, 5, F]` (schema-path.md 5.5, 5.13;
/// calc-encoding.md 8; `crate::field_options`).
const FIELD_DEFINITION: u64 = 0x02;
const FIELD_COMMENT: u64 = 0x03;
const FIELD_FORMULA: u64 = 0x05;
const FIELD_VALUE_LIST: u64 = 0x07;
const FIELD_LOOKUP: u64 = 0x08;
const FIELD_SUMMARY: u64 = 0x09;
const FIELD_LOOKUP_CONSTANT: u64 = 0x0b;
const FIELD_NEXT_SERIAL: u64 = 0x0c;
const FIELD_ENTERED_DATA: u64 = 0x0d;
const FIELD_MESSAGE: u64 = 0x18;
const FIELD_VALIDATION_FORMULA: u64 = 0x19;
const FIELD_MESSAGE_FORMULA: u64 = 0x20;
const FIELD_RANGE: [u64; 2] = [0x21, 0x22];
const FIELD_STORAGE_FORMULA: u64 = 0x27;
const FIELD_DISPLAY_NAMES: u64 = 0x31;
/// `26`: four zero bytes in every observation, meaning not known
/// (schema-path.md 5.5, 5.13).
const FIELD_ZEROS: u64 = 0x26;
/// Option bits of the field definition's words (schema-path.md 5.5).
const AUTO_CALCULATED: u32 = 0x8;
const AUTO_LOOKUP: u32 = 0x400;
const FURIGANA: u32 = 0x40;
const EXTERNAL_STORAGE: u32 = 0x1000;
const VALIDATE_BY_CALCULATION: u32 = 0x1;
const VALIDATE_IN_VALUE_LIST: u32 = 0x100;
const MESSAGE_CALCULATION: u32 = 0x8000;
/// Member `07`'s furigana field, after the value list's 6-byte buffer.
const FURIGANA_FIELD: usize = 6;
/// The summary definition `09` is 30 bytes (file-metadata.md 5.1).
const SUMMARY_BYTES: usize = 30;
/// The data source, value list, custom function and custom menu catalogs.
const DATA_SOURCES: u64 = 0x20;
const VALUE_LISTS: u64 = 0x21;
const CUSTOM_FUNCTIONS: u64 = 0x10;
const CUSTOM_MENUS: u64 = 0x41;
const UNKNOWN_CATALOG: u64 = 0x40;
/// A custom function's description `03`, parameter count `20`, parameter
/// names `21`, body `23` and `24` (calc-encoding.md 8, calc-functions.md
/// 6.0).
const FUNCTION_DESCRIPTION: u64 = 0x03;
const FUNCTION_ARITY: u64 = 0x20;
const FUNCTION_PARAMETERS: u64 = 0x21;
const FUNCTION_BODY: u64 = 0x23;
const FUNCTION_FLAG: u64 = 0x24;
/// A value list's definition `02`, custom values `03` and calculation `05`
/// (file-metadata.md 6).
const LIST_DEFINITION: u64 = 0x02;
const LIST_VALUES: u64 = 0x03;
const LIST_CALCULATION: u64 = 0x05;
/// A relationship's definition `02` and predicates `03` (file-metadata.md
/// 4).
const RELATION_DEFINITION: u64 = 0x02;
const RELATION_PREDICATES: u64 = 0x03;
/// An occurrence's definition `02` (file-metadata.md 1.1) and a base
/// table's comment `03` (schema-path.md 5).
const OCCURRENCE_DEFINITION: u64 = 0x02;
const TABLE_COMMENT: u64 = 0x03;
/// `fb`: a table's and a field's data version (schema-path.md 5.8, 5.10),
/// and on an occurrence the occurrences relationships join it to
/// (file-metadata.md 4.2, schema-path.md 5.3), which the relationships
/// themselves are read for.
const DATA_VERSION: u64 = 0xfb;
/// A privilege set's members (session-identity.md 2): default record
/// access `02`, description `03`, checksum `04`, tables `05`, layout,
/// script and value list access `09`, other privileges `0a`, password
/// days `0c` and length `0d`; under a table `05, T`, its access `02`, its
/// fields' access `05` and the calculations limiting create, view, edit
/// and delete `06`-`09`.
const SET_DEFAULT_ACCESS: u64 = 0x02;
const SET_DESCRIPTION: u64 = 0x03;
const SET_CHECKSUM: u64 = 0x04;
const SET_TABLES: u64 = 0x05;
const SET_OBJECT_ACCESS: u64 = 0x09;
const SET_PRIVILEGES: u64 = 0x0a;
const SET_PASSWORD: [u64; 2] = [0x0c, 0x0d];
const TABLE_ACCESS: u64 = 0x02;
const TABLE_FIELD_ACCESS: u64 = 0x05;
const TABLE_LIMITS: [(u64, &str); 4] = [
    (0x06, "create"),
    (0x07, "view"),
    (0x08, "edit"),
    (0x09, "delete"),
];

/// `[2]` besides the document preferences `18`: the version `03`, `05`,
/// the history `06` `08` `09`, the locale `0d`, the custom icons `1818`,
/// `5d`, page setup `85`-`88` and the creation stamp `fe` (file-format.md,
/// catalog 2; file-metadata.md 0, 2).
const FILE_STATUS: [u64; 13] = [
    0x03, 0x05, 0x06, 0x08, 0x09, 0x0d, 0x1818, 0x5d, 0x85, 0x86, 0x87, 0x88, 0xfe,
];
/// A data source's file reference `82` (file-metadata.md 1.1).
const DATA_SOURCE_PATHS: u64 = 0x82;
/// A catalog's index `01` and members `05` (schema-path.md 5.1).
const CATALOG_INDEX: u64 = 0x01;
const CATALOG_MEMBERS: u64 = 0x05;

/// The catalog node at `path`: its index declared, its stamps accounted
/// for, and its member list for [`Builder::members`].
fn catalog<'t, 'a>(t: &'t Layer<'t, 'a>, path: &[u64]) -> Option<At<'t, 'a>> {
    let node = t.at(path)?;
    if let Some(index) = node.get(CATALOG_INDEX) {
        index.declare(Declared::CatalogIndex);
    }
    stamps(node);
    node.get(CATALOG_MEMBERS)
}

/// Charge reading the member `node` of `holder` before any of it is
/// decoded. False once the work budget is spent.
fn charge(b: &mut Builder, holder: ObjectId, node: At<'_, '_>) -> bool {
    b.charge(node, holder, &Place::new("definition"))
}

/// Data sources `[32, 5, D]`, named at `10`, their file paths at `82`,
/// numbered by their keys' serial decode as `inspect` numbers them. A data
/// source names another file, never an object of this one.
pub(super) fn data_sources(b: &mut Builder, t: &Layer<'_, '_>) {
    let list = catalog(t, &[DATA_SOURCES]);
    for (_, entry) in b.members(list) {
        let Some(id) = serial::decode(entry.segment().0).filter(|id| *id != 0) else {
            continue;
        };
        let holder = ObjectId::DataSource(id);
        if !charge(b, holder, entry) {
            return;
        }
        b.visit(Structure::DataSource);
        let name = b.name(entry, holder);
        b.object(holder, name, "");
        entry.take_at(DATA_SOURCE_PATHS);
        stamps(entry);
        b.count("data_sources");
    }
}

/// A field as its definition `02` gives it (schema-path.md 5.5).
struct FieldDef {
    id: u64,
    name: String,
    kind: FieldKind,
    options: Option<FieldOptions>,
}

/// Base tables and their field definitions (schema-path.md 5): every
/// table and field listed first, then each field read.
pub(super) fn tables(b: &mut Builder, t: &Layer<'_, '_>) {
    // The schema catalog `[3]` holds the base tables `16`, occurrences `17`
    // and relationships `fb`, each read by its own reader.
    let _ = catalog(t, &[3]);
    let list = catalog(t, &[3, 16]);
    let mut tables: Vec<(u64, Vec<(FieldDef, At<'_, '_>)>)> = Vec::new();
    for (id, entry) in b.members(list) {
        let holder = ObjectId::Table(id);
        if !charge(b, holder, entry) {
            return;
        }
        b.visit(Structure::Table);
        let name = b.name(entry, holder);
        // The comment is text; `fb` the table's data version.
        entry.take_at(TABLE_COMMENT);
        entry.take_at(DATA_VERSION);
        stamps(entry);
        b.object(holder, &name, "");
        b.count("tables");
        let fields_list = catalog(t, &[id, 3]);
        let mut fields = Vec::new();
        for (field_id, field) in b.members(fields_list) {
            // The field's member is charged before its definition is read.
            let field_holder = ObjectId::Field {
                table: id,
                field: field_id,
            };
            if !charge(b, field_holder, field) {
                return;
            }
            let options = match field.bytes_at(FIELD_DEFINITION) {
                Read::Ok(bytes) => FieldOptions::decode(&bytes),
                _ => None,
            };
            let kind = match options.as_ref().map(|o| o.field_kind) {
                Some(1) => FieldKind::Normal,
                Some(2) => FieldKind::Calculation,
                Some(3) => FieldKind::Summary,
                Some(other) => FieldKind::Unknown(other),
                None => FieldKind::Unknown(0),
            };
            let def = FieldDef {
                id: field_id,
                name: b.name(field, field_holder),
                kind,
                options,
            };
            let detail = match def.kind {
                FieldKind::Normal => "normal",
                FieldKind::Calculation => "calculation",
                FieldKind::Summary => "summary",
                FieldKind::Unknown(_) => "unknown",
            };
            b.object(
                ObjectId::Field {
                    table: id,
                    field: field_id,
                },
                &def.name,
                detail,
            );
            b.count("fields");
            fields.push((def, field));
        }
        tables.push((id, fields));
    }
    for (table, fields) in &tables {
        for (def, node) in fields {
            if b.stopped() {
                return;
            }
            self::field(b, *table, def, *node);
        }
    }
}

/// One field definition's references: its formulas, lookup, value list,
/// furigana field and summary.
fn field(b: &mut Builder, table: u64, field: &FieldDef, node: At<'_, '_>) {
    let holder = ObjectId::Field {
        table,
        field: field.id,
    };
    // Charged with the field's listing.
    b.visit(Structure::Field);
    let options = match (field.kind, &field.options) {
        (FieldKind::Unknown(kind), Some(_)) => {
            b.unread_node(
                node,
                holder,
                Place::new("definition"),
                UnreadReason::NotDecoded,
                format!("a field of kind {kind}, which OpenMaker does not read"),
                &Kind::ANY,
            );
            return;
        }
        (_, None) => {
            b.unread_node(
                node,
                holder,
                Place::new("definition"),
                UnreadReason::Malformed,
                "a field definition that does not read",
                &Kind::ANY,
            );
            return;
        }
        (_, Some(options)) => options,
    };
    // `26`: four zero bytes in every observation, meaning not known.
    b.observed(node.get(FIELD_ZEROS), &[0, 0, 0, 0]);
    // Texts and values that name nothing: the comment, the next serial
    // value, auto-entered data, the custom message, the range's bounds
    // (field_options.rs).
    for key in [
        FIELD_COMMENT,
        FIELD_NEXT_SERIAL,
        FIELD_ENTERED_DATA,
        FIELD_MESSAGE,
        DATA_VERSION,
    ]
    .into_iter()
    .chain(FIELD_RANGE)
    {
        node.take_at(key);
    }
    stamps(node);
    // A formula is read where the definition uses it; one stored for an
    // option that is off is kept by FileMaker but never evaluated, and is
    // read as an inactive formula.
    let formula = node.get(FIELD_FORMULA);
    if field.kind == FieldKind::Calculation {
        match formula {
            Some(formula) => b.formula(holder, Place::new("calculation"), formula),
            None => b.unread(
                holder,
                Place::new("calculation"),
                UnreadReason::Malformed,
                "a calculation field without its formula",
                &Kind::ANY,
            ),
        }
    } else if options.options & AUTO_CALCULATED != 0 {
        if let Some(formula) = formula {
            b.formula(holder, Place::new("auto-enter calculation"), formula);
        }
    } else if let Some(formula) = formula {
        b.inactive_formula(holder, Place::new("inactive calculation"), formula);
    }
    // A setting kept for an option that is off is never used; it must
    // still read.
    if field.kind != FieldKind::Summary {
        node.take_at(FIELD_SUMMARY);
    }
    if options.options & AUTO_LOOKUP == 0 {
        node.take_at(FIELD_LOOKUP);
        node.take_at(FIELD_LOOKUP_CONSTANT);
    }
    if field.kind == FieldKind::Summary {
        match node.bytes_at(FIELD_SUMMARY) {
            Read::Ok(bytes) if bytes.len() == SUMMARY_BYTES => {
                let summary = SummaryDef::decode(&bytes);
                for (part, spec) in [
                    ("summarized field", summary.field),
                    ("additional field", summary.additional),
                ] {
                    let Some(spec) = spec else {
                        continue;
                    };
                    let target = match spec.occurrence {
                        Some(occurrence) => Target::Field {
                            occurrence,
                            field: spec.field,
                        },
                        None => Target::Object(ObjectId::Field {
                            table,
                            field: spec.field,
                        }),
                    };
                    b.reference(holder, Place::new(part), target);
                }
            }
            // No definition is a summary of nothing (`SummaryDef`).
            Read::Absent => {}
            Read::Ok(_) | Read::Unreadable => b.unread(
                holder,
                Place::new("summary"),
                UnreadReason::Malformed,
                "a summary definition that does not read",
                &[Kind::Field, Kind::Occurrence],
            ),
        }
    }
    if options.options & AUTO_LOOKUP != 0 {
        let constant = match node.text_at(FIELD_LOOKUP_CONSTANT) {
            Read::Ok(text) => Some(text),
            Read::Absent | Read::Unreadable => None,
        };
        let lookup = match node.bytes_at(FIELD_LOOKUP) {
            Read::Ok(bytes) => Lookup::decode(&bytes, constant),
            Read::Absent | Read::Unreadable => None,
        };
        match lookup {
            Some(lookup) => {
                for (part, target) in [
                    (
                        "lookup: starting occurrence",
                        Target::Object(ObjectId::Occurrence(lookup.from)),
                    ),
                    (
                        "lookup: related occurrence",
                        Target::Object(ObjectId::Occurrence(lookup.to)),
                    ),
                    (
                        "lookup: copied field",
                        Target::Field {
                            occurrence: lookup.to,
                            field: lookup.copied,
                        },
                    ),
                ] {
                    b.reference(holder, Place::new(part), target);
                }
            }
            None => b.unread(
                holder,
                Place::new("lookup"),
                UnreadReason::Malformed,
                "a lookup definition that does not read",
                &[Kind::Field, Kind::Occurrence],
            ),
        }
    }
    let in_list = options.validation_options & VALIDATE_IN_VALUE_LIST != 0;
    let furigana = options.options & FURIGANA != 0;
    if !in_list && !furigana {
        node.take_at(FIELD_VALUE_LIST);
    }
    if in_list || furigana {
        let list = match node.bytes_at(FIELD_VALUE_LIST) {
            Read::Ok(bytes) => Some(bytes),
            Read::Absent | Read::Unreadable => None,
        };
        if in_list {
            match list.as_deref().and_then(meta::key_string) {
                Some(id) => b.reference(
                    holder,
                    Place::new("validation: member of value list"),
                    Target::Object(ObjectId::ValueList(id)),
                ),
                None => b.unread(
                    holder,
                    Place::new("validation: member of value list"),
                    UnreadReason::Malformed,
                    "a value list reference that does not read",
                    &[Kind::ValueList],
                ),
            }
        }
        if furigana {
            match list
                .as_deref()
                .and_then(|bytes| bytes.get(FURIGANA_FIELD..))
                .and_then(meta::key_string)
            {
                Some(furigana) => b.reference(
                    holder,
                    Place::new("furigana field"),
                    Target::Object(ObjectId::Field {
                        table,
                        field: furigana,
                    }),
                ),
                None => b.unread(
                    holder,
                    Place::new("furigana field"),
                    UnreadReason::Malformed,
                    "a furigana field reference that does not read",
                    &[Kind::Field],
                ),
            }
        }
    }
    for (key, active, part) in [
        (
            FIELD_VALIDATION_FORMULA,
            options.validation_options & VALIDATE_BY_CALCULATION != 0,
            "validation calculation",
        ),
        (
            FIELD_MESSAGE_FORMULA,
            options.validation_options & MESSAGE_CALCULATION != 0,
            "validation message calculation",
        ),
        (
            FIELD_STORAGE_FORMULA,
            options.options & EXTERNAL_STORAGE != 0,
            "external storage path calculation",
        ),
        // Field display names: a formula (calc-encoding.md 8), read
        // whenever stored.
        (FIELD_DISPLAY_NAMES, true, "display names calculation"),
    ] {
        match (node.get(key), active) {
            (Some(formula), true) => b.formula(holder, Place::new(part), formula),
            (None, true) if key != FIELD_DISPLAY_NAMES => b.unread(
                holder,
                Place::new(part),
                UnreadReason::Malformed,
                format!("a {part} the definition turns on but does not store"),
                &Kind::ANY,
            ),
            // Stored for an option that is off: never evaluated.
            (Some(formula), false) => {
                b.inactive_formula(holder, Place::new(format!("inactive {part}")), formula)
            }
            _ => {}
        }
    }
}

/// Table occurrences `[3, 17, 5, O]`: each one's table, in this file or
/// another.
pub(super) fn occurrences(b: &mut Builder, t: &Layer<'_, '_>) {
    let list = catalog(t, &[3, 17]);
    for (id, entry) in b.members(list) {
        let holder = ObjectId::Occurrence(id);
        if !charge(b, holder, entry) {
            return;
        }
        b.visit(Structure::Occurrence);
        let name = b.name(entry, holder);
        stamps(entry);
        relation_index(b, holder, entry.get(DATA_VERSION));
        b.count("occurrences");
        // Its table, or why it is not known, said for what is stored.
        let base = match entry.bytes_at(OCCURRENCE_DEFINITION) {
            Read::Absent => Base::Unknown("it is stored without its definition"),
            Read::Unreadable => Base::Unknown("its definition does not read"),
            Read::Ok(bytes) => {
                let occurrence = om_hbam::TableOccurrence {
                    id,
                    name: name.clone(),
                    definition: bytes.into_owned(),
                };
                match occurrence.reference() {
                    Some((0, table)) => Base::Table(table),
                    Some((source, _)) => data_source_id(source).map_or(
                        Base::Unknown("its definition's data source key does not read"),
                        Base::External,
                    ),
                    None => Base::Unknown("its definition does not decode"),
                }
            }
        };
        match base {
            Base::Table(table) => {
                let detail = b
                    .graph
                    .object(ObjectId::Table(table))
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                b.object(holder, &name, detail);
                b.reference(
                    holder,
                    Place::new("table"),
                    Target::Object(ObjectId::Table(table)),
                );
            }
            Base::External(source) => {
                b.object(holder, &name, "another file's table");
                b.reference(
                    holder,
                    Place::new("data source"),
                    Target::Object(ObjectId::DataSource(source)),
                );
            }
            Base::Unknown(why) => {
                b.object(holder, &name, "");
                b.unread(
                    holder,
                    Place::new("table"),
                    UnreadReason::Malformed,
                    format!("a table occurrence whose table is not known: {why}"),
                    &[Kind::Table, Kind::DataSource],
                );
            }
        }
        b.graph.bases.insert(id, base);
    }
}

/// An occurrence's relationship index `fb, <other O>` (schema-path.md
/// 5.3): for each directly related occurrence, the key string of the
/// relationship joining them. Derived from the relationships, but stored:
/// each entry is read as a use of that occurrence and that relationship,
/// so an index that disagrees with the relationships cannot make either
/// look unused.
fn relation_index(b: &mut Builder, holder: ObjectId, index: Option<At<'_, '_>>) {
    let Some(index) = index else {
        return;
    };
    b.visit(Structure::OccurrenceRelationIndex);
    let Read::Ok(entries) = index.children() else {
        return;
    };
    for entry in entries {
        let other = be_uint(entry.segment().0);
        let relationship = match entry.bytes() {
            Read::Ok(value) => value.split_first().and_then(|(len, key)| {
                (usize::from(*len) == key.len())
                    .then(|| be_uint(key))
                    .flatten()
            }),
            Read::Absent | Read::Unreadable => continue,
        };
        match (other, relationship) {
            (Some(other), Some(relationship)) => {
                b.reference(
                    holder,
                    Place::new("relationship index: related occurrence"),
                    Target::Object(ObjectId::Occurrence(other)),
                );
                b.reference(
                    holder,
                    Place::new("relationship index: relationship"),
                    Target::Object(ObjectId::Relationship(relationship)),
                );
            }
            _ => b.unread(
                holder,
                Place::new("relationship index"),
                UnreadReason::Malformed,
                "a relationship index entry that does not read",
                &Kind::ANY,
            ),
        }
    }
}

/// Relationships `[3, fb, 5, R]` (file-metadata.md 4): both occurrences,
/// each match's fields, and the sort of a sorted side.
pub(super) fn relationships(b: &mut Builder, t: &Layer<'_, '_>) {
    let list = catalog(t, &[3, 0xfb]);
    for (id, entry) in b.members(list) {
        let holder = ObjectId::Relationship(id);
        if !charge(b, holder, entry) {
            return;
        }
        b.visit(Structure::Relationship);
        b.count("relationships");
        stamps(entry);
        let definition = entry.bytes_at(RELATION_DEFINITION);
        let mut predicates: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut readable = true;
        if let Some(list) = entry.get(RELATION_PREDICATES) {
            match list.children() {
                Read::Ok(children) => {
                    for predicate in children {
                        match (predicate.id(), predicate.bytes()) {
                            (Some(n), Read::Ok(bytes)) => predicates.push((n, bytes.into_owned())),
                            _ => readable = false,
                        }
                    }
                }
                _ => readable = false,
            }
        }
        predicates.sort_by_key(|(n, _)| *n);
        let bytes: Vec<&[u8]> = predicates.iter().map(|(_, b)| b.as_slice()).collect();
        let relationship = match (definition, readable) {
            (Read::Ok(definition), true) => meta::Relationship::decode(id, &definition, &bytes),
            _ => None,
        };
        let Some(relationship) = relationship else {
            b.object(holder, "", "");
            b.unread(
                holder,
                Place::new("definition"),
                UnreadReason::Malformed,
                "a relationship that does not read",
                &[Kind::Occurrence, Kind::Field],
            );
            continue;
        };
        let name = |o: u64| {
            b.graph
                .object(ObjectId::Occurrence(o))
                .map(|o| o.name.clone())
                .unwrap_or_else(|| format!("occurrence {o}"))
        };
        let label = format!("{} - {}", name(relationship.left), name(relationship.right));
        b.object(holder, label, "");
        for (part, occurrence) in [
            ("left occurrence", relationship.left),
            ("right occurrence", relationship.right),
        ] {
            b.reference(
                holder,
                Place::new(part),
                Target::Object(ObjectId::Occurrence(occurrence)),
            );
        }
        for (n, predicate) in relationship.predicates.iter().enumerate() {
            for (side, occurrence, field) in [
                ("left", relationship.left, predicate.left),
                ("right", relationship.right, predicate.right),
            ] {
                b.reference(
                    holder,
                    Place::new(format!("match {}: {side} field", n + 1)),
                    Target::Field { occurrence, field },
                );
            }
        }
        for (bit, sort, part) in [
            (
                relation_option::SORT_LEFT_TO_RIGHT,
                &relationship.left_to_right,
                "sort of the right occurrence's records",
            ),
            (
                relation_option::SORT_RIGHT_TO_LEFT,
                &relationship.right_to_left,
                "sort of the left occurrence's records",
            ),
        ] {
            if relationship.options & bit != 0 {
                let found = steps::sort_found(sort);
                b.apply(holder, &Place::new(part), &found);
            }
        }
    }
}

/// Custom functions `[16, 5, F]`: their bodies.
pub(super) fn custom_functions(b: &mut Builder, t: &Layer<'_, '_>) {
    let list = catalog(t, &[CUSTOM_FUNCTIONS]);
    for (id, entry) in b.members(list) {
        let holder = ObjectId::CustomFunction(id);
        if !charge(b, holder, entry) {
            return;
        }
        b.visit(Structure::CustomFunction);
        let name = b.name(entry, holder);
        b.object(holder, name, "");
        b.count("custom_functions");
        // The description, parameter count, parameter names and flag name
        // nothing (calc-encoding.md 8; calc-functions.md 6.0).
        for key in [
            FUNCTION_DESCRIPTION,
            FUNCTION_ARITY,
            FUNCTION_PARAMETERS,
            FUNCTION_FLAG,
        ] {
            entry.take_at(key);
        }
        stamps(entry);
        if let Some(body) = entry.get(FUNCTION_BODY) {
            b.formula(holder, Place::new("body"), body);
        }
    }
}

/// Value lists `[33, 5, V]` (file-metadata.md 6): their fields,
/// occurrence, file and calculation.
pub(super) fn value_lists(b: &mut Builder, t: &Layer<'_, '_>) {
    let list = catalog(t, &[VALUE_LISTS]);
    for (id, entry) in b.members(list) {
        let holder = ObjectId::ValueList(id);
        if !charge(b, holder, entry) {
            return;
        }
        b.count("value_lists");
        b.visit(Structure::ValueList);
        let name = b.name(entry, holder);
        stamps(entry);
        // Custom values are text (file-metadata.md 6).
        let values = entry.text_at(LIST_VALUES);
        let definition = entry.bytes_at(LIST_DEFINITION);
        let calculation = entry.get(LIST_CALCULATION);
        let readable =
            !matches!(definition, Read::Unreadable) && !matches!(values, Read::Unreadable);
        // The list decoder reads the definition, the name and the values,
        // each read above through the accessors.
        let decoded = entry.decode_value_list().filter(|_| readable);
        let Some(list) = decoded else {
            b.object(holder, name, "");
            b.unread(
                holder,
                Place::new("definition"),
                UnreadReason::Malformed,
                "a value list definition that does not read",
                &Kind::ANY,
            );
            continue;
        };
        let detail = match &list.source {
            ValueSource::Custom(_) => "custom values",
            ValueSource::Field { .. } => "values from a field",
            ValueSource::External { .. } => "another file's value list",
            ValueSource::Calculation(_) => "calculated",
        };
        b.object(holder, &list.name, detail);
        // A formula kept from when the list was calculated is never
        // evaluated.
        if let (Some(formula), false) = (
            calculation,
            matches!(list.source, ValueSource::Calculation(_)),
        ) {
            b.inactive_formula(holder, Place::new("inactive calculation"), formula);
        }
        match &list.source {
            ValueSource::Custom(_) => {}
            ValueSource::Field {
                primary,
                secondary,
                start,
            } => {
                let first = steps::spec_found(*primary);
                b.apply(holder, &Place::new("first field"), &first);
                if let Some(secondary) = secondary {
                    let second = steps::spec_found(*secondary);
                    b.apply(holder, &Place::new("second field"), &second);
                }
                if let Some(start) = start {
                    b.reference(
                        holder,
                        Place::new("related values starting from"),
                        Target::Object(ObjectId::Occurrence(*start)),
                    );
                }
            }
            ValueSource::External { file, list: _ } => match file.and_then(data_source_id) {
                Some(source) => b.reference(
                    holder,
                    Place::new("file"),
                    Target::Object(ObjectId::DataSource(source)),
                ),
                None => b.unread(
                    holder,
                    Place::new("file"),
                    UnreadReason::Malformed,
                    "a file reference that does not read",
                    &[Kind::DataSource],
                ),
            },
            // Read from the stored node, whatever the list decoder made
            // of it: one with no compiled stream is unread.
            ValueSource::Calculation(_) => match calculation {
                Some(node) => b.formula(holder, Place::new("calculation"), node),
                None => b.unread(
                    holder,
                    Place::new("calculation"),
                    UnreadReason::Malformed,
                    "a calculated value list without its formula",
                    &Kind::ANY,
                ),
            },
        }
    }
}

/// The file's options `[2, 18]` (file-metadata.md 3): the layout it opens
/// on and its script triggers. The rest of `[2]` is the file's status.
pub(super) fn file_options(b: &mut Builder, t: &Layer<'_, '_>) {
    let Some(status) = t.at(&[2]) else {
        return;
    };
    for key in FILE_STATUS {
        if let Some(node) = status.get(key) {
            node.declare(Declared::FileStatus);
        }
    }
    let Some(stored) = status.get(meta::DOCUMENT_PREFERENCES[1]) else {
        return;
    };
    if !b.charge(stored, ObjectId::File, &Place::new("file options")) {
        return;
    }
    b.visit(Structure::FileOptions);
    let options = match stored.bytes() {
        Read::Ok(bytes) => meta::FileOptions::decode(&bytes),
        _ => None,
    };
    let Some(options) = options else {
        b.unread(
            ObjectId::File,
            Place::new("file options"),
            UnreadReason::Malformed,
            "document preferences that do not read",
            &[Kind::Layout, Kind::Script, Kind::Field],
        );
        return;
    };
    if let Some(layout) = options.default_layout() {
        b.reference(
            ObjectId::File,
            Place::new("file options: switch to layout"),
            Target::Object(ObjectId::Layout(layout)),
        );
    }
    use meta::file_event::*;
    for (event, name) in [
        (FIRST_WINDOW_OPEN, "OnFirstWindowOpen"),
        (LAST_WINDOW_CLOSE, "OnLastWindowClose"),
        (WINDOW_OPEN, "OnWindowOpen"),
        (WINDOW_CLOSE, "OnWindowClose"),
        (AV_PLAYER_CHANGE, "OnFileAVPlayerChange"),
        (WINDOW_TRANSACTION, "OnWindowTransaction"),
    ] {
        if let Some(script) = options.trigger(event) {
            b.reference(
                ObjectId::File,
                Place::new(format!("file script trigger {name}")),
                Target::Object(ObjectId::Script(script)),
            );
        }
    }
    // OnWindowTransaction names its field by name (file-metadata.md 3).
    if options.window_transaction.is_some() && !options.window_transaction_field.is_empty() {
        b.by_name(
            ObjectId::File,
            Place::new("file script trigger OnWindowTransaction: field name"),
            NameUse::FieldByName,
            None,
            vec![Kind::Field],
        );
    }
}

/// Custom menus `[65, 5, M]`: listed, their items not decoded, which is
/// reported for each.
pub(super) fn custom_menus(b: &mut Builder, t: &Layer<'_, '_>) {
    let list = catalog(t, &[CUSTOM_MENUS]);
    for (id, entry) in b.members(list) {
        let holder = ObjectId::CustomMenu(id);
        if !charge(b, holder, entry) {
            return;
        }
        b.visit(Structure::CustomMenu);
        let name = b.name(entry, holder);
        b.object(holder, name, "");
        // A known structure OpenMaker does not decode: coverage that
        // could name anything, not an anomaly.
        b.undecoded_node(
            entry,
            holder,
            Place::new("menu items"),
            "a custom menu's items and conditions are not decoded",
            &Kind::ANY,
        );
    }
}

/// Privilege sets `[23, 2, 5, P]` (session-identity.md 2): the formulas
/// that limit a table's records. A set's layout, script and value list
/// access and its field access are settings on those objects, not uses of
/// them. Without the catalog every place a formula could be is unread.
pub(super) fn privilege_sets(b: &mut Builder, t: &Layer<'_, '_>) {
    let given = b
        .graph
        .coverage
        .unavailable
        .iter()
        .any(|u| u.path.first() == Some(&23));
    let list = catalog(t, &[23, 2]);
    if t.at(&[23, 2]).is_none() {
        if !given {
            b.unread(
                ObjectId::File,
                Place::new("privilege sets"),
                UnreadReason::NotDecoded,
                "the privilege sets were not given, so their record access calculations are \
                 not read",
                &Kind::ANY,
            );
        }
        return;
    }
    for (id, set) in b.members(list) {
        let holder = ObjectId::PrivilegeSet(id);
        if !charge(b, holder, set) {
            return;
        }
        b.visit(Structure::PrivilegeSet);
        let name = b.name(set, holder);
        b.object(holder, name, "");
        // The description, default record access, checksum, other
        // privileges and password settings are values; the layout, script
        // and value list access `09` a list of settings on those objects,
        // not uses of them (session-identity.md 2, 4.1).
        for key in [
            SET_DESCRIPTION,
            SET_DEFAULT_ACCESS,
            SET_CHECKSUM,
            SET_PRIVILEGES,
        ]
        .into_iter()
        .chain(SET_PASSWORD)
        {
            set.take_at(key);
        }
        if let Some(access) = set.get(SET_OBJECT_ACCESS) {
            access.declare(Declared::AccessSetting);
        }
        stamps(set);
        let Some(tables) = set.get(SET_TABLES) else {
            continue;
        };
        let Read::Ok(tables) = tables.children() else {
            continue;
        };
        for table in tables {
            let Some(table_id) = table.id().filter(|id| *id != 0) else {
                continue;
            };
            let name = b
                .graph
                .object(ObjectId::Table(table_id))
                .map_or_else(|| format!("table {table_id}"), |t| t.name.clone());
            // The table's record access is a value, its fields' access a
            // list of settings (session-identity.md 2).
            table.take_at(TABLE_ACCESS);
            if let Some(fields) = table.get(TABLE_FIELD_ACCESS) {
                fields.declare(Declared::AccessSetting);
            }
            for (key, action) in TABLE_LIMITS {
                if let Some(formula) = table.get(key) {
                    b.visit(Structure::RecordAccess);
                    b.formula(
                        holder,
                        Place::new(format!("record access: {action} calculation of {name}")),
                        formula,
                    );
                }
            }
        }
    }
}

/// Catalog 64 (`[64]`, file-format.md 8): FileMaker Pro reads it when it
/// opens a file, and every observed member holds only its UUID and stamps.
/// What it is for is not known, so anything else stored there is left to
/// the sweep.
pub(super) fn unknown_catalog(b: &mut Builder, t: &Layer<'_, '_>) {
    let list = catalog(t, &[UNKNOWN_CATALOG]);
    if let Some(Read::Ok(members)) = list.map(At::children) {
        for member in members {
            if !b.spend(1, ObjectId::File, &Place::new("catalog 64")) {
                return;
            }
            b.visit(Structure::UnknownCatalog);
            stamps(member);
        }
    }
}
