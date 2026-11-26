//! Adding a layout to the layout catalog `[4]` as FileMaker stores a new
//! one (docs/re/layout-model.md 10.6; schema-path.md 5.1, 5.6): a member
//! `[4, 5, L]` holding the layout's definition, one body part, an empty
//! object store and empty object indexes, its theme, name and view flags;
//! and in the catalog's index `[4, 1]` the last issued id, the name list,
//! the display order, the member count and the layout's entry. Everything
//! else in the catalog stays as it is, unknown keys included. OpenMaker
//! Server's authoring extension adds the UUIDs and the modification stamps
//! (schema-path.md 8.1) and saves the result; clients plan with the same
//! code, so a plan names the edit the host makes.
//!
//! The layout is a Form layout, as FileMaker Pro's New Layout/Report makes
//! one for a computer with Form chosen (it shows only in Form view), with
//! one empty body part: no fields are placed. Its theme and width are those
//! of a layout of the file (the one the user was on, or the first in the
//! display order that has a theme). The part set, the body's height, the
//! width and the theme are OpenMaker's choices: what FileMaker Pro writes
//! for them is not measured (layout-model.md 10.6).

use om_hbam::slot::{be_uint, uint_segment};
use om_hbam::{serial, OwnedNode};

use crate::field_catalog::{listed_key_or_why, name_key};

/// Keys of the layout catalog `[4]`: its index and members, and in the
/// index the last issued id, the name list, the display order, the member
/// count and the members' entries (schema-path.md 5.1, layout-model.md
/// 10.6).
const INDEX: u8 = 0x01;
const MEMBERS: u8 = 0x05;
const LAST_ID: u8 = 0x00;
const NAMES: u8 = 0x01;
const ORDER: u8 = 0x03;
const COUNT: u8 = 0x04;
const ENTRIES: u8 = 0x07;
/// Keys of a member (layout-model.md 10.5, 10.6).
const FIELD_REFERENCES: u8 = 0x01;
const DEFINITION: u8 = 0x02;
const PARTS: u8 = 0x03;
const OBJECTS: u8 = 0x07;
const NAMED_OBJECTS: u8 = 0x08;
const OBJECT_INDEX: u8 = 0x0a;
const THEME: u8 = 0x0b;
const NAME: u8 = 0x10;
const TAB_ORDER: u8 = 0x15;
const EMPTY_17: u8 = 0x17;
const EMPTY_21: u8 = 0x21;
const KIND: u8 = 0xf0;
/// Bits of a member's flags `f0`: a folder, a folder's end, a separator
/// (layout-model.md 10.6); without them the member is a layout.
const NOT_A_LAYOUT: u64 = 0x1 | 0x2 | 0x10;
/// The bit of `f0` set on a member Layout Setup leaves out of the layout
/// menus ("Include in layout menus" cleared; layout-model.md 10.11).
pub const MENU_HIDDEN: u64 = 0x08;
const VIEW_FLAGS: u8 = 0xf1;
/// A layout's stored table view (file-metadata.md 7.1).
const TABLE_VIEW: u8 = 0x0d;
/// A member's UUIDs (schema-path.md 8.1).
const UNIQUE_ID: u8 = 0xd8;
const SOURCE_ID: u8 = 0xd9;
/// Keys of a member's entry in the index besides its name and flags: the
/// occurrence and menu set, and a word FileMaker writes 0 for a new layout.
const ENTRY: u8 = 0x02;
const ENTRY_30: u8 = 0x30;
/// Keys of the part list: the last issued part key, and in a part its
/// definition and its list of objects.
const LAST_PART: u8 = 0x00;
const PART_DEFINITION: u8 = 0x02;
const PART_OBJECTS: u8 = 0x07;

/// The longest name list key FileMaker builds, its closing zero bytes
/// included (schema-path.md 5.7).
const MAX_NAME_KEY: usize = 210;

/// The longest layout name, in characters.
pub const MAX_NAME_CHARS: usize = 100;

/// The view flags (layout-model.md 4.2) of a layout shown only in Form
/// view: the flags FileMaker Pro saved for the starter solutions' Form
/// layouts (Contacts' Contact Details): List and Table view disabled, Form
/// the default.
pub const FORM_FLAGS: u64 = 0x10f3_080d;

/// The width of a new layout when no layout of the file gives one: every
/// layout of the six English starter solutions is this wide.
pub const DEFAULT_WIDTH: f64 = 1024.0;

/// The height of the new layout's body part (OpenMaker's choice).
pub const BODY_HEIGHT: f64 = 600.0;

/// The layout to add.
#[derive(Clone, Debug, PartialEq)]
pub struct NewLayout {
    pub name: String,
    /// The table occurrence it shows records of: its catalog id, the
    /// big-endian reading of its key (`0xd00002`).
    pub occurrence: u64,
    /// Its theme: the key of a theme of the file (`01` for theme 1), as a
    /// layout stores it in `[4, 5, L, 0b]` without the length byte.
    pub theme: Vec<u8>,
    /// Its width in points.
    pub width: f64,
}

/// A new layout as a window asks for it ([`crate::DataProvider::create_layout`]):
/// its name, the table occurrence it shows records of (its catalog id), and
/// the layout whose theme and width it takes (`None`: the first in the
/// display order that has a theme).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutRequest {
    pub name: String,
    pub occurrence: u64,
    pub like: Option<u32>,
}

/// A layout the host deleted, or found already deleted by a request whose
/// answer was lost (`changed` false).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemovedLayout {
    pub layout: u32,
    pub changed: bool,
}

/// A layout the host added, or found already added by a request whose
/// answer was lost (`changed` false).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreatedLayout {
    pub layout: u32,
    pub changed: bool,
}

/// Why a layout cannot be added; the catalog is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The name cannot be used, and why.
    Name(String),
    /// The request is not one this module makes, or the catalog lacks
    /// what the layout must be added to.
    Unsupported(String),
    /// A value out of range, such as a negative page margin.
    Invalid(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Name(reason) | Refusal::Unsupported(reason) | Refusal::Invalid(reason) => {
                f.write_str(reason)
            }
        }
    }
}

/// A layout planned into the catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    /// The layout's id as references give it: the big-endian reading of
    /// its key.
    pub layout: u64,
    /// Its key `[4, 5, L]`: the serial key after the last issued id.
    pub key: Vec<u8>,
    /// Its place in the display order.
    pub position: u32,
    /// The member `[4, 5, L]`, without UUIDs or stamps.
    pub member: OwnedNode,
    /// The index `[4, 1]` with the layout, without its stamp.
    pub index: OwnedNode,
    /// What changed: each changed key below `[4]` (as hex segments) and
    /// what it holds.
    pub changes: Vec<(String, String)>,
}

