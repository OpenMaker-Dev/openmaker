//! Layouts `[4, 5, L]` (layout-model.md): each layout's table occurrence,
//! parts and triggers, and each object's record (`om_layout`) and
//! attributes `[4, 5, L, 5, O]`, the attributes read key by key so that a
//! key OpenMaker does not read is reported, not skipped.

use om_hbam::{serial, KeyTree, Node};
use om_layout::{LayoutObject, ObjectKind};

use super::catalogs;
use super::read::{expect_empty, members, size, Builder, Found, MEMBER_STAMPS, NAME};
use super::steps;
use super::{Kind, NameUse, ObjectId, Place, Target, UnreadReason};
use crate::meta;

/// Keys of a layout member (layout-model.md 4, 10): its definition `02`,
/// parts `03`, object attributes `05`, object store `07`, theme `0b`,
/// table view `0d`, triggers `30` and view flags `f1`.
const DEFINITION: u64 = 0x02;
const PARTS: u64 = 0x03;
const ATTRIBUTES: u64 = 0x05;
const OBJECTS: u64 = 0x07;
const THEME: u64 = 0x0b;
const TABLE_VIEW: u64 = 0x0d;
const TRIGGERS: u64 = 0x30;
const VIEW_FLAGS: u64 = 0xf1;
/// The member's indexes over its objects: field bindings `01`, names
/// `08`, positions `0a` and tab order `15` (layout-model.md 10.5); its
/// member flags `f0` (10.6).
const FIELD_INDEX: u64 = 0x01;
const NAME_INDEX: u64 = 0x08;
const OBJECT_INDEX: u64 = 0x0a;
const TAB_ORDER_INDEX: u64 = 0x15;
const MEMBER_FLAGS: u64 = 0xf0;
/// Keys that are empty values on every stored layout (`16` written as
/// `00 00` by Layout Setup, `17` and `21`; layout-model.md 10.6, 10.14).
const EMPTY_KEYS: [u64; 3] = [0x16, 0x17, 0x21];
/// Keys of a part `[4, 5, L, 3, P]`: its definition `02` and its list of
/// objects `07` (layout-model.md 10.5).
const PART_DEFINITION: u64 = 0x02;
const PART_OBJECTS: u64 = 0x07;
/// Object attribute keys (layout-model.md 5.5, calc-encoding.md 8,
/// `om_layout::calcs`): the text `01`, label `02`, tooltip `03`, older
/// button text `04`, placeholder `06`, tab order `07`, portal sort `0a`,
/// format `0d`, step data or items `11`, filter `25`, triggers `30`,
/// calculated icon `5b`, conditional formatting `8a`, hide condition
/// `8d`, active segment `8e`, and the web address under `00 00`.
const TEXT: u64 = 0x01;
const LABEL: u64 = 0x02;
const TOOLTIP: u64 = 0x03;
const BUTTON_TEXT: u64 = 0x04;
const PLACEHOLDER: u64 = 0x06;
const TAB_ORDER: u64 = 0x07;
const PORTAL_SORT: u64 = 0x0a;
const FORMAT: u64 = 0x0d;
const ITEMS: u64 = 0x11;
const FILTER: u64 = 0x25;
const ICON: u64 = 0x5b;
const CONDITIONS: u64 = 0x8a;
const HIDE: u64 = 0x8d;
const ACTIVE_SEGMENT: u64 = 0x8e;
/// The web address's node `00 00` reads as id 0; its formula is `05`.
const WEB_ADDRESS: u64 = 0;
const WEB_ADDRESS_FORMULA: u64 = 5;
/// A conditional format's kind `01`, formula `05` and style `0d`.
const CONDITION_KIND: u64 = 0x01;
const CONDITION_FORMULA: u64 = 0x05;
const CONDITION_STYLE: u64 = 0x0d;
/// `06` in each conditional format, empty on every stored one, and the
/// count of formats `04` beside them.
const CONDITION_EMPTY: u64 = 0x06;
const CONDITION_COUNT: u64 = 0x04;
/// An item's formula `05`.
const ITEM_FORMULA: u64 = 0x05;
/// A trigger cache's records `05`, each with its record `01` and
/// parameter `31` (`om_layout::triggers`).
const TRIGGER_RECORDS: u64 = 0x05;
const TRIGGER_RECORD: u64 = 0x01;
const TRIGGER_PARAMETER: u64 = 0x31;
/// A trigger cache's count of triggers `04` (file-metadata.md 7.1).
const TRIGGER_COUNT: u64 = 0x04;
/// The object flag that turns the tooltip on (layout-model.md 5.1), and a
/// portal's flag that turns its stored sort on.
const TOOLTIP_FLAG: u64 = 0x4000;
const PORTAL_SORTED: i32 = 0x8;
/// Layout catalog flags of folders, their ends and separators.
const NOT_A_LAYOUT: u64 = 0x1 | 0x2 | 0x10;
/// A control token that opens a merge field in an object's text
/// (`LayoutObject::merge_field_specs`).
const MERGE_TOKEN: &str = "<<\u{1}";

