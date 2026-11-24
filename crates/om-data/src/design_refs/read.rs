//! Building the graph from a file's catalogs: each catalog read with the
//! decoder OpenMaker has for it, each reference resolved once every object
//! is known.

use std::collections::BTreeMap;

use om_formula::{Expr, NameTable, Names};
use om_hbam::catalog::FieldKind;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, KeyTree, Node};
use om_layout::ObjectKind;
use om_scripts::display::{step_text_fidelity, StepNames, MAX_PASTE_STEPS};
use om_scripts::{Script, ScriptKind};

use super::formula::{self, FormulaRef};
use super::steps;
use super::{
    Base, ByName, Coverage, DesignGraph, Kind, NameUse, Object, ObjectId, Place, StepPlace, Target,
    Unavailable, Unread, UnreadReason, Use,
};
use crate::field_options::Lookup;
use crate::meta::{self, relation_option, SummaryDef, ValueList, ValueSource};

/// The catalogs [`build`] reads, by their paths in the file's key tree, and
/// besides them each table's field catalog `[T, 3]`. A source gives these;
/// one it cannot give is passed to `build` as [`Unavailable`].
pub const DESIGN_CATALOGS: [(&[u64], &str); 8] = [
    (&[2], "the file's options"),
    (&[3], "tables, table occurrences and relationships"),
    (&[4], "layouts"),
    (&[16], "custom functions"),
    (&[17], "scripts"),
    (&[32], "data sources"),
    (&[33], "value lists"),
    (&[65], "custom menus"),
];

/// Keys under a field definition `[T, 3, 5, F]` (schema-path.md 5.5,
/// calc-encoding.md 8, field_options.rs).
const FIELD_FORMULA: u64 = 0x05;
const FIELD_VALUE_LIST: u64 = 0x07;
const FIELD_LOOKUP: u64 = 0x08;
const FIELD_SUMMARY: u64 = 0x09;
const FIELD_LOOKUP_CONSTANT: u64 = 0x0b;
const FIELD_VALIDATION_FORMULA: u64 = 0x19;
const FIELD_MESSAGE_FORMULA: u64 = 0x20;
const FIELD_STORAGE_FORMULA: u64 = 0x27;
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
/// The data source and value list catalogs `[32]`, `[33]`.
const DATA_SOURCES: u64 = 0x20;
const VALUE_LISTS: u64 = 0x21;
const CUSTOM_FUNCTIONS: u64 = 0x10;
/// A custom function's body (calc-encoding.md 8).
const FUNCTION_BODY: u64 = 35;
/// `KCustomMenus` (calc-encoding.md 8).
const CUSTOM_MENUS: u64 = 0x41;
const NAME: u64 = 0x10;
/// Layout catalog flags of folders, their ends and separators.
const LAYOUT_NOT_A_LAYOUT: u64 = 0x1 | 0x2 | 0x10;
/// A layout object's attribute record child of an item list (segments,
/// panels, chart items: calc-encoding.md 8).
const ITEMS: u64 = 17;

/// The data source id of a key read big-endian, as occurrence definitions
/// and value lists store it: its segment's serial decode, the id `inspect`
/// gives data sources.
fn data_source_id(key: u64) -> Option<u64> {
    serial::decode(&uint_segment(key)).filter(|id| *id != 0)
}

/// Build the reference graph of the file whose catalogs `tree` holds
/// ([`DESIGN_CATALOGS`]). `unavailable` lists the catalogs the source
/// could not give: references into them are left unresolved, and every
/// "nothing refers to it" becomes "nothing decoded refers to it".
pub fn build(tree: &KeyTree<'_>, unavailable: Vec<Unavailable>) -> DesignGraph {
    let mut b = Builder {
        graph: DesignGraph {
            coverage: Coverage {
                unavailable,
                ..Coverage::default()
            },
            ..DesignGraph::default()
        },
        pending: Vec::new(),
    };
    b.data_sources(tree);
    b.tables(tree);
    b.occurrences(tree);
    b.relationships(tree);
    b.custom_functions(tree);
    b.value_lists(tree);
    b.layouts(tree);
    b.scripts(tree);
    b.file_options(tree);
    b.custom_menus(tree);
    // Not read for analysis: the security catalog holds credentials, and
    // OpenMaker does not decode privilege sets' record access formulas.
    b.unread(
        ObjectId::File,
        Place::new("privilege sets"),
        UnreadReason::NotDecoded,
        "privilege sets' record access calculations are not read".into(),
        vec![Kind::Field, Kind::Occurrence, Kind::CustomFunction],
    );
    let Builder { mut graph, pending } = b;
    let uses: Vec<Use> = pending
        .into_iter()
        .map(|(holder, place, target)| Use {
            holder,
            resolution: graph.resolve(target),
            place,
            target,
        })
        .collect();
    graph.uses = uses;
    graph
}

/// The graph as it is being read.
pub(super) struct Builder {
    graph: DesignGraph,
    pending: Vec<(ObjectId, Place, Target)>,
}

impl Builder {
    fn object(&mut self, id: ObjectId, name: impl Into<String>, detail: impl Into<String>) {
        self.graph.objects.insert(
            id,
            Object {
                id,
                name: name.into(),
                detail: detail.into(),
            },
        );
    }