/// Check a new layout's name: at most [`MAX_NAME_CHARS`] characters, not
/// empty, no control characters, and a name list key OpenMaker can build
/// and FileMaker can store. Layouts may share a name (layout-model.md
/// 10.6). Returns its key in the name list
/// ([`crate::field_catalog::listed_key`]).
pub fn check_name(name: &str) -> Result<Vec<u8>, Refusal> {
    let refuse = |why: &str| {
        Err(Refusal::Name(format!(
            "“{name}” cannot be a layout name: {why}."
        )))
    };
    if name.trim().is_empty() {
        return Err(Refusal::Name("A layout needs a name.".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return refuse(&format!("names hold at most {MAX_NAME_CHARS} characters"));
    }
    if name.chars().any(char::is_control) {
        return refuse("it holds a control character or line break");
    }
    if name_key(name).is_empty() {
        return refuse("it has no character FileMaker tells names apart by");
    }
    let listed = match listed_key_or_why(name) {
        Ok(key) => key,
        Err(why) => return refuse(&why),
    };
    // The key and, for a name already listed, the id after it.
    if listed.len() + 8 > MAX_NAME_KEY {
        return refuse("its name key is longer than FileMaker stores");
    }
    Ok(listed)
}

/// A member's name, decoded.
pub fn member_name(member: &OwnedNode) -> Option<String> {
    member.child(&[NAME]).and_then(om_scripts::node_text)
}

/// The members of the layout catalog `[4]`: layouts, folders and
/// separators.
pub fn members(catalog: &OwnedNode) -> &[OwnedNode] {
    catalog
        .child(&[MEMBERS])
        .map_or(&[], |m| m.children.as_slice())
}

/// A member's theme key, without the length byte: `None` for a folder or
/// separator, which has none.
pub fn member_theme(member: &OwnedNode) -> Option<Vec<u8>> {
    let value = member.child(&[THEME])?.value.as_deref()?;
    let (&len, key) = value.split_first()?;
    (usize::from(len) == key.len() && !key.is_empty()).then(|| key.to_vec())
}

/// The theme and width a new layout takes from layout `like` of the
/// catalog, or, when it has no theme, from the first layout in the display
/// order that has one; `None` when no layout has a theme. The width is
/// [`DEFAULT_WIDTH`] when the layout's definition does not give one.
pub fn model(catalog: &OwnedNode, like: Option<u64>) -> Option<(Vec<u8>, f64)> {
    let member = |id: u64| catalog.at(&[&[MEMBERS], &uint_segment(id)]);
    let ordered = catalog
        .at(&[&[INDEX], &[ORDER]])
        .map(|order| {
            order
                .children
                .iter()
                .filter_map(|p| p.value.as_deref()?.get(1..).and_then(be_uint))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let source = like
        .into_iter()
        .chain(ordered)
        .filter_map(member)
        .find(|m| member_theme(m).is_some())?;
    let width = source
        .child(&[DEFINITION])
        .and_then(|d| d.assembled())
        .and_then(|d| om_layout::LayoutDef::decode(&d).ok())
        .map(|d| d.base_width)
        .filter(|w| w.is_finite() && *w > 0.0)
        .unwrap_or(DEFAULT_WIDTH);
    Some((member_theme(source)?, width))
}

/// The serial value `tableAliasKey` stores for occurrence `occurrence` (its
/// catalog id), as layout-model.md 4.1 names occurrences.
fn table_alias_key(occurrence: u64) -> Option<u64> {
    serial::decode(&uint_segment(occurrence))
}

/// A `[u8 length][serial key]` value.
fn key_string(key: &[u8]) -> Vec<u8> {
    let mut out = vec![key.len() as u8];
    out.extend(key);
    out
}

/// The serial value of a key string, `None` when it is not one.
fn key_string_value(value: &[u8]) -> Option<u64> {
    let (&len, key) = value.split_first()?;
    (usize::from(len) == key.len())
        .then(|| serial::decode(key))
        .flatten()
}

fn hex(segment: &[u8]) -> String {
    segment
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The data format record of a new table view column or field object: the
/// settings FileMaker Pro 26.0.3 stores for one, written field by field
/// (`om_layout::data_format::DataFormat::new_field`; layout-model.md 10.14).
pub(crate) fn new_column_format() -> Vec<u8> {
    om_layout::data_format::DataFormat::new_field()
        .encode()
        .expect("a new field's settings fit their record")
}
/// A layout's stored Table View layout `[4, 5, L, 0d]` (layout-model.md
/// 10.14), as FileMaker Pro 26 stores one on the layout of a file it makes
/// with Create New: a header part over a one-row body, one empty column
/// (an edit box showing no field) in the body, and the column's entries in
/// the field references, the body's object list, the object index and the
/// tab order. A layout with neither a field object nor this record makes
/// FileMaker Pro quit when it is shown as a table (#806). `occurrence` is
/// the layout's table occurrence.
///
/// The column's attribute record holds Pro's data format
/// ([`new_column_format`]): with columns that lack one, Pro emptied a view when
/// it saved the layout, and then quit on it (#886).
pub fn table_view(occurrence: u64) -> Result<OwnedNode, Refusal> {
    use crate::layout_placement::{object_index_entry, part_list_entry};
    use om_layout::encode;
    // Observed on FileMaker Pro 26.0.3's blank files: the header's height,
    // the column's width, and the body part's flags (meaning not decoded;
    // kept as observed). The row is the one Pro writes for OpenMaker
    // Plain's 16 pt edit box when it places a field (Pro's blank stores
    // 16 pt until then), with the header it writes then, Pro's blank's
    // 24 pt; a field OpenMaker places keeps them (#853, layout-model.md
    // 10.14).
    const HEADER_HEIGHT: f64 = 24.0;
    const ROW_HEIGHT: f64 = 21.0;
    const COLUMN_WIDTH: f64 = 120.0;
    const BODY_FLAGS: u64 = 0x200;
    const HEADER_PART: u8 = 1;
    const BODY_PART: u8 = 2;
    const COLUMN: u64 = 2;
    const ATTRIBUTES: u8 = 0x05;
    const LAST_OBJECT: u8 = 0x00;
    const FORMAT: u8 = 0x0d;

    let column = om_layout::Rect {
        top: HEADER_HEIGHT,
        left: 0.0,
        bottom: HEADER_HEIGHT + ROW_HEIGHT,
        right: COLUMN_WIDTH,
        other: Vec::new(),
    };
    let (index_key, index_value) = object_index_entry(column.top, column.left, 0, COLUMN)
        .map_err(|e| Refusal::Unsupported(e.to_string()))?;
    let part = |key: u8, def: Vec<u8>, objects: Vec<u8>| {
        OwnedNode::list(
            vec![key],
            vec![
                OwnedNode::leaf(vec![PART_DEFINITION], def),
                OwnedNode::leaf(vec![PART_OBJECTS], objects),
            ],
        )
    };
    let mut body_objects = 1u32.to_be_bytes().to_vec();
    body_objects.extend(part_list_entry(COLUMN));
    let column_key = serial::encode(COLUMN);

    let mut view = OwnedNode::list(vec![TABLE_VIEW], Vec::new());
    // The column shows field 0 of occurrence 0; the layout's own occurrence
    // has an empty list, as on a layout without fields.
    view.set_child(OwnedNode::list(
        vec![FIELD_REFERENCES],
        vec![
            OwnedNode::list(
                vec![0],
                vec![OwnedNode::list(
                    vec![0],
                    vec![OwnedNode::leaf(column_key.clone(), Vec::new())],
                )],
            ),
            OwnedNode::list(uint_segment(occurrence), Vec::new()),
        ],
    ));
    view.set_child(OwnedNode::list(
        vec![PARTS],
        vec![
            OwnedNode::leaf(vec![LAST_PART], key_string(&[BODY_PART])),
            part(
                HEADER_PART,
                encode::part_def(
                    encode::HEADER,
                    0,
                    0.0,
                    HEADER_HEIGHT,
                    encode::HEADER_STYLE,
                    encode::HEADER_STYLE,
                ),
                0u32.to_be_bytes().to_vec(),
            ),
            part(
                BODY_PART,
                encode::part_def(
                    encode::BODY,
                    BODY_FLAGS,
                    HEADER_HEIGHT,
                    ROW_HEIGHT,
                    encode::BODY_STYLE,
                    encode::BODY_ALT_STYLE,
                ),
                body_objects,
            ),
        ],
    ));
    view.set_child(OwnedNode::list(
        vec![ATTRIBUTES],
        vec![
            OwnedNode::leaf(vec![LAST_OBJECT], key_string(&column_key)),
            OwnedNode::list(
                column_key.clone(),
                vec![OwnedNode::leaf(vec![FORMAT], new_column_format())],
            ),
        ],
    ));
    view.set_child(OwnedNode::leaf(
        vec![OBJECTS],
        encode::append_to_store(
            &[],
            &[COLUMN],
            &[encode::empty_table_column(COLUMN, &column)],
        ),
    ));
    view.set_child(OwnedNode::list(vec![NAMED_OBJECTS], Vec::new()));
    view.set_child(OwnedNode::list(
        vec![OBJECT_INDEX],
        vec![OwnedNode::leaf(index_key, index_value.clone())],
    ));
    view.set_child(OwnedNode::list(
        vec![TAB_ORDER],
        vec![OwnedNode::leaf(1u16.to_be_bytes().to_vec(), index_value)],
    ));
    Ok(view)
}

/// Plan `layout` into `catalog`, the layout catalog `[4]` as stored.
///
/// The id is the serial key after the larger of the index's last issued id
/// and every member's, so an id is never issued twice; the layout goes
/// after every listed one in the display order. The index must hold a name
/// list, a display order and a member count (a file FileMaker made always
/// has them). A name another layout has is kept: the name list entry then
/// ends with the new layout's id, as FileMaker lists a second layout of the
/// same name (layout-model.md 10.6).
pub fn plan(catalog: &OwnedNode, layout: &NewLayout) -> Result<Planned, Refusal> {
    let key = check_name(&layout.name)?;
    let alias = table_alias_key(layout.occurrence).ok_or_else(|| {
        Refusal::Unsupported(format!(
            "{:#x} is not a table occurrence's key.",
            layout.occurrence
        ))
    })?;
    if layout.theme.is_empty() || serial::decode(&layout.theme).is_none() {
        return Err(Refusal::Unsupported(
            "The layout needs a theme of the file.".into(),
        ));
    }
    if !(layout.width.is_finite() && layout.width >= 1.0 && layout.width <= 32_000.0) {
        return Err(Refusal::Unsupported(format!(
            "A layout is 1 to 32,000 points wide, not {}.",
            layout.width
        )));
    }
    let slot = slot(catalog, key)?;
    let layout_key = slot.key.clone();

    let flags = FORM_FLAGS.to_be_bytes().to_vec();
    let definition = om_layout::encode::new_layout_def(alias, layout.width);
    let body = om_layout::encode::new_body_part(BODY_HEIGHT);
    let mut member = OwnedNode::list(layout_key.clone(), Vec::new());
    member.set_child(OwnedNode::list(
        vec![FIELD_REFERENCES],
        vec![OwnedNode::list(uint_segment(layout.occurrence), Vec::new())],
    ));
    member.set_child(OwnedNode::leaf(vec![DEFINITION], definition.clone()));
    member.set_child(OwnedNode::list(
        vec![PARTS],
        vec![
            OwnedNode::leaf(vec![LAST_PART], key_string(&[1])),
            OwnedNode::list(
                vec![1],
                vec![
                    OwnedNode::leaf(vec![PART_DEFINITION], body),
                    OwnedNode::leaf(vec![PART_OBJECTS], 0u32.to_be_bytes().to_vec()),
                ],
            ),
        ],
    ));
    member.set_child(OwnedNode::leaf(vec![OBJECTS], Vec::new()));
    member.set_child(OwnedNode::list(vec![NAMED_OBJECTS], Vec::new()));
    member.set_child(OwnedNode::list(vec![OBJECT_INDEX], Vec::new()));
    member.set_child(OwnedNode::leaf(vec![THEME], key_string(&layout.theme)));
    member.set_child(om_scripts::text_node(&[NAME], &layout.name));
    member.set_child(OwnedNode::list(vec![TAB_ORDER], Vec::new()));
    member.set_child(OwnedNode::leaf(vec![EMPTY_17], Vec::new()));
    member.set_child(OwnedNode::leaf(vec![EMPTY_21], Vec::new()));
    member.set_child(OwnedNode::leaf(vec![VIEW_FLAGS], flags.clone()));
    // A stored Table View layout with its empty column, as FileMaker Pro's
    // Create New stores on its blank layout: without it, a layout with no
    // field object makes Pro quit when shown as a table (#806, #814).
    member.set_child(table_view(layout.occurrence)?);

    let mut entry = OwnedNode::list(layout_key.clone(), Vec::new());
    entry.set_child(OwnedNode::leaf(
        vec![ENTRY],
        om_layout::encode::layout_entry(alias, 0),
    ));
    entry.set_child(om_scripts::text_node(&[NAME], &layout.name));
    entry.set_child(OwnedNode::leaf(vec![ENTRY_30], 0u32.to_be_bytes().to_vec()));
    entry.set_child(OwnedNode::leaf(vec![KIND], 0u64.to_be_bytes().to_vec()));
    entry.set_child(OwnedNode::leaf(vec![VIEW_FLAGS], flags));

    let mut changes = vec![(
        format!("[04, 05, {}]", hex(&layout_key)),
        format!(
            "the layout: Form view, a body part {BODY_HEIGHT} points high, no objects, \
             a stored Table View layout with an empty column, occurrence {:#x}, theme {}, \
             {} points wide",
            layout.occurrence,
            hex(&layout.theme),
            layout.width
        ),
    )];
    let index = add_to_index(
        catalog,
        &slot,
        entry,
        "the layout's entry: occurrence, name and view flags",
        &mut changes,
    );
    Ok(Planned {
        layout: slot.raw,
        key: layout_key,
        position: slot.position,
        member,
        index,
        changes,
    })
}

/// Where a new member goes in the layout catalog's index: its id and key,
/// its name list key, its display position and the member count after it.
struct Slot {
    id: u64,
    key: Vec<u8>,
    raw: u64,
    listed: Vec<u8>,
    position: u32,
    count: u32,
}

/// The slot of a new member whose name is listed under `listed` in
/// `catalog`. The id is the serial key after the larger of the index's last
/// issued id and every member's, so an id is never issued twice; the member
/// goes after every listed one in the display order. The index must hold a
/// name list, a display order and a member count (a file FileMaker made
/// always has them). A name another layout has is kept: the name list
/// entry then ends with the new layout's id, as FileMaker lists a second
/// layout of the same name (layout-model.md 10.6).
fn slot(catalog: &OwnedNode, mut listed: Vec<u8>) -> Result<Slot, Refusal> {
    let index = catalog.child(&[INDEX]).ok_or_else(|| {
        Refusal::Unsupported(
            "The file's layout catalog has no index [4, 1] (every file FileMaker made has \
             one); OpenMaker does not build one."
                .into(),
        )
    })?;
    let missing = |what: &str| {
        Refusal::Unsupported(format!(
            "The layout catalog's index has no {what}; OpenMaker does not rebuild one."
        ))
    };
    for (list, what) in [
        (NAMES, "name list"),
        (ORDER, "display order"),
        (ENTRIES, "layout entries"),
    ] {
        if index.child(&[list]).is_none() {
            return Err(missing(what));
        }
    }

    // The id: after every one issued.
    let last = match index.child(&[LAST_ID]) {
        Some(node) => node
            .value
            .as_deref()
            .and_then(key_string_value)
            .ok_or_else(|| missing("readable last issued id"))?,
        None => 0,
    };
    let highest = members(catalog)
        .iter()
        .chain(index.child(&[ENTRIES]).map_or(&[][..], |e| &e.children[..]))
        .filter_map(|m| serial::decode(&m.segment))
        .max()
        .unwrap_or(0);
    let id = last.max(highest) + 1;
    let key = serial::encode(id);
    let raw = be_uint(&key).ok_or_else(|| missing("layout id that fits in 8 bytes"))?;

    // The position after every listed one, and the count.
    let positions = index
        .child(&[ORDER])
        .map(|order| {
            order
                .children
                .iter()
                .map(|p| {
                    <[u8; 4]>::try_from(p.segment.as_slice())
                        .map(u32::from_be_bytes)
                        .map_err(|_| missing("display order of 4-byte positions"))
                })
                .collect::<Result<Vec<u32>, Refusal>>()
        })
        .transpose()?
        .unwrap_or_default();
    let position = positions
        .into_iter()
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| missing("display position after the last"))?;
    let count = match index.child(&[COUNT]) {
        Some(node) => node
            .value
            .as_deref()
            .and_then(|v| <[u8; 4]>::try_from(v).ok())
            .map(u32::from_be_bytes)
            .ok_or_else(|| missing("readable member count"))?,
        None => return Err(missing("member count")),
    };
    let count = count
        .checked_add(1)
        .ok_or_else(|| missing("member count below the largest"))?;

    // The name list key: the name's, or for a name already listed, its
    // followed by the new id.
    let names = index.child(&[NAMES]).expect("checked above");
    if names.child(&listed).is_some() {
        listed.extend(&key);
        if names.child(&listed).is_some() {
            return Err(missing("free name list key for the layout"));
        }
    }
    Ok(Slot {
        id,
        key,
        raw,
        listed,
        position,
        count,
    })
}

/// `catalog`'s index with a new member at `slot` and its entry `entry`
/// (`what` names it): the last issued id, the name list, the display order,
/// the count, every other key and byte kept.
fn add_to_index(
    catalog: &OwnedNode,
    slot: &Slot,
    entry: OwnedNode,
    what: &str,
    changes: &mut Vec<(String, String)>,
) -> OwnedNode {
    let mut next = catalog
        .child(&[INDEX])
        .cloned()
        .unwrap_or_else(|| OwnedNode::list(vec![INDEX], Vec::new()));
    let id_value = key_string(&slot.key);
    let mut index_entry = |path: &[&[u8]], node: OwnedNode, what: String| {
        changes.push((
            format!(
                "[04, 01, {}]",
                path.iter().map(|s| hex(s)).collect::<Vec<_>>().join(", ")
            ),
            what,
        ));
        next.put(path, node);
    };
    index_entry(
        &[&[LAST_ID]],
        OwnedNode::leaf(Vec::new(), id_value.clone()),
        format!("last issued id {}", slot.id),
    );
    index_entry(
        &[&[NAMES], &slot.listed],
        OwnedNode::leaf(Vec::new(), id_value.clone()),
        "the name in the name list".into(),
    );
    index_entry(
        &[&[ORDER], &slot.position.to_be_bytes()],
        OwnedNode::leaf(Vec::new(), id_value),
        format!("display position {}", slot.position),
    );
    index_entry(
        &[&[COUNT]],
        OwnedNode::leaf(Vec::new(), slot.count.to_be_bytes().to_vec()),
        format!("{} layouts", slot.count),
    );
    index_entry(&[&[ENTRIES], &slot.key], entry, what.into());
    next
}

/// The library items layout `member`'s objects show, each with the number
/// of objects showing it: the uses FileMaker counts (layout-model.md 10.8).
/// A layout whose own styles, parts' styles or objects' styles show a
/// library image is refused: how FileMaker counts those uses is not known.
fn library_uses(member: &OwnedNode) -> Result<std::collections::BTreeMap<u64, u32>, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    let image = || {
        unsupported(
            "The layout's styles show an image from the file's library; FileMaker counts each \
             use of a library image, and how it counts this one is not known, so OpenMaker \
             does not change the layout's place in the library's counts."
                .into(),
        )
    };
    let def =
        crate::layout_placement::definition(member).map_err(|r| unsupported(r.to_string()))?;
    let styles = [&def.background_style, &def.box_style, &def.table_grid_style];
    if styles
        .iter()
        .filter_map(|s| s.as_ref()?.local_styles.as_ref())
        .any(crate::layout_objects::style_image)
        || def.local_styles.as_ref().is_some_and(|theme| {
            theme
                .type_styles
                .iter()
                .chain(&theme.custom_styles)
                .chain(&theme.deleted_custom_styles)
                .filter_map(|t| t.styles.as_ref())
                .any(crate::layout_objects::style_image)
        })
    {
        return Err(image());
    }
    for part in crate::layout_placement::parts(member).map_err(|r| unsupported(r.to_string()))? {
        if [&part.style, &part.alt_style]
            .iter()
            .filter_map(|s| s.as_ref()?.local_styles.as_ref())
            .any(crate::layout_objects::style_image)
        {
            return Err(image());
        }
    }
    let (_, store) =
        crate::layout_placement::stored_objects(member).map_err(|r| unsupported(r.to_string()))?;
    let mut library = std::collections::BTreeMap::new();
    for object in &store.objects {
        if object
            .style
            .as_ref()
            .and_then(|s| s.local_styles.as_ref())
            .is_some_and(crate::layout_objects::style_image)
        {
            return Err(image());
        }
        for item in crate::layout_objects::library_keys(object) {
            let uses: &mut u32 = library.entry(item).or_insert(0);
            *uses = uses.saturating_add(1);
        }
    }
    // The stored table view's objects (file-metadata.md 7.1): how FileMaker
    // counts their library uses is not known, so a table view that shows a
    // library image is refused too.
    if let Some(view) = member.child(&[TABLE_VIEW]) {
        let (_, view_objects) = crate::layout_placement::stored_objects(view)
            .map_err(|r| unsupported(r.to_string()))?;
        if view_objects
            .objects
            .iter()
            .any(crate::layout_objects::uses_library)
        {
            return Err(image());
        }
    }
    Ok(library)
}

/// A layout deleted as FileMaker Pro's Delete Layout does
/// ([`plan_delete`]): the index without it, the member's key, the library
/// uses its objects release, and the layout the window shows next.
#[derive(Clone, Debug, PartialEq)]
pub struct DeletedLayout {
    /// The deleted layout's key `[4, 5, L]`.
    pub key: Vec<u8>,
    /// The index `[4, 1]` without the layout, without its stamp.
    pub index: OwnedNode,
    /// The library items its objects showed, each with its number of uses.
    pub library: std::collections::BTreeMap<u64, u32>,
    /// What changed: each changed key below `[4]` and what it holds.
    pub changes: Vec<(String, String)>,
}

/// Plan deleting layout `layout` (its id) from `catalog`, the layout
/// catalog `[4]` as stored, as FileMaker Pro's Delete Layout writes it
/// (layout-model.md 10.10): its member goes; in the index its name list
/// entry, its display position (the later positions close up, as Pro's
/// do), its entry, and the count lowered; the last issued id is kept. The
/// library uses its objects release ([`crate::library_uses::release`]) are
/// returned for the host to write.
///
/// A folder or separator is refused, and so is the file's last layout (a
/// file keeps one) and a layout whose styles show a library image.
/// References to the layout elsewhere (scripts, buttons, the file's options)
/// are kept as stored, as Pro's deletion keeps them.
pub fn plan_delete(catalog: &OwnedNode, layout: u64) -> Result<DeletedLayout, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    let key = uint_segment(layout);
    let member = catalog
        .at(&[&[MEMBERS], &key])
        .ok_or_else(|| unsupported(format!("The file has no layout {layout}.")))?;
    let index = catalog
        .child(&[INDEX])
        .ok_or_else(|| unsupported("The file's layout catalog has no index [4, 1].".into()))?;
    let entries = index
        .child(&[ENTRIES])
        .ok_or_else(|| unsupported("The layout catalog's index has no entries.".into()))?;
    let kind = |entry: &OwnedNode| {
        entry
            .child(&[KIND])
            .and_then(|n| n.value.as_deref())
            .and_then(be_uint)
            .unwrap_or(0)
    };
    let entry = entries.child(&key).ok_or_else(|| {
        unsupported(format!(
            "The layout catalog's index has no entry for layout {layout}."
        ))
    })?;
    if kind(entry) & NOT_A_LAYOUT != 0 || member.child(&[DEFINITION]).is_none() {
        return Err(unsupported(
            "This member of the layout catalog is a folder or a separator; Delete Layout \
             deletes layouts."
                .into(),
        ));
    }
    if entries
        .children
        .iter()
        .filter(|e| kind(e) & NOT_A_LAYOUT == 0)
        .count()
        < 2
    {
        return Err(unsupported(
            "This is the file's only layout; a file keeps at least one.".into(),
        ));
    }
    let library = library_uses(member)?;
    let id_value = key_string(&key);
    let mut next = index.clone();
    let mut changes = vec![(
        format!("[04, 05, {}]", hex(&key)),
        format!("layout {layout} removed"),
    )];
    // Its name list entries (the name's key, or the name's key and its id).
    if let Some(names) = next.child(&[NAMES]) {
        let mut names = names.clone();
        let before = names.children.len();
        names
            .children
            .retain(|n| n.value.as_deref() != Some(id_value.as_slice()));
        if names.children.len() != before {
            changes.push(("[04, 01, 01]".into(), "its name list entry removed".into()));
            next.set_child(names);
        }
    }
    // Its display position; the later ones close up.
    let order = next
        .child(&[ORDER])
        .ok_or_else(|| unsupported("The layout catalog's index has no display order.".into()))?;
    let mut positions = Vec::with_capacity(order.children.len());
    for p in &order.children {
        let at = <[u8; 4]>::try_from(p.segment.as_slice())
            .map(u32::from_be_bytes)
            .map_err(|_| unsupported("The display order does not read.".into()))?;
        positions.push((at, p.clone()));
    }
    let removed = positions
        .iter()
        .find(|(_, p)| p.value.as_deref() == Some(id_value.as_slice()))
        .map(|(at, _)| *at);
    let mut order = OwnedNode {
        children: Vec::new(),
        ..order.clone()
    };
    for (at, mut position) in positions {
        match removed {
            Some(gone) if at == gone => continue,
            Some(gone) if at > gone => position.segment = (at - 1).to_be_bytes().to_vec(),
            _ => {}
        }
        order.set_child(position);
    }
    if let Some(gone) = removed {
        changes.push((
            "[04, 01, 03]".into(),
            format!("display position {gone} removed, the later ones moved up"),
        ));
    }
    next.set_child(order);
    // The count, and its entry.
    let count = next
        .child(&[COUNT])
        .and_then(|n| n.value.as_deref())
        .and_then(|v| <[u8; 4]>::try_from(v).ok())
        .map(u32::from_be_bytes)
        .and_then(|c| c.checked_sub(1))
        .ok_or_else(|| unsupported("The layout catalog's count does not read.".into()))?;
    next.set_child(OwnedNode::leaf(vec![COUNT], count.to_be_bytes().to_vec()));
    next.remove(&[&[ENTRIES], &key]);
    changes.push(("[04, 01, 04]".into(), format!("{count} layouts")));
    changes.push((
        format!("[04, 01, 07, {}]", hex(&key)),
        "its entry removed".into(),
    ));
    Ok(DeletedLayout {
        key,
        index: next,
        library,
        changes,
    })
}

/// A layout duplicated as FileMaker Pro's Duplicate Layout does
/// ([`plan_duplicate`]): the plan, and the library items its objects show,
/// each with the number of objects showing it, whose use counts the host
/// raises (layout-model.md 10.8, 10.9).
#[derive(Clone, Debug, PartialEq)]
pub struct Duplicated {
    pub planned: Planned,
    pub library: std::collections::BTreeMap<u64, u32>,
}

/// The name Duplicate Layout gives a copy of the layout named `name`.
pub fn copy_name(name: &str) -> String {
    format!("{name} Copy")
}

/// Plan a copy of layout `source` (its id) into `catalog`, the layout
/// catalog `[4]` as stored, as FileMaker Pro's Duplicate Layout writes one
/// (layout-model.md 10.9): a member that is the source's with its name
/// [`copy_name`] and without its UUIDs `d8` and `d9` (the host gives it a
/// new `d8`, and each object a new `d8`), and in the index an entry that is
/// the source's with that name, at the end of the display order. Every
/// other key and byte of the source is copied as stored, its stamps
/// included (the host stamps the copy).
///
/// A folder or separator is refused, and so is a layout whose own styles,
/// parts' styles or objects' styles hold an image from the file's library:
/// FileMaker counts each use of a library item, and how it counts those is
/// not known.
pub fn plan_duplicate(catalog: &OwnedNode, source: u64) -> Result<Duplicated, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    let source_key = uint_segment(source);
    let member = catalog
        .at(&[&[MEMBERS], &source_key])
        .ok_or_else(|| unsupported(format!("The file has no layout {source}.")))?;
    let entry = catalog
        .at(&[&[INDEX], &[ENTRIES], &source_key])
        .ok_or_else(|| {
            unsupported(format!(
                "The layout catalog's index has no entry for layout {source}."
            ))
        })?;
    let kind = entry
        .child(&[KIND])
        .and_then(|n| n.value.as_deref())
        .and_then(be_uint)
        .unwrap_or(0);
    if kind & NOT_A_LAYOUT != 0 || member.child(&[DEFINITION]).is_none() {
        return Err(unsupported(
            "This member of the layout catalog is a folder or a separator; Duplicate Layout \
             copies layouts."
                .into(),
        ));
    }
    let source_name = member_name(member)
        .ok_or_else(|| unsupported(format!("OpenMaker cannot read layout {source}'s name.")))?;
    let name = copy_name(&source_name);
    let key = check_name(&name)?;

    let library = library_uses(member)?;

    let slot = slot(catalog, key)?;
    let mut copy = member.clone();
    copy.segment = slot.key.clone();
    copy.set_child(om_scripts::text_node(&[NAME], &name));
    copy.children
        .retain(|c| c.segment != [UNIQUE_ID] && c.segment != [SOURCE_ID]);
    let mut copied_entry = entry.clone();
    copied_entry.segment = slot.key.clone();
    copied_entry.set_child(om_scripts::text_node(&[NAME], &name));
    let mut changes = vec![(
        format!("[04, 05, {}]", hex(&slot.key)),
        format!(
            "a copy of layout {source} named {name:?}: every key as stored but the name and the \
             UUIDs"
        ),
    )];
    let index = add_to_index(
        catalog,
        &slot,
        copied_entry,
        "the layout's entry: the source's, with the copy's name",
        &mut changes,
    );
    for (item, uses) in &library {
        changes.push((
            format!("[1f, 05, {}, 12]", hex(&serial::encode(*item))),
            format!("library item {item}'s use count raised by {uses}"),
        ));
    }
    Ok(Duplicated {
        planned: Planned {
            layout: slot.raw,
            key: slot.key,
            position: slot.position,
            member: copy,
            index,
            changes,
        },
        library,
    })
}

/// The bits of a layout's flags `f1` Layout Setup's Views tab sets
/// (layout-model.md 4.2): Form, List and Table view not available, and the
/// default view List or Table (Form without either).
pub mod views {
    pub const FORM_OFF: u64 = 0x2;
    pub const LIST_OFF: u64 = 0x4;
    pub const TABLE_OFF: u64 = 0x8;
    pub const DEFAULT_LIST: u64 = 0x200;
    pub const DEFAULT_TABLE: u64 = 0x4000;
    pub const ALL: u64 = FORM_OFF | LIST_OFF | TABLE_OFF | DEFAULT_LIST | DEFAULT_TABLE;
}

/// Layout Setup's General options in a layout's flags `f1`, as FileMaker
/// Pro sets them from their checkboxes (layout-model.md 4.2, 10.11): the
/// option's name as `om-author` takes it, its label, its bit, and whether
/// the bit set means the option is on.
pub mod general {
    /// Show the field-entry alerts controlled by Layout Setup in Pro 26.1.1.
    pub const SHOW_FIELD_ALERTS: u64 = 0x2000;

    pub const OPTIONS: [(&str, &str, u64, bool); 6] = [
        (
            "save_record_changes",
            "Save record changes automatically",
            0x10,
            false,
        ),
        (
            "show_field_frames",
            "Show field frames when record is active",
            0x20,
            true,
        ),
        (
            "delineate_fields",
            "Delineate fields on current record only",
            0x1,
            true,
        ),
        (
            "show_record_indicator",
            "Show current record indicator in List View",
            0x1000_0000,
            false,
        ),
        (
            "show_field_alerts",
            "Show alerts when field isn't modifiable or no records are present",
            SHOW_FIELD_ALERTS,
            true,
        ),
        ("enable_quick_find", "Enable Quick Find", 0x8000, false),
    ];
    pub const ALL: u64 = 0x10 | 0x20 | 0x1 | 0x1000_0000 | SHOW_FIELD_ALERTS | 0x8000;

    /// The click refusal observed on an edit box in an empty Browse Form.
    /// The found-set count must be known before a click can report it empty.
    pub fn empty_field_alert(flags: u64, found_count: Option<u64>) -> Option<&'static str> {
        (flags & SHOW_FIELD_ALERTS != 0 && found_count == Some(0)).then_some(
            "No records are present. To create a new record choose the New Record menu command.",
        )
    }
}