/// Every layout of the catalog.
pub(super) fn layouts(b: &mut Builder, tree: &KeyTree<'_>) {
    // `om_hbam::catalog::layouts` copies every member's records at once:
    // its cost is charged first.
    if !b.spend(
        tree.at(&[4]).map_or(0, size),
        ObjectId::File,
        &Place::new("layouts"),
    ) {
        return;
    }
    catalogs::catalog(b, tree, &[4], "layout catalog", &[]);
    let layouts = match om_hbam::catalog::layouts(tree) {
        Ok(layouts) => layouts,
        Err(e) => {
            b.unavailable(
                vec![4],
                "the layout catalog",
                format!("it does not decode: {e}"),
            );
            return;
        }
    };
    for (id, entry) in members(tree, &[4, 5]) {
        match id {
            None => {
                b.lost(Kind::Layout);
                b.unread(
                    ObjectId::File,
                    Place::new(format!("layout {}", entry.segment)),
                    UnreadReason::Malformed,
                    "a layout whose key does not read",
                    &Kind::ANY,
                );
            }
            Some(id) if !layouts.iter().any(|l| l.id == id) && !catalogs::emptied(entry) => {
                catalogs::unlisted(b, ObjectId::Layout(id), "layout", Kind::Layout);
            }
            Some(_) => {}
        }
    }
    // Every layout is listed before any is read, so that a stop while
    // reading leaves the list whole.
    let layouts: Vec<&om_hbam::Layout> = layouts
        .iter()
        .filter(|raw| meta::layout_flags(tree, raw.id) & NOT_A_LAYOUT == 0)
        .collect();
    for raw in &layouts {
        b.object(ObjectId::Layout(raw.id), &raw.name, "");
        b.count("layouts");
    }
    b.inventoried(&[Kind::Layout]);
    for raw in &layouts {
        if b.stopped() {
            return;
        }
        let holder = ObjectId::Layout(raw.id);
        let Some(node) = tree.at(&[4, 5, raw.id]) else {
            b.unavailable(
                vec![4, 5, raw.id],
                &format!("layout {}", raw.id),
                "only the layout catalog's index lists it".into(),
            );
            continue;
        };
        if !b.charge(node, holder, &Place::new("definition")) {
            return;
        }
        match om_layout::Layout::decode_from_tree(tree, raw.id) {
            Ok(Some(layout)) => self::layout(b, holder, node, &layout),
            Ok(None) => b.unavailable(
                vec![4, 5, raw.id],
                &format!("layout {}", raw.id),
                "only the layout catalog's index lists it".into(),
            ),
            Err(e) => b.unread(
                holder,
                Place::new("definition"),
                UnreadReason::Malformed,
                format!("a layout that does not decode: {e}"),
                &Kind::ANY,
            ),
        }
    }
    b.inventoried(&[Kind::LayoutObject]);
}