    fn count(&mut self, what: &'static str) {
        self.graph.coverage.count(what, 1);
    }

    fn unavailable(&mut self, path: Vec<u64>, what: &str, reason: String) {
        self.graph.coverage.unavailable.push(Unavailable {
            path,
            what: what.into(),
            reason,
        });
    }

    pub(super) fn reference(&mut self, holder: ObjectId, place: Place, target: Target) {
        self.pending.push((holder, place, target));
    }

    pub(super) fn by_name(
        &mut self,
        holder: ObjectId,
        place: Place,
        how: NameUse,
        function: Option<String>,
        could_refer_to: Vec<Kind>,
    ) {
        self.graph.coverage.by_name.push(ByName {
            holder,
            place,
            how,
            function,
            could_refer_to,
        });
    }

    pub(super) fn unread(
        &mut self,
        holder: ObjectId,
        place: Place,
        reason: UnreadReason,
        detail: String,
        could_refer_to: Vec<Kind>,
    ) {
        self.graph.coverage.unread.push(Unread {
            holder,
            place,
            reason,
            detail,
            could_refer_to,
        });
    }

    /// The references of a decoded formula.
    fn expr(&mut self, holder: ObjectId, place: &Place, expr: &Expr) {
        self.count("formulas");
        for reference in formula::references(expr) {
            match reference {
                FormulaRef::Field { occurrence, field } => {
                    self.reference(holder, place.clone(), Target::Field { occurrence, field })
                }
                FormulaRef::CustomFunction(id) => self.reference(
                    holder,
                    place.clone(),
                    Target::Object(ObjectId::CustomFunction(id)),
                ),
                FormulaRef::ByName {
                    how,
                    function,
                    could_refer_to,
                } => self.by_name(holder, place.clone(), how, function, could_refer_to),
                FormulaRef::Unreadable(what) => self.unread(
                    holder,
                    place.clone(),
                    UnreadReason::Malformed,
                    what.into(),
                    vec![Kind::Field, Kind::Occurrence, Kind::CustomFunction],
                ),
            }
        }
    }

    /// A formula's context occurrence (child 4), when it has one.
    fn context(&mut self, holder: ObjectId, place: &Place, context: Option<&[u8]>) {
        let id = context
            .map(|key| om_formula::Key(key.to_vec()))
            .and_then(|key| key.id())
            .filter(|id| *id != 0);
        if let Some(occurrence) = id {
            self.reference(
                holder,
                Place {
                    step: place.step.clone(),
                    part: format!("{} context", place.part),
                },
                Target::Object(ObjectId::Occurrence(occurrence)),
            );
        }
    }

    /// The formula stored at `node` (children 4 context, 5 formatted, 6
    /// Polish; calc-encoding.md 1), when there is one.
    fn formula_node(&mut self, holder: ObjectId, place: Place, node: Option<&Node<'_>>) {
        let Some(node) = node else {
            return;
        };
        match node.get(6).and_then(Node::assembled) {
            Some(polish) => match om_formula::polish::decode(&polish) {
                Ok(expr) => {
                    let context = node.get(4).and_then(Node::assembled);
                    self.context(holder, &place, context.as_deref());
                    self.expr(holder, &place, &expr);
                }
                Err(e) => self.unread(
                    holder,
                    place,
                    UnreadReason::Malformed,
                    format!("a formula that does not decode: {e}"),
                    formula::unreadable_kinds(),
                ),
            },
            None => self.unread(
                holder,
                place,
                UnreadReason::NotDecoded,
                "a formula stored without its compiled form".into(),
                formula::unreadable_kinds(),
            ),
        }
    }

    /// A step's calculation (`om_scripts::Calculation`).
    pub(super) fn step_calculation(
        &mut self,
        holder: ObjectId,
        place: Place,
        calculation: &om_scripts::Calculation,
    ) {
        match calculation
            .polish
            .as_deref()
            .map(om_formula::polish::decode)
        {
            Some(Ok(expr)) => {
                self.context(holder, &place, calculation.context.as_deref());
                self.expr(holder, &place, &expr);
            }
            Some(Err(e)) => self.unread(
                holder,
                place,
                UnreadReason::Malformed,
                format!("a calculation that does not decode: {e}"),
                formula::unreadable_kinds(),
            ),
            None => self.unread(
                holder,
                place,
                UnreadReason::NotDecoded,
                "a calculation stored without its compiled form".into(),
                formula::unreadable_kinds(),
            ),
        }
    }

    /// A formula already decoded by a layout's reader.
    fn decoded(&mut self, holder: ObjectId, place: Place, formula: &om_formula::Formula) {
        if let Some(occurrence) = formula.occurrence() {
            self.reference(
                holder,
                Place {
                    step: None,
                    part: format!("{} context", place.part),
                },
                Target::Object(ObjectId::Occurrence(occurrence)),
            );
        }
        self.expr(holder, &place, &formula.polish);
    }

    /// Data sources `[32, 5, D]`, named at `10`, numbered by their keys'
    /// serial decode as `inspect` numbers them.
    fn data_sources(&mut self, tree: &KeyTree<'_>) {
        for entry in members(tree, &[DATA_SOURCES, 5]) {
            let Some(id) = serial::decode(entry.segment.0).filter(|id| *id != 0) else {
                continue;
            };
            let name = text(entry, NAME).unwrap_or_default();
            self.object(ObjectId::DataSource(id), name, "");
            self.count("data_sources");
        }
    }