/// Layout Setup's Printing options that OpenMaker sets, as bits of a
/// layout's flags `f1` (layout-model.md 4.2).
pub mod printing {
    /// "Use fixed page margins": the layout prints within its stored
    /// `customMargins` [O: FileMaker Pro 26.0.3 set it in the member's and
    /// the index entry's flags, #609].
    pub const FIXED_PAGE_MARGINS: u64 = om_layout::paper::FIXED_PAGE_MARGINS;
    pub const ALL: u64 = FIXED_PAGE_MARGINS;
    /// "Print in N columns", which OpenMaker's Layout Setup does not set.
    pub const COLUMNS: u64 = 0x400;
}

/// Layout Setup's name and view flags for layout `layout`, set on the
/// layout shown at revision `expected` (its member's
/// `om_protocol::authoring::layout_revision`), as the window asks for it
/// ([`crate::DataProvider::set_layout_setup`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutSetupChange {
    pub layout: u32,
    pub expected: crate::layout_placement::Revision,
    pub name: String,
    pub flags: u64,
    /// Left out of the layout menus.
    pub hidden: bool,
    /// The Printing tab's fixed page margins, when they changed: set
    /// before the flags, as their own edit
    /// (`layout_placement::plan_margins`).
    pub margins: Option<om_layout::Rect>,
}

