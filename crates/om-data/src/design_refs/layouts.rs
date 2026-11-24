//! Layouts `[4, 5, L]` (layout-model.md): each layout's table occurrence,
//! parts and triggers, and each object's record (`om_layout`) and
//! attributes `[4, 5, L, 5, O]`, read through the access layer
//! ([`super::access`]): what a reader does not take is reported by the
//! sweep.

use om_hbam::serial;
use om_hbam::slot::be_uint;
use om_layout::{LayoutObject, ObjectKind};

use super::access::{At, Declared, Layer, Read};
use super::read::{stamps, Builder, Found, Structure};
use super::steps;
use super::{Kind, NameUse, ObjectId, Place, Target, UnreadReason};

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
/// Keys with no meaning in the specs, each in the one form every stored
/// layout and table view shows: `16` two zero bytes (as Layout Setup
/// writes it), `17` and `21` empty values (layout-model.md 10.6, 10.14).
const OBSERVED_KEYS: [(u64, &[u8]); 3] = [(0x16, &[0, 0]), (0x17, &[]), (0x21, &[])];
/// The one-byte values a field index entry holds besides the empty one,
/// under portals, merge texts and buttons (layout-model.md 10.5).
const INDEX_VALUES: [&[u8]; 6] = [&[], &[0x00], &[0x02], &[0x09], &[0x0a], &[0x0d]];
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

/// Every layout of the catalog: every member listed first, then each
/// layout read.
pub(super) fn layouts(b: &mut Builder, t: &Layer<'_, '_>) {
    // A catalog the source gave without this node holds no layouts.
    let Some(catalog) = t.at(&[4]) else {
        return;
    };
    if let Some(index) = catalog.get(1) {
        index.declare(Declared::CatalogIndex);
    }
    stamps(catalog);
    let list = catalog.get(5);
    let mut layouts = Vec::new();
    for (id, entry) in b.members(list) {
        let holder = ObjectId::Layout(id);
        if !b.spend(1, holder, &Place::new("layouts")) {
            return;
        }
        let name = b.name(entry, holder);
        // Member flags (file-metadata.md 1.1), also in the catalog's index.
        let flags = match entry.uint_at(MEMBER_FLAGS) {
            Read::Ok(flags) => flags,
            // Failed on its node: an anomaly, and not known to be a layout.
            Read::Unreadable => continue,
            Read::Absent => match t.at(&[4, 1, 7, id, MEMBER_FLAGS]).map(At::uint) {
                Some(Read::Ok(flags)) => flags,
                _ => 0,
            },
        };
        if flags & NOT_A_LAYOUT != 0 {
            folder(b, holder, entry);
            continue;
        }
        b.object(holder, &name, "");
        b.count("layouts");
        layouts.push((id, entry));
    }
    for (id, node) in layouts {
        if b.stopped() {
            return;
        }
        let holder = ObjectId::Layout(id);
        if !b.charge(node, holder, &Place::new("definition")) {
            return;
        }
        b.visit(Structure::Layout);
        index_entry(b, holder, t.at(&[4, 1, 7, id, DEFINITION]));
        match node.decode_layout(id) {
            Ok(Some(layout)) => self::layout(b, holder, node, &layout),
            Ok(None) => {
                b.unread_node(
                    node,
                    holder,
                    Place::new("definition"),
                    UnreadReason::Malformed,
                    "a layout the layout decoder does not find",
                    &Kind::ANY,
                );
            }
            Err(e) => {
                b.unread_node(
                    node,
                    holder,
                    Place::new("definition"),
                    UnreadReason::Malformed,
                    format!("a layout that does not decode: {e}"),
                    &Kind::ANY,
                );
            }
        }
    }
}