    /// Base tables and their field definitions (schema-path.md 5.5).
    fn tables(&mut self, tree: &KeyTree<'_>) {
        let tables = match om_hbam::catalog::tables(tree) {
            Ok(tables) => tables,
            Err(e) => {
                self.unavailable(
                    vec![3, 16],
                    "the base table catalog",
                    format!("it does not decode: {e}"),
                );
                return;
            }
        };
        for table in &tables {
            self.object(ObjectId::Table(table.id), &table.name, "");
            self.count("tables");
            for field in &table.fields {
                let detail = match field.kind {
                    FieldKind::Normal => "normal",
                    FieldKind::Calculation => "calculation",
                    FieldKind::Summary => "summary",
                    FieldKind::Unknown(_) => "unknown",
                };
                self.object(
                    ObjectId::Field {
                        table: table.id,
                        field: field.id,
                    },
                    &field.name,
                    detail,
                );
                self.count("fields");
            }
        }
        for table in &tables {
            for field in &table.fields {
                self.field(tree, table.id, field);
            }
        }
    }

    /// One field definition's references: its formulas, lookup, value
    /// list, furigana field and summary.
    fn field(&mut self, tree: &KeyTree<'_>, table: u64, field: &om_hbam::Field) {
        let holder = ObjectId::Field {
            table,
            field: field.id,
        };
        let Some(options) = field.decoded_options() else {
            self.unread(
                holder,
                Place::new("definition"),
                UnreadReason::Malformed,
                "a field definition that does not read".into(),
                formula::unreadable_kinds(),
            );
            return;
        };
        let node = tree.at(&[table, 3, 5, field.id]);
        let member = |key: u64| node.and_then(|n| n.get(key));
        match field.kind {
            FieldKind::Calculation => {
                self.formula_node(holder, Place::new("calculation"), member(FIELD_FORMULA))
            }
            FieldKind::Summary => {
                let bytes = member(FIELD_SUMMARY).and_then(Node::assembled);
                let summary = SummaryDef::decode(bytes.as_deref().unwrap_or_default());
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
                    self.reference(holder, Place::new(part), target);
                }
            }
            FieldKind::Normal | FieldKind::Unknown(_) => {}
        }
        if field.kind != FieldKind::Calculation && options.options & AUTO_CALCULATED != 0 {
            self.formula_node(
                holder,
                Place::new("auto-enter calculation"),
                member(FIELD_FORMULA),
            );
        }
        if options.options & AUTO_LOOKUP != 0 {
            let constant = text_at(member(FIELD_LOOKUP_CONSTANT));
            let lookup = member(FIELD_LOOKUP)
                .and_then(Node::assembled)
                .and_then(|bytes| Lookup::decode(&bytes, constant));
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
                        self.reference(holder, Place::new(part), target);
                    }
                }
                None => self.unread(
                    holder,
                    Place::new("lookup"),
                    UnreadReason::Malformed,
                    "a lookup definition that does not read".into(),
                    vec![Kind::Field, Kind::Occurrence],
                ),
            }
        }
        let list = member(FIELD_VALUE_LIST).and_then(Node::assembled);
        if options.validation_options & VALIDATE_IN_VALUE_LIST != 0 {
            match list.as_deref().and_then(meta::key_string) {
                Some(id) => self.reference(
                    holder,
                    Place::new("validation: member of value list"),
                    Target::Object(ObjectId::ValueList(id)),
                ),
                None => self.unread(
                    holder,
                    Place::new("validation: member of value list"),
                    UnreadReason::Malformed,
                    "a value list reference that does not read".into(),
                    vec![Kind::ValueList],
                ),
            }
        }
        if options.options & FURIGANA != 0 {
            match list
                .as_deref()
                .and_then(|bytes| bytes.get(FURIGANA_FIELD..))
                .and_then(meta::key_string)
            {
                Some(furigana) => self.reference(
                    holder,
                    Place::new("furigana field"),
                    Target::Object(ObjectId::Field {
                        table,
                        field: furigana,
                    }),
                ),
                None => self.unread(
                    holder,
                    Place::new("furigana field"),
                    UnreadReason::Malformed,
                    "a furigana field reference that does not read".into(),
                    vec![Kind::Field],
                ),
            }
        }
        if options.validation_options & VALIDATE_BY_CALCULATION != 0 {
            self.formula_node(
                holder,
                Place::new("validation calculation"),
                member(FIELD_VALIDATION_FORMULA),
            );
        }
        if options.validation_options & MESSAGE_CALCULATION != 0 {
            self.formula_node(
                holder,
                Place::new("validation message calculation"),
                member(FIELD_MESSAGE_FORMULA),
            );
        }
        if options.options & EXTERNAL_STORAGE != 0 {
            self.formula_node(
                holder,
                Place::new("external storage path calculation"),
                member(FIELD_STORAGE_FORMULA),
            );
        }
    }

    /// Table occurrences: each one's table, in this file or another.
    fn occurrences(&mut self, tree: &KeyTree<'_>) {
        let occurrences = match om_hbam::catalog::table_occurrences(tree) {
            Ok(occurrences) => occurrences,
            Err(e) => {
                self.unavailable(
                    vec![3, 17],
                    "the table occurrence catalog",
                    format!("it does not decode: {e}"),
                );
                return;
            }
        };
        for occurrence in occurrences {
            let id = ObjectId::Occurrence(occurrence.id);
            self.count("occurrences");
            match occurrence.reference() {
                Some((0, table)) => {
                    self.graph.bases.insert(occurrence.id, Base::Table(table));
                    let detail = self
                        .graph
                        .object(ObjectId::Table(table))
                        .map(|t| t.name.clone())
                        .unwrap_or_default();
                    self.object(id, &occurrence.name, detail);
                    self.reference(
                        id,
                        Place::new("table"),
                        Target::Object(ObjectId::Table(table)),
                    );
                }
                Some((source, _)) => match data_source_id(source) {
                    Some(source) => {
                        self.graph
                            .bases
                            .insert(occurrence.id, Base::External(source));
                        self.object(id, &occurrence.name, "another file's table");
                        self.reference(
                            id,
                            Place::new("data source"),
                            Target::Object(ObjectId::DataSource(source)),
                        );
                    }
                    None => self.unknown_occurrence(&occurrence),
                },
                None => self.unknown_occurrence(&occurrence),
            }
        }
    }

    fn unknown_occurrence(&mut self, occurrence: &om_hbam::TableOccurrence) {
        let id = ObjectId::Occurrence(occurrence.id);
        self.graph.bases.insert(occurrence.id, Base::Unknown);
        self.object(id, &occurrence.name, "");
        self.unread(
            id,
            Place::new("table"),
            UnreadReason::Malformed,
            "a table occurrence definition that does not read".into(),
            vec![Kind::Table, Kind::DataSource],
        );
    }

    /// Relationships `[3, fb, 5, R]` (file-metadata.md 4): their
    /// occurrences, match fields and sorted sides.
    fn relationships(&mut self, tree: &KeyTree<'_>) {
        for entry in members(tree, &meta::RELATIONSHIPS) {
            let Some(id) = entry.id() else {
                continue;
            };
            let holder = ObjectId::Relationship(id);
            self.count("relationships");
            let Some(relationship) = meta::Relationship::from_node(entry) else {
                self.object(holder, "", "");
                self.unread(
                    holder,
                    Place::new("definition"),
                    UnreadReason::Malformed,
                    "a relationship that does not read".into(),
                    vec![Kind::Occurrence, Kind::Field],
                );
                continue;
            };
            let name = |o: u64| {
                self.graph
                    .object(ObjectId::Occurrence(o))
                    .map(|o| o.name.clone())
                    .unwrap_or_else(|| format!("occurrence {o}"))
            };
            let label = format!("{} - {}", name(relationship.left), name(relationship.right));
            self.object(holder, label, "");
            for (part, occurrence) in [
                ("left occurrence", relationship.left),
                ("right occurrence", relationship.right),
            ] {
                self.reference(
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
                    self.reference(
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
                    steps::sort_order(self, holder, &Place::new(part), sort);
                }
            }
        }
    }

    /// Custom functions `[16, 5, F]`: their bodies.
    fn custom_functions(&mut self, tree: &KeyTree<'_>) {
        for entry in members(tree, &[CUSTOM_FUNCTIONS, 5]) {
            let Some(id) = entry.id() else {
                continue;
            };
            let holder = ObjectId::CustomFunction(id);
            self.object(holder, text(entry, NAME).unwrap_or_default(), "");
            self.count("custom_functions");
            self.formula_node(holder, Place::new("body"), entry.get(FUNCTION_BODY));
        }
    }

    /// Value lists `[33, 5, V]` (file-metadata.md 6): their fields,
    /// occurrence, file and calculation.
    fn value_lists(&mut self, tree: &KeyTree<'_>) {
        for entry in members(tree, &[VALUE_LISTS, 5]) {
            let Some(id) = entry.id() else {
                continue;
            };
            let holder = ObjectId::ValueList(id);
            self.count("value_lists");
            let Some(list) = ValueList::from_node(entry) else {
                self.object(holder, text(entry, NAME).unwrap_or_default(), "");
                self.unread(
                    holder,
                    Place::new("definition"),
                    UnreadReason::Malformed,
                    "a value list definition that does not read".into(),
                    vec![Kind::Field, Kind::Occurrence, Kind::DataSource],
                );
                continue;
            };
            let detail = match &list.source {
                ValueSource::Custom(_) => "custom values",
                ValueSource::Field { .. } => "values from a field",
                ValueSource::External { .. } => "another file's value list",
                ValueSource::Calculation(_) => "calculated",
            };
            self.object(holder, &list.name, detail);
            match &list.source {
                ValueSource::Custom(_) => {}
                ValueSource::Field {
                    primary,
                    secondary,
                    start,
                } => {
                    let first = Place::new("first field");
                    steps_field(self, holder, &first, *primary);
                    if let Some(secondary) = secondary {
                        steps_field(self, holder, &Place::new("second field"), *secondary);
                    }
                    if let Some(start) = start {
                        self.reference(
                            holder,
                            Place::new("related values starting from"),
                            Target::Object(ObjectId::Occurrence(*start)),
                        );
                    }
                }
                ValueSource::External { file, list: _ } => match file.and_then(data_source_id) {
                    Some(source) => self.reference(
                        holder,
                        Place::new("file"),
                        Target::Object(ObjectId::DataSource(source)),
                    ),
                    None => self.unread(
                        holder,
                        Place::new("file"),
                        UnreadReason::Malformed,
                        "a file reference that does not read".into(),
                        vec![Kind::DataSource],
                    ),
                },
                ValueSource::Calculation(Some(stored)) => {
                    let place = Place::new("calculation");
                    match om_formula::polish::decode(&stored.polish) {
                        Ok(expr) => {
                            self.context(holder, &place, stored.context.as_deref());
                            self.expr(holder, &place, &expr);
                        }
                        Err(e) => self.unread(
                            holder,
                            place,
                            UnreadReason::Malformed,
                            format!("a formula that does not decode: {e}"),
                            formula::unreadable_kinds(),
                        ),
                    }
                }
                ValueSource::Calculation(None) => {}
            }
        }
    }

    /// Layouts `[4, 5, L]` (layout-model.md): each layout's occurrence,
    /// parts, triggers and objects.
    fn layouts(&mut self, tree: &KeyTree<'_>) {
        let layouts = match om_hbam::catalog::layouts(tree) {
            Ok(layouts) => layouts,
            Err(e) => {
                self.unavailable(
                    vec![4],
                    "the layout catalog",
                    format!("it does not decode: {e}"),
                );
                return;
            }
        };
        for raw in &layouts {
            if meta::layout_flags(tree, raw.id) & LAYOUT_NOT_A_LAYOUT != 0 {
                continue;
            }
            let holder = ObjectId::Layout(raw.id);
            self.object(holder, &raw.name, "");
            self.count("layouts");
            match om_layout::Layout::decode_from_tree(tree, raw.id) {
                Ok(Some(layout)) => self.layout(tree, holder, &layout),
                Ok(None) => self.unavailable(
                    vec![4, 5, raw.id],
                    &format!("layout {}", raw.id),
                    "only the layout catalog's index lists it".into(),
                ),
                Err(e) => self.unread(
                    holder,
                    Place::new("definition"),
                    UnreadReason::Malformed,
                    format!("a layout that does not decode: {e}"),
                    Kind::REFERABLE.to_vec(),
                ),
            }
        }
    }

    fn layout(&mut self, tree: &KeyTree<'_>, holder: ObjectId, layout: &om_layout::Layout) {
        let ObjectId::Layout(id) = holder else {
            return;
        };
        if let Some(occurrence) = layout
            .def
            .as_ref()
            .map(|d| d.table_alias_key)
            .filter(|key| *key != 0)
            .and_then(om_layout::occurrence_id)
        {
            self.reference(
                holder,
                Place::new("table occurrence"),
                Target::Object(ObjectId::Occurrence(occurrence)),
            );
        }
        for part in &layout.parts {
            match part.break_field() {
                Ok(Some(field)) => self.reference(
                    holder,
                    Place::new(format!("part {}: sorted by", part.id)),
                    Target::Field {
                        occurrence: field.occurrence,
                        field: field.field,
                    },
                ),
                Ok(None) => {}
                Err(e) => self.unread(
                    holder,
                    Place::new(format!("part {}", part.id)),
                    UnreadReason::Malformed,
                    e,
                    vec![Kind::Field, Kind::Occurrence],
                ),
            }
        }
        self.triggers(holder, "", &layout.triggers);
        for (object, triggers) in &layout.table_view_triggers {
            self.triggers(holder, &format!("table view object {object}: "), triggers);
        }
        for object in &layout.objects.objects {
            let holder = ObjectId::LayoutObject {
                layout: id,
                object: object.id,
            };
            self.object(
                holder,
                object.name.clone().unwrap_or_default(),
                object_kind(object.kind()),
            );
            self.count("layout_objects");
            self.layout_object(tree, holder, object);
        }
    }

    fn layout_object(
        &mut self,
        tree: &KeyTree<'_>,
        holder: ObjectId,
        object: &om_layout::LayoutObject,
    ) {
        if let Some(field_obj) = &object.field_obj {
            if let Some(spec) = &field_obj.field_spec {
                match (om_layout::occurrence_id(spec.table_key), spec.field_id()) {
                    (Some(occurrence), Some(field)) if spec.table_key != 0 => self.reference(
                        holder,
                        Place::new("field"),
                        Target::Field { occurrence, field },
                    ),
                    _ => self.unread(
                        holder,
                        Place::new("field"),
                        UnreadReason::Malformed,
                        "a field binding whose keys do not read".into(),
                        vec![Kind::Field, Kind::Occurrence],
                    ),
                }
            }
            if field_obj.value_list_key != 0 {
                match serial::raw_of_id(field_obj.value_list_key) {
                    Some(list) => self.reference(
                        holder,
                        Place::new("value list"),
                        Target::Object(ObjectId::ValueList(list)),
                    ),
                    None => self.unread(
                        holder,
                        Place::new("value list"),
                        UnreadReason::Malformed,
                        "a value list key that does not read".into(),
                        vec![Kind::ValueList],
                    ),
                }
            }
        }
        if let Some(portal) = &object.portal {
            match om_layout::occurrence_id(portal.table_alias_key)
                .filter(|_| portal.table_alias_key != 0)
            {
                Some(occurrence) => self.reference(
                    holder,
                    Place::new("portal occurrence"),
                    Target::Object(ObjectId::Occurrence(occurrence)),
                ),
                None => self.unread(
                    holder,
                    Place::new("portal occurrence"),
                    UnreadReason::Malformed,
                    "a portal occurrence key that does not read".into(),
                    vec![Kind::Occurrence],
                ),
            }
            // Flag 0x8 turns the stored sort order on (om-gui's portal reads).
            if portal.flags & 0x8 != 0 {
                if let Some(bytes) = &object.calcs.portal_sort {
                    let place = Place::new("portal sort");
                    match om_scripts::specs::SortSpec::decode(bytes) {
                        Some(sort) => steps::sort_order(self, holder, &place, &sort),
                        None => self.unread(
                            holder,
                            place,
                            UnreadReason::Malformed,
                            "a portal sort order that does not read".into(),
                            vec![Kind::Field, Kind::Occurrence, Kind::ValueList],
                        ),
                    }
                }
            }
        }
        let step = object.script_step();
        if let Some(stored) = step {
            match om_scripts::ScriptStep::from_layout(stored) {
                Some(step) => steps::read(self, holder, None, "button: ", &step, true),
                None => self.unread(
                    holder,
                    Place::new("button"),
                    UnreadReason::Malformed,
                    "a button step that does not read".into(),
                    Kind::REFERABLE.to_vec(),
                ),
            }
        }
        self.triggers(holder, "", &object.triggers);
        let calcs = &object.calcs;
        let mut formulas: Vec<(String, &om_formula::Formula)> = Vec::new();
        for (part, formula) in [
            ("label calculation", &calcs.label),
            ("tooltip", &calcs.tooltip),
            ("placeholder", &calcs.placeholder),
            ("hide condition", &calcs.hide),
            ("active segment calculation", &calcs.active_segment),
            ("web address", &calcs.web_address),
            ("portal filter", &calcs.portal_filter),
            ("calculated icon", &calcs.button_icon),
        ] {
            if let Some(formula) = formula {
                formulas.push((part.into(), formula));
            }
        }
        for (n, condition) in calcs.conditions.iter().enumerate() {
            if let Some(formula) = &condition.formula {
                formulas.push((format!("conditional formatting {}", n + 1), formula));
            }
        }
        for (part, formula) in formulas {
            self.decoded(holder, Place::new(part), formula);
        }
        for key in &calcs.undecodable {
            self.unread(
                holder,
                Place::new(format!("attribute {key}")),
                UnreadReason::Malformed,
                "a calculation that does not decode".into(),
                formula::unreadable_kinds(),
            );
        }
        for spec in object.merge_field_specs() {
            // Merge tokens carry raw catalog keys (`merge_field_specs`).
            self.reference(
                holder,
                Place::new("merge field"),
                Target::Field {
                    occurrence: spec.table_key,
                    field: spec.field_key,
                },
            );
        }
        if object.kind() == ObjectKind::External && object.web_viewer().is_none() {
            self.unread(
                holder,
                Place::new("setup"),
                UnreadReason::NotDecoded,
                "a chart or plug-in object's setup".into(),
                vec![
                    Kind::Field,
                    Kind::Occurrence,
                    Kind::CustomFunction,
                    Kind::Script,
                ],
            );
        }
        // `17` holds a button's step data, read with its step; on other
        // objects, item calculations (segments, panels, chart items:
        // `17/<n>/5`, calc-encoding.md 8), the first of which is what
        // `ObjectCalcs::script_parameter` reads.
        if step.is_none() {
            let ObjectId::LayoutObject {
                layout,
                object: key,
            } = holder
            else {
                return;
            };
            let items = tree
                .at(&[4, 5, layout, 5])
                .and_then(|attributes| attributes.child(&serial::encode(key)))
                .and_then(|attribute| attribute.get(ITEMS));
            for item in items.map(|n| n.children.as_slice()).unwrap_or_default() {
                let n = item.id().unwrap_or_default();
                self.formula_node(holder, Place::new(format!("item {n}")), item.get(5));
            }
        }
    }

    /// Script triggers of a layout or layout object: each one's script and
    /// parameter.
    fn triggers(&mut self, holder: ObjectId, prefix: &str, triggers: &[om_layout::ScriptTrigger]) {
        for trigger in triggers {
            let part = format!("{prefix}script trigger {}", trigger_name(trigger.event));
            match (trigger.external, trigger.file_reference, trigger.script) {
                (true, Some(source), script) => self.reference(
                    holder,
                    Place::new(&part),
                    Target::ExternalScript {
                        data_source: source,
                        script,
                    },
                ),
                (false, _, Some(script)) => self.reference(
                    holder,
                    Place::new(&part),
                    Target::Object(ObjectId::Script(script)),
                ),
                (false, _, None) => {}
                (true, None, _) => self.unread(
                    holder,
                    Place::new(&part),
                    UnreadReason::Malformed,
                    "a trigger's file reference that does not read".into(),
                    vec![Kind::DataSource],
                ),
            }
            if let Some(parameter) = &trigger.parameter {
                let place = Place::new(format!("{part} parameter"));
                match om_formula::polish::decode(&parameter.polish) {
                    Ok(expr) => {
                        self.context(holder, &place, parameter.context.as_deref());
                        self.expr(holder, &place, &expr);
                    }
                    Err(e) => self.unread(
                        holder,
                        place,
                        UnreadReason::Malformed,
                        format!("a formula that does not decode: {e}"),
                        formula::unreadable_kinds(),
                    ),
                }
            }
        }
    }
}

