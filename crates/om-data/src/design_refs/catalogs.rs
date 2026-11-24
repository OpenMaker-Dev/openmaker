//! The schema's catalogs (tables, fields, occurrences, relationships),
//! custom functions, value lists, data sources, the file's options, custom
//! menus and privilege sets: each member read through a
//! [`Member`](super::read::Member), so a key the reader does not take is
//! reported unread.

use om_hbam::catalog::FieldKind;
use om_hbam::{serial, KeyTree, Node};

use super::read::{data_source_id, members, text_at, Builder, MEMBER_STAMPS, NAME};
use super::steps;
use super::{Base, Kind, NameUse, ObjectId, Place, Target, UnreadReason};
use crate::field_options::Lookup;
use crate::meta::{self, relation_option, SummaryDef, ValueList, ValueSource};

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

/// A member whose key does not read as an id.
fn unreadable_member(b: &mut Builder, holder: ObjectId, what: &str, node: &Node<'_>) {
    b.unread(
        holder,
        Place::new(format!("{what} {}", node.segment)),
        UnreadReason::Malformed,
        format!("a {what} whose key does not read"),
        &Kind::ANY,
    );
}

/// Data sources `[32, 5, D]`, named at `10`, numbered by their keys'
/// serial decode as `inspect` numbers them. A data source names another
/// file, never an object of this one, so nothing else of it is read.
pub(super) fn data_sources(b: &mut Builder, tree: &KeyTree<'_>) {
    for (_, entry) in members(tree, &[DATA_SOURCES, 5]) {
        let Some(id) = serial::decode(entry.segment.0).filter(|id| *id != 0) else {
            unreadable_member(b, ObjectId::File, "data source", entry);
            continue;
        };
        let name = text_at(entry.get(NAME)).unwrap_or_default();
        b.object(ObjectId::DataSource(id), name, "");
        b.count("data_sources");
    }
}

/// Base tables and their field definitions (schema-path.md 5).
pub(super) fn tables(b: &mut Builder, tree: &KeyTree<'_>) {
    let tables = match om_hbam::catalog::tables(tree) {
        Ok(tables) => tables,
        Err(e) => {
            b.unavailable(
                vec![3, 16],
                "the base table catalog",
                format!("it does not decode: {e}"),
            );
            return;
        }
    };
    for (id, entry) in members(tree, &[3, 16, 5]) {
        let Some(id) = id else {
            unreadable_member(b, ObjectId::File, "base table", entry);
            continue;
        };
        let holder = ObjectId::Table(id);
        let mut member = b.member(holder, entry, "base table");
        member.get(NAME);
        member.known(&[TABLE_COMMENT, DATA_VERSION]);
        member.known(&MEMBER_STAMPS);
        b.close(member);
    }
    for table in &tables {
        b.object(ObjectId::Table(table.id), &table.name, "");
        b.count("tables");
        for field in &table.fields {
            let detail = match field.kind {
                FieldKind::Normal => "normal",
                FieldKind::Calculation => "calculation",
                FieldKind::Summary => "summary",
                FieldKind::Unknown(_) => "unknown",
            };
            b.object(
                ObjectId::Field {
                    table: table.id,
                    field: field.id,
                },
                &field.name,
                detail,
            );
            b.count("fields");
        }
        for (id, entry) in members(tree, &[table.id, 3, 5]) {
            if id.is_none() {
                unreadable_member(b, ObjectId::Table(table.id), "field", entry);
            }
        }
    }
    for table in &tables {
        for field in &table.fields {
            if b.stopped() {
                return;
            }
            if let Some(node) = tree.at(&[table.id, 3, 5, field.id]) {
                self::field(b, table.id, field, node);
            }
        }
    }
}