/// A stored value that does not assemble (a piece missing), which a
/// decoder reading it takes for an absent one: unread.
fn unassembled(b: &mut Builder, holder: ObjectId, what: &str, node: Option<&Node<'_>>) -> bool {
    let broken = node.is_some_and(|n| n.assembled().is_none());
    if broken {
        b.unread(
            holder,
            Place::new(what),
            UnreadReason::Malformed,
            format!("the {what}, stored but not assembling"),
            &Kind::ANY,
        );
    }
    broken
}

/// The field reference index `01, <occurrence>, <field>, <object>`
/// (layout-model.md 10.5) names nothing the objects themselves do not:
/// it is accounted for only when every object it names was read with its
/// bindings. An entry for any other object, or of another shape, is
/// unread.
fn field_index(
    b: &mut Builder,
    holder: ObjectId,
    prefix: &str,
    index: Option<&Node<'_>>,
    read: &[u64],
) {
    let Some(index) = index else {
        return;
    };
    let stray = |b: &mut Builder, at: String| {
        b.unread(
            holder,
            Place::new(format!("{prefix}field index {at}")),
            UnreadReason::NotDecoded,
            "a field index entry OpenMaker does not read",
            &[Kind::Field, Kind::Occurrence],
        )
    };
    if index.value.is_some() {
        stray(b, "value".to_owned());
    }
    for occurrence in &index.children {
        if occurrence.value.is_some() || occurrence.children.is_empty() {
            stray(b, occurrence.segment.to_string());
        }
        for field in &occurrence.children {
            if field.value.is_some() || field.children.is_empty() {
                stray(b, format!("{}/{}", occurrence.segment, field.segment));
            }
            for object in &field.children {
                let named = serial::decode(object.segment.0);
                if object.children.is_empty() && named.is_some_and(|o| read.contains(&o)) {
                    continue;
                }
                b.unread(
                    holder,
                    Place::new(format!(
                        "{prefix}field index {}/{}/{}",
                        occurrence.segment, field.segment, object.segment
                    )),
                    UnreadReason::NotDecoded,
                    "a field index entry for an object that was not read",
                    &[Kind::Field, Kind::Occurrence],
                );
            }
        }
    }
}

fn layout(b: &mut Builder, holder: ObjectId, node: &Node<'_>, layout: &om_layout::Layout) {
    let mut member = b.member(holder, node, "layout");
    member.get(NAME);
    let definition = member.get(DEFINITION);
    unassembled(b, holder, "layout definition", definition);
    // A theme names styles; `01` indexes the objects' own field bindings,
    // `08` their names, `0a` their positions and `15` the tab order; `f0`
    // and `f1` are flags (layout-model.md 4.1, 7.5, 10.5).
    member.known(&[THEME, NAME_INDEX, OBJECT_INDEX, TAB_ORDER_INDEX]);
    member.known(&[MEMBER_FLAGS, VIEW_FLAGS]);
    member.known(&MEMBER_STAMPS);
    for key in EMPTY_KEYS {
        expect_empty(b, holder, "layout", key, member.get(key));
    }
    if let Some(def) = &layout.def {
        if let Some(occurrence) = Some(def.table_alias_key)
            .filter(|key| *key != 0)
            .and_then(om_layout::occurrence_id)
        {
            b.reference(
                holder,
                Place::new("table occurrence"),
                Target::Object(ObjectId::Occurrence(occurrence)),
            );
        }
        unknown_fields(b, holder, "layout definition", &def.other, &[]);
    }
    if let Some(parts) = member.get(PARTS) {
        self::parts(b, holder, "", parts);
    }
    if let Some(triggers) = member.get(TRIGGERS) {
        self::triggers(b, holder, "", triggers);
    }
    if let Some(table_view) = member.get(TABLE_VIEW) {
        self::table_view(b, holder, table_view);
    }
    let store = member.get(OBJECTS);
    unassembled(b, holder, "object store", store);
    unknown_fields(b, holder, "object store", &layout.objects.other, &[]);
    let attributes = member.get(ATTRIBUTES);
    let ObjectId::Layout(id) = holder else {
        return;
    };
    let objects: Vec<(ObjectId, &LayoutObject)> = layout
        .objects
        .objects
        .iter()
        .map(|o| {
            (
                ObjectId::LayoutObject {
                    layout: id,
                    object: o.id,
                },
                o,
            )
        })
        .collect();
    for (object_holder, object) in &objects {
        b.object(
            *object_holder,
            object.name.clone().unwrap_or_default(),
            object_kind(object.kind()),
        );
        b.count("layout_objects");
    }
    self::objects(b, holder, "", &objects, attributes);
    let read: Vec<u64> = layout.objects.objects.iter().map(|o| o.id).collect();
    let index = member.get(FIELD_INDEX);
    field_index(b, holder, "", index, &read);
    b.close(member);
}