impl Builder {
    /// Scripts `[17, 5, S]`: each step of each script, folders left out.
    fn scripts(&mut self, tree: &KeyTree<'_>) {
        let mut scripts = Vec::new();
        for entry in members(tree, &[meta::SCRIPTS, 5]) {
            // A deleted script's member is emptied (scripts.md).
            if entry.children.is_empty() && entry.value.is_none() {
                continue;
            }
            let Some(id) = serial::decode(entry.segment.0).filter(|id| *id != 0) else {
                continue;
            };
            match Script::decode(entry) {
                Ok(script) if script.kind == ScriptKind::Script => {
                    self.object(ObjectId::Script(id), &script.name, "");
                    scripts.push(script);
                }
                Ok(_) => {}
                Err(e) => {
                    self.object(
                        ObjectId::Script(id),
                        text(entry, NAME).unwrap_or_default(),
                        "",
                    );
                    self.unread(
                        ObjectId::Script(id),
                        Place::new("definition"),
                        UnreadReason::Malformed,
                        format!("a script that does not decode: {e}"),
                        Kind::REFERABLE.to_vec(),
                    );
                }
            }
        }
        let names = GraphNames::new(&self.graph);
        for script in &scripts {
            self.script(script, &names);
        }
    }

    /// One script's steps, up to the text form's limit. Each step's line
    /// is the one `get_script`'s source text gives it: the lines of the
    /// steps before it (om-author's `source::script_text`).
    fn script(&mut self, script: &Script, names: &GraphNames) {
        let holder = ObjectId::Script(script.id);
        self.count("scripts");
        let mut line = 1;
        for (i, step) in script.steps.iter().take(MAX_PASTE_STEPS).enumerate() {
            let (text, _) = step_text_fidelity(step, names);
            let at = StepPlace {
                index: i + 1,
                line,
                id: step.id(),
                name: step.name(),
                enabled: step.is_enabled(),
            };
            steps::read(self, holder, Some(&at), "", step, false);
            self.count("steps");
            line += text.split('\n').count();
        }
        if script.steps.len() > MAX_PASTE_STEPS {
            self.unread(
                holder,
                Place::new(format!(
                    "steps {} to {}",
                    MAX_PASTE_STEPS + 1,
                    script.steps.len()
                )),
                UnreadReason::Limit,
                format!("only the first {MAX_PASTE_STEPS} steps of a script are read"),
                Kind::REFERABLE.to_vec(),
            );
        }
    }