/// A layout folder, folder end or separator (member flags `01`, `02`,
/// `10`; file-metadata.md 1.1): not a layout and never shown, so what it
/// names is no use (OpenMaker's choice; a separator keeps a table
/// occurrence). It keeps a layout's keys with no objects, each read for
/// its form: the definition, view flags and theme as values, parts with
/// their definitions and object lists, an empty object store, the
/// object indexes, and a field index holding only the empty list
/// `01, 00`. Anything else is left to the sweep.
fn folder(b: &mut Builder, holder: ObjectId, entry: At<'_, '_>) {
    if !b.charge(entry, holder, &Place::new("folder")) {
        return;
    }
    b.visit(Structure::LayoutFolder);
    stamps(entry);
    for key in [DEFINITION, THEME, VIEW_FLAGS] {
        entry.take_at(key);
    }
    if let Some(Read::Ok(parts)) = entry.get(PARTS).map(At::children) {
        for part in parts {
            if part.id() == Some(0) {
                part.counter();
                continue;
            }
            part.take_at(PART_DEFINITION);
            part.take_at(PART_OBJECTS);
            stamps(part);
        }
    }
    if let Some(store) = entry.get(OBJECTS) {
        store.observed(&[]);
    }
    for key in [NAME_INDEX, OBJECT_INDEX, TAB_ORDER_INDEX] {
        if let Some(index) = entry.get(key) {
            index.declare(Declared::LayoutIndex);
        }
    }
    for (key, form) in OBSERVED_KEYS {
        b.observed(entry.get(key), form);
    }
    field_index(b, 0, &|_| holder, entry.get(FIELD_INDEX), &[]);
}

/// A layout's entry in the layout catalog's index `[4, 1, 07, L, 02]`:
/// its table occurrence and menu set (layout-model.md 10.2, 10.6). Derived
/// from the layout, but stored: its occurrence is read as a use, so an
/// index that disagrees with the layout cannot make the occurrence look
/// unused. The rest of the index is declared with it.
fn index_entry(b: &mut Builder, holder: ObjectId, entry: Option<At<'_, '_>>) {
    let Some(entry) = entry else {
        return;
    };
    b.visit(Structure::LayoutIndexEntry);
    let Read::Ok(bytes) = entry.bytes() else {
        return;
    };
    match om_layout::LayoutDef::decode(&bytes) {
        Ok(def) => {
            if let Some(occurrence) = Some(def.table_alias_key)
                .filter(|key| *key != 0)
                .and_then(om_layout::occurrence_id)
            {
                b.reference(
                    holder,
                    Place::new("layout index: table occurrence"),
                    Target::Object(ObjectId::Occurrence(occurrence)),
                );
            }
        }
        Err(e) => b.unread(
            holder,
            Place::new("layout index"),
            UnreadReason::Malformed,
            format!("a layout index entry that does not decode: {e}"),
            &Kind::ANY,
        ),
    }
}

/// Account for a list's counter `00`, the last key issued under it
/// (layout-model.md 10.5, 10.6), in its observed form.
fn counter(node: Option<At<'_, '_>>) {
    if let Some(counter) = node.and_then(|n| n.get(0)) {
        counter.counter();
    }
}

/// The field reference index `01, <occurrence>, <field>, <object>`
/// (layout-model.md 10.5): which objects show which fields. Derived from
/// the objects' bindings, but stored: each entry is read as a use of the
/// field it names through its occurrence (field `00`, under a portal, of
/// the occurrence), held by the object it names (`holder`) at the place
/// `field index`, so an index that disagrees with the bindings cannot make
/// a field look unused. An entry is taken only for an object that was
/// read, under keys that read, with a value in one of the forms observed;
/// any other is left to the sweep. A layout without references holds an
/// empty list at `01, 00`.
fn field_index(
    b: &mut Builder,
    _layout: u64,
    holder: &dyn Fn(u64) -> ObjectId,
    index: Option<At<'_, '_>>,
    read: &[u64],
) {
    let Some(index) = index else {
        return;
    };
    b.visit(Structure::FieldIndex);
    let Read::Ok(occurrences) = index.children() else {
        return;
    };
    for occurrence_node in occurrences {
        let Some(occurrence) = be_uint(occurrence_node.segment().0) else {
            continue;
        };
        let Read::Ok(fields) = occurrence_node.children() else {
            continue;
        };
        if occurrence == 0 {
            // The empty list a layout without references holds; anything
            // under it is left to the sweep.
            continue;
        }
        for field_node in fields {
            let Some(field) = be_uint(field_node.segment().0) else {
                continue;
            };
            let Read::Ok(objects) = field_node.children() else {
                continue;
            };
            for object_node in objects {
                let Some(object) = serial::decode(object_node.segment().0) else {
                    continue;
                };
                if !read.contains(&object) || !object_node.observed_any(&INDEX_VALUES) {
                    continue;
                }
                let target = if field == 0 {
                    Target::Object(ObjectId::Occurrence(occurrence))
                } else {
                    Target::Field { occurrence, field }
                };
                b.reference(holder(object), Place::new("field index"), target);
            }
        }
    }
}