/// [`plan_setup`], and with `margins` the layout's fixed page margins
/// (`LayoutDef.customMargins`) in the same member, as
/// `layout_placement::plan_margins` sets them: one commit, so a refused
/// setup changes no margins. A layout printing in columns
/// ([`printing::COLUMNS`]) keeps its margins: FileMaker Pro recomputes the
/// columns' width from them when it saves Layout Setup (layout-model.md
/// 4.2), which OpenMaker does not.
pub fn plan_setup_with_margins(
    catalog: &OwnedNode,
    layout: u64,
    (name, flags, hidden): (&str, u64, bool),
    margins: Option<&om_layout::Rect>,
) -> Result<SetUp, Refusal> {
    let mut set = plan_setup(catalog, layout, name, flags, hidden)?;
    let Some(margins) = margins else {
        // "Use fixed page margins" on a layout that stores no margins would
        // print within the printer's (`om_layout::paper`): the margins come
        // with it, unless the definition holds readable ones. The flags it
        // had are [`plan_setup`]'s, the member's or else the entry's.
        let turned_on = flags & printing::FIXED_PAGE_MARGINS != 0
            && stored_flags(catalog, layout)
                .is_none_or(|stored| stored & printing::FIXED_PAGE_MARGINS == 0);
        let margins_stored = crate::layout_placement::definition(&set.member)
            .is_ok_and(|def| def.custom_margins.is_some());
        if turned_on && !margins_stored {
            return Err(Refusal::Invalid(
                "This layout stores no fixed page margins: give them with \"Use fixed page \
                 margins\"."
                    .into(),
            ));
        }
        return Ok(set);
    };
    if crate::layout_placement::already_margins(&set.member, margins) {
        return Ok(set);
    }
    if flags & printing::COLUMNS != 0 {
        return Err(Refusal::Unsupported(
            "This layout prints in columns, whose width FileMaker Pro works out from the page \
             margins; OpenMaker keeps its margins as they are."
                .into(),
        ));
    }
    let planned = crate::layout_placement::plan_margins(&set.member, margins).map_err(
        |refusal| match refusal {
            crate::layout_placement::Refusal::Invalid(why) => Refusal::Invalid(why),
            crate::layout_placement::Refusal::Unsupported(why) => Refusal::Unsupported(why),
        },
    )?;
    set.member = planned.member;
    set.changes.extend(planned.changes);
    set.changed = true;
    Ok(set)
}

