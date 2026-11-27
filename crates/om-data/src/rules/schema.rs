//! A hosted file's schema for calculations and record rules, read from its
//! catalogs (schema-path.md 5, file-metadata.md): the file's locale and
//! options, tables and field definitions with their formulas, table
//! occurrences, relationships, summaries, value lists, custom functions,
//! layouts and scripts. The desktop client reads them from downloads,
//! OpenMaker Server from the file it hosts.

use std::collections::HashMap;
use std::sync::Arc;

use om_calc::eval::CustomFunction;
use om_calc::{Locale, ValueType};
use om_formula::Formula;
use om_hbam::catalog::{Catalog, DataType, FieldKind};
use om_hbam::{KeyTree, Node};

use crate::host::sql::{SqlCatalog, SqlFieldInfo};
use crate::host::{
    CalcSchema, ExternalOccurrence, FieldClass, FieldDef, LayoutEntry, LayoutField, LayoutKind,
    RelationshipDef,
};
use crate::meta::{self, SummaryDef};

/// Option bits of the field definition (schema-path.md 5.5).
/// A serial number generated on creation (`0x2`) or on commit (`0x200`).
const AUTO_ENTER_SERIAL: u32 = 0x202;
const AUTO_ENTER_CALCULATION: u32 = 0x8;
const GLOBAL: u32 = 0x1_0000;
const UNSTORED: u32 = 0x2_0000;
/// Validation by calculation (`mValidationSet`).
const VALIDATE_BY_CALCULATION: u32 = 0x1;
/// Keys under a field definition `[T, 3, 5, F]`.
const FIELD_COMMENT: u64 = 3;
const FIELD_FORMULA: u64 = 5;
const FIELD_NEXT_SERIAL: u64 = 0x0c;
const FIELD_SUMMARY: u64 = 9;
const FIELD_VALIDATION: u64 = 25;
/// Custom functions `[16, 5, id]`: name `16`, parameters `33` joined by
/// `;`, body `35` (calc-encoding.md 8).
const CUSTOM_FUNCTIONS: u64 = 16;
/// The base table and occurrence catalogs `[3, 16]`, `[3, 17]`; a base
/// table's comment is `[3, 16, 5, T, 03]`.
const BASE_TABLES: [u64; 2] = [3, 16];
const TABLE_COMMENT: u64 = 3;
/// External data sources `[32, 5, D]`, named at `10`.
const DATA_SOURCES: u64 = 0x20;
/// Field object flag: the edit box shows a vertical scroll bar
/// (layout-model.md 5.4).
const FIELD_SCROLL_BAR: u32 = 0x100;
const OCCURRENCES: [u64; 2] = [3, 17];

/// Cap only the temporary lookup allocation, never the accepted field count.
const MAX_INDEXED_FIELD_NODES: usize = 16_384;

/// One immutable table's field members for a metadata read. Repeated full
/// sibling scans make wide field catalogs quadratic. Keep `Node::get`'s raw
/// numeric IDs and first match, including leading-zero aliases and duplicates.
pub(crate) struct FieldNodes<'tree, 'data> {
    members: Option<&'tree Node<'data>>,
    indexed: Option<HashMap<u64, &'tree Node<'data>>>,
}

impl<'tree, 'data> FieldNodes<'tree, 'data> {
    pub(crate) fn new(tree: &'tree KeyTree<'data>, table: u64) -> Self {
        let members = tree.at(&[table, 3, 5]);
        let indexed = members.and_then(|members| {
            if members.children.len() > MAX_INDEXED_FIELD_NODES {
                return None;
            }
            let mut index = HashMap::new();
            // Allocation pressure changes the lookup strategy, not which
            // definitions the reader accepts or the defaults it supplies.
            index.try_reserve(members.children.len()).ok()?;
            for child in &members.children {
                if let Some(id) = child.id() {
                    index.entry(id).or_insert(child);
                }
            }
            Some(index)
        });
        Self { members, indexed }
    }

    pub(crate) fn get(&self, id: u64) -> Option<&'tree Node<'data>> {
        match &self.indexed {
            Some(index) => index.get(&id).copied(),
            None => self.members.and_then(|members| members.get(id)),
        }
    }
}