fn layout(b: &mut Builder, holder: ObjectId, node: At<'_, '_>, layout: &om_layout::Layout) {
    let ObjectId::Layout(id) = holder else {
        return;
    };
    // A theme names styles; `08` indexes the objects' names, `0a` their
    // positions and `15` the tab order; `f1` holds view flags
    // (layout-model.md 4.1, 7.5, 10.5).
    for key in [THEME, VIEW_FLAGS] {
        if let Some(setting) = node.get(key) {
            setting.take();
        }
    }
    for key in [NAME_INDEX, OBJECT_INDEX, TAB_ORDER_INDEX] {
        if let Some(index) = node.get(key) {
            index.declare(Declared::LayoutIndex);
        }
    }
    stamps(node);
    for (key, form) in OBSERVED_KEYS {
        b.observed(node.get(key), form);
    }
    // The definition, read whole or failed on its node.
    let _ = node.bytes_at(DEFINITION);
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
    if let Some(parts) = node.get(PARTS) {
        self::parts(b, holder, "", parts);
    }
    if let Some(triggers) = node.get(TRIGGERS) {
        self::triggers(b, holder, "", triggers);
    }
    if let Some(table_view) = node.get(TABLE_VIEW) {
        self::table_view(b, holder, table_view);
    }
    // The object store: read whole or not at all. One that does not read
    // leaves the layout's objects unknown, so layout objects are not
    // listed whole.
    if matches!(node.bytes_at(OBJECTS), Read::Unreadable) {
        return;
    }
    unknown_fields(b, holder, "object store", &layout.objects.other, &[]);
    let attributes = node.get(ATTRIBUTES);
    counter(attributes);
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
    self::objects(b, "", &objects, attributes);
    let read: Vec<u64> = layout.objects.objects.iter().map(|o| o.id).collect();
    let object_holder = |object| ObjectId::LayoutObject { layout: id, object };
    field_index(b, id, &object_holder, node.get(FIELD_INDEX), &read);
}

/// Objects and their attributes `05, O`; `05, 00` is the last object key
/// issued (layout-model.md 10.5). Attributes of an object the store does
/// not have are left to the sweep.
fn objects(
    b: &mut Builder,
    prefix: &str,
    objects: &[(ObjectId, &LayoutObject)],
    attributes: Option<At<'_, '_>>,
) {
    // One map of the attributes, not a scan per object.
    let attributes = match attributes.map(At::by_segment) {
        Some(Read::Ok(map)) => map,
        _ => Default::default(),
    };
    for (holder, object) in objects {
        if !b.spend(1, *holder, &Place::new(format!("{prefix}object"))) {
            return;
        }
        let attribute = attributes
            .get(serial::encode(object.id).as_slice())
            .copied();
        self::object(b, *holder, prefix, object, attribute);
    }
}