/// Objects and their attributes `05, O`; `05, 00` is the last object key
/// issued (layout-model.md 10.5).
fn objects(
    b: &mut Builder,
    layout: ObjectId,
    prefix: &str,
    objects: &[(ObjectId, &LayoutObject)],
    attributes: Option<&Node<'_>>,
) {
    let mut attributed = Vec::new();
    for (holder, object) in objects {
        if !b.spend(1, *holder, &Place::new(format!("{prefix}object"))) {
            return;
        }
        let attribute = attributes.and_then(|a| a.child(&serial::encode(object.id)));
        if let Some(attribute) = attribute {
            attributed.push(attribute.segment.0);
        }
        self::object(b, *holder, prefix, object, attribute);
    }
    for attribute in attributes
        .map(|a| a.children.as_slice())
        .unwrap_or_default()
    {
        if attribute.id() == Some(0) || attributed.contains(&attribute.segment.0) {
            continue;
        }
        b.unread(
            layout,
            Place::new(format!("{prefix}attributes {}", attribute.segment)),
            UnreadReason::NotDecoded,
            "attributes of an object the layout's object store does not have",
            &Kind::ANY,
        );
    }
}

/// Parts `03, P`: each part's definition (its sub-summary break field)
/// and its list of objects; `03, 00` is the last part key issued
/// (layout-model.md 10.6).
fn parts(b: &mut Builder, holder: ObjectId, prefix: &str, parts: &Node<'_>) {
    for part_node in &parts.children {
        let Some(part_id) = part_node.id() else {
            b.unread(
                holder,
                Place::new(format!("{prefix}part {}", part_node.segment)),
                UnreadReason::Malformed,
                "a part whose key does not read",
                &Kind::ANY,
            );
            continue;
        };
        if part_id == 0 {
            continue;
        }
        let what = format!("{prefix}part {part_id}");
        let mut part_member = b.member(holder, part_node, what.clone());
        part_member.known(&[PART_OBJECTS]);
        part_member.known(&MEMBER_STAMPS);
        let stored = part_member.get(PART_DEFINITION);
        b.close(part_member);
        if unassembled(b, holder, &format!("{what} definition"), stored) {
            continue;
        }
        let Some(definition) = stored.and_then(Node::assembled) else {
            continue;
        };
        let part = match om_layout::Part::decode(part_id, &definition) {
            Ok(part) => part,
            Err(e) => {
                b.unread(
                    holder,
                    Place::new(what),
                    UnreadReason::Malformed,
                    format!("a part definition that does not decode: {e}"),
                    &[Kind::Field, Kind::Occurrence],
                );
                continue;
            }
        };
        match part.break_field() {
            Ok(Some(field)) => b.reference(
                holder,
                Place::new(format!("{what}: sorted by")),
                Target::Field {
                    occurrence: field.occurrence,
                    field: field.field,
                },
            ),
            Ok(None) => {}
            Err(e) => b.unread(
                holder,
                Place::new(what.clone()),
                UnreadReason::Malformed,
                e,
                &[Kind::Field, Kind::Occurrence],
            ),
        }
        unknown_fields(b, holder, &what, &part.other, &[]);
    }
}