/// The calculation type of a field's data (or result) type.
pub fn value_type(data_type: DataType) -> ValueType {
    match data_type {
        DataType::Number => ValueType::Number,
        DataType::Date => ValueType::Date,
        DataType::Time => ValueType::Time,
        DataType::Timestamp => ValueType::Timestamp,
        DataType::Container => ValueType::Container,
        DataType::Text | DataType::Unknown(_) => ValueType::Text,
    }
}

/// The formula stored at `node`: context child 4, Polish child 6.
fn formula_at(node: Option<&Node<'_>>) -> Option<Formula> {
    let node = node?;
    let polish = node.get(6)?.assembled()?;
    let context = node.get(4).and_then(|n| n.assembled());
    Formula::decode(context.as_deref(), None, &polish).ok()
}

/// One field's definition for calculations, `None` for a definition this
/// client does not read (an unknown kind or a malformed definition).
pub fn field_def(tree: &KeyTree<'_>, table: u64, field: &om_hbam::Field) -> Option<FieldDef> {
    let options = field.decoded_options()?;
    let node = tree.at(&[table, 3, 5, field.id]);
    field_def_at(node, field, &options)
}

/// Decode from the node and options the record planner already resolved.
pub(super) fn field_def_at(
    node: Option<&Node<'_>>,
    field: &om_hbam::Field,
    options: &om_hbam::FieldOptions,
) -> Option<FieldDef> {
    let class = match field.kind {
        FieldKind::Normal => FieldClass::Data,
        FieldKind::Calculation => FieldClass::Calculation,
        FieldKind::Summary => FieldClass::Summary,
        FieldKind::Unknown(_) => return None,
    };
    let formula = || formula_at(node.and_then(|n| n.get(FIELD_FORMULA))).map(Arc::new);
    let mut def = FieldDef::data(field.id, field.name.clone(), value_type(field.data_type))
        .with_repetitions(u32::from(options.max_repetitions))
        .with_global(options.options & GLOBAL != 0);
    def.class = class;
    def.options = options.options;
    def.validation_options = options.validation_options;
    def.language = u32::from_be_bytes(options.raw[4..8].try_into().ok()?);
    def.comment = node
        .and_then(|n| n.get(FIELD_COMMENT))
        .and_then(Node::text)
        .and_then(Result::ok)
        .unwrap_or_default();
    if options.options & AUTO_ENTER_SERIAL != 0 {
        def.next_serial = node
            .and_then(|n| n.get(FIELD_NEXT_SERIAL))
            .and_then(Node::text)
            .and_then(Result::ok);
    }
    match class {
        FieldClass::Calculation => {
            def.formula = formula();
            def.stored = options.options & UNSTORED == 0;
        }
        FieldClass::Summary => {
            // A missing definition reads as Total of nothing.
            let bytes = node
                .and_then(|n| n.get(FIELD_SUMMARY))
                .and_then(|n| n.assembled());
            def.summary = Some(SummaryDef::decode(bytes.as_deref().unwrap_or_default()));
        }
        FieldClass::Data if options.options & AUTO_ENTER_CALCULATION != 0 => {
            def.auto_enter = formula();
        }
        FieldClass::Data => {}
    }
    if options.validation_options & VALIDATE_BY_CALCULATION != 0 {
        def.validation = formula_at(node.and_then(|n| n.get(FIELD_VALIDATION))).map(Arc::new);
    }
    Some(def)
}