    /// The file's options `[2, 18]` (file-metadata.md 3): the layout it
    /// opens on and its script triggers.
    fn file_options(&mut self, tree: &KeyTree<'_>) {
        if tree.at(&meta::DOCUMENT_PREFERENCES).is_none() {
            return;
        }
        let Some(options) = meta::file_options(tree) else {
            self.unread(
                ObjectId::File,
                Place::new("file options"),
                UnreadReason::Malformed,
                "document preferences that do not read".into(),
                vec![Kind::Layout, Kind::Script, Kind::Field],
            );
            return;
        };
        if let Some(layout) = options.default_layout() {
            self.reference(
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
                self.reference(
                    ObjectId::File,
                    Place::new(format!("file script trigger {name}")),
                    Target::Object(ObjectId::Script(script)),
                );
            }
        }
        // OnWindowTransaction names its field by name (file-metadata.md 3).
        if options.window_transaction.is_some() && !options.window_transaction_field.is_empty() {
            self.by_name(
                ObjectId::File,
                Place::new("file script trigger OnWindowTransaction: field name"),
                NameUse::FieldByName,
                None,
                vec![Kind::Field],
            );
        }
    }

    /// Custom menus `[65, 5, M]`: listed, their items not decoded.
    fn custom_menus(&mut self, tree: &KeyTree<'_>) {
        for entry in members(tree, &[CUSTOM_MENUS, 5]) {
            let Some(id) = entry.id() else {
                continue;
            };
            let holder = ObjectId::CustomMenu(id);
            self.object(holder, text(entry, NAME).unwrap_or_default(), "");
            self.unread(
                holder,
                Place::new("menu items"),
                UnreadReason::NotDecoded,
                "a custom menu's items and conditions are not decoded".into(),
                Kind::REFERABLE.to_vec(),
            );
        }
    }
}