/// A stored table view `0d` (layout-model.md 10.14): a layout of its own,
/// whose columns are field objects in its own object store `07`, with
/// their attributes `05` and parts `03`.
fn table_view(b: &mut Builder, holder: ObjectId, node: &Node<'_>) {
    let prefix = "table view: ";
    let mut member = b.member(holder, node, "table view");
    member.known(&[NAME_INDEX, OBJECT_INDEX, TAB_ORDER_INDEX]);
    member.known(&MEMBER_STAMPS);
    for key in EMPTY_KEYS {
        expect_empty(b, holder, "table view", key, member.get(key));
    }
    if let Some(parts) = member.get(PARTS) {
        self::parts(b, holder, prefix, parts);
    }
    let attributes = member.get(ATTRIBUTES);
    let stored = member.get(OBJECTS);
    let mut read = Vec::new();
    // Present but not assembling is unread, not "no objects".
    let store = if unassembled(b, holder, "table view: object store", stored) {
        None
    } else {
        stored
            .and_then(Node::assembled)
            .filter(|bytes| !bytes.is_empty())
    };
    match store.map(|bytes| om_layout::ObjectStore::decode(&bytes)) {
        Some(Ok(store)) => {
            unknown_fields(b, holder, "table view object store", &store.other, &[]);
            let objects: Vec<(ObjectId, &LayoutObject)> =
                store.objects.iter().map(|o| (holder, o)).collect();
            self::objects(b, holder, prefix, &objects, attributes);
            read = store.objects.iter().map(|o| o.id).collect();
        }
        Some(Err(e)) => b.unread(
            holder,
            Place::new("table view: objects"),
            UnreadReason::Malformed,
            format!("a table view's object store that does not decode: {e}"),
            &Kind::ANY,
        ),
        None => {
            // No objects: attributes can only be stale, which `objects`
            // reports.
            self::objects(b, holder, prefix, &[], attributes);
        }
    }
    let index = member.get(FIELD_INDEX);
    field_index(b, holder, prefix, index, &read);
    b.close(member);
}