/// One field definition's references: its formulas, lookup, value list,
/// furigana field and summary.
fn field(b: &mut Builder, table: u64, field: &om_hbam::Field, node: &Node<'_>) {
    let holder = ObjectId::Field {
        table,
        field: field.id,
    };
    let options = match (field.kind, field.decoded_options()) {
        (FieldKind::Unknown(kind), _) => {
            b.unread(
                holder,
                Place::new("definition"),
                UnreadReason::NotDecoded,
                format!("a field of kind {kind}, which OpenMaker does not read"),
                &Kind::ANY,
            );
            return;
        }
        (_, None) => {
            b.unread(
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
    let mut member = b.member(holder, node, "field definition");
    member.get(NAME);
    member.get(FIELD_DEFINITION);
    if let Some(zeros) = member.get(FIELD_ZEROS) {
        if zeros.value.is_some_and(|v| v.iter().any(|b| *b != 0)) || !zeros.children.is_empty() {
            b.unread(
                holder,
                Place::new("field definition: key 26"),
                UnreadReason::NotDecoded,
                "a value under 26 of the field definition, zeros in every observation",
                &Kind::ANY,
            );
        }
    }
    // Texts and values that name nothing: the comment, the next serial
    // value, auto-entered data, the custom message, the range's bounds
    // (field_options.rs).
    member.known(&[
        FIELD_COMMENT,
        FIELD_NEXT_SERIAL,
        FIELD_ENTERED_DATA,
        FIELD_MESSAGE,
    ]);
    member.known(&FIELD_RANGE);
    member.known(&[DATA_VERSION]);
    member.known(&MEMBER_STAMPS);
    // A formula is read where the definition uses it; one stored for an
    // option that is off is kept by FileMaker but never evaluated.
    let calculated = field.kind == FieldKind::Calculation;
    let formula = member.get(FIELD_FORMULA);
    if calculated {
        match formula {
            Some(node) => b.formula(holder, Place::new("calculation"), node),
            None => b.unread(
                holder,
                Place::new("calculation"),
                UnreadReason::Malformed,
                "a calculation field without its formula",
                &Kind::ANY,
            ),
        }
    } else if options.options & AUTO_CALCULATED != 0 {
        if let Some(node) = formula {
            b.formula(holder, Place::new("auto-enter calculation"), node);
        }
    }
    let summary = member.get(FIELD_SUMMARY);
    if field.kind == FieldKind::Summary {
        let bytes = summary.and_then(Node::assembled);
        match bytes.as_deref() {
            Some(bytes) if bytes.len() == SUMMARY_BYTES => {
                let summary = SummaryDef::decode(bytes);
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
            None => {}
            Some(_) => b.unread(
                holder,
                Place::new("summary"),
                UnreadReason::Malformed,
                "a summary definition that does not read",
                &[Kind::Field, Kind::Occurrence],
            ),
        }
    }
    let lookup = member.get(FIELD_LOOKUP);
    let constant = member.get(FIELD_LOOKUP_CONSTANT);
    if options.options & AUTO_LOOKUP != 0 {
        let lookup = lookup
            .and_then(Node::assembled)
            .and_then(|bytes| Lookup::decode(&bytes, text_at(constant)));
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
    let list = member.get(FIELD_VALUE_LIST).and_then(Node::assembled);
    if options.validation_options & VALIDATE_IN_VALUE_LIST != 0 {
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
    if options.options & FURIGANA != 0 {
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
        let node = member.get(key);
        match (node, active) {
            (Some(node), true) => b.formula(holder, Place::new(part), node),
            (None, true) if key != FIELD_DISPLAY_NAMES => b.unread(
                holder,
                Place::new(part),
                UnreadReason::Malformed,
                format!("a {part} the definition turns on but does not store"),
                &Kind::ANY,
            ),
            _ => {}
        }
    }
    b.close(member);
}

/// Table occurrences `[3, 17, 5, O]`: each one's table, in this file or
/// another.
pub(super) fn occurrences(b: &mut Builder, tree: &KeyTree<'_>) {
    let occurrences = match om_hbam::catalog::table_occurrences(tree) {
        Ok(occurrences) => occurrences,
        Err(e) => {
            b.unavailable(
                vec![3, 17],
                "the table occurrence catalog",
                format!("it does not decode: {e}"),
            );
            return;
        }
    };
    for occurrence in occurrences {
        let id = ObjectId::Occurrence(occurrence.id);
        b.count("occurrences");
        let base = match occurrence.reference() {
            Some((0, table)) => Some(Base::Table(table)),
            Some((source, _)) => data_source_id(source).map(Base::External),
            None => None,
        };
        match base {
            Some(Base::Table(table)) => {
                let detail = b
                    .graph
                    .object(ObjectId::Table(table))
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                b.object(id, &occurrence.name, detail);
                b.reference(
                    id,
                    Place::new("table"),
                    Target::Object(ObjectId::Table(table)),
                );
            }
            Some(Base::External(source)) => {
                b.object(id, &occurrence.name, "another file's table");
                b.reference(
                    id,
                    Place::new("data source"),
                    Target::Object(ObjectId::DataSource(source)),
                );
            }
            Some(Base::Unknown) | None => {
                b.object(id, &occurrence.name, "");
                b.unread(
                    id,
                    Place::new("table"),
                    UnreadReason::Malformed,
                    "a table occurrence definition that does not read",
                    &[Kind::Table, Kind::DataSource],
                );
            }
        }
        b.graph
            .bases
            .insert(occurrence.id, base.unwrap_or(Base::Unknown));
    }
    for (id, entry) in members(tree, &[3, 17, 5]) {
        let Some(id) = id else {
            unreadable_member(b, ObjectId::File, "table occurrence", entry);
            continue;
        };
        let mut member = b.member(ObjectId::Occurrence(id), entry, "table occurrence");
        member.get(NAME);
        member.get(OCCURRENCE_DEFINITION);
        member.known(&[DATA_VERSION]);
        member.known(&MEMBER_STAMPS);
        b.close(member);
    }
}

/// Relationships `[3, fb, 5, R]` (file-metadata.md 4): their occurrences,
/// match fields and sorted sides.
pub(super) fn relationships(b: &mut Builder, tree: &KeyTree<'_>) {
    for (id, entry) in members(tree, &meta::RELATIONSHIPS) {
        let Some(id) = id else {
            unreadable_member(b, ObjectId::File, "relationship", entry);
            continue;
        };
        let holder = ObjectId::Relationship(id);
        b.count("relationships");
        let mut member = b.member(holder, entry, "relationship");
        member.get(RELATION_DEFINITION);
        member.get(RELATION_PREDICATES);
        member.known(&MEMBER_STAMPS);
        let Some(relationship) = meta::Relationship::from_node(entry) else {
            b.object(holder, "", "");
            b.unread(
                holder,
                Place::new("definition"),
                UnreadReason::Malformed,
                "a relationship that does not read",
                &[Kind::Occurrence, Kind::Field],
            );
            b.close(member);
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
        b.close(member);
    }
}

/// Custom functions `[16, 5, F]`: their bodies.
pub(super) fn custom_functions(b: &mut Builder, tree: &KeyTree<'_>) {
    for (id, entry) in members(tree, &[CUSTOM_FUNCTIONS, 5]) {
        let Some(id) = id else {
            unreadable_member(b, ObjectId::File, "custom function", entry);
            continue;
        };
        let holder = ObjectId::CustomFunction(id);
        b.object(holder, text_at(entry.get(NAME)).unwrap_or_default(), "");
        b.count("custom_functions");
        let mut member = b.member(holder, entry, "custom function");
        member.get(NAME);
        member.known(&[
            FUNCTION_DESCRIPTION,
            FUNCTION_ARITY,
            FUNCTION_PARAMETERS,
            FUNCTION_FLAG,
        ]);
        member.known(&MEMBER_STAMPS);
        if let Some(body) = member.get(FUNCTION_BODY) {
            b.formula(holder, Place::new("body"), body);
        }
        b.close(member);
    }
}

/// Value lists `[33, 5, V]` (file-metadata.md 6): their fields,
/// occurrence, file and calculation.
pub(super) fn value_lists(b: &mut Builder, tree: &KeyTree<'_>) {
    for (id, entry) in members(tree, &[VALUE_LISTS, 5]) {
        let Some(id) = id else {
            unreadable_member(b, ObjectId::File, "value list", entry);
            continue;
        };
        let holder = ObjectId::ValueList(id);
        b.count("value_lists");
        let mut member = b.member(holder, entry, "value list");
        member.get(NAME);
        member.get(LIST_DEFINITION);
        // Custom values are text (file-metadata.md 6).
        member.get(LIST_VALUES);
        let calculation = member.get(LIST_CALCULATION);
        member.known(&MEMBER_STAMPS);
        let Some(list) = ValueList::from_node(entry) else {
            b.object(holder, text_at(entry.get(NAME)).unwrap_or_default(), "");
            b.unread(
                holder,
                Place::new("definition"),
                UnreadReason::Malformed,
                "a value list definition that does not read",
                &Kind::ANY,
            );
            b.close(member);
            continue;
        };
        let detail = match &list.source {
            ValueSource::Custom(_) => "custom values",
            ValueSource::Field { .. } => "values from a field",
            ValueSource::External { .. } => "another file's value list",
            ValueSource::Calculation(_) => "calculated",
        };
        b.object(holder, &list.name, detail);
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
        b.close(member);
    }
}

/// The file's options `[2, 18]` (file-metadata.md 3): the layout it opens
/// on and its script triggers.
pub(super) fn file_options(b: &mut Builder, tree: &KeyTree<'_>) {
    if tree.at(&meta::DOCUMENT_PREFERENCES).is_none() {
        return;
    }
    let Some(options) = meta::file_options(tree) else {
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

/// Custom menus `[65, 5, M]`: listed, their items not decoded.
pub(super) fn custom_menus(b: &mut Builder, tree: &KeyTree<'_>) {
    for (id, entry) in members(tree, &[CUSTOM_MENUS, 5]) {
        let Some(id) = id else {
            unreadable_member(b, ObjectId::File, "custom menu", entry);
            continue;
        };
        let holder = ObjectId::CustomMenu(id);
        b.object(holder, text_at(entry.get(NAME)).unwrap_or_default(), "");
        b.unread(
            holder,
            Place::new("menu items"),
            UnreadReason::NotDecoded,
            "a custom menu's items and conditions are not decoded",
            &Kind::ANY,
        );
    }
}

/// Privilege sets `[23, 2, 5, P]` (session-identity.md 2): the formulas
/// that limit a table's records. A set's layout, script and value list
/// access and its field access are settings on those objects, not uses of
/// them. Without the catalog every place a formula could be is unread.
pub(super) fn privilege_sets(b: &mut Builder, tree: &KeyTree<'_>) {
    let given = b
        .graph
        .coverage
        .unavailable
        .iter()
        .any(|u| u.path.first() == Some(&23));
    let Some(sets) = tree.at(&[23, 2, 5]) else {
        if !given && tree.at(&[23, 2]).is_none() {
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
    };
    for set in &sets.children {
        if b.stopped() {
            return;
        }
        let Some(id) = set.id() else {
            unreadable_member(b, ObjectId::File, "privilege set", set);
            continue;
        };
        if id == 0 {
            continue;
        }
        let holder = ObjectId::PrivilegeSet(id);
        b.object(holder, text_at(set.get(NAME)).unwrap_or_default(), "");
        let mut member = b.member(holder, set, "privilege set");
        member.get(NAME);
        member.known(&[
            SET_DEFAULT_ACCESS,
            SET_DESCRIPTION,
            SET_CHECKSUM,
            SET_OBJECT_ACCESS,
            SET_PRIVILEGES,
        ]);
        member.known(&SET_PASSWORD);
        member.known(&MEMBER_STAMPS);
        if let Some(tables) = member.get(SET_TABLES) {
            for table in &tables.children {
                let Some(table_id) = table.id() else {
                    unreadable_member(b, holder, "privilege set table", table);
                    continue;
                };
                let name = b
                    .graph
                    .object(ObjectId::Table(table_id))
                    .map_or_else(|| format!("table {table_id}"), |t| t.name.clone());
                let mut access = b.member(holder, table, format!("record access of {name}"));
                access.known(&[TABLE_ACCESS, TABLE_FIELD_ACCESS]);
                for (key, action) in TABLE_LIMITS {
                    if let Some(formula) = access.get(key) {
                        b.formula(
                            holder,
                            Place::new(format!("record access: {action} calculation of {name}")),
                            formula,
                        );
                    }
                }
                b.close(access);
            }
        }
        b.close(member);
    }
}

/// Catalog 64 (`[64]`, file-format.md 8): FileMaker Pro reads it when it
/// opens a file, and every observed member holds only its UUID and stamps.
/// What it is for is not known, so anything else stored there is unread.
pub(super) fn unknown_catalog(b: &mut Builder, tree: &KeyTree<'_>) {
    for (_, entry) in members(tree, &[UNKNOWN_CATALOG, 5]) {
        let mut member = b.member(
            ObjectId::File,
            entry,
            format!("catalog 64 member {}", entry.segment),
        );
        member.known(&MEMBER_STAMPS);
        b.close(member);
    }
}