/// The names a step's text shows, from the objects read: what numbering
/// a step's lines as `get_script` does needs.
struct GraphNames {
    calc: NameTable,
    layouts: BTreeMap<u64, String>,
    scripts: BTreeMap<u64, String>,
    tables: BTreeMap<u64, String>,
    data_sources: BTreeMap<u64, String>,
}

impl GraphNames {
    fn new(graph: &DesignGraph) -> GraphNames {
        let mut names = GraphNames {
            calc: NameTable::default(),
            layouts: BTreeMap::new(),
            scripts: BTreeMap::new(),
            tables: BTreeMap::new(),
            data_sources: BTreeMap::new(),
        };
        for object in graph.objects() {
            let name = object.name.clone();
            match object.id {
                ObjectId::Table(id) => {
                    names.tables.insert(id, name);
                }
                ObjectId::Field { table, field } => names.calc.add_field(table, field, &name),
                ObjectId::Occurrence(id) => {
                    if let Some(table) = graph.occurrence_table(id) {
                        names.calc.add_occurrence(id, &name, table);
                    }
                }
                ObjectId::Layout(id) => {
                    names.layouts.insert(id, name);
                }
                ObjectId::Script(id) => {
                    names.scripts.insert(id, name);
                }
                ObjectId::CustomFunction(id) => names.calc.add_custom_function(id, &name, 0),
                ObjectId::DataSource(id) => {
                    names.data_sources.insert(id, name);
                }
                _ => {}
            }
        }
        names
    }
}