/// One object: its record's references, then its attributes.
fn object(
    b: &mut Builder,
    holder: ObjectId,
    prefix: &str,
    object: &LayoutObject,
    attribute: Option<&Node<'_>>,
) {
    let place = |part: &str| Place::new(format!("{prefix}{part}"));
    if let ObjectKind::Unknown(kind) = object.kind() {
        b.unread(
            holder,
            place("object"),
            UnreadReason::NotDecoded,
            format!("an object of type {kind}, which OpenMaker does not know"),
            &Kind::ANY,
        );
    }
    // Text, shape and slide control and panel records hold only flags and
    // types (LayoutObject.proto 7, 9, 19, 20).
    unknown_fields(
        b,
        holder,
        &format!("{prefix}object"),
        &object.other,
        &[7, 9, 19, 20],
    );
    if let Some(field_obj) = &object.field_obj {
        unknown_fields(
            b,
            holder,
            &format!("{prefix}field object"),
            &field_obj.other,
            &[],
        );
        if let Some(spec) = &field_obj.field_spec {
            unknown_fields(
                b,
                holder,
                &format!("{prefix}field binding"),
                &spec.other,
                &[],
            );
            match (om_layout::occurrence_id(spec.table_key), spec.field_id()) {
                (Some(occurrence), Some(field)) if spec.table_key != 0 => {
                    b.reference(holder, place("field"), Target::Field { occurrence, field })
                }
                _ => b.unread(
                    holder,
                    place("field"),
                    UnreadReason::Malformed,
                    "a field binding whose keys do not read",
                    &[Kind::Field, Kind::Occurrence],
                ),
            }
        }
        if field_obj.value_list_key != 0 {
            match serial::raw_of_id(field_obj.value_list_key) {
                Some(list) => b.reference(
                    holder,
                    place("value list"),
                    Target::Object(ObjectId::ValueList(list)),
                ),
                None => b.unread(
                    holder,
                    place("value list"),
                    UnreadReason::Malformed,
                    "a value list key that does not read",
                    &[Kind::ValueList],
                ),
            }
        }
    }
    if let Some(portal) = &object.portal {
        unknown_fields(b, holder, &format!("{prefix}portal"), &portal.other, &[]);
        match om_layout::occurrence_id(portal.table_alias_key)
            .filter(|_| portal.table_alias_key != 0)
        {
            Some(occurrence) => b.reference(
                holder,
                place("portal occurrence"),
                Target::Object(ObjectId::Occurrence(occurrence)),
            ),
            None => b.unread(
                holder,
                place("portal occurrence"),
                UnreadReason::Malformed,
                "a portal occurrence key that does not read",
                &[Kind::Occurrence],
            ),
        }
    }
    if let Some(button) = &object.button {
        unknown_fields(b, holder, &format!("{prefix}button"), &button.other, &[]);
    }
    if let Some(group) = &object.group {
        unknown_fields(b, holder, &format!("{prefix}group"), &group.other, &[]);
    }
    let step = object.script_step();
    if let Some(stored) = step {
        unknown_fields(
            b,
            holder,
            &format!("{prefix}button step"),
            &stored.other,
            &[],
        );
        // The step's data is the object's attribute `11` (layout-model.md
        // 5.3.3), read here whatever the layout decoder filled in.
        let mut stored = stored.clone();
        stored.data = attribute
            .and_then(|a| a.get(ITEMS))
            .map(Node::to_owned_node);
        match om_scripts::ScriptStep::from_layout(&stored) {
            Some(step) => steps::read(
                b,
                holder,
                None,
                &format!("{prefix}button: "),
                &step,
                true,
                None,
            ),
            None => b.unread(
                holder,
                place("button"),
                UnreadReason::Malformed,
                "a button step that does not read",
                &Kind::ANY,
            ),
        }
    }
    if object.kind() == ObjectKind::External {
        match object.web_viewer() {
            // A web viewer whose page may call `FileMaker.PerformScript`
            // (layout-model.md 9.6).
            Some(options) if options.allows_javascript_scripts() => b.by_name(
                holder,
                place("web viewer: JavaScript"),
                NameUse::WebViewerScript,
                None,
                vec![Kind::Script],
            ),
            Some(_) => {}
            None => b.unread(
                holder,
                place("setup"),
                UnreadReason::NotDecoded,
                "a chart or plug-in object's setup",
                &Kind::ANY,
            ),
        }
    }
    if let Some(attribute) = attribute {
        let context = ObjectContext {
            object,
            has_step: step.is_some(),
        };
        attributes(b, holder, prefix, attribute, &context);
    }
}

/// What reading an object's attributes needs from its record.
struct ObjectContext<'o> {
    object: &'o LayoutObject,
    /// Its `11` is the button step's data, read with the step.
    has_step: bool,
}