/// The view flags `f1` layout `layout` has: its member's, else its index
/// entry's, as FileMaker keeps both ([`plan_setup`]).
fn stored_flags(catalog: &OwnedNode, layout: u64) -> Option<u64> {
    layout_word(
        catalog.at(&[&[MEMBERS], &uint_segment(layout)]),
        catalog.child(&[INDEX]),
        layout,
        LayoutWord::ViewFlags,
    )
}

/// A word Layout Setup reads and writes in both a layout's member and its
/// index entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutWord {
    /// `f1`: the views, General and Printing options.
    ViewFlags,
    /// `f0`: the member flags, "Include in layout menus" among them
    /// ([`MENU_HIDDEN`]).
    MemberFlags,
}

/// Layout `layout`'s `word` as Layout Setup reads it: its member's
/// (`[4, 5, L]`), else its entry's in the catalog's index `index`
/// (`[4, 1]`, its entries `[4, 1, 7, L]`), by the one rule every reader
/// uses (`om_layout::stored_layout_word`). FileMaker keeps both; a request
/// built from the member alone would drop what only the entry holds when
/// [`plan_setup`], the one writer, writes it to both (#977).
pub fn layout_word(
    member: Option<&OwnedNode>,
    index: Option<&OwnedNode>,
    layout: u64,
    word: LayoutWord,
) -> Option<u64> {
    let key = match word {
        LayoutWord::ViewFlags => VIEW_FLAGS,
        LayoutWord::MemberFlags => KIND,
    };
    let own = member.and_then(|m| m.child(&[key]));
    let entry = index.and_then(|i| i.at(&[&[ENTRIES], &uint_segment(layout), &[key]]));
    om_layout::stored_layout_word(
        own.and_then(|n| n.value.as_deref()),
        entry.and_then(|n| n.value.as_deref()),
    )
}

/// A layout's member and index after Layout Setup ([`plan_setup`]).
#[derive(Clone, Debug, PartialEq)]
pub struct SetUp {
    /// The member `[4, 5, L]`, without its new stamp.
    pub member: OwnedNode,
    /// The index `[4, 1]`, without its new stamp.
    pub index: OwnedNode,
    /// Whether anything changed.
    pub changed: bool,
    pub changes: Vec<(String, String)>,
}