impl StepNames for GraphNames {
    fn calc_names(&self) -> &dyn Names {
        &self.calc
    }
    fn layout_name(&self, id: u64) -> Option<&str> {
        self.layouts.get(&id).map(String::as_str)
    }
    fn script_name(&self, id: u64) -> Option<&str> {
        self.scripts.get(&id).map(String::as_str)
    }
    fn table_name(&self, id: u64) -> Option<&str> {
        self.tables.get(&id).map(String::as_str)
    }
    fn data_source_name(&self, id: u64) -> Option<&str> {
        self.data_sources.get(&id).map(String::as_str)
    }
}

/// A layout or object trigger's name by its event id (`TriggerEventID`,
/// script-runtime.md); the old view change id reads as OnViewChange, as
/// `om_layout::triggers` reads it.
fn trigger_name(event: u16) -> String {
    let name = match event {
        1 => "OnObjectEnter",
        2 => "OnObjectExit",
        3 => "OnObjectModify",
        4 => "OnObjectSave",
        5 => "OnObjectKeystroke",
        6 => "OnObjectValidate",
        7 => "OnPanelSwitch",
        8 => "OnObjectAVPlayerChange",
        101 => "OnLayoutEnter",
        102 => "OnLayoutKeystroke",
        103 => "OnRecordLoad",
        104 => "OnRecordCommit",
        105 => "OnRecordRevert",
        106 => "OnModeEnter",
        107 => "OnModeExit",
        108 | 110 => "OnViewChange",
        109 => "OnLayoutExit",
        111 => "OnLayoutSizeChange",
        112 => "OnGestureTap",
        113 => "OnExternalCommandReceived",
        other => return format!("event {other}"),
    };
    name.into()
}