/// An object's attributes `[4, 5, L, 5, O]`, key by key.
fn attributes(
    b: &mut Builder,
    holder: ObjectId,
    prefix: &str,
    node: &Node<'_>,
    object: &ObjectContext<'_>,
) {
    let place = |part: &str| Place::new(format!("{prefix}{part}"));
    let mut member = b.member(holder, node, format!("{prefix}object attributes"));
    // The name, the tab order, the text's format (layout-model.md 5.5).
    member.get(NAME);
    member.known(&[TAB_ORDER, FORMAT]);
    member.known(&MEMBER_STAMPS);
    let record = object.object;
    let calculated_icon = record.button.as_ref().is_some_and(|b| b.calculated_icon())
        || record
            .popover_button
            .as_ref()
            .is_some_and(|b| b.calculated_icon());
    for (key, part, active) in [
        (LABEL, "label calculation", true),
        (TOOLTIP, "tooltip", record.flags & TOOLTIP_FLAG != 0),
        (BUTTON_TEXT, "button text calculation", true),
        (PLACEHOLDER, "placeholder", true),
        (FILTER, "portal filter", true),
        (HIDE, "hide condition", true),
        // Drawn only with the calculated-icon flag (layout-model.md
        // 5.3.4); one stored without it is never evaluated.
        (ICON, "calculated icon", calculated_icon),
    ] {
        if let Some(formula) = member.get(key) {
            if active {
                b.formula(holder, place(part), formula);
            }
        }
    }
    if let Some(formula) = member.get(ACTIVE_SEGMENT) {
        b.formula(holder, place("active segment calculation"), formula);
        // Its result names the segment to make active.
        b.by_name(
            holder,
            place("active segment calculation"),
            NameUse::ObjectByName,
            None,
            vec![Kind::LayoutObject],
        );
    }
    if let Some(web) = member.get(WEB_ADDRESS) {
        let mut web_member = b.member(holder, web, format!("{prefix}web address"));
        if let Some(formula) = web_member.get(WEB_ADDRESS_FORMULA) {
            b.formula(holder, place("web address"), formula);
        }
        b.close(web_member);
    }
    if let Some(conditions) = member.get(CONDITIONS) {
        self::conditions(b, holder, prefix, conditions);
    }
    if let Some(sort) = member.get(PORTAL_SORT) {
        let sorted = record
            .portal
            .as_ref()
            .is_some_and(|p| p.flags & PORTAL_SORTED != 0);
        if sorted {
            let found = match sort
                .assembled()
                .and_then(|bytes| om_scripts::specs::SortSpec::decode(&bytes))
            {
                Some(sort) => steps::sort_found(&sort),
                None => vec![Found::unread(
                    UnreadReason::Malformed,
                    "a portal sort order that does not read",
                    &[Kind::Field, Kind::Occurrence, Kind::ValueList],
                )],
            };
            b.apply(holder, &place("portal sort"), &found);
        }
    }
    if let Some(items) = member.get(ITEMS) {
        if !object.has_step {
            for item in &items.children {
                let part = format!("item {}", item.segment);
                let mut item_member = b.member(holder, item, format!("{prefix}{part}"));
                if let Some(formula) = item_member.get(ITEM_FORMULA) {
                    b.formula(holder, place(&part), formula);
                }
                b.close(item_member);
            }
        }
    }
    if let Some(triggers) = member.get(TRIGGERS) {
        self::triggers(b, holder, prefix, triggers);
    }
    if let Some(text) = member.get(TEXT) {
        let decoded = record.merge_source.is_some() || record.text.is_some();
        if decoded {
            merge_fields(b, holder, &place("merge field"), record);
        } else if text.value.is_some_and(|v| !v.is_empty()) || !text.children.is_empty() {
            b.unread(
                holder,
                place("text"),
                UnreadReason::NotDecoded,
                "an object's text that OpenMaker does not decode",
                &[Kind::Field, Kind::Occurrence],
            );
        }
    }
    b.close(member);
}

/// Conditional formatting `8a`: each entry `n` (a list) with its kind
/// `01`, formula `05`, `06` (empty on every stored one) and style `0d`;
/// beside them the count `04` (a value).
fn conditions(b: &mut Builder, holder: ObjectId, prefix: &str, node: &Node<'_>) {
    for entry in &node.children {
        if entry.children.is_empty() {
            // The count beside the formats: a two-byte value on every
            // stored one (observed, not specified).
            let count =
                entry.id() == Some(CONDITION_COUNT) && entry.value.is_some_and(|v| v.len() == 2);
            if !count {
                b.unread(
                    holder,
                    Place::new(format!(
                        "{prefix}conditional formatting: key {}",
                        entry.segment
                    )),
                    UnreadReason::NotDecoded,
                    "a conditional formatting value OpenMaker does not read",
                    &Kind::ANY,
                );
            }
            continue;
        }
        let part = format!("conditional formatting {}", entry.segment);
        let mut member = b.member(holder, entry, format!("{prefix}{part}"));
        member.known(&[CONDITION_KIND, CONDITION_STYLE]);
        expect_empty(
            b,
            holder,
            &format!("{prefix}{part}"),
            CONDITION_EMPTY,
            member.get(CONDITION_EMPTY),
        );
        if let Some(formula) = member.get(CONDITION_FORMULA) {
            b.formula(holder, Place::new(format!("{prefix}{part}")), formula);
        }
        b.close(member);
    }
}