/// Plan Layout Setup's name and views for layout `layout` of `catalog`:
/// `name` in the member `10`, its entry `[4, 1, 07, L, 10]` and the name
/// list (re-keyed), `flags` in the member's and the entry's `f1`, and
/// `hidden` ("Include in layout menus" cleared) as [`MENU_HIDDEN`] in their
/// `f0` (layout-model.md 10.11). Only the view bits ([`views::ALL`]), the
/// General options' ([`general::ALL`]) and "Use fixed page margins"
/// ([`printing::ALL`]) may differ from the stored flags; at least one view
/// stays available, and the default view is one of them.
pub fn plan_setup(
    catalog: &OwnedNode,
    layout: u64,
    name: &str,
    flags: u64,
    hidden: bool,
) -> Result<SetUp, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    let key = uint_segment(layout);
    let member = catalog
        .at(&[&[MEMBERS], &key])
        .ok_or_else(|| unsupported(format!("The file has no layout {layout}.")))?;
    let mut index = catalog
        .child(&[INDEX])
        .cloned()
        .ok_or_else(|| unsupported("The file's layout catalog has no index [4, 1].".into()))?;
    let entry = index.at(&[&[ENTRIES], &key]).cloned().ok_or_else(|| {
        unsupported(format!(
            "The layout catalog's index has no entry for layout {layout}."
        ))
    })?;
    // As every reader takes it: the member's, else the entry's (#977).
    let kind =
        layout_word(Some(member), Some(&index), layout, LayoutWord::MemberFlags).unwrap_or(0);
    if kind & NOT_A_LAYOUT != 0 || member.child(&[DEFINITION]).is_none() {
        return Err(unsupported(
            "This member of the layout catalog is a folder or a separator; Layout Setup sets \
             up layouts."
                .into(),
        ));
    }
    let stored = stored_flags(catalog, layout)
        .ok_or_else(|| unsupported(format!("Layout {layout} has no readable view flags.")))?;
    if (stored ^ flags) & !(views::ALL | general::ALL | printing::ALL) != 0 {
        return Err(unsupported(
            "OpenMaker's Layout Setup changes a layout's name, views, General options and \
             fixed page margins; its other settings are kept as stored."
                .into(),
        ));
    }
    // Which margins a layout prints within sets its columns' width, which
    // FileMaker Pro works out again when it saves Layout Setup
    // (layout-model.md 4.2) and OpenMaker does not.
    if (stored ^ flags) & printing::FIXED_PAGE_MARGINS != 0 && flags & printing::COLUMNS != 0 {
        return Err(unsupported(
            "This layout prints in columns, whose width FileMaker Pro works out from the page \
             margins; OpenMaker keeps whether it uses fixed page margins as it is."
                .into(),
        ));
    }
    let off = [views::FORM_OFF, views::LIST_OFF, views::TABLE_OFF];
    let default = if flags & views::DEFAULT_TABLE != 0 {
        views::TABLE_OFF
    } else if flags & views::DEFAULT_LIST != 0 {
        views::LIST_OFF
    } else {
        views::FORM_OFF
    };
    if off.iter().all(|bit| flags & bit != 0) || flags & default != 0 {
        return Err(Refusal::Name(
            "At least one view must be available, the default view among them.".into(),
        ));
    }

    let mut next = member.clone();
    let mut next_entry = entry;
    let mut changes = Vec::new();
    if flags != stored {
        let value = flags.to_be_bytes().to_vec();
        next.set_child(OwnedNode::leaf(vec![VIEW_FLAGS], value.clone()));
        next_entry.set_child(OwnedNode::leaf(vec![VIEW_FLAGS], value));
        changes.push((
            format!("[04, 05, {0}, f1], [04, 01, 07, {0}, f1]", hex(&key)),
            format!("view flags {flags:#x}"),
        ));
    }
    // Include in layout menus: bit 8 of `f0`, in the entry, which every
    // entry has, and in the member, which has `f0` only when it is not 0.
    let menus = if hidden {
        kind | MENU_HIDDEN
    } else {
        kind & !MENU_HIDDEN
    };
    if menus != kind {
        next_entry.set_child(OwnedNode::leaf(vec![KIND], menus.to_be_bytes().to_vec()));
        if menus == 0 {
            next.remove(&[&[KIND]]);
        } else {
            next.set_child(OwnedNode::leaf(vec![KIND], menus.to_be_bytes().to_vec()));
        }
        changes.push((
            format!("[04, 05, {0}, f0], [04, 01, 07, {0}, f0]", hex(&key)),
            format!("member flags {menus:#x}"),
        ));
    }
    if member_name(member).as_deref() != Some(name) {
        let mut listed = check_name(name)?;
        next.set_child(om_scripts::text_node(&[NAME], name));
        next_entry.set_child(om_scripts::text_node(&[NAME], name));
        // The name list: its entry under the new name's key, or the key
        // and its id when another layout has the name (layout-model.md 10.6).
        let id_value = key_string(&key);
        let mut names = index
            .child(&[NAMES])
            .cloned()
            .ok_or_else(|| unsupported("The layout catalog's index has no name list.".into()))?;
        names
            .children
            .retain(|n| n.value.as_deref() != Some(id_value.as_slice()));
        if names.child(&listed).is_some() {
            listed.extend(&key);
            // Its own entries are gone: one left there is another
            // layout's (only a damaged name list has one).
            if names.child(&listed).is_some() {
                return Err(unsupported(
                    "The layout catalog's name list has no free key for the name.".into(),
                ));
            }
        }
        names.set_child(OwnedNode::leaf(listed, id_value));
        index.set_child(names);
        changes.push((
            format!(
                "[04, 05, {0}, 10], [04, 01, 07, {0}, 10], [04, 01, 01]",
                hex(&key)
            ),
            format!("the name {name:?}, listed under its key"),
        ));
    }
    index.put(&[&[ENTRIES], &key], next_entry);
    Ok(SetUp {
        changed: !changes.is_empty(),
        member: next,
        index,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Contacts starter's layout catalog index and members as
    /// FileMaker stores them (layout-model.md 10.6): folder "Contacts" (1),
    /// "Contact List" (2) and "Contact Details" (3) on occurrence
    /// `d0 00 02` in theme 1, and the folder's end "--" (4).
    fn contacts() -> OwnedNode {
        let names: [(u64, &str, Option<&[u8]>); 4] = [
            (1, "Contacts", None),
            (2, "Contact List", Some(&[1])),
            (3, "Contact Details", Some(&[1])),
            (4, "--", None),
        ];
        let mut catalog = OwnedNode::list(vec![4], Vec::new());
        let mut index = OwnedNode::list(vec![INDEX], Vec::new());
        index.set_child(OwnedNode::leaf(vec![LAST_ID], vec![1, 4]));
        index.set_child(OwnedNode::list(vec![NAMES], Vec::new()));
        index.set_child(OwnedNode::list(vec![ORDER], Vec::new()));
        index.set_child(OwnedNode::leaf(vec![COUNT], 4u32.to_be_bytes().to_vec()));
        index.set_child(OwnedNode::list(vec![ENTRIES], Vec::new()));
        for (id, name, theme) in names {
            let mut listed = name_key(name);
            listed.extend([0, 0, 0]);
            index.put(
                &[&[NAMES], &listed],
                OwnedNode::leaf(Vec::new(), vec![1, id as u8]),
            );
            index.put(
                &[&[ORDER], &(id as u32).to_be_bytes()],
                OwnedNode::leaf(Vec::new(), vec![1, id as u8]),
            );
            index.put(
                &[&[ENTRIES], &[id as u8], &[NAME]],
                om_scripts::text_node(&[NAME], name),
            );
            let mut member = OwnedNode::list(vec![id as u8], Vec::new());
            member.set_child(om_scripts::text_node(&[NAME], name));
            if let Some(theme) = theme {
                member.set_child(OwnedNode::leaf(vec![THEME], key_string(theme)));
                member.set_child(OwnedNode::leaf(
                    vec![DEFINITION],
                    om_layout::encode::new_layout_def(1_065_090, 1024.0),
                ));
            }
            catalog.put(&[&[MEMBERS], &[id as u8]], member);
        }
        catalog.set_child(index);
        catalog
    }

    fn layout(name: &str) -> NewLayout {
        NewLayout {
            name: name.into(),
            occurrence: 0xd0_0002,
            theme: vec![1],
            width: 1024.0,
        }
    }

    #[test]
    fn a_layout_takes_the_next_id_and_every_index_entry_filemaker_writes() {
        // FMUpgradeTool 26.0.3 adding "Minimal" to the Contacts starter
        // (layout-model.md 10.6): id 5, position 5, five layouts, the
        // name key below and the entry `10 82 81 41 18 00`, with the
        // member's keys as here.
        let catalog = contacts();
        let planned = plan(&catalog, &layout("Minimal")).unwrap();
        assert_eq!((planned.layout, planned.position), (5, 5));
        let index = &planned.index;
        let value = |path: &[&[u8]]| index.at(path).and_then(|n| n.value.clone());
        assert_eq!(value(&[&[LAST_ID]]), Some(vec![1, 5]));
        assert_eq!(value(&[&[COUNT]]), Some(vec![0, 0, 0, 5]));
        assert_eq!(value(&[&[ORDER], &[0, 0, 0, 5]]), Some(vec![1, 5]));
        let minimal: Vec<u8> = [
            0x13, 0x5f, 0x12, 0xec, 0x13, 0x6d, 0x12, 0xec, 0x13, 0x5f, 0x12, 0x0f, 0x13, 0x30, 0,
            0, 0,
        ]
        .to_vec();
        assert_eq!(value(&[&[NAMES], &minimal]), Some(vec![1, 5]));
        assert_eq!(
            value(&[&[ENTRIES], &[5], &[ENTRY]]),
            Some(vec![0x10, 0x82, 0x81, 0x41, 0x18, 0x00])
        );
        assert_eq!(
            value(&[&[ENTRIES], &[5], &[VIEW_FLAGS]]),
            Some(vec![0, 0, 0, 0, 0x10, 0xf3, 0x08, 0x0d])
        );
        let keys: Vec<Vec<u8>> = planned
            .member
            .children
            .iter()
            .map(|c| c.segment.clone())
            .collect();
        // FileMaker's keys, and the stored Table View layout `0d` (#814).
        let expected: Vec<Vec<u8>> = [
            0x01, 0x02, 0x03, 0x07, 0x08, 0x0a, 0x0b, 0x0d, 0x10, 0x15, 0x17, 0x21, 0xf1,
        ]
        .iter()
        .map(|&k| vec![k])
        .collect();
        assert_eq!(keys, expected);
        assert_eq!(
            planned.member.at(&[&[FIELD_REFERENCES], &[0xd0, 0, 2]]),
            Some(&OwnedNode::list(vec![0xd0, 0, 2], Vec::new()))
        );
        assert_eq!(
            planned
                .member
                .at(&[&[PARTS], &[LAST_PART]])
                .and_then(|n| n.value.clone()),
            Some(vec![1, 1])
        );
        // Every stored member and index entry stays.
        for stored in members(&catalog) {
            assert!(planned.index.at(&[&[ENTRIES], &stored.segment]).is_some());
        }
    }

    /// Duplicate Layout copies the member and its entry with the copy's
    /// name, without the UUIDs, last in the order, and counts the library
    /// items its objects show (layout-model.md 10.9).
    #[test]
    fn a_copy_is_the_source_renamed_and_counts_its_library_images() {
        let mut catalog = contacts();
        // Contact Details: UUIDs, an unknown key, and a store of one button
        // showing library item 3 (type 10, key 7, buttonObj.libraryKey 3).
        let button = [0x08, 0x0a, 0x10, 0x07, 0x82, 0x01, 0x02, 0x20, 0x03];
        let mut store = vec![0x08, 0x07, 0x12, button.len() as u8];
        store.extend(button);
        for (key, value) in [
            (UNIQUE_ID, vec![1; 16]),
            (SOURCE_ID, vec![2; 16]),
            (0x77, vec![9]),
            (OBJECTS, store),
        ] {
            catalog.put(
                &[&[MEMBERS], &[3], &[key]],
                OwnedNode::leaf(Vec::new(), value),
            );
        }
        catalog.put(
            &[&[INDEX], &[ENTRIES], &[3], &[ENTRY_30]],
            OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 7]),
        );
        let Duplicated { planned, library } = plan_duplicate(&catalog, 3).unwrap();
        assert_eq!((planned.layout, planned.position), (5, 5));
        assert_eq!(library, [(3, 1)].into());
        let source = catalog.at(&[&[MEMBERS], &[3]]).unwrap();
        assert_eq!(
            member_name(&planned.member).as_deref(),
            Some("Contact Details Copy")
        );
        for key in [UNIQUE_ID, SOURCE_ID] {
            assert!(planned.member.child(&[key]).is_none());
        }
        for key in [[DEFINITION], [THEME], [OBJECTS], [0x77]] {
            assert_eq!(planned.member.child(&key), source.child(&key));
        }
        let entry = planned.index.at(&[&[ENTRIES], &[5]]).unwrap();
        assert_eq!(
            entry.child(&[ENTRY_30]).and_then(|n| n.value.clone()),
            Some(vec![0, 0, 0, 7])
        );
        assert_eq!(
            entry
                .child(&[NAME])
                .and_then(om_scripts::node_text)
                .as_deref(),
            Some("Contact Details Copy")
        );
        assert_eq!(
            planned.index.child(&[COUNT]).and_then(|n| n.value.clone()),
            Some(5u32.to_be_bytes().to_vec())
        );
        // A folder is not a layout to copy.
        catalog.put(
            &[&[INDEX], &[ENTRIES], &[1], &[KIND]],
            OwnedNode::leaf(Vec::new(), 1u64.to_be_bytes().to_vec()),
        );
        assert!(plan_duplicate(&catalog, 1).is_err());
        // Nor is a layout whose stored table view shows a library image:
        // how FileMaker counts that use is not known.
        let mut view = vec![0x08, 0x07, 0x12, button.len() as u8];
        view.extend(button);
        catalog.put(
            &[&[MEMBERS], &[3], &[TABLE_VIEW], &[OBJECTS]],
            OwnedNode::leaf(Vec::new(), view),
        );
        assert!(plan_duplicate(&catalog, 3).is_err());
    }

    /// Delete Layout removes the member, its name list entry, its position
    /// (the later ones close up), its entry, and lowers the count; the last
    /// issued id stays (layout-model.md 10.10).
    #[test]
    fn a_deleted_layout_leaves_the_index_closed_up() {
        let mut catalog = contacts();
        for id in [1u8, 4] {
            catalog.put(
                &[&[INDEX], &[ENTRIES], &[id], &[KIND]],
                OwnedNode::leaf(Vec::new(), 1u64.to_be_bytes().to_vec()),
            );
        }
        for id in [2u8, 3] {
            catalog.put(
                &[&[INDEX], &[ENTRIES], &[id], &[KIND]],
                OwnedNode::leaf(Vec::new(), 0u64.to_be_bytes().to_vec()),
            );
        }
        let deleted = plan_delete(&catalog, 2).unwrap();
        assert_eq!(deleted.key, [2]);
        assert!(deleted.library.is_empty());
        let index = &deleted.index;
        let order: Vec<(Vec<u8>, Option<Vec<u8>>)> = index
            .child(&[ORDER])
            .unwrap()
            .children
            .iter()
            .map(|p| (p.segment.clone(), p.value.clone()))
            .collect();
        assert_eq!(
            order,
            [
                (1u32.to_be_bytes().to_vec(), Some(vec![1, 1])),
                (2u32.to_be_bytes().to_vec(), Some(vec![1, 3])),
                (3u32.to_be_bytes().to_vec(), Some(vec![1, 4])),
            ]
        );
        assert!(index.at(&[&[ENTRIES], &[2]]).is_none());
        assert!(index
            .child(&[NAMES])
            .unwrap()
            .children
            .iter()
            .all(|n| n.value.as_deref() != Some(&[1, 2][..])));
        assert_eq!(
            index.child(&[COUNT]).and_then(|n| n.value.clone()),
            Some(3u32.to_be_bytes().to_vec())
        );
        assert_eq!(index.child(&[LAST_ID]), catalog.at(&[&[INDEX], &[LAST_ID]]));
        // A folder, and the only layout left, are not deleted.
        assert!(plan_delete(&catalog, 1).is_err());
        let mut alone = catalog.clone();
        alone.put(&[&[INDEX]], index.clone());
        alone.remove(&[&[MEMBERS], &[2]]);
        assert!(plan_delete(&alone, 3).is_err());
    }

    /// Layout Setup with fixed page margins (operation 23) plans both in one
    /// member: a refused setup (an empty name) changes no margins, margins
    /// held already change nothing, and a layout printing in columns keeps
    /// its margins.
    #[test]
    fn layout_setup_plans_its_margins_with_the_setup() {
        let flags_at = |flags: u64| {
            let mut catalog = contacts();
            catalog.put(
                &[&[MEMBERS], &[3], &[VIEW_FLAGS]],
                OwnedNode::leaf(Vec::new(), flags.to_be_bytes().to_vec()),
            );
            catalog
        };
        let catalog = flags_at(0x10f3_080d);
        let margins = om_layout::Rect {
            top: 50.0,
            left: 70.0,
            bottom: 60.0,
            right: 80.0,
            ..Default::default()
        };
        let fixed = 0x10f3_084d;
        let plan = |catalog: &OwnedNode, name: &str, flags: u64| {
            plan_setup_with_margins(catalog, 3, (name, flags, false), Some(&margins))
        };
        let set = plan(&catalog, "Contact Details", fixed).unwrap();
        assert!(set.changed);
        assert!(crate::layout_placement::already_margins(
            &set.member,
            &margins
        ));
        assert!(plan(&catalog, "", fixed).is_err());
        let mut stored = catalog.clone();
        stored.put(&[&[MEMBERS], &[3]], set.member.clone());
        stored.put(&[&[INDEX]], set.index.clone());
        assert!(!plan(&stored, "Contact Details", fixed).unwrap().changed);
        let columns = 0x10f3_0c4d;
        assert!(matches!(
            plan(&flags_at(columns), "Contact Details", columns),
            Err(Refusal::Unsupported(_))
        ));
        // Nor does such a layout's "Use fixed page margins" change, which
        // would change the margins its columns' width was worked out from.
        let only_box = |catalog: &OwnedNode, flags: u64| {
            plan_setup_with_margins(catalog, 3, ("Contact Details", flags, false), None)
        };
        assert!(only_box(&flags_at(0x10f3_0c0d), columns).is_err());
        // The box alone on a layout storing no margins is refused: it would
        // print within the printer's. On one storing them it is set.
        assert!(only_box(&catalog, fixed).unwrap().changed);
        let mut bare = catalog.clone();
        let mut def = Vec::new();
        om_layout::encode::double(8, 1024.0, &mut def);
        bare.put(
            &[&[MEMBERS], &[3], &[DEFINITION]],
            OwnedNode::leaf(Vec::new(), def),
        );
        assert!(matches!(only_box(&bare, fixed), Err(Refusal::Invalid(_))));
        assert!(plan(&bare, "Contact Details", fixed).unwrap().changed);
        // The same with the flags it had only in its index entry, which
        // Layout Setup reads when the member has none.
        let mut entry_only = bare.clone();
        entry_only.remove(&[&[MEMBERS], &[3], &[VIEW_FLAGS]]);
        entry_only.put(
            &[&[INDEX], &[ENTRIES], &[3], &[VIEW_FLAGS]],
            OwnedNode::leaf(Vec::new(), 0x10f3_080d_u64.to_be_bytes().to_vec()),
        );
        assert!(matches!(
            only_box(&entry_only, fixed),
            Err(Refusal::Invalid(_))
        ));
        // And with a definition that does not read: no margins are known.
        let mut unread = catalog.clone();
        unread.put(
            &[&[MEMBERS], &[3], &[DEFINITION]],
            OwnedNode::leaf(Vec::new(), vec![0x0a, 0xff]),
        );
        assert!(matches!(only_box(&unread, fixed), Err(Refusal::Invalid(_))));
    }

    /// Layout Setup renames Contact Details in its member, entry and name
    /// list, and sets its views in both `f1`s; other options, and a view
    /// set leaving no view or an unavailable default, are refused.
    #[test]
    fn layout_setup_renames_and_sets_the_views_in_member_and_entry() {
        let mut catalog = contacts();
        let details = 0x10f3_080d_u64.to_be_bytes().to_vec();
        catalog.put(
            &[&[MEMBERS], &[3], &[VIEW_FLAGS]],
            OwnedNode::leaf(Vec::new(), details),
        );
        let list_too = 0x10f3_080d & !views::LIST_OFF | views::DEFAULT_LIST;
        let set = plan_setup(&catalog, 3, "Person", list_too, false).unwrap();
        assert!(set.changed);
        let flags = Some(list_too.to_be_bytes().to_vec());
        assert_eq!(
            set.member
                .child(&[VIEW_FLAGS])
                .and_then(|n| n.value.clone()),
            flags
        );
        let entry = set.index.at(&[&[ENTRIES], &[3]]).unwrap();
        assert_eq!(
            entry.child(&[VIEW_FLAGS]).and_then(|n| n.value.clone()),
            flags
        );
        assert_eq!(member_name(&set.member).as_deref(), Some("Person"));
        assert_eq!(member_name(entry).as_deref(), Some("Person"));
        let names = set.index.child(&[NAMES]).unwrap();
        let mut person = name_key("Person");
        person.extend([0, 0, 0]);
        assert_eq!(
            names.child(&person).and_then(|n| n.value.clone()),
            Some(vec![1, 3])
        );
        assert_eq!(
            names
                .children
                .iter()
                .filter(|n| n.value.as_deref() == Some(&[1, 3][..]))
                .count(),
            1
        );
        // The same name and flags change nothing; a name another layout has
        // is listed with the id.
        assert!(
            !plan_setup(&catalog, 3, "Contact Details", 0x10f3_080d, false)
                .unwrap()
                .changed
        );
        let shared = plan_setup(&catalog, 3, "Contact List", 0x10f3_080d, false).unwrap();
        let mut listed = name_key("Contact List");
        listed.extend([0, 0, 0, 3]);
        assert!(shared.index.at(&[&[NAMES], &listed]).is_some());
        // That key held by another layout (a damaged list) is refused, not
        // taken over.
        let mut damaged = catalog.clone();
        damaged.put(
            &[&[INDEX], &[NAMES], &listed],
            OwnedNode::leaf(Vec::new(), vec![1, 4]),
        );
        assert!(plan_setup(&damaged, 3, "Contact List", 0x10f3_080d, false).is_err());
        // "Save record changes automatically" off (`0x10`), a General
        // option, is set in the member and the entry, and so is `0x40`, "Use
        // fixed page margins", as FileMaker Pro set it (#609); `0x400`,
        // "Print in 2 columns", which OpenMaker's Layout Setup does not
        // set, is refused.
        let saved_off = plan_setup(&catalog, 3, "Contact Details", 0x10f3_081d, false).unwrap();
        let word = |node: &OwnedNode| node.child(&[VIEW_FLAGS]).and_then(|n| n.value.clone());
        let flags = Some(0x10f3_081d_u64.to_be_bytes().to_vec());
        assert_eq!(word(&saved_off.member), flags);
        assert_eq!(
            saved_off
                .index
                .at(&[&[ENTRIES], &uint_segment(3)])
                .and_then(word),
            flags
        );
        let fixed = plan_setup(&catalog, 3, "Contact Details", 0x10f3_084d, false).unwrap();
        let flags = Some(0x10f3_084d_u64.to_be_bytes().to_vec());
        assert_eq!(word(&fixed.member), flags);
        assert_eq!(
            fixed
                .index
                .at(&[&[ENTRIES], &uint_segment(3)])
                .and_then(word),
            flags
        );
        for flags in [
            0x10f3_0c0d,                        // not a Layout Setup bit OpenMaker sets
            0x10f3_080f,                        // no view
            0x10f3_080d | views::DEFAULT_TABLE, // default not available
        ] {
            assert!(
                plan_setup(&catalog, 3, "Contact Details", flags, false).is_err(),
                "{flags:#x}"
            );
        }
        assert!(plan_setup(&catalog, 1, "Folder", 0, false).is_err());
        // Left out of the layout menus, as Tasks' Assignee Chooser is: bit 8
        // of the entry's `f0` and a member `f0`; taken back in, the member's
        // goes, as on the starters' other layouts.
        catalog.put(
            &[&[INDEX], &[ENTRIES], &[3], &[KIND]],
            OwnedNode::leaf(Vec::new(), 0u64.to_be_bytes().to_vec()),
        );
        let hidden = plan_setup(&catalog, 3, "Contact Details", 0x10f3_080d, true).unwrap();
        let f0 = Some(MENU_HIDDEN.to_be_bytes().to_vec());
        assert_eq!(
            hidden.member.child(&[KIND]).and_then(|n| n.value.clone()),
            f0
        );
        let entry = hidden.index.at(&[&[ENTRIES], &[3], &[KIND]]);
        assert_eq!(entry.and_then(|n| n.value.clone()), f0);
        let mut hidden_catalog = catalog.clone();
        hidden_catalog.put(&[&[INDEX]], hidden.index);
        hidden_catalog.put(&[&[MEMBERS], &[3]], hidden.member);
        let shown = plan_setup(&hidden_catalog, 3, "Contact Details", 0x10f3_080d, false).unwrap();
        assert!(shown.member.child(&[KIND]).is_none());
        // A layout left out of the menus is a layout to Delete and
        // Duplicate Layout too.
        assert!(plan_duplicate(&hidden_catalog, 3).is_ok());
    }

    #[test]
    fn a_second_layout_of_a_name_is_listed_after_its_id() {
        // Assets lists its second separator "-" (layout 7) as `02 23 00 00
        // 00 07`, after the first's `02 23 00 00 00`.
        let mut catalog = contacts();
        let first = plan(&catalog, &layout("Contact List")).unwrap();
        let mut listed = name_key("Contact List");
        listed.extend([0, 0, 0, 5]);
        assert!(first.index.at(&[&[NAMES], &listed]).is_some());
        catalog.set_child(first.index);
        catalog.put(&[&[MEMBERS], &first.key], first.member);
        let second = plan(&catalog, &layout("contact list")).unwrap();
        assert_eq!(second.layout, 6);
        // "contact list" has the same name key as the first two; the free
        // key is the one after id 6.
        listed.truncate(listed.len() - 1);
        listed.push(6);
        assert!(second.index.at(&[&[NAMES], &listed]).is_some());
    }

    #[test]
    fn a_new_layout_takes_its_theme_and_width_from_a_layout_of_the_file() {
        let catalog = contacts();
        // The folder has no theme: the first layout in the order that has
        // one, "Contact List".
        assert_eq!(model(&catalog, Some(1)), Some((vec![1], 1024.0)));
        assert_eq!(model(&catalog, Some(3)), Some((vec![1], 1024.0)));
        assert_eq!(model(&catalog, None), Some((vec![1], 1024.0)));
        let mut bare = catalog.clone();
        for id in [2u8, 3] {
            bare.remove(&[&[MEMBERS], &[id], &[THEME]]);
        }
        assert_eq!(model(&bare, None), None);
    }

    #[test]
    fn unusable_names_and_catalogs_without_an_index_are_refused() {
        let catalog = contacts();
        for name in ["", "   ", "Line\rbreak", &"x".repeat(101)] {
            assert!(
                matches!(plan(&catalog, &layout(name)), Err(Refusal::Name(_))),
                "{name:?}"
            );
        }
        let mut bare = catalog.clone();
        bare.remove(&[&[INDEX], &[ORDER]]);
        assert!(matches!(
            plan(&bare, &layout("New")),
            Err(Refusal::Unsupported(_))
        ));
        let mut themeless = layout("New");
        themeless.theme.clear();
        assert!(matches!(
            plan(&catalog, &themeless),
            Err(Refusal::Unsupported(_))
        ));
    }
}