/// A field of a stored spec (value lists), which names its occurrence.
fn steps_field(
    b: &mut Builder,
    holder: ObjectId,
    place: &Place,
    field: om_scripts::specs::SpecField,
) {
    match field.occurrence {
        Some(occurrence) => b.reference(
            holder,
            place.clone(),
            Target::Field {
                occurrence,
                field: field.field,
            },
        ),
        None => b.unread(
            holder,
            place.clone(),
            UnreadReason::Malformed,
            "a field named without its table occurrence".into(),
            vec![Kind::Field],
        ),
    }
}

/// A layout object's kind as results name it.
fn object_kind(kind: ObjectKind) -> String {
    match kind {
        ObjectKind::Field => "field".into(),
        ObjectKind::Text => "text".into(),
        ObjectKind::Graphic => "graphic".into(),
        ObjectKind::Line => "line".into(),
        ObjectKind::Rectangle => "rectangle".into(),
        ObjectKind::RoundedRectangle => "rounded_rectangle".into(),
        ObjectKind::Oval => "oval".into(),
        ObjectKind::Group => "group".into(),
        ObjectKind::Portal => "portal".into(),
        ObjectKind::Button => "button".into(),
        ObjectKind::TabControl => "tab_control".into(),
        ObjectKind::TabPanel => "tab_panel".into(),
        ObjectKind::External => "external".into(),
        ObjectKind::PopoverButton => "popover_button".into(),
        ObjectKind::Popover => "popover".into(),
        ObjectKind::SlideControl => "slide_control".into(),
        ObjectKind::SlidePanel => "slide_panel".into(),
        ObjectKind::ButtonBar => "button_bar".into(),
        ObjectKind::Unknown(n) => format!("unknown ({n})"),
    }
}

/// The members of the catalog at `path` (`[.., 5]`), but the `0` entry.
fn members<'t, 'a>(tree: &'t KeyTree<'a>, path: &[u64]) -> impl Iterator<Item = &'t Node<'a>> {
    tree.at(path)
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|n| n.id().is_some_and(|id| id != 0))
}

fn text(node: &Node<'_>, key: u64) -> Option<String> {
    text_at(node.get(key))
}

fn text_at(node: Option<&Node<'_>>) -> Option<String> {
    node.and_then(Node::text).and_then(Result::ok)
}