/// The merge fields of an object's text: each control token
/// (`LayoutObject::merge_field_specs`). A token that does not read is
/// unread.
fn merge_fields(b: &mut Builder, holder: ObjectId, place: &Place, object: &LayoutObject) {
    let specs = object.merge_field_specs();
    for spec in &specs {
        // Merge tokens carry raw catalog keys (`merge_field_specs`).
        b.reference(
            holder,
            place.clone(),
            Target::Field {
                occurrence: spec.table_key,
                field: spec.field_key,
            },
        );
    }
    let text = object.merge_source.as_deref().or(object.text.as_deref());
    let tokens = text.map_or(0, |t| t.matches(MERGE_TOKEN).count());
    if tokens > specs.len() {
        b.unread(
            holder,
            place.clone(),
            UnreadReason::Malformed,
            format!(
                "{} merge field tokens that do not read",
                tokens - specs.len()
            ),
            &[Kind::Field, Kind::Occurrence],
        );
    }
}

/// A trigger cache `30` (`om_layout::triggers`): each record's script,
/// read one by one so a record that does not decode is reported, and its
/// parameter.
fn triggers(b: &mut Builder, holder: ObjectId, prefix: &str, cache: &Node<'_>) {
    let mut member = b.member(holder, cache, format!("{prefix}script triggers"));
    member.known(&[TRIGGER_COUNT]);
    member.known(&MEMBER_STAMPS);
    if let Some(records) = member.get(TRIGGER_RECORDS) {
        for entry in &records.children {
            let event = entry.id();
            let part = format!(
                "{prefix}script trigger {}",
                event.map_or_else(|| entry.segment.to_string(), |e| trigger_name(e as u16))
            );
            let mut entry_member = b.member(holder, entry, part.clone());
            let record = entry_member
                .get(TRIGGER_RECORD)
                .and_then(Node::assembled)
                .map(|bytes| om_layout::triggers::decode_record(&bytes));
            match record {
                Some(Some(trigger)) => {
                    match (trigger.external, trigger.file_reference, trigger.script) {
                        (true, Some(source), script) => b.reference(
                            holder,
                            Place::new(&part),
                            Target::ExternalScript {
                                data_source: source,
                                script,
                            },
                        ),
                        (false, _, Some(script)) => b.reference(
                            holder,
                            Place::new(&part),
                            Target::Object(ObjectId::Script(script)),
                        ),
                        (false, _, None) => {}
                        (true, None, _) => b.unread(
                            holder,
                            Place::new(&part),
                            UnreadReason::Malformed,
                            "a trigger's file reference that does not read",
                            &[Kind::Script, Kind::DataSource],
                        ),
                    }
                }
                Some(None) | None => b.unread(
                    holder,
                    Place::new(&part),
                    UnreadReason::Malformed,
                    "a trigger record that does not read",
                    &[Kind::Script, Kind::DataSource],
                ),
            }
            if let Some(parameter) = entry_member.get(TRIGGER_PARAMETER) {
                b.formula(holder, Place::new(format!("{part} parameter")), parameter);
            }
            b.close(entry_member);
        }
    }
    b.close(member);
}

/// A stored record with fields its decoder does not know: each may hold a
/// reference, but those whose numbers the recovered `.proto` gives
/// messages without keys (`no_references`).
fn unknown_fields(
    b: &mut Builder,
    holder: ObjectId,
    what: &str,
    other: &[om_layout::RawField],
    no_references: &[u32],
) {
    let unknown: Vec<u32> = other
        .iter()
        .map(|f| f.number)
        .filter(|n| !no_references.contains(n))
        .collect();
    if !unknown.is_empty() {
        b.unread(
            holder,
            Place::new(what),
            UnreadReason::NotDecoded,
            format!("fields {unknown:?} of the {what}'s record, which OpenMaker does not decode"),
            &Kind::ANY,
        );
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