/// Parts `03, P`: each part's definition (its sub-summary break field)
/// and its list of objects; `03, 00` is the last part key issued
/// (layout-model.md 10.6).
fn parts(b: &mut Builder, holder: ObjectId, prefix: &str, parts: At<'_, '_>) {
    b.visit(Structure::Part);
    counter(Some(parts));
    let Read::Ok(children) = parts.children() else {
        return;
    };
    for part_node in children {
        let Some(part_id) = part_node.id().filter(|id| *id != 0) else {
            continue;
        };
        let what = format!("{prefix}part {part_id}");
        if let Some(objects) = part_node.get(PART_OBJECTS) {
            objects.take();
        }
        stamps(part_node);
        let definition = match part_node.bytes_at(PART_DEFINITION) {
            Read::Ok(bytes) => bytes,
            Read::Absent | Read::Unreadable => continue,
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
fn table_view(b: &mut Builder, holder: ObjectId, node: At<'_, '_>) {
    b.visit(Structure::TableView);
    let prefix = "table view: ";
    for key in [NAME_INDEX, OBJECT_INDEX, TAB_ORDER_INDEX] {
        if let Some(index) = node.get(key) {
            index.declare(Declared::LayoutIndex);
        }
    }
    stamps(node);
    for (key, form) in OBSERVED_KEYS {
        b.observed(node.get(key), form);
    }
    if let Some(parts) = node.get(PARTS) {
        self::parts(b, holder, prefix, parts);
    }
    let attributes = node.get(ATTRIBUTES);
    counter(attributes);
    let mut read = Vec::new();
    match node.bytes_at(OBJECTS) {
        Read::Ok(bytes) if !bytes.is_empty() => match om_layout::ObjectStore::decode(&bytes) {
            Ok(store) => {
                unknown_fields(b, holder, "table view object store", &store.other, &[]);
                let objects: Vec<(ObjectId, &LayoutObject)> =
                    store.objects.iter().map(|o| (holder, o)).collect();
                self::objects(b, prefix, &objects, attributes);
                read = store.objects.iter().map(|o| o.id).collect();
            }
            Err(e) => {
                b.unread(
                    holder,
                    Place::new("table view: objects"),
                    UnreadReason::Malformed,
                    format!("a table view's object store that does not decode: {e}"),
                    &Kind::ANY,
                );
            }
        },
        // Not readable: failed on its node, and the sweep reports it.
        Read::Unreadable => {}
        Read::Ok(_) | Read::Absent => {}
    }
    field_index(b, 0, &|_| holder, node.get(FIELD_INDEX), &read);
}

/// One object: its record's references, then its attributes.
fn object(
    b: &mut Builder,
    holder: ObjectId,
    prefix: &str,
    object: &LayoutObject,
    attribute: Option<At<'_, '_>>,
) {
    let place = |part: &str| Place::new(format!("{prefix}{part}"));
    b.visit(Structure::LayoutObject);
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
        // 5.3.3), read here whatever the layout decoder filled in, and
        // accounted for once the step's schema has read it.
        let items = attribute.and_then(|a| a.get(ITEMS));
        let mut stored = stored.clone();
        stored.data = items.map(At::owned);
        match om_scripts::ScriptStep::from_layout(&stored) {
            Some(step) => {
                b.visit(Structure::ButtonStep);
                let consumed = steps::read(
                    b,
                    holder,
                    None,
                    &format!("{prefix}button: "),
                    &step,
                    true,
                    None,
                );
                // Taken whole only when the step's schema read all of it;
                // otherwise the step reported what it did not.
                if let Some(items) = items {
                    items.declare(if consumed {
                        Declared::StepData
                    } else {
                        Declared::Reported
                    });
                }
            }
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
            // A known structure OpenMaker does not decode: coverage that
            // could name anything, not an anomaly.
            None => b.undecoded(
                holder,
                place("setup"),
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
    node: At<'_, '_>,
    object: &ObjectContext<'_>,
) {
    let place = |part: &str| Place::new(format!("{prefix}{part}"));
    b.visit(Structure::Attributes);
    // The name, the tab order, the text's format (layout-model.md 5.5).
    let _ = b.name(node, holder);
    for key in [TAB_ORDER, FORMAT] {
        if let Some(setting) = node.get(key) {
            setting.take();
        }
    }
    stamps(node);
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
        if let Some(formula) = node.get(key) {
            if active {
                b.formula(holder, place(part), formula);
            } else {
                b.inactive_formula(holder, place(&format!("inactive {part}")), formula);
            }
        }
    }
    if let Some(formula) = node.get(ACTIVE_SEGMENT) {
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
    if let Some(web) = node.get(WEB_ADDRESS) {
        b.visit(Structure::WebAddress);
        if let Some(formula) = web.get(WEB_ADDRESS_FORMULA) {
            b.formula(holder, place("web address"), formula);
        }
    }
    if let Some(conditions) = node.get(CONDITIONS) {
        self::conditions(b, holder, prefix, conditions);
    }
    if let Some(sort) = node.get(PORTAL_SORT) {
        b.visit(Structure::PortalSort);
        let sorted = record
            .portal
            .as_ref()
            .is_some_and(|p| p.flags & PORTAL_SORTED != 0);
        if sorted {
            let found = match sort.bytes() {
                Read::Ok(bytes) => match om_scripts::specs::SortSpec::decode(&bytes) {
                    Some(sort) => steps::sort_found(&sort),
                    None => vec![Found::unread(
                        UnreadReason::Malformed,
                        "a portal sort order that does not read",
                        &[Kind::Field, Kind::Occurrence, Kind::ValueList],
                    )],
                },
                // Failed on its node; the sweep reports it.
                _ => Vec::new(),
            };
            b.apply(holder, &place("portal sort"), &found);
        } else {
            // Kept for a portal that does not sort: never used, but it
            // must still read.
            sort.take();
        }
    }
    if let Some(items) = node.get(ITEMS) {
        if !object.has_step {
            b.visit(Structure::Items);
            if let Read::Ok(items) = items.children() {
                for item in items {
                    let part = format!("item {}", item.segment());
                    if let Some(formula) = item.get(ITEM_FORMULA) {
                        b.formula(holder, place(&part), formula);
                    }
                }
            }
        }
    }
    if let Some(triggers) = node.get(TRIGGERS) {
        self::triggers(b, holder, prefix, triggers);
    }
    if let Some(text) = node.get(TEXT) {
        let decoded = record.merge_source.is_some() || record.text.is_some();
        if decoded {
            if let Read::Ok(_) = text.bytes() {
                merge_fields(b, holder, &place("merge field"), record);
            }
        } else if !text.is_empty() {
            b.unread_node(
                text,
                holder,
                place("text"),
                UnreadReason::NotDecoded,
                "an object's text that OpenMaker does not decode",
                &[Kind::Field, Kind::Occurrence],
            );
        }
    }
}

/// Conditional formatting `8a`: each format (a list keyed `00 n`) with its
/// kind `01`, formula `05`, `06` and style `0d`; beside them the count of
/// formats `04`. `06` and the count have no meaning in the specs and are
/// accepted only in the form every observation shows: `06` an empty value,
/// the count two bytes equal to the number of formats.
fn conditions(b: &mut Builder, holder: ObjectId, prefix: &str, node: At<'_, '_>) {
    b.visit(Structure::ConditionalFormat);
    let Read::Ok(entries) = node.children() else {
        return;
    };
    let formats: Vec<At<'_, '_>> = entries
        .iter()
        .copied()
        .filter(|e| e.segment().0.len() == 2)
        .collect();
    let count = u16::try_from(formats.len()).unwrap_or(u16::MAX);
    b.observed(node.get(CONDITION_COUNT), &count.to_be_bytes());
    for format in formats {
        let part = format!("conditional formatting {}", format.segment());
        for key in [CONDITION_KIND, CONDITION_STYLE] {
            if let Some(setting) = format.get(key) {
                setting.take();
            }
        }
        b.observed(format.get(CONDITION_EMPTY), &[]);
        if let Some(formula) = format.get(CONDITION_FORMULA) {
            b.formula(holder, Place::new(format!("{prefix}{part}")), formula);
        }
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
fn triggers(b: &mut Builder, holder: ObjectId, prefix: &str, cache: At<'_, '_>) {
    b.visit(Structure::Triggers);
    if let Some(count) = cache.get(TRIGGER_COUNT) {
        count.take();
    }
    stamps(cache);
    let Some(records) = cache.get(TRIGGER_RECORDS) else {
        return;
    };
    let Read::Ok(entries) = records.children() else {
        return;
    };
    for entry in entries {
        let event = entry.id();
        let part = format!(
            "{prefix}script trigger {}",
            event.map_or_else(|| entry.segment().to_string(), |e| trigger_name(e as u16))
        );
        let record = match entry.bytes_at(TRIGGER_RECORD) {
            Read::Ok(bytes) => Some(om_layout::triggers::decode_record(&bytes)),
            Read::Unreadable => Some(None),
            Read::Absent => None,
        };
        match record {
            Some(Some(trigger)) => match (trigger.external, trigger.file_reference, trigger.script)
            {
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
            },
            Some(None) | None => b.unread(
                holder,
                Place::new(&part),
                UnreadReason::Malformed,
                "a trigger record that does not read",
                &[Kind::Script, Kind::DataSource],
            ),
        }
        if let Some(parameter) = entry.get(TRIGGER_PARAMETER) {
            b.formula(holder, Place::new(format!("{part} parameter")), parameter);
        }
    }
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