fn custom_function(entry: &Node<'_>) -> Option<(u64, CustomFunction)> {
    let id = entry.id()?;
    let name = entry.get(16)?.text()?.ok()?;
    let parameters = entry
        .get(33)
        .and_then(|n| n.text())
        .and_then(Result::ok)
        .map(|p| {
            p.split(';')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let body = entry.get(35)?.get(6)?.assembled()?;
    let body = om_formula::polish::decode(&body).ok()?;
    Some((
        id,
        CustomFunction {
            name,
            parameters,
            body,
        },
    ))
}

/// The file's font families, by the id styled text names them with, from
/// its font catalog `[25, 5, F]` (each one's name at `10`): none when the
/// tree does not hold the catalog.
pub fn font_families(tree: &KeyTree<'_>) -> std::collections::BTreeMap<u64, String> {
    tree.at(&[25, 5])
        .into_iter()
        .flat_map(|node| &node.children)
        .filter_map(|node| Some((node.id()?, node.get(16)?.text()?.ok()?)))
        .collect()
}

/// The file's schema for calculations from its downloaded catalogs: `tree`
/// holds `[3]`, every `[T, 3]` and, when the file has any, `[16]`; with
/// `[2, 0d]` the file's locale (else en-US, a record that does not decode
/// marking [`CalcSchema::locale_unreadable`]), with `[2, 18]` its options and
/// file triggers, with `[2, 87]` its page setup, with `[21]` its value lists (also ExecuteSQL's), with
/// `[17]` its scripts, `[25, 5]` its fonts, and with `[4, 1]` the layouts'
/// display order.
pub fn calc_schema(tree: &KeyTree<'_>, catalog: &Catalog, file_name: &str) -> CalcSchema {
    let locale = meta::locale(tree);
    let locale_unreadable = locale.is_none() && tree.at(&meta::LOCALE).is_some();
    let mut schema = CalcSchema::new(file_name, locale.unwrap_or_else(Locale::en_us));
    schema.locale_unreadable = locale_unreadable;
    schema.font_families = font_families(tree);
    schema.file_options = meta::file_options(tree);
    schema.page_format = meta::page_format(tree);
    schema.set_display_order(
        meta::display_order(tree, &BASE_TABLES),
        meta::display_order(tree, &OCCURRENCES),
    );
    let mut sql_fields = HashMap::new();
    for table in &catalog.tables {
        let nodes = FieldNodes::new(tree, table.id);
        schema.add_table(table.id, table.name.clone());
        if let Some(comment) = tree
            .at(&[BASE_TABLES[0], BASE_TABLES[1], 5, table.id, TABLE_COMMENT])
            .and_then(Node::text)
            .and_then(Result::ok)
        {
            schema.set_table_comment(table.id, comment);
        }
        for field in &table.fields {
            let node = nodes.get(field.id);
            let options = field.decoded_options();
            // The same immutable node/options feed rules and SQL, including
            // SQL entries for fields whose rule definition is unreadable.
            sql_fields.insert(
                (table.id, field.id),
                SqlFieldInfo::read(node, field, options.as_ref()),
            );
            if let Some(def) = options
                .as_ref()
                .and_then(|options| field_def_at(node, field, options))
            {
                schema.add_field(table.id, def);
            }
        }
    }
    for occurrence in &catalog.table_occurrences {
        if let Some(table) = occurrence.base_table() {
            schema.add_occurrence(occurrence.id, occurrence.name.clone(), table);
        } else if let Some((source, _)) = occurrence.reference().filter(|(s, _)| *s != 0) {
            // A table of another file, named by its data source's key.
            schema.add_external_occurrence(ExternalOccurrence {
                id: occurrence.id,
                name: occurrence.name.clone(),
                source: tree
                    .at(&[DATA_SOURCES, 5, source, 0x10])
                    .and_then(Node::text)
                    .and_then(Result::ok)
                    .unwrap_or_default(),
            });
        }
    }
    for relationship in meta::relationships(tree) {
        schema.add_relationship(RelationshipDef::from(&relationship));
    }
    for list in meta::value_lists(tree) {
        schema.add_value_list(list);
    }
    schema.set_scripts(meta::scripts(tree));
    if let Some(entries) = tree.at(&[CUSTOM_FUNCTIONS, 5]) {
        for entry in &entries.children {
            if let Some((id, function)) = custom_function(entry) {
                schema.add_custom_function(id, function);
            }
        }
    }
    let order = meta::display_order(tree, &[meta::LAYOUTS]).unwrap_or_default();
    let mut layouts: Vec<&om_hbam::Layout> = catalog.layouts.iter().collect();
    layouts.sort_by_key(|l| {
        (
            order.iter().position(|o| *o == l.id).unwrap_or(usize::MAX),
            l.id,
        )
    });
    for layout in layouts {
        if let Some(entry) = layout_entry(tree, layout) {
            // Listed from the catalog's index only (on demand, #1019): its
            // objects and fields come with its member.
            let indexed =
                layout.object_store.is_none() && tree.at(&[meta::LAYOUTS, 5, layout.id]).is_none();
            if indexed {
                schema.set_layout_pending(entry.id);
            }
            schema.add_layout_entry(entry);
        }
    }
    schema.set_sql(SqlCatalog::read_with_fields(tree, catalog, sql_fields));
    schema
}

/// A layout catalog member: its kind, table occurrence and objects, decoded
/// from `tree` with the objects' names and texts when it holds the layout's
/// subtree `[4, 5, L]`, else from the catalog's object store alone.
fn layout_entry(tree: &KeyTree<'_>, raw: &om_hbam::Layout) -> Option<LayoutEntry> {
    let id = u32::try_from(raw.id).ok()?;
    let flags = meta::layout_flags(tree, raw.id);
    let kind = if flags & 0x1 != 0 {
        LayoutKind::Folder
    } else if flags & 0x2 != 0 {
        LayoutKind::FolderEnd
    } else {
        LayoutKind::Layout
    };
    // Only the catalog's index entry was read (no object store, no member
    // `[4, 5, L]`): its definition `02` names the table occurrence, and the
    // layout itself waits for its member.
    let indexed = raw.object_store.is_none() && tree.at(&[meta::LAYOUTS, 5, raw.id]).is_none();
    let layout = if indexed {
        None
    } else {
        om_layout::Layout::decode_from_tree(tree, raw.id)
            .ok()
            .flatten()
            .or_else(|| om_layout::Layout::decode(raw).ok())
    };
    let occurrence = match &layout {
        Some(layout) => layout
            .def
            .as_ref()
            .and_then(|d| om_layout::occurrence_id(d.table_alias_key)),
        None => raw
            .layout_def
            .as_deref()
            .and_then(|def| om_layout::LayoutDef::decode(def).ok())
            .and_then(|d| om_layout::occurrence_id(d.table_alias_key)),
    };
    Some(LayoutEntry {
        id,
        name: raw.name.clone(),
        kind,
        occurrence,
        fields: layout.as_ref().map(layout_fields).unwrap_or_default(),
        layout: layout.map(Arc::new),
    })
}

/// A layout's field objects in object order (the tree depth first, each
/// container before its contents), one per object. The object store
/// numbers fields (and value lists [I]) by their serial ids; the catalogs
/// by their keys' big-endian reading. Bounds are in layout coordinates; a
/// portal's fields are placed in its first row, inside the portal's left
/// and top borders where it draws them (FileMaker Server's `FieldBounds`).
fn layout_fields(layout: &om_layout::Layout) -> Vec<LayoutField> {
    let mut fields = Vec::new();
    let mut stack: Vec<u64> = layout.objects.root.iter().rev().copied().collect();
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        let Some(object) = layout.objects.get(id) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        stack.extend(object.children.iter().rev());
        let Some(field_obj) = object.field_obj.as_ref() else {
            continue;
        };
        let Some(spec) = field_obj.field_spec.as_ref() else {
            continue;
        };
        let (Some(occurrence), Some(field)) = (
            om_layout::occurrence_id(spec.table_key),
            om_hbam::serial::raw_of_id(spec.field_key),
        ) else {
            continue;
        };
        let mut bounds = layout.absolute_bounds(id).unwrap_or_default();
        let (dx, dy) = portal_inset(layout, object);
        bounds.left += dx;
        bounds.right += dx;
        bounds.top += dy;
        bounds.bottom += dy;
        fields.push(LayoutField {
            object: id,
            occurrence,
            field,
            control: field_obj.display_type,
            value_list: Some(field_obj.value_list_key)
                .filter(|key| *key != 0)
                .and_then(om_hbam::serial::raw_of_id),
            repetitions: field_obj.num_of_reps.unsigned_abs().max(1),
            horizontal: field_obj.num_of_reps < 0,
            scrolling: field_obj.flags & FIELD_SCROLL_BAR != 0,
            bounds: [bounds.left, bounds.top, bounds.right, bounds.bottom],
            rotation: object.rotation,
        });
    }
    fields
}

/// How far a portal's rows sit inside the portal: its left and top border
/// widths, where those borders are drawn. Zero outside a portal.
fn portal_inset(layout: &om_layout::Layout, object: &om_layout::LayoutObject) -> (f64, f64) {
    let mut parent = object.parent;
    let mut depth = 0;
    while let Some(id) = parent {
        let Some(container) = layout.objects.get(id) else {
            break;
        };
        if container.kind() == om_layout::ObjectKind::Portal {
            return layout.portal_row_inset(container);
        }
        depth += 1;
        if depth > 64 {
            break;
        }
        parent = container.parent;
    }
    (0.0, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_metadata_keeps_the_first_raw_numeric_member() {
        use om_hbam::catalog::{Field, Table};
        use om_hbam::slot::KeySegment;

        fn member<'a>(key: &'a [u8], comment: &'a [u8], fc: &'a [u8]) -> Node<'a> {
            Node {
                segment: KeySegment(key),
                children: vec![
                    Node {
                        segment: KeySegment(&[3]),
                        value: Some(comment),
                        ..Node::default()
                    },
                    Node {
                        segment: KeySegment(&[0xfc]),
                        value: Some(fc),
                        ..Node::default()
                    },
                ],
                ..Node::default()
            }
        }
        let raw = om_scsu::encode_scsu("Raw key");
        let alias = om_scsu::encode_scsu("First alias");
        let later = om_scsu::encode_scsu("Later duplicate");
        let serial = om_scsu::encode_scsu("Different raw key");
        // Node children are public: numeric order and unique segment bytes
        // cannot be assumed. 80 01 is raw 32769, not its serial decode 129.
        let members = Node {
            segment: KeySegment(&[5]),
            children: vec![
                member(&[0x80, 1], &raw, &[1, 11]),
                member(&[], &later, &[1, 99]),
                member(&[0; 9], &later, &[1, 99]),
                member(&[0, 2], &alias, &[1, 7]),
                member(&[2], &later, &[1, 99]),
                member(&[0x80, 1], &later, &[1, 99]),
                member(&[0x81], &serial, &[1, 13]),
                member(&[3], &[], &[2, 1]), // Malformed fc defaults to zero.
            ],
            ..Node::default()
        };
        let mut tree = KeyTree {
            root: Node {
                children: vec![Node {
                    segment: KeySegment(&[0x80, 2]),
                    children: vec![Node {
                        segment: KeySegment(&[3]),
                        children: vec![members],
                        ..Node::default()
                    }],
                    ..Node::default()
                }],
                ..Node::default()
            },
        };
        let field = |id, name: &str, kind| {
            let mut options = vec![0; 26];
            options[25] = 1;
            Field {
                id,
                name: name.into(),
                kind,
                data_type: DataType::Text,
                options,
            }
        };
        let flagged = |id, name: &str, kind, word: u32, language: u32, data_type| {
            let mut field = field(id, name, kind);
            field.options[4..8].copy_from_slice(&language.to_be_bytes());
            field.options[8..12].copy_from_slice(&word.to_be_bytes());
            field.data_type = data_type;
            field
        };
        let mut malformed = field(5, "Malformed", FieldKind::Normal);
        malformed.options.truncate(1);
        let catalog = Catalog {
            tables: vec![
                Table {
                    id: 0x8002,
                    name: "Rows".into(),
                    fields: vec![
                        flagged(
                            0x8001,
                            "Calculation",
                            FieldKind::Calculation,
                            0x8002_0020,
                            21,
                            DataType::Text,
                        ),
                        flagged(
                            2,
                            "Alias",
                            FieldKind::Normal,
                            0x8000_0000,
                            21,
                            DataType::Text,
                        ),
                        flagged(
                            3,
                            "Summary",
                            FieldKind::Summary,
                            0x8000_0020,
                            21,
                            DataType::Text,
                        ),
                        flagged(
                            4,
                            "Unknown",
                            FieldKind::Unknown(99),
                            0x8000_0020,
                            21,
                            DataType::Text,
                        ),
                        malformed,
                        flagged(
                            6,
                            "Missing node",
                            FieldKind::Normal,
                            0x8000_0000,
                            77,
                            DataType::Text,
                        ),
                        flagged(
                            129,
                            "Raw 129",
                            FieldKind::Normal,
                            0x8000_0000,
                            1,
                            DataType::Text,
                        ),
                        flagged(
                            0,
                            "Nonnumeric nodes are not zero",
                            FieldKind::Normal,
                            0x8000_0000,
                            0,
                            DataType::Date,
                        ),
                        // Catalog iteration order still decides a repeated id.
                        flagged(
                            2,
                            "Last catalog name",
                            FieldKind::Normal,
                            0x8001_0020,
                            21,
                            DataType::Text,
                        ),
                    ],
                },
                Table {
                    id: 0x8003,
                    name: "Missing table nodes".into(),
                    fields: vec![field(2, "Default", FieldKind::Normal)],
                },
            ],
            ..Catalog::default()
        };
        // The same format vector exercises the bounded allocation fallback;
        // exceeding its index budget must not become a product field limit.
        for fallback in [false, true] {
            if fallback {
                tree.root.children[0].children[0].children[0]
                    .children
                    .resize(MAX_INDEXED_FIELD_NODES + 1, Node::default());
            }
            let schema = calc_schema(&tree, &catalog, "Fields");
            let fields = &schema.table(0x8002).unwrap().fields;
            assert_eq!(fields.len(), 6);
            for (id, comment, fc) in [
                (0, "", 0),
                (0x8001, "Raw key", 11),
                (2, "First alias", 7),
                (3, "", 0),
                (6, "", 0),
                (129, "Different raw key", 13),
            ] {
                assert_eq!(fields[&id].comment, comment);
                assert_eq!(schema.sql().fields[&(0x8002, id)].mod_count, fc);
            }
            assert_eq!(fields[&2].name, "Last catalog name");
            assert!(fields[&2].global);
            assert_eq!(fields[&0x8001].class, FieldClass::Calculation);
            assert!(fields[&0x8001].formula.is_none());
            assert!(!fields[&0x8001].stored);
            assert!(fields[&3].summary.is_some());
            for id in [4, 5] {
                assert!(!fields.contains_key(&id));
                assert_eq!(schema.sql().fields[&(0x8002, id)].mod_count, 0);
            }
            assert_eq!(schema.table(0x8003).unwrap().fields[&2].comment, "");
            assert_eq!(schema.sql().fields[&(0x8003, 2)].mod_count, 0);
            // Fixed metadata expectations apply to both the standalone reader
            // and the fused builder, including omitted rule definitions.
            let standalone = SqlCatalog::read(&tree, &catalog);
            for sql in [schema.sql(), &standalone] {
                assert_eq!(sql.fields.len(), 9);
                for (id, always, findable, mod_count) in [
                    (0, false, true, 0),
                    (0x8001, true, false, 11),
                    (2, true, false, 7),
                    (3, true, false, 0),
                    (4, true, false, 0),
                    (5, false, false, 0),
                    (6, false, true, 0),
                    (129, false, false, 13),
                ] {
                    assert_eq!(
                        sql.fields[&(0x8002, id)],
                        SqlFieldInfo {
                            always,
                            findable,
                            mod_count
                        },
                        "field {id}"
                    );
                }
                assert_eq!(sql.fields[&(0x8003, 2)], SqlFieldInfo::default());
            }
        }
    }

    #[test]
    fn layout_fields_follow_the_object_tree_one_per_object() {
        use om_layout::{FieldObject, FieldSpec, Layout, LayoutObject, ObjectStore, Rect};
        let field =
            |id: u64, parent: Option<u64>, field: u64, reps: i32, flags: u32| LayoutObject {
                id,
                object_type: 1,
                parent,
                bounds: Some(Rect {
                    left: 10.0,
                    top: 20.25,
                    right: 30.0,
                    bottom: 40.0,
                    ..Rect::default()
                }),
                field_obj: Some(FieldObject {
                    flags,
                    num_of_reps: reps,
                    display_type: 0,
                    field_spec: Some(FieldSpec {
                        table_key: 1_065_090,
                        field_key: field,
                        repetition: 1,
                        ..FieldSpec::default()
                    }),
                    ..FieldObject::default()
                }),
                ..LayoutObject::default()
            };
        // A group (2) holds field 3; fields 1 and 4 at the root; field 1
        // shows three repetitions across, field 4 a scroll bar.
        let layout = Layout {
            objects: ObjectStore {
                root: vec![1, 2, 4],
                objects: vec![
                    field(1, None, 7, -3, 0),
                    LayoutObject {
                        id: 2,
                        object_type: 8,
                        children: vec![3],
                        bounds: Some(Rect::default()),
                        ..LayoutObject::default()
                    },
                    field(3, Some(2), 7, 1, 0),
                    field(4, None, 9, 1, FIELD_SCROLL_BAR),
                ],
                ..ObjectStore::default()
            },
            ..Layout::default()
        };
        let fields = layout_fields(&layout);
        let summary: Vec<_> = fields
            .iter()
            .map(|f| (f.object, f.field, f.repetitions, f.horizontal, f.scrolling))
            .collect();
        assert_eq!(
            summary,
            [
                (1, 7, 3, true, false),
                (3, 7, 1, false, false),
                (4, 9, 1, false, true)
            ]
        );
        assert_eq!(fields[0].occurrence, 0xd0_0002);
        assert_eq!(fields[0].bounds, [10.0, 20.25, 30.0, 40.0]);
    }

    #[test]
    fn the_schema_reads_the_locale_relationships_and_value_lists() {
        use om_hbam::{OwnedNode, SlotReader};
        let hex = |text: &str| -> Vec<u8> {
            let text: String = text.split_whitespace().collect();
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
                .collect()
        };
        // Sorted from the right (0x80) by field 2 descending of d0 00 02.
        let definition = hex("0403d00002 0403d00003 0000 0000 0000 0001 \
             0403d00002 020102 0001 020100 020100 0001 00 01 00000000 00000080");
        let relationship = OwnedNode::list(
            [1],
            vec![
                OwnedNode::leaf([2], definition),
                OwnedNode::list([3], vec![OwnedNode::leaf([1], hex("05 020109 02010a"))]),
            ],
        );
        let list = OwnedNode::list(
            [1],
            vec![
                OwnedNode::leaf([3], om_scsu::encode_scsu("a\rb")),
                OwnedNode::leaf([0x10], om_scsu::encode_scsu("Letters")),
            ],
        );
        let wire = OwnedNode::list(
            Vec::new(),
            vec![
                OwnedNode::list(
                    [2],
                    vec![OwnedNode::leaf([0x0d], Locale::de_de().to_record())],
                ),
                OwnedNode::list(
                    [3],
                    vec![OwnedNode::list(
                        [0xfb],
                        vec![OwnedNode::list([5], vec![relationship])],
                    )],
                ),
                OwnedNode::list([0x21], vec![OwnedNode::list([5], vec![list])]),
            ],
        )
        .to_wire()
        .unwrap();
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let schema = calc_schema(&tree, &Catalog::default(), "f");
        assert_eq!(schema.locale, Locale::de_de());
        let r = &schema.relationships()[0];
        assert_eq!((r.left, r.right), (0xd0_0002, 0xd0_0003));
        assert_eq!(r.predicates[0].op, meta::JoinOp::GreaterEqual);
        assert!(r.sort_right.is_none());
        let order = r.sort_left.as_ref().unwrap();
        assert!(order.fields[0].descending);
        assert_eq!(schema.value_lists()[0].name, "Letters");
        assert!(schema.value_list_named("letters").is_some());
    }

    #[test]
    fn occurrences_of_other_files_keep_data_sources_past_one_byte() {
        use om_hbam::catalog::TableOccurrence;
        use om_hbam::{OwnedNode, SlotReader};
        let source = |key: &[u8], name: &str| {
            OwnedNode::list(
                key.to_vec(),
                vec![OwnedNode::leaf([0x10], om_scsu::encode_scsu(name))],
            )
        };
        let wire = OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(
                [DATA_SOURCES as u8],
                vec![OwnedNode::list(
                    [5],
                    vec![source(&[1], "First"), source(&[0x80, 0x00], "Second")],
                )],
            )],
        )
        .to_wire()
        .unwrap();
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let occurrence = |id: u64, name: &str, definition: &[u8]| TableOccurrence {
            id,
            name: name.into(),
            definition: definition.to_vec(),
        };
        let catalog = Catalog {
            table_occurrences: vec![
                occurrence(0x81, "Local", &[2, 1, 0, 2, 1, 0x81, 0, 0, 1]),
                occurrence(0x82, "First rows", &[2, 1, 1, 2, 1, 0x81, 0, 0, 1]),
                // Data source 128 is the two-byte key `80 00`.
                occurrence(0x83, "Second rows", &[3, 2, 0x80, 0, 2, 1, 0x81, 0, 0, 1]),
            ],
            ..Catalog::default()
        };
        let schema = calc_schema(&tree, &catalog, "f");
        assert_eq!(schema.occurrence(0x81).unwrap().table, 0x81);
        let external: Vec<_> = schema
            .external_occurrences()
            .map(|o| (o.id, o.name.as_str(), o.source.as_str()))
            .collect();
        assert_eq!(
            external,
            [
                (0x82, "First rows", "First"),
                (0x83, "Second rows", "Second")
            ]
        );
    }
}
