//! Placing a field on a layout, with an optional text label beside it, as
//! one edit of the layout's catalog member `[4, 5, L]` (layout-model.md 5,
//! 10.1): what FileMaker Pro's Layout mode stores when a field is dropped
//! on a Form layout's body. OpenMaker Server's authoring extension adds
//! the objects' UUIDs and the layout's modification stamp and saves the
//! result; clients plan with the same code, so a plan names the edit the
//! host makes.
//!
//! The new objects are added on top of the layout's root stack. Everything
//! the layout holds stays as stored, unknown keys and protobuf fields
//! included: the objects are appended to the stored object store's
//! encoding ([`om_layout::encode::append_to_store`]), so its bytes are
//! kept, and the attribute records and the field reference index only
//! gain entries.
//!
//! What is placed:
//!
//! - an edit box (`LayoutObject` type 1, style type 61) showing the first
//!   repetition of a field of the layout's own table occurrence, anchored
//!   left and top, styled by the layout's theme;
//! - optionally a text object (type 2, style type 70) holding a plain
//!   label, stored as FileMaker Pro stores a plain label's text
//!   (`[4, 5, L, 5, O, 01]`);
//! - each object's attribute record `[4, 5, L, 5, O]`: an empty name `10`
//!   (and the host's UUIDs `d8`, `d9`), as every object in the shipped
//!   templates has;
//! - the field in the layout's field reference index
//!   `[4, 5, L, 01, <occurrence>, <field>, <object>]` (an empty value, as
//!   for every field object in the templates);
//! - both objects at the end of the body part's list of its top-level
//!   objects `[4, 5, L, 03, <body>, 07]`, which every top-level object
//!   FileMaker saved is in; FileMaker Pro's Browse did not show an edit box
//!   missing from it (layout-model.md 10.5);
//! - the edit box in the layout's object index `[4, 5, L, 0a]`, which every
//!   field object FileMaker saved is in (layout-model.md 10.5);
//! - the last object key issued, `[4, 5, L, 05, 00]`, raised to the new
//!   objects' keys.
//!
//! The edit box is appended to the tab order (`[4, 5, L, 15]` and its
//! attribute `07`). New objects are unnamed and get no script triggers,
//! conditional formatting or hide condition.

use std::collections::BTreeSet;

use om_hbam::slot::{be_uint, uint_segment};
use om_hbam::{serial, OwnedNode};
use om_layout::encode;
use om_layout::{LayoutDef, LayoutObject, ObjectKind, ObjectStore, Part, PartKind, Rect};

mod parts;
mod portal;
mod web_viewer;
pub use parts::{
    already_parts, part_rank, part_type_name, plan_parts, PartChange, PartsChange, ADDABLE_PARTS,
};
pub use portal::{
    already_placed_portal, plan_portal, portal_field_frames, theme_key, LayoutPortalPlacement,
    PlacedLayoutPortal, PlannedPortal, PortalPlacement, MAX_PORTAL_FIELDS, MAX_PORTAL_ROWS,
};
pub use web_viewer::{
    already_placed_web_viewer, compile_address, plan_web_viewer, LayoutWebViewerPlacement,
    PlacedLayoutWebViewer, PlannedWebViewer, WebViewerPlacement, WebViewerSetup, MAX_ADDRESS_BYTES,
};

/// Keys of a layout member `[4, 5, L]` (file-format.md 3, layout-model.md
/// 10.1; file-metadata.md 1.1 for the flags).
pub(crate) const FIELD_INDEX: u8 = 0x01;
const DEFINITION: u8 = 0x02;
pub(crate) const PARTS: u8 = 0x03;
pub(crate) const ATTRIBUTES: u8 = 0x05;
pub(crate) const OBJECTS: u8 = 0x07;
pub(crate) const OBJECT_INDEX: u8 = 0x0a;
/// The layout's stored Table View layout and its tab order (layout-model.md
/// 10.5, 10.14).
const TABLE_VIEW: u8 = 0x0d;
const TAB_ORDER: u8 = 0x15;
/// A field's column width in a stored table view, and the empty column's
/// beside field columns, as FileMaker Pro 26.0.3's Field tool wrote them
/// (layout-model.md 10.14).
const COLUMN_WIDTH: f64 = 120.0;
const EMPTY_WIDTH: f64 = 16.0;
pub(crate) const MEMBER_FLAGS: u8 = 0xf0;
/// The key, below a part `[4, 5, L, 03, P]`, of its list of the top-level
/// objects in it (layout-model.md 10.5).
pub(crate) const PART_OBJECTS: u8 = 0x07;
/// The key under the attribute records holding the last object key issued
/// (layout-model.md 10.5).
const LAST_KEY: u8 = 0x00;
/// Keys of an object's attribute record: its text, its tab position, its
/// data format (`KFormat`) and its name.
const TEXT: u8 = 0x01;
const TAB_POSITION: u8 = 0x07;
const FORMAT: u8 = 0x0d;
const NAME: u8 = 0x10;
/// A button's step data in its attribute record (its options, a script
/// parameter included; script-steps.md 4.2, layout-model.md 5.3.3).
pub const STEP_DATA: u8 = 0x11;

/// Member flags of a layout folder, the end of one, and a separator
/// (file-metadata.md 1.1).
pub(crate) const NOT_A_LAYOUT: u64 = 0x01 | 0x02 | 0x10;
/// `LayoutDef.layoutClientType` of a responsive layout (layout-model.md
/// 4.1).
pub(crate) const RESPONSIVE: u32 = 2;

/// The largest value written inline; longer ones are chunked as files
/// store them (`om_hbam::writer`, file-format.md 4.3).
const MAX_INLINE_VALUE: usize = 1000;

/// The largest coordinate a frame may have, in points.
pub const MAX_COORDINATE: f64 = 32_000.0;
/// The longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 1_000;

/// What to place.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// The table occurrence the field is shown through, which must be the
    /// layout's own, and the field: their catalog ids (their keys read
    /// big-endian).
    pub occurrence: u64,
    pub field: u64,
    /// The field object's frame in layout coordinates (points).
    pub frame: Rect,
    pub label: Option<Label>,
}

/// A text label placed with the field.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub frame: Rect,
}

/// Why the placement cannot be made; the layout is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A frame or the label cannot be used, and why.
    Invalid(String),
    /// The layout, or what is asked of it, is not one this version edits.
    Unsupported(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Invalid(reason) | Refusal::Unsupported(reason) => f.write_str(reason),
        }
    }
}

/// A placement planned into a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    /// The new objects' keys: the next after every key the layout uses.
    pub field_object: u64,
    pub label_object: Option<u64>,
    /// The part the objects are in.
    pub part: u64,
    /// Where the objects are in the root stack: from this position on,
    /// on top of every stored object.
    pub stacking: usize,
    /// The stored objects the new ones overlap (they stay beneath them).
    pub overlaps: Vec<u64>,
    /// The new object store's encoding.
    pub store: Vec<u8>,
    /// The layout member `[4, 5, L]` with the objects, without UUIDs or
    /// stamps.
    pub member: OwnedNode,
    /// What changed in the member: each changed key below `[4, 5, L]` (as
    /// hex segments) and what it holds.
    pub changes: Vec<(String, String)>,
}

/// A layout's revision: the SHA-256 of its catalog member `[4, 5, L]`
/// without its modification stamps and with stored credentials hidden
/// (`om_protocol::authoring::layout_revision`). Any other change to the
/// layout changes it.
pub type Revision = [u8; 32];

/// A placement asked of the open file's provider
/// ([`crate::DataProvider::place_layout_field`]): what the desktop's Layout
/// mode saves, and what `om-author`'s `place_field` sends, on layout
/// `layout` (its catalog id).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutFieldPlacement {
    pub layout: u32,
    /// The base table the field was chosen from: the table the layout's
    /// occurrence (`placement.occurrence`) showed then; `placement.field`
    /// is a field of it. If the occurrence now shows another table, or
    /// none, the placement is refused rather than placing that table's
    /// field of the same id; the host checks it again under its lock.
    pub table: u64,
    pub placement: Placement,
    /// The layout's revision the placement was made against: the one the
    /// window showed ([`crate::LayoutGeometry::revision`]) or the plan was
    /// read at. A layout changed since is refused (`conflict`, 306) and
    /// nothing is written, unless these objects are already there from an
    /// earlier request whose answer was lost, which the host decides.
    pub expected: Revision,
}

/// The code ([`crate::DataError::code`]) of a placement refused because the
/// layout changed since its expected revision: FileMaker's 306, which the
/// host's authoring extension answers (`om_protocol::authoring::status::
/// CONFLICT`). Nothing was written.
pub const CONFLICT: i32 = 306;

/// A placement the host made, or found already made by an earlier request
/// whose answer was lost (`changed` false).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedLayoutField {
    pub changed: bool,
    /// The new objects' keys.
    pub field_object: u64,
    pub label_object: Option<u64>,
    /// The layout's revision after the placement.
    pub revision: Revision,
}

/// The serial value of a catalog id: how layouts store an occurrence or a
/// field (`tableAliasKey`, `FieldSpec`; layout-model.md 4.1).
pub fn serial_value(id: u64) -> Option<u64> {
    serial::decode(&uint_segment(id))
}

/// A value stored under `segment` as a file stores it: inline up to 1000
/// bytes, else chunked (child `00` the u32 length, then 1000-byte pieces
/// keyed 1, 2, … as serial keys), so a tree read back from the saved file
/// holds the same nodes.
pub fn stored_value(segment: &[u8], value: &[u8]) -> OwnedNode {
    if value.len() <= MAX_INLINE_VALUE {
        return OwnedNode::leaf(segment.to_vec(), value.to_vec());
    }
    let mut children = vec![OwnedNode::leaf(
        vec![0],
        (value.len() as u32).to_be_bytes().to_vec(),
    )];
    for (i, piece) in value.chunks(MAX_INLINE_VALUE).enumerate() {
        children.push(OwnedNode::leaf(
            serial::encode(i as u64 + 1),
            piece.to_vec(),
        ));
    }
    OwnedNode::list(segment.to_vec(), children)
}

/// A layout's definition, as stored in its member.
pub fn definition(member: &OwnedNode) -> Result<LayoutDef, Refusal> {
    let bytes = member
        .child(&[DEFINITION])
        .and_then(|d| d.assembled())
        .ok_or_else(|| Refusal::Unsupported("The layout has no definition (LayoutDef).".into()))?;
    LayoutDef::decode(&bytes).map_err(|e| {
        Refusal::Unsupported(format!(
            "OpenMaker cannot read the layout's definition ({e})."
        ))
    })
}

/// The layout `member` with its width (`LayoutDef.baseWidth`, layout-model.md
/// 10.11) set to `width` points, every other key and byte of its definition
/// kept as stored (`om_layout::encode::with_base_width`), without the stamp.
/// The width is at least 1 point and at most [`MAX_COORDINATE`]; objects may
/// lie beyond it, off the layout, as FileMaker files keep them.
pub fn plan_width(
    member: &OwnedNode,
    width: f64,
) -> Result<crate::layout_objects::Planned, Refusal> {
    if !usable_width(width) {
        return Err(Refusal::Invalid(format!(
            "A layout is 1 to {MAX_COORDINATE} points wide."
        )));
    }
    definition(member)?;
    let stored = member
        .child(&[DEFINITION])
        .and_then(|d| d.assembled())
        .ok_or_else(|| Refusal::Unsupported("The layout has no definition (LayoutDef).".into()))?;
    let def = om_layout::encode::with_base_width(&stored, width).ok_or_else(|| {
        Refusal::Unsupported(
            "The layout's definition holds its width in a form OpenMaker does not change.".into(),
        )
    })?;
    let mut next = member.clone();
    next.set_child(stored_value(&[DEFINITION], &def));
    Ok(crate::layout_objects::Planned {
        member: next,
        changes: vec![("[02]".into(), format!("LayoutDef baseWidth {width}"))],
    })
}

/// Part Setup's options in a part's `PartDef`, as FileMaker Pro 26.0.3's
/// Part Definition sets and offers them (layout-model.md 10.15).
pub mod part_options {
    /// The checkboxes, as bits of `PartDef.flags`. "Page break after
    /// every N occurrences" also stores N, in `pageBreakAfter`.
    pub const PAGE_BREAK_BEFORE: u64 = 0x1;
    pub const PAGE_BREAK_EVERY: u64 = 0x2;
    pub const RESTART_PAGE_NUMBERS: u64 = 0x4;
    pub const BREAK_ACROSS_PAGES: u64 = 0x40;
    pub const DISCARD_REMAINDER: u64 = 0x100;
    /// The largest N Pro takes.
    pub const MAX_PAGE_BREAK_AFTER: u32 = 32_767;

    /// The options Part Definition offers for a part of type `part_type`
    /// (layout-model.md 4.3), `None` for a type it was not seen to offer
    /// any for (a trailing sub-summary, navigation parts). Discarding the
    /// remainder was seen offered for a body only, once breaking across
    /// pages is on.
    pub fn offered(part_type: i32) -> Option<u64> {
        let (before, every) = (PAGE_BREAK_BEFORE, PAGE_BREAK_EVERY);
        let (restart, across) = (RESTART_PAGE_NUMBERS, BREAK_ACROSS_PAGES);
        Some(match part_type {
            0 | 1 | 7 => restart,
            2 => every | restart | across,
            3 => before | every | restart | across,
            4 => before | every | restart | across | DISCARD_REMAINDER,
            6 => before | every | across,
            8 => 0,
            _ => return None,
        })
    }
}

/// Part Setup's options for part `part` (its key, `[4, 5, L, 03, P]`) of
/// layout `layout`, set on the layout shown at revision `expected`
/// ([`crate::DataProvider::set_part_options`]): its `flags`, page break
/// after every `page_break_after` occurrences (0: off) and, for a
/// sub-summary, the field it is sorted by (`None`: as stored), as catalog
/// ids of the occurrence (the layout's own), its base table and the field:
/// the host refuses the field if the occurrence no longer shows that table
/// when it saves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartOptionsChange {
    pub layout: u32,
    pub expected: Revision,
    pub part: u64,
    pub flags: u64,
    pub page_break_after: u32,
    pub break_field: Option<(u64, u64, u64)>,
}

/// Plan part `part` of layout `member` with Part Setup's options
/// ([`PartOptionsChange`]): its `PartDef`'s flags, page break after every N
/// and break field rewritten in place (`om_layout::encode::with_part_options`),
/// every other byte as stored, without the stamp. Only the option bits
/// ([`part_options::ALL`]) may differ from the stored flags, and only a
/// sub-summary has a break field. `field_exists`: the host found the break
/// field among the file's fields.
pub fn plan_part_options(
    member: &OwnedNode,
    part: u64,
    (flags, page_break_after, break_field): (u64, u32, Option<(u64, u64)>),
    field_exists: bool,
) -> Result<crate::layout_objects::Planned, Refusal> {
    let unsupported = |why: &str| Refusal::Unsupported(why.into());
    let node = part_node(member, part)
        .ok_or_else(|| Refusal::Invalid(format!("The layout has no part {part}.")))?;
    let bytes = node
        .child(&[DEFINITION])
        .and_then(|d| d.assembled())
        .ok_or_else(|| unsupported("The part has no definition (PartDef)."))?;
    let stored = Part::decode(part, &bytes)
        .map_err(|e| Refusal::Unsupported(format!("OpenMaker cannot read part {part} ({e}).")))?;
    check_part_options(&stored, (flags, page_break_after, break_field))?;
    let key = match break_field {
        None => None,
        Some(_) if !field_exists => {
            return Err(Refusal::Invalid("The file has no such field.".into()))
        }
        Some((occurrence, field)) => Some(encode::break_field_key(occurrence, field)),
    };
    let def = encode::with_part_options(&bytes, flags, page_break_after.into(), key.as_deref())
        .ok_or_else(|| {
            unsupported("The part holds its options in a form OpenMaker does not change.")
        })?;
    let segment = node.segment.clone();
    let mut next = member.clone();
    next.at_mut(&[&[PARTS], &segment])
        .expect("the part read above")
        .set_child(stored_value(&[DEFINITION], &def));
    Ok(crate::layout_objects::Planned {
        member: next,
        changes: vec![(
            format!("[03, part {part}, 02]"),
            format!("PartDef flags {flags:#x}, page break after {page_break_after}"),
        )],
    })
}

/// Part Definition's rules for a request of these options on part
/// `stored` (layout-model.md 10.15): only options its type offers change,
/// not both page breaks, a discard only with breaking across pages, N from
/// 1 to 32,767 with every N, a sort field only on a leading sub-summary.
fn check_part_options(
    stored: &Part,
    (flags, page_break_after, break_field): (u64, u32, Option<(u64, u64)>),
) -> Result<(), Refusal> {
    use part_options::{
        BREAK_ACROSS_PAGES, DISCARD_REMAINDER, PAGE_BREAK_BEFORE, PAGE_BREAK_EVERY,
    };
    let unsupported = |why: &str| Refusal::Unsupported(why.into());
    let offered = part_options::offered(stored.part_type)
        .ok_or_else(|| unsupported("OpenMaker does not set the options of this kind of part."))?;
    if (stored.flags ^ flags) & !offered != 0 {
        return Err(unsupported(
            "Part Definition does not offer these options for this kind of part; the part's \
             other settings are kept as stored.",
        ));
    }
    if flags & PAGE_BREAK_BEFORE != 0 && flags & PAGE_BREAK_EVERY != 0 {
        return Err(Refusal::Invalid(
            "A part breaks the page before each occurrence or after every N, not both.".into(),
        ));
    }
    if flags & DISCARD_REMAINDER != 0 && flags & BREAK_ACROSS_PAGES == 0 {
        return Err(Refusal::Invalid(
            "Only a part that may break across pages discards its remainder.".into(),
        ));
    }
    let every = flags & PAGE_BREAK_EVERY != 0;
    if page_break_after > part_options::MAX_PAGE_BREAK_AFTER || (every && page_break_after == 0) {
        return Err(Refusal::Invalid(format!(
            "A page break comes after 1 to {} occurrences.",
            part_options::MAX_PAGE_BREAK_AFTER
        )));
    }
    if break_field.is_some() && stored.part_type != 3 {
        return Err(Refusal::Invalid(
            "Only a leading sub-summary is sorted by a field here.".into(),
        ));
    }
    Ok(())
}

/// Whether part `part` of layout `member` holds these options already: a
/// request whose answer was lost.
pub fn already_part_options(
    member: &OwnedNode,
    part: u64,
    (flags, page_break_after, break_field): (u64, u32, Option<(u64, u64)>),
) -> bool {
    let Some(stored) = part_node(member, part)
        .and_then(|node| node.child(&[DEFINITION])?.assembled())
        .and_then(|bytes| Part::decode(part, &bytes).ok())
    else {
        return false;
    };
    let field = stored
        .break_field()
        .ok()
        .flatten()
        .map(|f| (f.occurrence, f.field));
    // A request the planner refuses is never one applied already.
    check_part_options(&stored, (flags, page_break_after, break_field)).is_ok()
        && stored.flags == flags
        && stored.page_break_after == page_break_after
        && break_field.is_none_or(|wanted| field == Some(wanted))
}

/// The longest text OpenMaker writes into a text object, in characters
/// [I: OpenMaker's bound; FileMaker Pro's is not measured].
pub const MAX_TEXT_CHARS: usize = 1_000;

/// A text object's new words, set on the layout shown at revision
/// `expected` ([`crate::DataProvider::set_layout_text`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChange {
    pub layout: u32,
    pub expected: Revision,
    pub object: u64,
    pub text: String,
}

/// Plan text object `object` of layout `member` showing `text` (a field's
/// label is such a text): its attribute record's words `01` written again
/// as a placed text's are (`om_scripts::text_node`), every other key of the
/// record and of the member as stored, without the stamp (layout-model.md
/// 10.16). Only words stored as one plain value are changed: styled words
/// (`ff`), merge fields and words kept as a calculation (`02`) are refused,
/// as is a new text that is empty, holds a line break, a merge field's
/// `<<` or another control character, or is longer than
/// [`MAX_TEXT_CHARS`].
pub fn plan_words(
    member: &OwnedNode,
    object: u64,
    text: &str,
) -> Result<crate::layout_objects::Planned, Refusal> {
    let invalid = |why: String| Refusal::Invalid(why);
    let unsupported = |why: &str| Refusal::Unsupported(why.into());
    if text.is_empty() {
        return Err(invalid(
            "A text object shows at least one character.".into(),
        ));
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(format!(
            "A text object here holds at most {MAX_TEXT_CHARS} characters."
        )));
    }
    if text.chars().any(char::is_control) || text.contains("<<") {
        return Err(unsupported(
            "OpenMaker writes one line of plain text: no line breaks or merge fields yet.",
        ));
    }
    text_object(member, object)?;
    let segment = serial::encode(object);
    let record = member
        .at(&[&[ATTRIBUTES], &segment])
        .ok_or_else(|| unsupported("The text has no attribute record."))?;
    plain_words(record).ok_or_else(|| {
        unsupported(
            "The text's words are styled, hold a merge field or are a calculation; OpenMaker \
             changes plain words only.",
        )
    })?;
    let mut next = member.clone();
    next.at_mut(&[&[ATTRIBUTES], &segment])
        .expect("the record read above")
        .set_child(om_scripts::text_node(&[TEXT], text));
    Ok(crate::layout_objects::Planned {
        member: next,
        changes: vec![(
            format!("[05, object {object}, 01]"),
            format!("text {text:?}"),
        )],
    })
}

/// That `object` of layout `member` is a text object: the one object with
/// that key (a store holding the key twice is refused, not guessed at).
fn text_object(member: &OwnedNode, object: u64) -> Result<(), Refusal> {
    let (_, store) = stored_objects(member)?;
    let mut matching = store.objects.iter().filter(|o| o.id == object);
    let found = matching
        .next()
        .ok_or_else(|| Refusal::Invalid(format!("The layout has no object {object}.")))?;
    if matching.next().is_some() {
        return Err(Refusal::Unsupported(format!(
            "The layout holds more than one object with key {object}."
        )));
    }
    if found.kind() != ObjectKind::Text {
        return Err(Refusal::Invalid(format!("Object {object} is not a text.")));
    }
    Ok(())
}

/// The words of a text's attribute record `record` when they are one
/// plain value: `01` without formatting `ff`, holding no merge field or
/// control character, beside no calculation `02`.
fn plain_words(record: &OwnedNode) -> Option<String> {
    let words = record.child(&[TEXT])?;
    if words.child(&[0xff]).is_some() || record.child(&[0x02]).is_some() {
        return None;
    }
    let text = om_scripts::node_text(words)?;
    (!text.chars().any(char::is_control) && !text.contains("<<")).then_some(text)
}

/// Whether text object `object` of layout `member` shows `text` already: a
/// request whose answer was lost. A request [`plan_words`] refuses never
/// is.
pub fn already_words(member: &OwnedNode, object: u64, text: &str) -> bool {
    text_object(member, object).is_ok()
        && !text.is_empty()
        && text.chars().count() <= MAX_TEXT_CHARS
        && member
            .at(&[&[ATTRIBUTES], &serial::encode(object)])
            .and_then(plain_words)
            .is_some_and(|words| words == text)
}

/// A layout's new width in points, set on the layout shown at revision
/// `expected` ([`crate::DataProvider::set_layout_width`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutWidthChange {
    pub layout: u32,
    pub expected: Revision,
    pub width: f64,
}

/// Whether layout `member` is `width` points wide already: a request whose
/// answer was lost. A width a request could not set is never held already.
pub fn already_width(member: &OwnedNode, width: f64) -> bool {
    usable_width(width) && definition(member).is_ok_and(|def| def.base_width == width)
}

/// Whether a layout may be `width` points wide: 1 to [`MAX_COORDINATE`].
fn usable_width(width: f64) -> bool {
    width.is_finite() && (1.0..=MAX_COORDINATE).contains(&width)
}

/// The widest fixed page margin, in points: the largest paper's side
/// (`om_layout::paper::PaperSettings::validate`). OpenMaker's bound;
/// FileMaker Pro's is unmeasured.
pub const MAX_MARGIN: f64 = 14_400.0;

/// Layout Setup's fixed page margins for a layout (insets in points: top,
/// left, bottom, right in [`Rect`]'s fields), set on the layout shown at
/// revision `expected` ([`crate::DataProvider::set_layout_margins`]).
/// "Use fixed page margins" is a bit of the layout's flags, set by Layout
/// Setup's own operation (`layout_catalog::printing`).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutMarginsChange {
    pub layout: u32,
    pub expected: Revision,
    pub margins: Rect,
}

/// Whether `margins` are fixed page margins a layout may store: each 0 to
/// [`MAX_MARGIN`] points. That they leave room on the paper is checked when
/// the layout prints, on its paper (`om_layout::paper`).
fn usable_margins(margins: &Rect) -> bool {
    [margins.top, margins.left, margins.bottom, margins.right]
        .iter()
        .all(|v| v.is_finite() && (0.0..=MAX_MARGIN).contains(v))
}

/// The layout `member` with its fixed page margins
/// (`LayoutDef.customMargins`, layout-model.md 4.1, 4.2) set to `margins`,
/// every other key and byte of its definition kept as stored
/// (`om_layout::encode::with_custom_margins`), without the stamp. FileMaker
/// Pro's Layout Setup rewrites the stored rectangle in place, top, left,
/// bottom and right [O: #609's copies]; it also recomputes
/// `printColumnWidth`, which OpenMaker leaves as stored, as the starter
/// layouts keep 0 beside their 36-point margins.
pub fn plan_margins(
    member: &OwnedNode,
    margins: &Rect,
) -> Result<crate::layout_objects::Planned, Refusal> {
    if !usable_margins(margins) {
        return Err(Refusal::Invalid(format!(
            "A fixed page margin is 0 to {MAX_MARGIN} points."
        )));
    }
    definition(member)?;
    let stored = member
        .child(&[DEFINITION])
        .and_then(|d| d.assembled())
        .ok_or_else(|| Refusal::Unsupported("The layout has no definition (LayoutDef).".into()))?;
    let def = om_layout::encode::with_custom_margins(&stored, margins).ok_or_else(|| {
        Refusal::Unsupported(
            "The layout's definition holds its page margins in a form OpenMaker does not change."
                .into(),
        )
    })?;
    let mut next = member.clone();
    next.set_child(stored_value(&[DEFINITION], &def));
    Ok(crate::layout_objects::Planned {
        member: next,
        changes: vec![(
            "[02]".into(),
            format!(
                "LayoutDef customMargins top {} left {} bottom {} right {}",
                margins.top, margins.left, margins.bottom, margins.right
            ),
        )],
    })
}

/// Whether layout `member` stores `margins` as its fixed page margins
/// already: a request whose answer was lost. Margins a request could not
/// set are never held already.
pub fn already_margins(member: &OwnedNode, margins: &Rect) -> bool {
    usable_margins(margins)
        && definition(member).is_ok_and(|def| {
            def.custom_margins.as_ref().is_some_and(|m| {
                (m.top, m.left, m.bottom, m.right)
                    == (margins.top, margins.left, margins.bottom, margins.right)
            })
        })
}

/// A layout's parts, in key order.
pub fn parts(member: &OwnedNode) -> Result<Vec<Part>, Refusal> {
    let Some(parts) = member.child(&[PARTS]) else {
        return Ok(Vec::new());
    };
    parts
        .children
        .iter()
        .filter_map(|part| {
            let id = serial::decode(&part.segment)?;
            let bytes = part.child(&[DEFINITION])?.assembled()?;
            Some(Part::decode(id, &bytes).map_err(|e| {
                Refusal::Unsupported(format!("OpenMaker cannot read part {id} ({e})."))
            }))
        })
        .collect()
}

/// A layout's object store as stored (empty when it has none).
pub fn stored_objects(member: &OwnedNode) -> Result<(Vec<u8>, ObjectStore), Refusal> {
    let bytes = match member.child(&[OBJECTS]) {
        Some(node) => node
            .assembled()
            .ok_or_else(|| {
                Refusal::Unsupported("The layout's object store is not a readable value.".into())
            })?
            .into_owned(),
        None => Vec::new(),
    };
    let store = ObjectStore::decode(&bytes).map_err(|e| {
        Refusal::Unsupported(format!("OpenMaker cannot read the layout's objects ({e})."))
    })?;
    Ok((bytes, store))
}

/// Whether objects of this kind hold others (or show over them), so that
/// an object placed in or over one would belong to it.
pub(crate) fn is_container(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Group
            | ObjectKind::Portal
            | ObjectKind::TabControl
            | ObjectKind::TabPanel
            | ObjectKind::SlideControl
            | ObjectKind::SlidePanel
            | ObjectKind::Popover
            | ObjectKind::ButtonBar
            | ObjectKind::Unknown(_)
    )
}

pub(crate) fn kind_name(kind: ObjectKind) -> String {
    match kind {
        ObjectKind::Unknown(t) => format!("layout object of unknown type {t}"),
        kind => format!("{kind:?}")
            .chars()
            .enumerate()
            .flat_map(|(i, c)| {
                let lower = c.to_ascii_lowercase();
                (i > 0 && c.is_ascii_uppercase())
                    .then_some(' ')
                    .into_iter()
                    .chain([lower])
            })
            .collect(),
    }
}

pub(crate) fn overlap(a: &Rect, b: &Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// Check a frame: finite, not negative, not empty, within
/// [`MAX_COORDINATE`] and, when the layout has a width, within it.
pub fn check_frame(frame: &Rect, width: f64, what: &str) -> Result<(), Refusal> {
    let invalid = |why: String| Err(Refusal::Invalid(format!("The {what}'s frame {why}.")));
    let edges = [frame.top, frame.left, frame.bottom, frame.right];
    if edges.iter().any(|e| !e.is_finite()) {
        return invalid("has an edge that is not a number".into());
    }
    if frame.top < 0.0 || frame.left < 0.0 {
        return invalid("starts above or left of the layout".into());
    }
    if frame.right <= frame.left || frame.bottom <= frame.top {
        return invalid("has no width or no height".into());
    }
    if frame.right > MAX_COORDINATE || frame.bottom > MAX_COORDINATE {
        return invalid(format!("reaches past {MAX_COORDINATE} points"));
    }
    if width > 0.0 && frame.right > width {
        return invalid(format!(
            "reaches past the layout's width of {width} points, where Browse does not show it"
        ));
    }
    Ok(())
}

/// Check a label's text: one line of at most [`MAX_LABEL_CHARS`]
/// characters, not empty.
pub fn check_label(text: &str) -> Result<(), Refusal> {
    if text.trim().is_empty() {
        return Err(Refusal::Invalid("A label needs some text.".into()));
    }
    if text.chars().count() > MAX_LABEL_CHARS {
        return Err(Refusal::Invalid(format!(
            "A label holds at most {MAX_LABEL_CHARS} characters."
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(Refusal::Invalid(
            "A label is one line without control characters.".into(),
        ));
    }
    Ok(())
}

pub(crate) fn hex(segment: &[u8]) -> String {
    segment
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn frame_text(frame: &Rect) -> String {
    format!(
        "left {}, top {}, width {}, height {}",
        frame.left,
        frame.top,
        frame.width(),
        frame.height()
    )
}

/// An object key as a key string: its length, then the serial key.
pub(crate) fn key_string(key: u64) -> Vec<u8> {
    let encoded = serial::encode(key);
    let mut out = vec![encoded.len() as u8];
    out.extend(encoded);
    out
}

/// The last object key the layout issued, `[4, 5, L, 05, 00]` (a key
/// string), if it holds one.
pub fn last_issued(member: &OwnedNode) -> Result<Option<u64>, Refusal> {
    let Some(node) = member.at(&[&[ATTRIBUTES], &[LAST_KEY]]) else {
        return Ok(None);
    };
    node.value
        .as_deref()
        .and_then(|value| {
            let (&len, key) = value.split_first()?;
            (key.len() == usize::from(len))
                .then(|| serial::decode(key))
                .flatten()
        })
        .map(Some)
        .ok_or_else(|| {
            Refusal::Unsupported(
                "The layout's last issued object key is not one OpenMaker reads.".into(),
            )
        })
}

/// The number of objects in the layout's object tree, counted once each
/// from the roots: the pre-order index of a new last root
/// (layout-model.md 10.5). A malformed tree is refused ([`preorder`]).
fn tree_size(store: &ObjectStore) -> Result<usize, Refusal> {
    Ok(preorder(store)?.len())
}

/// The layout's objects in pre-order, from the roots: each object's place
/// in it is its pre-order index, which the object index keys it by
/// (layout-model.md 10.5), and its length the index of a new last root. A
/// tree that names an object it does not hold, reaches one twice or leaves
/// one out is refused; every tree FileMaker saved reaches each of its
/// objects once.
pub(crate) fn preorder(store: &ObjectStore) -> Result<Vec<u64>, Refusal> {
    let malformed = || {
        Err(Refusal::Unsupported(
            "The layout's object tree does not reach each of its objects once; OpenMaker does \
             not change objects on it."
                .into(),
        ))
    };
    let mut objects = std::collections::HashMap::new();
    for object in &store.objects {
        if objects.insert(object.id, object).is_some() {
            return malformed();
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut order = Vec::with_capacity(objects.len());
    let mut stack: Vec<u64> = store.root.iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        let Some(object) = objects.get(&id) else {
            return malformed();
        };
        if !seen.insert(id) {
            return malformed();
        }
        order.push(id);
        stack.extend(object.children.iter().rev());
    }
    if seen.len() != objects.len() {
        return malformed();
    }
    Ok(order)
}

/// The largest pre-order index the layout's object index `[4, 5, L, 0a]`
/// holds, its entries under containers included.
fn largest_indexed(member: &OwnedNode) -> Option<usize> {
    let mut largest = None;
    let mut pending: Vec<&OwnedNode> = member.child(&[OBJECT_INDEX]).into_iter().collect();
    while let Some(node) = pending.pop() {
        for entry in &node.children {
            if let [_, _, _, _, high, low] = entry.segment[..] {
                largest = largest.max(Some(usize::from(u16::from_be_bytes([high, low]))));
            }
            pending.push(entry);
        }
    }
    largest
}

/// The object index entry `[4, 5, L, 0a, <key>]` of a top-level object
/// at `top`, `left` with pre-order index `sequence`, showing repetition 1
/// of a field, as FileMaker Pro writes one (layout-model.md 10.5): the key
/// is the three as big-endian `u16`s; the value the length of the object's
/// key string, the key string padded to three bytes, then `u16`s 0 and the
/// repetition.
pub(crate) fn object_index_entry(
    top: f64,
    left: f64,
    sequence: usize,
    object: u64,
) -> Result<(Vec<u8>, Vec<u8>), Refusal> {
    let whole =
        |v: f64| (v.fract() == 0.0 && (0.0..=f64::from(u16::MAX)).contains(&v)).then_some(v as u16);
    let (Some(top), Some(left)) = (whole(top), whole(left)) else {
        return Err(Refusal::Unsupported(
            "The field's frame starts between whole points; OpenMaker places fields at whole \
             points only, as how FileMaker indexes another position is not known."
                .into(),
        ));
    };
    let key = key_string(object);
    let (Ok(sequence), true) = (u16::try_from(sequence), key.len() <= 3) else {
        return Err(Refusal::Unsupported(
            "The layout has too many objects for OpenMaker to index another one.".into(),
        ));
    };
    let mut index_key = Vec::with_capacity(6);
    for v in [top, left, sequence] {
        index_key.extend(v.to_be_bytes());
    }
    let mut value = vec![0, key.len() as u8];
    value.extend(&key);
    value.resize(5, 0);
    value.extend([0, 0, 0, 1]);
    Ok((index_key, value))
}

/// Whether the layout's object index lists `object` at the top level, as
/// a placement lists its field object.
pub fn indexes(member: &OwnedNode, object: u64) -> bool {
    let key = key_string(object);
    let mut prefix = vec![0, key.len() as u8];
    prefix.extend(key);
    member.child(&[OBJECT_INDEX]).is_some_and(|index| {
        index.children.iter().any(|entry| {
            entry
                .value
                .as_deref()
                .is_some_and(|v| v.starts_with(&prefix))
        })
    })
}

/// The stored Table View layout `[4, 5, L, 0d]` of `member` after
/// [`plan`] places `placement`'s field on it: `None` when the layout stores
/// none (layout-model.md 10.14, #815).
pub fn planned_table_view(
    member: &OwnedNode,
    placement: &Placement,
) -> Result<Option<OwnedNode>, Refusal> {
    let Some(view) = member.child(&[TABLE_VIEW]) else {
        return Ok(None);
    };
    let (Some(table_key), Some(field_key)) = (
        serial_value(placement.occurrence),
        serial_value(placement.field),
    ) else {
        return Err(Refusal::Unsupported(
            "The field's occurrence or key cannot be stored in a field spec.".into(),
        ));
    };
    with_table_column(view, (table_key, field_key), placement).map(Some)
}

/// A stored Table View layout `[4, 5, L, 0d]` with a column for the field
/// `placement` places, `keys` its field spec's table and field keys, as
/// FileMaker Pro 26.0.3's Field tool writes one (layout-model.md 10.14,
/// #815): see [`renew_columns`].
fn with_table_column(
    view: &OwnedNode,
    keys: (u64, u64),
    placement: &Placement,
) -> Result<OwnedNode, Refusal> {
    let column = NewColumn {
        keys,
        occurrence: uint_segment(placement.occurrence),
        field: uint_segment(placement.field),
    };
    renew_columns(view, &|_| false, &[column])
}

/// A field's column [`renew_columns`] adds: its field spec's table and
/// field keys, and its occurrence's and field's key segments for the
/// field references.
pub(crate) struct NewColumn {
    pub(crate) keys: (u64, u64),
    pub(crate) occurrence: Vec<u8>,
    pub(crate) field: Vec<u8>,
}

/// A stored Table View layout `[4, 5, L, 0d]` without the columns of
/// `fields` (field specs' table and field keys), as FileMaker Pro 26.0.3
/// keeps one when the last field object of the layout is deleted
/// (layout-model.md 10.14, F88): see [`renew_columns`].
pub fn without_table_columns(
    view: &OwnedNode,
    fields: &BTreeSet<(u64, u64)>,
) -> Result<OwnedNode, Refusal> {
    let shows = |o: &LayoutObject| {
        o.field_spec()
            .is_some_and(|s| fields.contains(&(s.table_key, s.field_key)))
    };
    renew_columns(view, &shows, &[])
}

/// A stored Table View layout `[4, 5, L, 0d]` with its columns renewed as
/// FileMaker Pro 26.0.3 renews them (layout-model.md 10.14, #815, F88): the
/// empty columns (Create Field) and the columns `drop` names are taken out
/// with their attribute records, field references, object index and tab
/// order entries; the columns of `fields` (each one's field spec's keys,
/// and its occurrence and field key segments), in order, go right of the
/// columns kept, and a new empty column right of the last, narrow beside columns
/// and full width alone; they take the next object keys, and entries in the
/// body's list, the field references, the object index and the tab order,
/// and the last key is raised. Every other entry is kept as stored. A view
/// this version does not renew (an object that is not a top-level column, a
/// column taken out before one kept, no body list, a position between whole
/// points) is refused.
/// The data format record FileMaker Pro gives each column of a stored
/// table view and each field object it places (`05, <key>, 0d`): the stored
/// empty column's, else any column's, read and written again field by field
/// (`om_layout::data_format`), as Pro copies it; else a new field's settings
/// ([`crate::layout_catalog::new_column_format`]) for a view OpenMaker made
/// before it wrote one (layout-model.md 10.14, #886). A stored record the
/// field-by-field writing does not give back byte for byte is not one
/// OpenMaker reads: refused, not copied.
pub(crate) fn column_format(view: &OwnedNode) -> Result<Vec<u8>, Refusal> {
    let empty = |o: &&LayoutObject| o.flags & encode::EMPTY_COLUMN_FLAG != 0;
    let stored = stored_objects(view).ok().and_then(|(_, store)| {
        store
            .objects
            .iter()
            .filter(empty)
            .chain(store.objects.iter().filter(|o| !empty(o)))
            .find_map(|o| {
                let record = view.at(&[&[ATTRIBUTES], &serial::encode(o.id), &[FORMAT]])?;
                Some(record.assembled()?.into_owned())
            })
    });
    let Some(stored) = stored else {
        return Ok(crate::layout_catalog::new_column_format());
    };
    om_layout::data_format::DataFormat::decode(&stored)
        .and_then(|format| format.encode())
        .filter(|written| *written == stored)
        .ok_or_else(|| {
            Refusal::Unsupported(
                "The stored table view's data format record is not one OpenMaker reads, so it \
                 does not write one for a new column."
                    .into(),
            )
        })
}

pub(crate) fn renew_columns(
    view: &OwnedNode,
    drop: &dyn Fn(&LayoutObject) -> bool,
    fields: &[NewColumn],
) -> Result<OwnedNode, Refusal> {
    let refuse = |why: &str| {
        Refusal::Unsupported(format!(
            "OpenMaker does not change the columns of this layout's stored Table View layout \
             ({why})."
        ))
    };
    let (stored, store) = stored_objects(view)?;
    let columns_only = store
        .objects
        .iter()
        .all(|o| o.kind() == ObjectKind::Field && o.parent.is_none() && o.children.is_empty());
    if !columns_only || store.root.len() != store.objects.len() {
        return Err(refuse("it holds objects other than top-level columns"));
    }
    let goes = |key: &u64| {
        store
            .objects
            .iter()
            .any(|o| o.id == *key && (o.flags & encode::EMPTY_COLUMN_FLAG != 0 || drop(o)))
    };
    let removed: Vec<u64> = store.root.iter().copied().filter(|k| goes(k)).collect();
    // The columns taken out come last, so the columns kept keep their
    // places in the object index.
    let kept = store.root.len() - removed.len();
    if store.root[kept..].iter().any(|k| !goes(k)) {
        return Err(refuse("a column taken out comes before one kept"));
    }
    let body = parts(view)?
        .into_iter()
        .find(|p| p.kind() == PartKind::Body)
        .ok_or_else(|| refuse("it has no body"))?;
    // The row: the columns' top and height as stored, or the body's when
    // there is no column (an empty object store, which FileMaker Pro wrote
    // when a theme was changed and which it cannot show, 10.14).
    let row = match store.objects.first() {
        Some(column) => column
            .bounds
            .clone()
            .ok_or_else(|| refuse("a column has no bounds"))?,
        None => Rect {
            top: body.top,
            left: 0.0,
            bottom: body.top + body.height,
            right: 0.0,
            other: Vec::new(),
        },
    };
    let mut left = 0.0f64;
    for object in store.objects.iter().filter(|o| !removed.contains(&o.id)) {
        let bounds = object
            .bounds
            .as_ref()
            .ok_or_else(|| refuse("a column has no bounds"))?;
        left = left.max(bounds.right);
    }
    let first = next_key(view, &store)?.max(next_object_key(last_issued(view)?.unwrap_or(0))?);
    let at = |left: f64, width: f64| Rect {
        left,
        right: left + width,
        ..row.clone()
    };
    // The new objects: the fields' columns, then the empty column.
    let mut added: Vec<(u64, Vec<u8>, Rect)> = Vec::new();
    let mut references: Vec<(Vec<u8>, Vec<u8>, u64)> = Vec::new();
    let mut key = first;
    for column in fields {
        let (table_key, field_key) = column.keys;
        let frame = at(left, COLUMN_WIDTH);
        added.push((
            key,
            encode::table_column(key, &frame, table_key, field_key),
            frame,
        ));
        references.push((column.occurrence.clone(), column.field.clone(), key));
        left += COLUMN_WIDTH;
        key = next_object_key(key)?;
    }
    let width = if kept > 0 || !fields.is_empty() {
        EMPTY_WIDTH
    } else {
        COLUMN_WIDTH
    };
    let frame = at(left, width);
    added.push((key, encode::empty_table_column(key, &frame), frame));
    references.push((vec![0], vec![0], key));
    let empty = key;

    let without = encode::without_objects(&stored, &removed)
        .ok_or_else(|| refuse("its object store does not read"))?;
    let roots: Vec<u64> = added.iter().map(|(k, ..)| *k).collect();
    let objects: Vec<Vec<u8>> = added.iter().map(|(_, o, _)| o.clone()).collect();
    let new_store = encode::append_to_store(&without, &roots, &objects);

    let mut next = view.clone();
    next.set_child(stored_value(&[OBJECTS], &new_store));
    // Each new column gets the data format record Pro gives every column:
    // with columns that lack one, Pro emptied the view when it saved the
    // layout, and then quit on it (#886).
    let format = column_format(view)?;
    for (key, ..) in &added {
        let segment = serial::encode(*key);
        next.put(
            &[&[ATTRIBUTES], &segment],
            OwnedNode::list(
                segment.clone(),
                vec![OwnedNode::leaf(vec![FORMAT], format.clone())],
            ),
        );
    }
    let names_removed = |value: Option<&[u8]>| {
        value
            .and_then(crate::layout_objects::indexed_object)
            .is_some_and(|key| removed.contains(&key))
    };
    // The object index and the tab order: entries of the columns taken out
    // go; the new ones follow the columns kept.
    let mut index = view
        .child(&[OBJECT_INDEX])
        .cloned()
        .unwrap_or_else(|| OwnedNode::list(vec![OBJECT_INDEX], Vec::new()));
    index
        .children
        .retain(|e| !names_removed(e.value.as_deref()));
    let mut tab = view
        .child(&[TAB_ORDER])
        .cloned()
        .unwrap_or_else(|| OwnedNode::list(vec![TAB_ORDER], Vec::new()));
    tab.children.retain(|e| !names_removed(e.value.as_deref()));
    let mut last = tab
        .children
        .iter()
        .map(|e| <[u8; 2]>::try_from(e.segment.as_slice()).map(u16::from_be_bytes))
        .try_fold(0u16, |most, order| order.map(|o| most.max(o)))
        .map_err(|_| refuse("its tab order is not one OpenMaker reads"))?;
    for (sequence, (key, _, frame)) in added.iter().enumerate() {
        let (entry_key, value) = object_index_entry(frame.top, frame.left, kept + sequence, *key)?;
        if index.child(&entry_key).is_some() {
            return Err(refuse(
                "its object index already has an entry at a new column's place",
            ));
        }
        index.set_child(OwnedNode::leaf(entry_key, value.clone()));
        last = last
            .checked_add(1)
            .ok_or_else(|| refuse("its tab order is full"))?;
        tab.set_child(OwnedNode::leaf(last.to_be_bytes().to_vec(), value));
    }
    next.set_child(index);
    next.set_child(tab);
    // The body's list: the columns kept, then the new ones.
    let body_segment = serial::encode(body.id);
    let list_path: [&[u8]; 3] = [&[PARTS], &body_segment, &[PART_OBJECTS]];
    let list_node = view
        .at(&list_path)
        .ok_or_else(|| refuse("its body has no list"))?;
    let listed = list_node
        .value
        .as_deref()
        .and_then(part_list)
        .ok_or_else(|| refuse("its body's list does not read"))?;
    let mut keys: Vec<u64> = listed
        .into_iter()
        .filter(|k| !removed.contains(k))
        .collect();
    keys.extend(&roots);
    let count = u32::try_from(keys.len()).map_err(|_| refuse("its body lists too many"))?;
    let mut list = count.to_be_bytes().to_vec();
    for key in keys {
        list.extend(part_list_entry(key));
    }
    let mut list_node = list_node.clone();
    list_node.value = Some(list);
    next.put(&list_path, list_node);
    // Attribute records and field references of the columns taken out; a
    // field's node left without columns goes, as on a layout (10.8).
    for old in &removed {
        let segment = serial::encode(*old);
        next.remove(&[&[ATTRIBUTES], &segment]);
        if let Some(references) = next.child_mut(&[FIELD_INDEX]) {
            for occurrence in &mut references.children {
                // Only a field's node this edit empties goes (10.8).
                occurrence.children.retain_mut(|field| {
                    let before = field.children.len();
                    field.children.retain(|o| o.segment != segment);
                    field.children.len() == before
                        || !field.children.is_empty()
                        || field.value.is_some()
                });
            }
        }
    }
    for (occurrence, field_segment, key) in references {
        next.put(
            &[
                &[FIELD_INDEX],
                &occurrence,
                &field_segment,
                &serial::encode(key),
            ],
            OwnedNode::leaf(Vec::new(), Vec::new()),
        );
    }
    let mut counter = view
        .at(&[&[ATTRIBUTES], &[LAST_KEY]])
        .cloned()
        .unwrap_or_else(|| OwnedNode::leaf(Vec::new(), Vec::new()));
    counter.value = Some(key_string(empty));
    next.put(&[&[ATTRIBUTES], &[LAST_KEY]], counter);
    Ok(next)
}

/// The objects a part's list `[4, 5, L, 03, P, 07]` names, in order: a
/// big-endian `u32` count, then each object's key string after a byte
/// giving its length (`02 01 33` for object 51; layout-model.md 10.5).
/// `None` unless the count matches the entries and each entry is a whole
/// key string of a serial key.
pub(crate) fn part_list(value: &[u8]) -> Option<Vec<u64>> {
    let (count, mut rest) = value.split_first_chunk::<4>()?;
    let count = usize::try_from(u32::from_be_bytes(*count)).ok()?;
    // Each entry takes at least three bytes, whatever the count claims.
    let mut keys = Vec::with_capacity(count.min(rest.len() / 3));
    while let Some((&total, tail)) = rest.split_first() {
        let entry = tail.get(..usize::from(total))?;
        let (&len, segment) = entry.split_first()?;
        if usize::from(len) != segment.len() {
            return None;
        }
        keys.push(serial::decode(segment)?);
        rest = &tail[entry.len()..];
    }
    (keys.len() == count).then_some(keys)
}

/// The entry a part's list holds for object `key`.
pub(crate) fn part_list_entry(key: u64) -> Vec<u8> {
    let key = key_string(key);
    let mut entry = vec![key.len() as u8];
    entry.extend(key);
    entry
}

/// A part's node `[4, 5, L, 03, P]`, by the part's id.
pub(crate) fn part_node(member: &OwnedNode, part: u64) -> Option<&OwnedNode> {
    member
        .child(&[PARTS])?
        .children
        .iter()
        .find(|node| serial::decode(&node.segment) == Some(part))
}

/// The largest object key the layout's part lists name (0 for none; lists
/// OpenMaker does not read name none).
fn largest_listed(member: &OwnedNode) -> u64 {
    member
        .child(&[PARTS])
        .into_iter()
        .flat_map(|parts| parts.children.iter())
        .filter_map(|part| part_list(part.child(&[PART_OBJECTS])?.value.as_deref()?))
        .flatten()
        .max()
        .unwrap_or(0)
}

/// Whether `member`'s parts are `base`'s with `objects` added, in this
/// order, to the end of a part's list of its objects, and nothing else of
/// them changed: every other part, key, entry and byte as it was. Objects
/// `base` lists already (a placement that was applied before) are not
/// added again; every one of `objects` must be listed in `member`.
pub fn part_lists_extended(base: &OwnedNode, member: &OwnedNode, objects: &[u64]) -> bool {
    let (Some(old), Some(new)) = (base.child(&[PARTS]), member.child(&[PARTS])) else {
        return false;
    };
    if old.value != new.value || old.children.len() != new.children.len() {
        return false;
    }
    let listed = |parts: &OwnedNode| -> Vec<u64> {
        parts
            .children
            .iter()
            .filter_map(|part| part_list(part.child(&[PART_OBJECTS])?.value.as_deref()?))
            .flatten()
            .collect()
    };
    let mut appended = Vec::new();
    for (was, is) in old.children.iter().zip(&new.children) {
        if was == is {
            continue;
        }
        let without_list = |part: &OwnedNode| {
            let mut part = part.clone();
            part.remove(&[&[PART_OBJECTS]]);
            part
        };
        if without_list(was) != without_list(is) {
            return false;
        }
        let (Some(was), Some(is)) = (was.child(&[PART_OBJECTS]), is.child(&[PART_OBJECTS])) else {
            return false;
        };
        let (Some(was_value), Some(is_value)) = (was.value.as_deref(), is.value.as_deref()) else {
            return false;
        };
        let (Some(was_keys), Some(is_keys)) = (part_list(was_value), part_list(is_value)) else {
            return false;
        };
        if was.children != is.children
            || was_value.get(4..) != is_value.get(4..was_value.len())
            || !is_keys.starts_with(&was_keys)
        {
            return false;
        }
        appended.extend_from_slice(&is_keys[was_keys.len()..]);
    }
    let before = listed(old);
    let expected: Vec<u64> = objects
        .iter()
        .copied()
        .filter(|key| !before.contains(key))
        .collect();
    let after = listed(new);
    appended == expected && objects.iter().all(|key| after.contains(key))
}

/// Whether `member` holds the object index and tab order [`plan`] writes
/// when it places `placement`'s field on `base` as object `field_object`:
/// `base`'s object index `[4, 5, L, 0a]` with the edit box's entry added,
/// `base`'s tab order `[4, 5, L, 15]` with every stored entry and gap as it
/// was and the same reference after the greatest stored position, and that
/// position in the edit box's attribute `07` (layout-model.md 10.5,
/// [#316](https://github.com/OpenMaker-Dev/openmaker/issues/316)).
pub fn object_index_and_tab_order_as_planned(
    base: &OwnedNode,
    member: &OwnedNode,
    placement: &Placement,
    field_object: u64,
) -> bool {
    let position = |m: &OwnedNode| {
        m.at(&[
            &[ATTRIBUTES],
            &serial::encode(field_object),
            &[TAB_POSITION],
        ])
        .cloned()
    };
    plan(base, placement).is_ok_and(|planned| {
        planned.field_object == field_object
            && planned.member.child(&[OBJECT_INDEX]) == member.child(&[OBJECT_INDEX])
            && planned.member.child(&[TAB_ORDER]) == member.child(&[TAB_ORDER])
            && position(&planned.member).is_some()
            && position(&planned.member) == position(member)
    })
}

fn next_object_key(key: u64) -> Result<u64, Refusal> {
    key.checked_add(1).ok_or_else(|| {
        Refusal::Unsupported(
            "The layout has exhausted its object keys; OpenMaker does not place fields on it."
                .into(),
        )
    })
}

/// The key after every object key `member` uses: in its object store
/// (objects, the root stack, containers' children), its attribute records,
/// its field reference index, and the entries of its object index `0a`
/// and tab order `15`, nested ones included (a portal's object index
/// entry holds its contents'). An entry left naming an object that is
/// gone keeps its key from a new object, which the entry would then name
/// too.
fn next_key(member: &OwnedNode, store: &ObjectStore) -> Result<u64, Refusal> {
    let stored = store
        .objects
        .iter()
        .flat_map(|o| std::iter::once(o.id).chain(o.children.iter().copied()))
        .chain(store.root.iter().copied());
    let attributes = member
        .child(&[ATTRIBUTES])
        .into_iter()
        .flat_map(|a| a.children.iter())
        .filter_map(|record| serial::decode(&record.segment));
    let indexed = member
        .child(&[FIELD_INDEX])
        .into_iter()
        .flat_map(|index| index.children.iter())
        .flat_map(|occurrence| occurrence.children.iter())
        .flat_map(|field| field.children.iter())
        .filter_map(|object| serial::decode(&object.segment));
    let mut referenced = Vec::new();
    let mut pending: Vec<&OwnedNode> = [OBJECT_INDEX, TAB_ORDER]
        .into_iter()
        .filter_map(|key| member.child(&[key]))
        .collect();
    while let Some(node) = pending.pop() {
        for entry in &node.children {
            referenced.extend(
                entry
                    .value
                    .as_deref()
                    .and_then(crate::layout_objects::indexed_object),
            );
            pending.push(entry);
        }
    }
    next_object_key(
        stored
            .chain(attributes)
            .chain(indexed)
            .chain(referenced)
            .max()
            .unwrap_or(0),
    )
}

/// The attribute record of a new object: its empty name, and its text.
fn attributes(key: u64, label: Option<&str>) -> OwnedNode {
    let mut record = OwnedNode::list(serial::encode(key), Vec::new());
    if let Some(text) = label {
        record.set_child(om_scripts::text_node(&[TEXT], text));
        // A plain label's data format in the templates: a text height of 0.
        record.set_child(OwnedNode::leaf(vec![FORMAT], vec![0, 0]));
    }
    record.set_child(OwnedNode::leaf(vec![NAME], Vec::new()));
    record
}

/// Whether `object` is the field object [`plan`] makes for `placement`,
/// whatever its key.
pub fn is_placed_field(object: &LayoutObject, placement: &Placement) -> bool {
    let (Some(table_key), Some(field_key)) = (
        serial_value(placement.occurrence),
        serial_value(placement.field),
    ) else {
        return false;
    };
    let expected = encode::field_object(object.id, &placement.frame, table_key, field_key);
    LayoutObject::decode(&expected).is_ok_and(|made| made == strip(object))
}

/// Whether `object` is the label object [`plan`] makes for `label`, its
/// text as decoded from the layout's attributes.
pub fn is_placed_label(object: &LayoutObject, label: &Label) -> bool {
    let made = encode::text_object(object.id, &label.frame);
    object.text.as_deref() == Some(label.text.as_str())
        && LayoutObject::decode(&made).is_ok_and(|made| made == strip(object))
}

/// A layout member `[4, 5, L]` decoded as the desktop client decodes a
/// hosted layout (`om_layout::Layout::decode_from_tree`): its objects with
/// their attributes (names, UUIDs, texts). `None` when it does not decode.
pub fn decode(member: &OwnedNode) -> Option<om_layout::Layout> {
    let id = be_uint(&member.segment)?;
    let wire = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            vec![4],
            vec![OwnedNode::list(vec![5], vec![member.clone()])],
        )],
    )
    .to_wire()?;
    let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&wire)).ok()?;
    om_layout::Layout::decode_from_tree(&tree, id).ok()?
}

/// The objects an earlier placement of `placement` made on the layout
/// `member`, if it has them on top of its root stack: the field object as
/// [`plan`] makes it, then its label if it has one. Their keys.
pub fn already_placed(member: &OwnedNode, placement: &Placement) -> Option<(u64, Option<u64>)> {
    let layout = decode(member)?;
    let root = &layout.objects.root;
    let top = |back: usize| {
        root.len()
            .checked_sub(back)
            .and_then(|i| layout.objects.get(root[i]))
    };
    match &placement.label {
        None => top(1)
            .filter(|field| is_placed_field(field, placement))
            .map(|field| (field.id, None)),
        Some(label) => {
            let field = top(2).filter(|f| is_placed_field(f, placement))?;
            let text = top(1).filter(|t| is_placed_label(t, label))?;
            Some((field.id, Some(text.id)))
        }
    }
}

/// `object` as its protobuf alone decodes, without what a layout's
/// attribute records add to it.
fn strip(object: &LayoutObject) -> LayoutObject {
    LayoutObject {
        parent: None,
        name: None,
        uuid: None,
        tab_order: 0,
        text: None,
        text_runs: Vec::new(),
        rich_text: None,
        data_format: None,
        font_height: None,
        graphic_format: None,
        script_parameter: None,
        script_parameter_unsupported: false,
        placeholder: None,
        tooltip: None,
        calcs: Default::default(),
        triggers: Vec::new(),
        ..object.clone()
    }
}

/// Plan `placement` into `member`, a layout's catalog member `[4, 5, L]`
/// as stored.
///
/// The layout must be a layout (not a folder or separator) with a
/// definition, not responsive, showing records of the placement's
/// occurrence, with a body part holding both frames and a list of its
/// objects, and objects OpenMaker reads. The frames may overlap the layout's simple objects
/// (which stay beneath) but not containers (groups, portals, tab and
/// slide controls, popovers, button bars), whose contents FileMaker Pro
/// would make them, nor each other.
pub fn plan(member: &OwnedNode, placement: &Placement) -> Result<Planned, Refusal> {
    let planned = plan_objects(member, Some(placement), placement.label.as_ref())?;
    Ok(Planned {
        field_object: planned
            .field_object
            .expect("a field placement places a field"),
        label_object: planned.label_object,
        part: planned.part,
        stacking: planned.stacking,
        overlaps: planned.overlaps,
        store: planned.store,
        member: planned.member,
        changes: planned.changes,
    })
}

/// A text the Text tool places on layout `layout`'s body on its own, as
/// the window asks for it ([`crate::DataProvider::place_layout_text`]):
/// its text and frame, and the layout's revision the window showed.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutTextPlacement {
    pub layout: u32,
    pub text: Label,
    pub expected: Revision,
}

/// A text the host placed, or found already placed by an earlier request
/// whose answer was lost (`changed` false): its key, and the layout's
/// revision after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedLayoutText {
    pub changed: bool,
    pub text_object: u64,
    pub revision: Revision,
}

/// A text placed on its own (the Text tool), planned into a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedText {
    /// The text object's key: the next after every key the layout uses.
    pub text_object: u64,
    pub part: u64,
    /// Its position in the root stack: on top of every stored object.
    pub stacking: usize,
    /// The stored objects it overlaps (they stay beneath it).
    pub overlaps: Vec<u64>,
    /// The layout member `[4, 5, L]` with the text, without UUIDs or
    /// stamps.
    pub member: OwnedNode,
    /// What changed in the member, as [`Planned::changes`].
    pub changes: Vec<(String, String)>,
}

/// Plan `text` into `member` on its own, as the Text tool places one: a
/// text object as [`plan`] makes a field's label, on top of the body's
/// objects, with its attribute record (its text) and its entry in the
/// body's list, and the last key issued raised. A text has no object
/// index entry and shows no field (layout-model.md 10.5).
pub fn plan_text(member: &OwnedNode, text: &Label) -> Result<PlannedText, Refusal> {
    let planned = plan_objects(member, None, Some(text))?;
    Ok(PlannedText {
        text_object: planned
            .label_object
            .expect("a text placement places a text"),
        part: planned.part,
        stacking: planned.stacking,
        overlaps: planned.overlaps,
        member: planned.member,
        changes: planned.changes,
    })
}

/// The text object an earlier placement of `text` on its own made on the
/// layout `member`, if it is on top of its root stack: its key.
pub fn already_placed_text(member: &OwnedNode, text: &Label) -> Option<u64> {
    let layout = decode(member)?;
    let top = layout.objects.get(*layout.objects.root.last()?)?;
    is_placed_label(top, text).then_some(top.id)
}

/// New objects planned into a layout: a field object, a text, or both.
struct PlannedObjects {
    field_object: Option<u64>,
    label_object: Option<u64>,
    part: u64,
    stacking: usize,
    overlaps: Vec<u64>,
    store: Vec<u8>,
    member: OwnedNode,
    changes: Vec<(String, String)>,
}

/// The definition of `member`, a layout objects can be placed on: not a
/// folder or separator, and not responsive.
fn placeable(member: &OwnedNode) -> Result<LayoutDef, Refusal> {
    let flags = member
        .child(&[MEMBER_FLAGS])
        .and_then(|n| n.value.as_deref())
        .and_then(be_uint)
        .unwrap_or(0);
    if flags & NOT_A_LAYOUT != 0 {
        return Err(Refusal::Unsupported(
            "This member of the layout catalog is a folder or a separator, not a layout.".into(),
        ));
    }
    let def = definition(member)?;
    if def.layout_client_type == RESPONSIVE {
        return Err(Refusal::Unsupported(
            "The layout is responsive; OpenMaker places objects on fixed layouts only.".into(),
        ));
    }
    Ok(def)
}

/// Where new objects go: the body part, and the stored objects their
/// frames overlap.
struct OnBody {
    body: Part,
    stored: Vec<u8>,
    objects: ObjectStore,
    /// The stored objects a new frame overlaps (they stay beneath).
    overlaps: Vec<u64>,
}

/// Check that each of `frames` (what it frames, and the frame) is inside
/// `member`'s body part and overlaps no container: objects are placed on
/// the body, and not in or over groups, portals, tab and slide controls,
/// popovers or button bars.
fn on_body(member: &OwnedNode, frames: &[(&str, &Rect)]) -> Result<OnBody, Refusal> {
    let body = parts(member)?
        .into_iter()
        .find(|p| p.kind() == PartKind::Body)
        .ok_or_else(|| {
            Refusal::Unsupported(
                "The layout has no body part; this version places objects in the body only.".into(),
            )
        })?;
    for (what, frame) in frames {
        if frame.top < body.top || frame.bottom > body.bottom() {
            return Err(Refusal::Invalid(format!(
                "The {what}'s frame ({}) is not inside the body part, which runs from {} to {} \
                 points down the layout.",
                frame_text(frame),
                body.top,
                body.bottom()
            )));
        }
    }
    let (stored, objects) = stored_objects(member)?;
    let geometry = om_layout::Layout {
        objects: objects.clone(),
        ..Default::default()
    };
    let mut overlaps = Vec::new();
    for object in &objects.objects {
        let Some(bounds) = geometry.absolute_bounds(object.id) else {
            continue;
        };
        if !frames.iter().any(|(_, frame)| overlap(frame, &bounds)) {
            continue;
        }
        if is_container(object.kind()) {
            return Err(Refusal::Unsupported(format!(
                "The frame overlaps object {} (a {}); this version does not place objects in or \
                 over groups, portals, tab and slide controls, popovers or button bars.",
                object.id,
                kind_name(object.kind())
            )));
        }
        overlaps.push(object.id);
    }
    overlaps.sort_unstable();
    Ok(OnBody {
        body,
        stored,
        objects,
        overlaps,
    })
}

/// The key of a new entry in `member`'s tab order `15`: one past the
/// greatest stored, so the stored order and its gaps stay
/// (layout-model.md 10.5).
fn next_tab_position(member: &OwnedNode) -> Result<[u8; 2], Refusal> {
    let unsupported = |why: &str| Refusal::Unsupported(why.into());
    let tab_order = member.child(&[TAB_ORDER]);
    let bad_order = || unsupported("The layout's tab order is not one OpenMaker reads.");
    if tab_order.is_some_and(|order| order.value.is_some()) {
        return Err(bad_order());
    }
    let last = tab_order
        .into_iter()
        .flat_map(|order| &order.children)
        .map(|entry| <[u8; 2]>::try_from(entry.segment.as_slice()).map(u16::from_be_bytes))
        .try_fold(0u16, |last, position| {
            position.map(|position| last.max(position))
        })
        .map_err(|_| bad_order())?;
    let position = last
        .checked_add(1)
        .ok_or_else(|| unsupported("The layout's tab order is full."))?;
    Ok(position.to_be_bytes())
}

/// The key of the first object a placement makes: after every key the
/// layout uses, every key its part lists name and the last one it issued.
fn first_new_key(member: &OwnedNode, objects: &ObjectStore) -> Result<u64, Refusal> {
    let issued = last_issued(member)?;
    Ok(next_key(member, objects)?
        .max(issued.map(next_object_key).transpose()?.unwrap_or(0))
        .max(next_object_key(largest_listed(member))?))
}

/// Plan a field object showing `field`'s field (`field.label` is not
/// read) and a text `label`, either or both, into `member` ([`plan`],
/// [`plan_text`]).
fn plan_objects(
    member: &OwnedNode,
    field: Option<&Placement>,
    label: Option<&Label>,
) -> Result<PlannedObjects, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    let def = placeable(member)?;
    let keys = match field {
        Some(placement) => {
            let occurrence = om_layout::occurrence_id(def.table_alias_key);
            if occurrence != Some(placement.occurrence) {
                return Err(unsupported(format!(
                    "The field is not one of the layout's own table occurrence ({}); this \
                     version places fields of the layout's table only, not related fields.",
                    occurrence.map_or_else(|| "none".into(), |o| o.to_string())
                )));
            }
            let (Some(table_key), Some(field_key)) = (
                serial_value(placement.occurrence),
                serial_value(placement.field),
            ) else {
                return Err(Refusal::Invalid(
                    "The occurrence or field id is not a catalog key.".into(),
                ));
            };
            check_frame(&placement.frame, def.base_width, "field")?;
            Some((table_key, field_key))
        }
        None => None,
    };
    // A text placed with a field is its label; on its own, a text.
    let label_name = if field.is_some() { "label" } else { "text" };
    if let Some(label) = label {
        check_label(&label.text)?;
        check_frame(&label.frame, def.base_width, label_name)?;
        if field.is_some_and(|placement| overlap(&label.frame, &placement.frame)) {
            return Err(Refusal::Invalid(
                "The label's frame overlaps the field's.".into(),
            ));
        }
    }

    let frames: Vec<(&str, &Rect)> = field
        .map(|placement| ("field", &placement.frame))
        .into_iter()
        .chain(label.map(|label| (label_name, &label.frame)))
        .collect();
    let OnBody {
        body,
        stored,
        objects,
        overlaps,
    } = on_body(member, &frames)?;

    // The new objects, on top of the root stack, with keys after every
    // key the layout uses, every key its part lists name and the last one
    // it issued.
    let issued = last_issued(member)?;
    let first = first_new_key(member, &objects)?;
    let field_object = field.map(|_| first);
    let label_object = match (field_object, label) {
        (Some(key), Some(_)) => Some(next_object_key(key)?),
        (None, Some(_)) => Some(first),
        (_, None) => None,
    };
    // The body part's list of its top-level objects gains them at its end,
    // in stacking order; every stored entry and byte stays as it was.
    let unreadable_list = || {
        unsupported(
            "The body part has no list of its objects that OpenMaker reads; every part FileMaker \
             saves has one, and OpenMaker does not place objects without it."
                .into(),
        )
    };
    let body_node = part_node(member, body.id).ok_or_else(unreadable_list)?;
    let stored_list_node = body_node
        .child(&[PART_OBJECTS])
        .ok_or_else(unreadable_list)?;
    let stored_list = stored_list_node
        .value
        .as_deref()
        .ok_or_else(unreadable_list)?;
    let listed = part_list(stored_list).ok_or_else(unreadable_list)?;
    let added: Vec<u64> = field_object.into_iter().chain(label_object).collect();
    let mut part_list_value = Vec::with_capacity(stored_list.len() + 8);
    part_list_value.extend(
        u32::try_from(listed.len() + added.len())
            .map_err(|_| unreadable_list())?
            .to_be_bytes(),
    );
    part_list_value.extend_from_slice(&stored_list[4..]);
    for &key in &added {
        part_list_value.extend(part_list_entry(key));
    }
    if part_list_value.len() > MAX_INLINE_VALUE {
        return Err(unsupported(
            "The body part lists too many objects for OpenMaker to add another.".into(),
        ));
    }
    // The edit box's object index entry: as the last root, its pre-order
    // index is the number of objects in the tree before it, which no entry
    // may hold already. A text has none.
    let index_entry = match (field, field_object) {
        (Some(placement), Some(key)) => {
            let sequence = tree_size(&objects)?;
            if largest_indexed(member).is_some_and(|largest| largest >= sequence) {
                return Err(unsupported(
                    "The layout's object index does not match its objects; OpenMaker does not \
                     place objects on it."
                        .into(),
                ));
            }
            let (index_key, index_value) =
                object_index_entry(placement.frame.top, placement.frame.left, sequence, key)?;
            let position = next_tab_position(member)?;
            Some((index_key, index_value, sequence, position))
        }
        _ => None,
    };
    let mut new_objects = Vec::new();
    let mut roots = Vec::new();
    if let (Some(key), Some(placement), Some((table_key, field_key))) = (field_object, field, keys)
    {
        new_objects.push(encode::field_object(
            key,
            &placement.frame,
            table_key,
            field_key,
        ));
        roots.push(key);
    }
    if let (Some(key), Some(label)) = (label_object, label) {
        new_objects.push(encode::text_object(key, &label.frame));
        roots.push(key);
    }
    let store = encode::append_to_store(&stored, &roots, &new_objects);

    let mut next = member.clone();
    let mut changes = Vec::new();
    next.set_child(stored_value(&[OBJECTS], &store));
    let described = match (field_object, label_object) {
        (Some(field), Some(label)) => format!("field object {field} and label object {label}"),
        (Some(field), None) => format!("field object {field}"),
        (None, Some(text)) => format!("text object {text}"),
        (None, None) => String::new(),
    };
    changes.push((
        "[4, 5, L, 07]".to_owned(),
        format!(
            "the object store: {described} added on top of the root stack ({} bytes appended to \
             the {} stored, which are kept)",
            store.len() - stored.len(),
            stored.len()
        ),
    ));
    if let (Some(key), Some((_, _, _, position))) = (field_object, &index_entry) {
        let mut record = attributes(key, None);
        record.set_child(OwnedNode::leaf(vec![TAB_POSITION], position.to_vec()));
        changes.push((
            format!("[4, 5, L, 05, {}]", hex(&record.segment)),
            format!("object {key}'s attributes: tab position {}, no name, and the UUIDs d8 and d9 the host makes", u16::from_be_bytes(*position)),
        ));
        next.put(&[&[ATTRIBUTES], &record.segment.clone()], record);
    }
    if let (Some(key), Some(label)) = (label_object, label) {
        let record = attributes(key, Some(&label.text));
        changes.push((
            format!("[4, 5, L, 05, {}]", hex(&record.segment)),
            format!(
                "object {key}'s attributes: the {label_name} text, a text height of 0, no name, \
                 and the UUIDs the host makes"
            ),
        ));
        next.put(&[&[ATTRIBUTES], &record.segment.clone()], record);
    }
    if let (Some(placement), Some(key)) = (field, field_object) {
        let occurrence_key = uint_segment(placement.occurrence);
        let field_segment = uint_segment(placement.field);
        let object_segment = serial::encode(key);
        changes.push((
            format!(
                "[4, 5, L, 01, {}, {}, {}]",
                hex(&occurrence_key),
                hex(&field_segment),
                hex(&object_segment)
            ),
            format!(
                "the field reference index: field {} of occurrence {} shown by object {key}",
                placement.field, placement.occurrence
            ),
        ));
        next.put(
            &[
                &[FIELD_INDEX],
                &occurrence_key,
                &field_segment,
                &object_segment,
            ],
            OwnedNode::leaf(Vec::new(), Vec::new()),
        );
    }
    if let (Some(placement), Some(key), Some((index_key, index_value, sequence, position))) =
        (field, field_object, index_entry)
    {
        changes.push((
            format!("[4, 5, L, 0a, {}]", hex(&index_key)),
            format!(
                "the object index: object {key} at top {}, left {}, pre-order index \
                 {sequence}, repetition 1",
                placement.frame.top, placement.frame.left
            ),
        ));
        next.put(
            &[&[OBJECT_INDEX], &index_key],
            OwnedNode::leaf(Vec::new(), index_value.clone()),
        );
        next.put(
            &[&[TAB_ORDER], &position],
            OwnedNode::leaf(Vec::new(), index_value),
        );
        changes.push((
            format!("[4, 5, L, 15, {}]", hex(&position)),
            format!("the tab order: field object {key} after the existing entries"),
        ));
    }
    let last = label_object
        .or(field_object)
        .ok_or_else(|| Refusal::Invalid("A placement places a field, a text or both.".into()))?;
    changes.push((
        "[4, 5, L, 05, 00]".to_owned(),
        format!(
            "the last object key issued: {last} (was {})",
            issued.map_or_else(|| "none".into(), |k| k.to_string())
        ),
    ));
    let mut counter = member
        .at(&[&[ATTRIBUTES], &[LAST_KEY]])
        .cloned()
        .unwrap_or_else(|| OwnedNode::leaf(Vec::new(), Vec::new()));
    counter.value = Some(key_string(last));
    next.put(&[&[ATTRIBUTES], &[LAST_KEY]], counter);
    changes.push((
        format!("[4, 5, L, 03, {}, 07]", hex(&body_node.segment)),
        format!(
            "the body part's list of its objects: {} added at its end ({} listed before)",
            added
                .iter()
                .map(|key| format!("object {key}"))
                .collect::<Vec<_>>()
                .join(" and "),
            listed.len()
        ),
    ));
    let mut list_node = stored_list_node.clone();
    list_node.value = Some(part_list_value);
    next.put(
        &[&[PARTS], &body_node.segment.clone(), &[PART_OBJECTS]],
        list_node,
    );
    // The field's column in the stored table view, where the layout has
    // one: FileMaker Pro shows that table, so a field without its column
    // there is no column of it (#815).
    if let (Some(placement), Some(key), Some(keys)) = (field, field_object, keys) {
        if let Some(view) = next.child(&[TABLE_VIEW]) {
            let format = column_format(view)?;
            let view = with_table_column(view, keys, placement)?;
            changes.push((
                "[4, 5, L, 0d]".to_owned(),
                format!(
                    "the stored table view: a column for object {key}'s field, and a new empty \
                     column after it in place of the one there was, each with the columns' \
                     data format record"
                ),
            ));
            next.set_child(view);
            // The field object's data format, as Pro gives a field it
            // places the columns' record (layout-model.md 10.14).
            next.put(
                &[&[ATTRIBUTES], &serial::encode(key), &[FORMAT]],
                OwnedNode::leaf(vec![FORMAT], format),
            );
            changes.push((
                format!("[4, 5, L, 05, {}, 0d]", hex(&serial::encode(key))),
                format!("object {key}'s data format: the table view columns' record"),
            ));
        }
    }
    Ok(PlannedObjects {
        field_object,
        label_object,
        part: body.id,
        stacking: objects.root.len(),
        overlaps,
        store,
        member: next,
        changes,
    })
}

/// A button to place on a layout's body (#226): its frame, its label, and
/// what it does when clicked.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonPlacement {
    pub frame: Rect,
    pub label: String,
    pub action: ButtonAction,
}

/// A button's action, as Button Setup sets it.
#[derive(Clone, Debug, PartialEq)]
pub enum ButtonAction {
    /// Perform Script with a script chosen from the list and an optional
    /// parameter.
    PerformScript {
        /// The script's id as FileMaker shows it (`ScriptIDs`; the serial
        /// decode of its catalog key, as [`om_scripts::Script::id`]).
        script: u64,
        /// The script parameter: a step calculation, as the script
        /// workspace compiles one.
        parameter: Option<om_scripts::Calculation>,
    },
    /// Single Step: one other script step with its options, as the Script
    /// Workspace stores a step ([`check_button_step`]).
    Step(om_scripts::ScriptStep),
}

impl ButtonAction {
    /// The step the button stores, or why it cannot be placed.
    pub fn step(&self) -> Result<om_scripts::ScriptStep, Refusal> {
        match self {
            ButtonAction::PerformScript { script, parameter } => {
                perform_script_step(*script, parameter.as_ref())
            }
            ButtonAction::Step(step) => {
                check_button_step(step)?;
                Ok(step.clone())
            }
        }
    }
}

/// The most a single step's data may take, encoded in the wire form: far
/// above any step the Script Workspace makes, and small next to a layout.
pub const MAX_BUTTON_STEP_DATA: usize = 64 * 1024;

/// Whether `step` can be a button's Single Step here: a step the Script
/// Workspace offers (not an obsolete id), one FileMaker Pro allows on a
/// button (15 are not: the control blocks, comments, error capture, user
/// abort, transactions and Set Layout Object Animation; the step catalog's
/// `not_button_compatible`), not Perform Script (the Perform Script
/// action), with nothing stored that its schema does not explain (what
/// the Script Workspace cannot edit), every calculation's streams
/// decodable, and its data within [`MAX_BUTTON_STEP_DATA`].
pub fn check_button_step(step: &om_scripts::ScriptStep) -> Result<(), Refusal> {
    let id = step.id();
    let name = om_scripts::step_name(id)
        .filter(|_| !om_scripts::OBSOLETE_STEP_IDS.contains(&id))
        .ok_or_else(|| Refusal::Invalid(format!("Step {id} is not a script step.")))?;
    if id == 1 {
        return Err(Refusal::Invalid(
            "Perform Script is the button's Perform Script action, not a single step.".into(),
        ));
    }
    if !step.is_enabled() {
        return Err(Refusal::Invalid(format!(
            "A button's \u{201c}{name}\u{201d} cannot be a disabled step."
        )));
    }
    if !om_scripts::step_compat(id).is_some_and(|c| c.button_compatible()) {
        return Err(Refusal::Invalid(format!(
            "FileMaker Pro does not allow \u{201c}{name}\u{201d} on a button."
        )));
    }
    if !step.unclassified().is_empty() {
        return Err(Refusal::Unsupported(format!(
            "\u{201c}{name}\u{201d} stores options OpenMaker does not edit; it is not placed on a \
             button."
        )));
    }
    let Some(data) = step.data.as_ref().filter(|data| !data.is_empty()) else {
        return Ok(());
    };
    let size = OwnedNode::list(Vec::new(), vec![data.encode(&[STEP_DATA])])
        .to_wire()
        .map(|wire| wire.len());
    if size.is_none_or(|size| size > MAX_BUTTON_STEP_DATA) {
        return Err(Refusal::Invalid(format!(
            "\u{201c}{name}\u{201d}'s options take more than {} KB.",
            MAX_BUTTON_STEP_DATA / 1024
        )));
    }
    // Every typed node a slot holds must read as its kind: text, a number,
    // a field spec, and a calculation with a stream to evaluate whose
    // streams decode. A node that is there but does not read is refused,
    // not skipped.
    for (slot, node) in data.slots() {
        let unreadable = (node.child(0x01).is_some() && step.text(slot).is_none())
            || (node.child(0x03).is_some() && step.number(slot).is_none())
            || (slot != 0 && node.child(0x10).is_some() && step.slot_field_spec(slot).is_none());
        if unreadable {
            return Err(Refusal::Invalid(format!(
                "\u{201c}{name}\u{201d} has an option OpenMaker does not read."
            )));
        }
        if node.child(0x05).is_none() {
            continue;
        }
        let readable = step.calculation(slot).is_some_and(|calc| {
            let polish = calc
                .polish
                .as_deref()
                .is_none_or(|bytes| crate::formula::polish::decode(bytes).is_ok());
            let formatted = calc.formatted.as_deref().is_none_or(|bytes| {
                bytes.is_empty() || crate::formula::formatted::decode(bytes).is_ok()
            });
            calc.stream().is_some() && polish && formatted
        });
        if !readable {
            return Err(Refusal::Invalid(format!(
                "\u{201c}{name}\u{201d} has a calculation OpenMaker does not read."
            )));
        }
    }
    Ok(())
}

/// A button placement planned into a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedButton {
    /// The button object's key: the next after every key the layout uses.
    pub button: u64,
    pub part: u64,
    /// Its position in the root stack: on top of every stored object.
    pub stacking: usize,
    /// The stored objects it overlaps (they stay beneath it).
    pub overlaps: Vec<u64>,
    /// The layout member `[4, 5, L]` with the button, without UUIDs or
    /// stamps.
    pub member: OwnedNode,
    /// What changed in the member, as [`Planned::changes`].
    pub changes: Vec<(String, String)>,
}

/// The `mStepValue` the templates' Perform Script buttons store (34 of 42;
/// the others 0). What it selects is not mapped (script-steps.md 3).
pub const PERFORM_SCRIPT_BUTTON_VALUE: u16 = 4;

/// The single step of a button that performs `script` with `parameter`:
/// Perform Script (1), the script key in `mReserved` (`kScriptKey`) and
/// the parameter in calculation slot 0 (`kCalculation`), as the templates'
/// buttons store them (script-steps.md 4.1, 5.1; layout-model.md 5.3.3).
pub fn perform_script_step(
    script: u64,
    parameter: Option<&om_scripts::Calculation>,
) -> Result<om_scripts::ScriptStep, Refusal> {
    let key = om_scripts::StoredKey::from_serial_id(script)
        .ok_or_else(|| Refusal::Invalid(format!("Script {script} is not a script id.")))?;
    let mut step = om_scripts::ScriptStep::new(1);
    step.value = PERFORM_SCRIPT_BUTTON_VALUE;
    step.set_script_key(Some(key));
    if let Some(parameter) = parameter {
        step.set_calculation(0, Some(parameter));
    }
    Ok(step)
}

/// Plan placing `placement`'s button on the layout `member`: a text
/// button (`LayoutObject` type 10, style type 80) with its action's step,
/// on top of the body part's objects, kept in the layout's indexes and
/// part list as [`plan`] keeps a field object. Its attribute record holds
/// its label (`01`), a text height of 0 (`0d`), an empty name (`10`) and,
/// when the step has data (a parameter, a single step's options), the
/// step's data (`11`); it is not added to the tab order. The same refusals as [`plan`]: a folder, a responsive layout, a
/// frame outside the body, between whole points or over a container.
pub fn plan_button(
    member: &OwnedNode,
    placement: &ButtonPlacement,
) -> Result<PlannedButton, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    let flags = member
        .child(&[MEMBER_FLAGS])
        .and_then(|n| n.value.as_deref())
        .and_then(be_uint)
        .unwrap_or(0);
    if flags & NOT_A_LAYOUT != 0 {
        return Err(unsupported(
            "This member of the layout catalog is a folder or a separator, not a layout.".into(),
        ));
    }
    let def = definition(member)?;
    if def.layout_client_type == RESPONSIVE {
        return Err(unsupported(
            "The layout is responsive; OpenMaker places objects on fixed layouts only.".into(),
        ));
    }
    check_label(&placement.label)?;
    check_frame(&placement.frame, def.base_width, "button")?;
    let step = placement.action.step()?;
    let parts = parts(member)?;
    let body = parts
        .iter()
        .find(|p| p.kind() == PartKind::Body)
        .ok_or_else(|| {
            unsupported(
                "The layout has no body part; this version places buttons in the body only.".into(),
            )
        })?;
    let frame = &placement.frame;
    if frame.top < body.top || frame.bottom > body.bottom() {
        return Err(Refusal::Invalid(format!(
            "The button's frame ({}) is not inside the body part, which runs from {} to {} \
             points down the layout.",
            frame_text(frame),
            body.top,
            body.bottom()
        )));
    }
    let (stored, objects) = stored_objects(member)?;
    let geometry = om_layout::Layout {
        objects: objects.clone(),
        ..Default::default()
    };
    let mut overlaps = Vec::new();
    for object in &objects.objects {
        let Some(bounds) = geometry.absolute_bounds(object.id) else {
            continue;
        };
        if !overlap(frame, &bounds) {
            continue;
        }
        if is_container(object.kind()) {
            return Err(unsupported(format!(
                "The frame overlaps object {} (a {}); this version does not place objects in or \
                 over groups, portals, tab and slide controls, popovers or button bars.",
                object.id,
                kind_name(object.kind())
            )));
        }
        overlaps.push(object.id);
    }
    overlaps.sort_unstable();

    let issued = last_issued(member)?;
    let button = next_key(member, &objects)?
        .max(issued.map(next_object_key).transpose()?.unwrap_or(0))
        .max(next_object_key(largest_listed(member))?);
    let unreadable_list = || {
        unsupported(
            "The body part has no list of its objects that OpenMaker reads; every part FileMaker \
             saves has one, and OpenMaker does not place objects without it."
                .into(),
        )
    };
    let body_node = part_node(member, body.id).ok_or_else(unreadable_list)?;
    let stored_list_node = body_node
        .child(&[PART_OBJECTS])
        .ok_or_else(unreadable_list)?;
    let stored_list = stored_list_node
        .value
        .as_deref()
        .ok_or_else(unreadable_list)?;
    let listed = part_list(stored_list).ok_or_else(unreadable_list)?;
    let mut part_list_value = Vec::with_capacity(stored_list.len() + 4);
    part_list_value.extend(
        u32::try_from(listed.len() + 1)
            .map_err(|_| unreadable_list())?
            .to_be_bytes(),
    );
    part_list_value.extend_from_slice(&stored_list[4..]);
    part_list_value.extend(part_list_entry(button));
    if part_list_value.len() > MAX_INLINE_VALUE {
        return Err(unsupported(
            "The body part lists too many objects for OpenMaker to add another.".into(),
        ));
    }
    let sequence = tree_size(&objects)?;
    if largest_indexed(member).is_some_and(|largest| largest >= sequence) {
        return Err(unsupported(
            "The layout's object index does not match its objects; OpenMaker does not place \
             objects on it."
                .into(),
        ));
    }
    let (index_key, index_value) = object_index_entry(frame.top, frame.left, sequence, button)?;
    let object = encode::button_object(
        button,
        frame,
        step.id(),
        step.value,
        step.flags,
        &step.reserved,
    );
    let store = encode::append_to_store(&stored, &[button], &[object]);

    let mut next = member.clone();
    let mut changes = vec![(
        "[4, 5, L, 07]".to_owned(),
        format!(
            "the object store: button object {button} added on top of the root stack ({} bytes \
             appended to the {} stored, which are kept)",
            store.len() - stored.len(),
            stored.len()
        ),
    )];
    next.set_child(stored_value(&[OBJECTS], &store));
    let mut record = attributes(button, Some(&placement.label));
    let data = step_data_node(&placement.action, &step);
    if let Some(data) = data.clone() {
        record.set_child(data);
    }
    changes.push((
        format!("[4, 5, L, 05, {}]", hex(&record.segment)),
        format!(
            "object {button}'s attributes: the label, a text height of 0, no name{}, and the \
             UUIDs the host makes",
            match (&data, &placement.action) {
                (None, _) => "",
                (Some(_), ButtonAction::PerformScript { .. }) => {
                    ", the script parameter (step data, calculation slot 0)"
                }
                (Some(_), ButtonAction::Step(_)) => ", the step's data (its options, if any)",
            }
        ),
    ));
    next.put(&[&[ATTRIBUTES], &record.segment.clone()], record);
    changes.push((
        format!("[4, 5, L, 0a, {}]", hex(&index_key)),
        format!(
            "the object index: object {button} at top {}, left {}, pre-order index {sequence}",
            frame.top, frame.left
        ),
    ));
    next.put(
        &[&[OBJECT_INDEX], &index_key],
        OwnedNode::leaf(Vec::new(), index_value),
    );
    changes.push((
        "[4, 5, L, 05, 00]".to_owned(),
        format!(
            "the last object key issued: {button} (was {})",
            issued.map_or_else(|| "none".into(), |k| k.to_string())
        ),
    ));
    let mut counter = member
        .at(&[&[ATTRIBUTES], &[LAST_KEY]])
        .cloned()
        .unwrap_or_else(|| OwnedNode::leaf(Vec::new(), Vec::new()));
    counter.value = Some(key_string(button));
    next.put(&[&[ATTRIBUTES], &[LAST_KEY]], counter);
    changes.push((
        format!("[4, 5, L, 03, {}, 07]", hex(&body_node.segment)),
        format!(
            "the body part's list of its objects: object {button} added at its end ({} listed \
             before)",
            listed.len()
        ),
    ));
    let mut list_node = stored_list_node.clone();
    list_node.value = Some(part_list_value);
    next.put(
        &[&[PARTS], &body_node.segment.clone(), &[PART_OBJECTS]],
        list_node,
    );
    Ok(PlannedButton {
        button,
        part: body.id,
        stacking: objects.root.len(),
        overlaps,
        member: next,
        changes,
    })
}

/// A button placement asked of the open file's provider
/// ([`crate::DataProvider::place_layout_button`]): what the desktop's Layout
/// mode saves after Button Setup, on layout `layout` (its catalog id), if
/// it is still at revision `expected` (as [`LayoutFieldPlacement`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutButtonPlacement {
    pub layout: u32,
    pub placement: ButtonPlacement,
    pub expected: Revision,
}

/// A button the host placed, or found already placed by an earlier request
/// whose answer was lost (`changed` false).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedLayoutButton {
    pub changed: bool,
    pub button: u64,
    /// The layout's revision after the placement.
    pub revision: Revision,
}

/// The button's step data node `11`, as the starter solutions store it:
/// a Perform Script button has one only with a parameter (2 of 35 text
/// buttons have none); every single-step button has one, empty when the
/// step has no options (42 of 42; layout-model.md 5.3.3).
fn step_data_node(action: &ButtonAction, step: &om_scripts::ScriptStep) -> Option<OwnedNode> {
    match step.data.as_ref().filter(|data| !data.is_empty()) {
        Some(data) => Some(data.encode(&[STEP_DATA])),
        None => matches!(action, ButtonAction::Step(_))
            .then(|| OwnedNode::list(vec![STEP_DATA], Vec::new())),
    }
}

/// Whether `object` has the frame, label and step descriptor (id, value,
/// flags and `mReserved`) [`plan_button`] gives the button for
/// `placement`'s `step`, whatever its key. The step's data is in the
/// attribute record, which [`already_placed_button`] compares too.
fn is_placed_button(
    object: &LayoutObject,
    placement: &ButtonPlacement,
    step: &om_scripts::ScriptStep,
) -> bool {
    let Some(stored) = object.button.as_ref().and_then(|b| b.script_step.as_ref()) else {
        return false;
    };
    object.kind() == ObjectKind::Button
        && object.bounds.as_ref().is_some_and(|b| {
            b.left == placement.frame.left
                && b.top == placement.frame.top
                && b.right == placement.frame.right
                && b.bottom == placement.frame.bottom
        })
        && stored.id == i32::from(step.id())
        && stored.value == u32::from(step.value)
        && stored.flags as u32 == step.flags
        && stored.reserved.as_slice() == step.reserved.as_slice()
        && object.text.as_deref() == Some(placement.label.as_str())
}

/// The button an earlier request placed for `placement`, if the layout
/// holds it: its key. Its step data (a parameter or a single step's
/// options, or none) must be the one the plan writes, byte for byte: a
/// button that differs only there is another placement, which a retry
/// must not report as its own.
pub fn already_placed_button(member: &OwnedNode, placement: &ButtonPlacement) -> Option<u64> {
    let layout = decode(member)?;
    let step = placement.action.step().ok()?;
    let data = step_data_node(&placement.action, &step);
    layout
        .objects
        .objects
        .iter()
        .find(|object| {
            is_placed_button(object, placement, &step)
                && member
                    .at(&[&[ATTRIBUTES], &serial::encode(object.id)])
                    .and_then(|record| record.child(&[STEP_DATA]))
                    == data.as_ref()
        })
        .map(|object| object.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_layout::encode::{bytes, double, uint};

    const OCCURRENCE: u64 = 0xd0_0001;
    const FIELD: u64 = 0x8000;

    fn rect(left: f64, top: f64, width: f64, height: f64) -> Rect {
        Rect {
            top,
            left,
            bottom: top + height,
            right: left + width,
            other: Vec::new(),
        }
    }

    /// A Form layout 480 points wide on [`OCCURRENCE`] with a header (0 to
    /// 40) and a body (40 to 240), a text object 1 and a field object 2 on
    /// its field 1, a portal 3, each part's list of its objects (the header
    /// empty, the body 1, 2, 3), and keys the decoder does not know (one of
    /// them under the body part).
    fn layout() -> OwnedNode {
        let mut def = Vec::new();
        uint(2, serial_value(OCCURRENCE).unwrap(), &mut def);
        double(8, 480.0, &mut def);
        let mut member = OwnedNode::list(vec![1], Vec::new());
        member.set_child(OwnedNode::leaf(vec![DEFINITION], def));
        for (id, kind, top, height) in [(1u8, 1u64, 0.0, 40.0), (2, 4, 40.0, 200.0)] {
            let mut part = Vec::new();
            uint(1, kind, &mut part);
            double(4, top, &mut part);
            double(5, height, &mut part);
            member.put(
                &[&[PARTS], &[id], &[DEFINITION]],
                OwnedNode::leaf(Vec::new(), part),
            );
            let list: &[u8] = match id {
                1 => &[0, 0, 0, 0],
                _ => &[0, 0, 0, 3, 2, 1, 1, 2, 1, 2, 2, 1, 3],
            };
            member.put(
                &[&[PARTS], &[id], &[PART_OBJECTS]],
                OwnedNode::leaf(Vec::new(), list.to_vec()),
            );
        }
        member.put(
            &[&[PARTS], &[2], &[0x77]],
            OwnedNode::leaf(Vec::new(), vec![9, 9]),
        );
        let mut portal = Vec::new();
        uint(1, 9, &mut portal);
        uint(2, 3, &mut portal);
        bytes(
            4,
            &encode::rect(&rect(20.0, 150.0, 300.0, 60.0)),
            &mut portal,
        );
        let mut store = Vec::new();
        for root in [1, 2, 3] {
            uint(1, root, &mut store);
        }
        bytes(
            2,
            &encode::text_object(1, &rect(20.0, 50.0, 80.0, 20.0)),
            &mut store,
        );
        bytes(
            2,
            &encode::field_object(2, &rect(110.0, 50.0, 200.0, 20.0), 1_065_089, 1),
            &mut store,
        );
        bytes(2, &portal, &mut store);
        uint(99, 5, &mut store);
        member.set_child(stored_value(&[OBJECTS], &store));
        member.put(
            &[&[ATTRIBUTES], &[1], &[TEXT]],
            om_scripts::text_node(&[], "Name"),
        );
        member.put(
            &[&[FIELD_INDEX], &uint_segment(OCCURRENCE), &[1], &[2]],
            OwnedNode::leaf(Vec::new(), Vec::new()),
        );
        member.set_child(OwnedNode::leaf(vec![0x77], vec![1, 2, 3]));
        member
    }

    fn placement(frame: Rect) -> Placement {
        Placement {
            occurrence: OCCURRENCE,
            field: FIELD,
            frame,
            label: Some(Label {
                text: "Follow up".into(),
                frame: rect(20.0, 80.0, 80.0, 20.0),
            }),
        }
    }

    fn decoded(member: &OwnedNode) -> om_layout::Layout {
        decode(member).unwrap()
    }

    /// A web viewer (#896) gets the records FileMaker Pro 26.1.1 wrote for
    /// its W2 viewer: the address `"https://example.com"` as the streams
    /// Pro stored under `00 00` (context `01 00`), the data format `00 0a`,
    /// no name; the body's list, the object index and the tab order end
    /// with it, and the last key issued is its key. An address reading a
    /// field adds that field's reference, valued `0d`. A retry finds it,
    /// and options Pro was not seen to store are refused.
    #[test]
    fn a_web_viewer_gets_the_records_filemaker_pro_wrote() {
        let unhex = |text: &str| -> Vec<u8> {
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
                .collect()
        };
        let compile = |source: &str| {
            let names = om_formula::NoNames;
            let compiled = om_formula::compile(source, &om_formula::Context::new(&names)).unwrap();
            (
                om_formula::formatted::encode(&compiled.tokens),
                om_formula::polish::encode(&compiled.expr),
            )
        };
        let before = layout();
        let (formatted, polish) = compile("\"https://example.com\"");
        let pro = unhex("1313322e2e2a296075753f223b372a363f7439353700");
        assert_eq!((&formatted, &polish), (&pro, &pro));
        let viewer = WebViewerPlacement {
            frame: rect(340.0, 60.0, 120.0, 60.0),
            flags: 0x800d,
            formatted,
            polish,
        };
        let planned = plan_web_viewer(&before, &viewer).unwrap();
        let key = planned.viewer;
        let member = &planned.member;
        let record = member.at(&[&[ATTRIBUTES], &serial::encode(key)]).unwrap();
        let value = |path: &[&[u8]]| record.at(path).and_then(|n| n.value.clone());
        assert_eq!(value(&[&[0, 0], &[5], &[4]]), Some(vec![1, 0]));
        assert_eq!(value(&[&[0, 0], &[5], &[5]]), Some(pro.clone()));
        assert_eq!(value(&[&[0, 0], &[5], &[6]]), Some(pro.clone()));
        assert_eq!(value(&[&[FORMAT]]), Some(vec![0, 0x0a]));
        assert!(record.child(&[NAME]).is_none());
        let layout = decoded(member);
        assert_eq!(layout.objects.root.last(), Some(&key));
        let object = layout.objects.get(key).unwrap();
        assert_eq!(object.web_viewer().map(|o| o.0), Some(0x800d));
        assert_eq!(
            part_list(
                member
                    .at(&[&[PARTS], &[2], &[PART_OBJECTS]])
                    .and_then(|n| n.value.as_deref())
                    .unwrap()
            )
            .unwrap(),
            [1, 2, 3, key]
        );
        let sequence = tree_size(&stored_objects(&before).unwrap().1).unwrap();
        let (index_key, index_value) = object_index_entry(60.0, 340.0, sequence, key).unwrap();
        assert_eq!(
            member
                .at(&[&[OBJECT_INDEX], &index_key])
                .and_then(|n| n.value.clone()),
            Some(index_value.clone())
        );
        assert_eq!(
            member
                .at(&[&[TAB_ORDER], &[0, 1]])
                .and_then(|n| n.value.clone()),
            Some(index_value)
        );
        assert_eq!(last_issued(member).unwrap(), Some(key));
        assert_eq!(already_placed_web_viewer(member, &viewer), Some(key));
        assert_eq!(already_placed_web_viewer(&before, &viewer), None);

        // A field the address reads, as Pro stored W1's address
        // `"https://example.com/?q=" & Contacts::Website` (field 7 of
        // occurrence d0 00 02): its reference, valued 0d.
        let reads = WebViewerPlacement {
            formatted: unhex(
                "1317322e2e2a296075753f223b372a363f7439353775652b67000c13017a00500c13017a0016040403d000020201070001",
            ),
            polish: unhex(
                "501317322e2e2a296075753f223b372a363f7439353775652b670016040403d000020201070001",
            ),
            ..viewer.clone()
        };
        let planned = plan_web_viewer(&before, &reads).unwrap();
        assert_eq!(
            planned
                .member
                .at(&[
                    &[FIELD_INDEX],
                    &[0xd0, 0, 2],
                    &[7],
                    &serial::encode(planned.viewer)
                ])
                .and_then(|n| n.value.clone()),
            Some(vec![0x0d])
        );
        // Text and evaluated calculation that disagree are refused: the
        // field shown would be neither read nor indexed.
        let mismatched = WebViewerPlacement {
            polish: viewer.polish.clone(),
            ..reads
        };
        assert!(matches!(
            plan_web_viewer(&before, &mismatched),
            Err(Refusal::Invalid(_))
        ));

        let unmeasured = WebViewerPlacement {
            flags: 0x800c,
            ..viewer
        };
        assert!(matches!(
            plan_web_viewer(&before, &unmeasured),
            Err(Refusal::Unsupported(_))
        ));
        assert_eq!(WebViewerSetup::DEFAULT.flags(), Some(0x800d));
    }

    /// A button performing script 3 with the parameter "Report" (#226):
    /// on top of the body's objects, indexed and listed as a field object
    /// is, its step and parameter as the templates' buttons store them
    /// (layout-model.md 5.3.3), and found again by a retry.
    /// A width replaces the definition's `baseWidth` in its place and
    /// keeps its other bytes; one out of range, or not a number, is refused.
    #[test]
    fn a_width_replaces_only_the_definitions_base_width() {
        let member = layout();
        let set = plan_width(&member, 700.0).unwrap();
        let def = |m: &OwnedNode| m.child(&[DEFINITION]).unwrap().value.clone().unwrap();
        let (before, after) = (def(&member), def(&set.member));
        assert_eq!(before.len(), after.len());
        assert_eq!(definition(&set.member).unwrap().base_width, 700.0);
        assert_eq!(
            definition(&set.member).unwrap().table_alias_key,
            definition(&member).unwrap().table_alias_key
        );
        assert!(already_width(&set.member, 700.0) && !already_width(&member, 700.0));
        // A width out of range, even one stored so, is never taken for one
        // set already.
        let wide = MAX_COORDINATE + 1.0;
        let mut stored = member.clone();
        let def = om_layout::encode::with_base_width(&def(&member), wide).unwrap();
        stored.set_child(OwnedNode::leaf(vec![DEFINITION], def));
        assert_eq!(definition(&stored).unwrap().base_width, wide);
        assert!(!already_width(&stored, wide));
        for width in [0.0, -1.0, f64::NAN, MAX_COORDINATE + 1.0] {
            assert!(plan_width(&member, width).is_err(), "{width}");
        }
    }

    /// Fixed page margins replace the definition's `customMargins` and keep
    /// its other fields; margins out of range are refused, and never taken
    /// for ones set already.
    #[test]
    fn fixed_page_margins_replace_only_the_definitions_margins() {
        let member = layout();
        let margins = Rect {
            top: 50.0,
            left: 70.0,
            bottom: 60.0,
            right: 80.0,
            ..Default::default()
        };
        let set = plan_margins(&member, &margins).unwrap();
        let (before, after) = (
            definition(&member).unwrap(),
            definition(&set.member).unwrap(),
        );
        assert_eq!(after.custom_margins.as_ref().unwrap().right, 80.0);
        assert_eq!(
            (after.base_width, after.table_alias_key),
            (before.base_width, before.table_alias_key)
        );
        assert!(already_margins(&set.member, &margins) && !already_margins(&member, &margins));
        for bad in [-1.0, f64::NAN, MAX_MARGIN + 1.0] {
            let margins = Rect {
                left: bad,
                ..margins.clone()
            };
            assert!(plan_margins(&member, &margins).is_err(), "{bad}");
            assert!(!already_margins(&set.member, &margins));
        }
    }

    /// Part Setup's options go in the part's `PartDef` in place, the rest
    /// of the member as stored; a leading sub-summary takes its break
    /// field. An option Part Definition does not offer for the part, a bit
    /// no option sets, both page breaks, a discard without breaking across
    /// pages, every N without N, a body's break field, a field the file
    /// lacks, an N out of range and a missing part are refused.
    #[test]
    fn part_options_go_in_the_parts_definition_only() {
        let mut member = layout();
        let mut summary = Vec::new();
        uint(1, 3, &mut summary);
        double(4, 240.0, &mut summary);
        double(5, 30.0, &mut summary);
        member.put(
            &[&[PARTS], &[3], &[DEFINITION]],
            OwnedNode::leaf(Vec::new(), summary),
        );
        let flags = part_options::PAGE_BREAK_EVERY | part_options::BREAK_ACROSS_PAGES;
        let set = plan_part_options(&member, 2, (flags, 3, None), false).unwrap();
        let body = &parts(&set.member).unwrap()[1];
        assert_eq!(
            (body.flags, body.page_break_after, body.top),
            (flags, 3, 40.0)
        );
        assert!(already_part_options(&set.member, 2, (flags, 3, None)));
        assert!(!already_part_options(&member, 2, (flags, 3, None)));
        let mut rest = set.member.clone();
        let stored = member.at(&[&[PARTS], &[2], &[DEFINITION]]).unwrap().clone();
        rest.at_mut(&[&[PARTS], &[2]]).unwrap().set_child(stored);
        assert_eq!(rest, member);
        let sorted = plan_part_options(&member, 3, (0, 0, Some((OCCURRENCE, 4))), true).unwrap();
        let field = parts(&sorted.member).unwrap()[2]
            .break_field()
            .unwrap()
            .unwrap();
        assert_eq!((field.occurrence, field.field), (OCCURRENCE, 4));
        // As Pro: a header restarts page numbers only; before and every N
        // exclude each other; discarding needs breaking across pages; N is
        // 1 to 32,767.
        // A navigation part's options are never set, so a request matching
        // them was never applied either.
        let mut navigation = Vec::new();
        uint(1, 12, &mut navigation);
        member.put(
            &[&[PARTS], &[4], &[DEFINITION]],
            OwnedNode::leaf(Vec::new(), navigation),
        );
        assert!(!already_part_options(&member, 4, (0, 0, None)));
        let restart = (part_options::RESTART_PAGE_NUMBERS, 0, None);
        assert!(plan_part_options(&member, 1, restart, false).is_ok());
        for refused in [
            plan_part_options(
                &member,
                1,
                (part_options::PAGE_BREAK_BEFORE, 0, None),
                false,
            ),
            plan_part_options(&member, 2, (0x8, 0, None), false),
            plan_part_options(&member, 2, (0x3, 3, None), false),
            plan_part_options(
                &member,
                2,
                (part_options::DISCARD_REMAINDER, 0, None),
                false,
            ),
            plan_part_options(&member, 2, (part_options::PAGE_BREAK_EVERY, 0, None), false),
            plan_part_options(&member, 2, (0, 0, Some((OCCURRENCE, 4))), true),
            plan_part_options(&member, 3, (0, 0, Some((OCCURRENCE, 9))), false),
            plan_part_options(
                &member,
                2,
                (0, part_options::MAX_PAGE_BREAK_AFTER + 1, None),
                false,
            ),
            plan_part_options(&member, 7, (0, 0, None), false),
        ] {
            assert!(refused.is_err());
        }
    }

    /// A text's words are written again in its attribute record `01`, the
    /// rest of the member as stored; long or non-ASCII words go in pieces
    /// and read back. A field object, empty words, a line break, a merge
    /// field, words over the bound, styled words and a missing object are
    /// refused.
    #[test]
    fn a_texts_words_change_in_its_attribute_record_only() {
        let member = layout();
        let words =
            |m: &OwnedNode| om_scripts::node_text(m.at(&[&[ATTRIBUTES], &[1], &[TEXT]]).unwrap());
        let set = plan_words(&member, 1, "Full name").unwrap();
        assert_eq!(words(&set.member).as_deref(), Some("Full name"));
        let mut rest = set.member.clone();
        let stored = member.at(&[&[ATTRIBUTES], &[1], &[TEXT]]).unwrap().clone();
        rest.at_mut(&[&[ATTRIBUTES], &[1]])
            .unwrap()
            .set_child(stored);
        assert_eq!(rest, member);
        assert!(already_words(&set.member, 1, "Full name"));
        assert!(!already_words(&member, 1, "Full name") && already_words(&member, 1, "Name"));
        let long = "é".repeat(600);
        let pieces = plan_words(&member, 1, &long).unwrap();
        assert_eq!(words(&pieces.member).as_deref(), Some(long.as_str()));
        // A store holding the key twice is refused, and never taken for
        // one where the words are already.
        let stored = member.child(&[OBJECTS]).unwrap().assembled().unwrap();
        let mut twice = stored.into_owned();
        bytes(
            2,
            &encode::text_object(1, &rect(20.0, 90.0, 80.0, 20.0)),
            &mut twice,
        );
        let mut doubled = member.clone();
        doubled.set_child(stored_value(&[OBJECTS], &twice));
        assert!(plan_words(&doubled, 1, "Full name").is_err());
        assert!(!already_words(&doubled, 1, "Name"));
        let mut styled = member.clone();
        styled.put(
            &[&[ATTRIBUTES], &[1], &[TEXT], &[0xff], &[1]],
            OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 0]),
        );
        for refused in [
            plan_words(&member, 2, "Name"),
            plan_words(&member, 1, ""),
            plan_words(&member, 1, "Full\rname"),
            plan_words(&member, 1, "<<Name>>"),
            plan_words(&member, 1, &"x".repeat(MAX_TEXT_CHARS + 1)),
            plan_words(&styled, 1, "Full name"),
            plan_words(&member, 9, "Full name"),
        ] {
            assert!(refused.is_err());
        }
        assert!(!already_words(&styled, 1, "Name") && !already_words(&member, 1, ""));
    }

    #[test]
    fn a_button_that_performs_a_script_goes_on_top_with_its_label_and_parameter() {
        let before = layout();
        let literal: Vec<u8> = [0x13, 6]
            .into_iter()
            .chain(b"Report".iter().map(|b| b ^ 0x5a))
            .chain([0])
            .collect();
        let wanted = ButtonPlacement {
            frame: rect(110.0, 80.0, 90.0, 24.0),
            label: "Print".into(),
            action: ButtonAction::PerformScript {
                script: 3,
                parameter: Some(om_scripts::Calculation::new(None, literal)),
            },
        };
        let planned = plan_button(&before, &wanted).unwrap();
        assert_eq!((planned.button, planned.part, planned.stacking), (4, 2, 3));
        assert!(planned.overlaps.is_empty());

        let layout = decoded(&planned.member);
        assert_eq!(layout.objects.root, [1, 2, 3, 4]);
        let button = layout.objects.get(4).unwrap();
        assert_eq!(button.kind(), ObjectKind::Button);
        assert_eq!(button.text.as_deref(), Some("Print"));
        assert_eq!(button.script_parameter.as_deref(), Some("Report"));
        let object = button.button.as_ref().unwrap();
        assert_eq!(
            (object.text_flags, object.display_type, object.icon_size),
            (2, 0, 16)
        );
        let step = object.script_step.as_ref().unwrap();
        assert_eq!((step.id, step.value), (1, 4));
        // kScriptKey and kCalculation; the script key `02 01 03`.
        assert_eq!(step.flags, 0x4040);
        assert_eq!(step.reserved[..3], [2, 1, 3]);
        assert_eq!(already_placed_button(&planned.member, &wanted), Some(4));
        assert_eq!(already_placed_button(&before, &wanted), None);
        // The same button with another parameter, or none, is another
        // placement: not found as this one's retry.
        let other: Vec<u8> = [0x13, 5]
            .into_iter()
            .chain(b"Other".iter().map(|b| b ^ 0x5a))
            .chain([0])
            .collect();
        for parameter in [None, Some(om_scripts::Calculation::new(None, other))] {
            let other = ButtonPlacement {
                action: ButtonAction::PerformScript {
                    script: 3,
                    parameter,
                },
                ..wanted.clone()
            };
            assert_eq!(already_placed_button(&planned.member, &other), None);
        }

        // Indexed at its top and left after the three stored objects, at
        // the end of the body's list, and the last key issued.
        let entry = planned
            .member
            .at(&[&[OBJECT_INDEX], &[0, 80, 0, 110, 0, 3]]);
        assert_eq!(
            entry.and_then(|e| e.value.clone()),
            Some(vec![0, 2, 1, 4, 0, 0, 0, 0, 1])
        );
        let list = planned
            .member
            .at(&[&[PARTS], &[2], &[PART_OBJECTS]])
            .and_then(|n| n.value.clone())
            .unwrap();
        assert_eq!(list, [0, 0, 0, 4, 2, 1, 1, 2, 1, 2, 2, 1, 3, 2, 1, 4]);
        // Everything stored stays.
        let (stored, _) = stored_objects(&before).unwrap();
        let (store, _) = stored_objects(&planned.member).unwrap();
        assert_eq!(store[..stored.len()], stored[..]);
        assert_eq!(planned.member.child(&[0x77]), before.child(&[0x77]));

        // Without a parameter the step has no calculation and no data.
        let plain = ButtonPlacement {
            action: ButtonAction::PerformScript {
                script: 3,
                parameter: None,
            },
            ..wanted.clone()
        };
        let planned = plan_button(&before, &plain).unwrap();
        let layout = decoded(&planned.member);
        let step = layout.objects.get(4).unwrap().button.as_ref().unwrap();
        assert_eq!(step.script_step.as_ref().unwrap().flags, 0x40);
        assert!(planned
            .member
            .at(&[&[ATTRIBUTES], &[4], &[STEP_DATA]])
            .is_none());
        // Over the portal, and outside the body, it is refused.
        for frame in [rect(30.0, 160.0, 90.0, 24.0), rect(110.0, 10.0, 90.0, 24.0)] {
            let refused = plan_button(
                &before,
                &ButtonPlacement {
                    frame,
                    ..plain.clone()
                },
            );
            assert!(refused.is_err(), "{refused:?}");
        }
    }

    /// A Single Step button (Set Field [field 1; "Done"]): its descriptor
    /// on the object, its options (the field spec and the calculation in
    /// slot 0) in the attribute record's `11`, found again by a retry and
    /// not by one with other options; steps FileMaker keeps off buttons, or
    /// with data OpenMaker does not explain, are refused.
    #[test]
    fn a_single_step_button_stores_its_step_and_its_options() {
        let before = layout();
        let literal: Vec<u8> = [0x13, 4]
            .into_iter()
            .chain(b"Done".iter().map(|b| b ^ 0x5a))
            .chain([0])
            .collect();
        let set_field = |polish: Vec<u8>| {
            let mut step = om_scripts::ScriptStep::new(76);
            step.set_calculation(0, Some(&om_scripts::Calculation::new(None, polish)));
            step.set_field_spec(
                0,
                Some(om_scripts::FieldSpec::new(
                    om_scripts::StoredKey::from_id(OCCURRENCE).unwrap(),
                    om_scripts::StoredKey::from_id(1).unwrap(),
                    0,
                )),
            );
            step
        };
        let step = set_field(literal.clone());
        let wanted = ButtonPlacement {
            frame: rect(110.0, 80.0, 90.0, 24.0),
            label: "Finish".into(),
            action: ButtonAction::Step(step.clone()),
        };
        let planned = plan_button(&before, &wanted).unwrap();
        let layout = decoded(&planned.member);
        let button = layout.objects.get(4).unwrap();
        let stored = button
            .button
            .as_ref()
            .unwrap()
            .script_step
            .as_ref()
            .unwrap();
        assert_eq!(
            (stored.id, stored.value, stored.flags as u32),
            (76, 0, step.flags)
        );
        // The run time reads the step back with its options.
        let read = om_scripts::ScriptStep::from_layout(stored).unwrap();
        assert_eq!(read.calculation(0), step.calculation(0));
        assert_eq!(
            planned
                .member
                .at(&[&[ATTRIBUTES], &[4], &[STEP_DATA]])
                .cloned(),
            step.data.as_ref().map(|data| data.encode(&[STEP_DATA]))
        );
        assert_eq!(already_placed_button(&planned.member, &wanted), Some(4));
        let other = ButtonPlacement {
            action: ButtonAction::Step(set_field(literal[..5].to_vec())),
            ..wanted.clone()
        };
        assert_eq!(already_placed_button(&planned.member, &other), None);

        // A step without options (New Record/Request) still gets an empty
        // `11`, as every single-step button in the starters has.
        let new_record = plan_button(
            &before,
            &ButtonPlacement {
                action: ButtonAction::Step(om_scripts::ScriptStep::new(7)),
                ..wanted.clone()
            },
        )
        .unwrap();
        let data = new_record
            .member
            .at(&[&[ATTRIBUTES], &[4], &[STEP_DATA]])
            .unwrap();
        assert!(data.children.is_empty() && data.value.is_none());

        // If (68), a comment (89), Perform Script (1) as a single step and a
        // disabled step are refused; so is a step with data its schema does
        // not explain.
        let mut disabled = step.clone();
        disabled.set_enabled(false);
        let mut unexplained = step.clone();
        unexplained.set_list_value(9, Some(3));
        // A calculation node that is a leaf (`ff`) rather than streams.
        let mut unreadable = step.clone();
        unreadable.set_slot_child(0, 0x05, Some(OwnedNode::leaf(vec![0x05], vec![0xff])));
        assert!(unreadable.unclassified().is_empty());
        for step in [
            om_scripts::ScriptStep::new(68),
            om_scripts::ScriptStep::new(89),
            om_scripts::ScriptStep::new(1),
            disabled,
            unexplained,
            unreadable,
        ] {
            let refused = plan_button(
                &before,
                &ButtonPlacement {
                    action: ButtonAction::Step(step),
                    ..wanted.clone()
                },
            );
            assert!(refused.is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_field_and_its_label_go_on_top_and_keep_everything_stored() {
        let mut before = layout();
        let existing_entry = vec![0, 2, 1, 2, 0, 0, 0, 0, 1];
        before.put(
            &[&[ATTRIBUTES], &[2], &[TAB_POSITION]],
            OwnedNode::leaf(Vec::new(), vec![0, 3]),
        );
        before.put(
            &[&[TAB_ORDER], &[0, 3]],
            OwnedNode::leaf(Vec::new(), existing_entry.clone()),
        );
        let wanted = placement(rect(110.0, 80.0, 200.0, 20.0));
        let planned = plan(&before, &wanted).unwrap();
        assert_eq!(planned.field_object, 4);
        assert_eq!(planned.label_object, Some(5));
        assert_eq!((planned.part, planned.stacking), (2, 3));
        assert!(planned.overlaps.is_empty());

        let layout = decoded(&planned.member);
        assert_eq!(layout.objects.root, [1, 2, 3, 4, 5]);
        let field = layout.objects.get(4).unwrap();
        assert_eq!(field.kind(), ObjectKind::Field);
        let spec = field.field_spec().unwrap();
        // 0x8000 is the serial key `80 00`, value 128.
        assert_eq!((spec.table_key, spec.field_key), (1_065_089, 128));
        assert_eq!(field.bounds.as_ref().unwrap().right, 310.0);
        assert!(is_placed_field(field, &wanted));
        assert_eq!(field.tab_order, 4);
        assert_eq!(layout.objects.get(2).unwrap().tab_order, 3);
        let label = layout.objects.get(5).unwrap();
        assert_eq!(label.text.as_deref(), Some("Follow up"));
        assert_eq!(label.tab_order, 0);
        assert!(is_placed_label(label, wanted.label.as_ref().unwrap()));
        assert_eq!(layout.objects.other.len(), 1);

        // The stored store's bytes, every other key and record, and the
        // index's entries are kept; the field is indexed.
        let (stored, _) = stored_objects(&before).unwrap();
        assert_eq!(planned.store[..stored.len()], stored[..]);
        for key in [&[DEFINITION][..], &[0x77]] {
            assert_eq!(planned.member.child(key), before.child(key));
        }
        let record = |member: &OwnedNode, key: u8| member.at(&[&[ATTRIBUTES], &[key]]).cloned();
        assert_eq!(record(&planned.member, 1), record(&before, 1));
        let index = planned.member.child(&[FIELD_INDEX]).unwrap();
        assert!(index.at(&[&uint_segment(OCCURRENCE), &[1], &[2]]).is_some());
        assert!(index
            .at(&[&uint_segment(OCCURRENCE), &[0x80, 0x00], &[4]])
            .is_some());
        // The edit box, not the label, is in the object index: at its top
        // and left, after the three stored objects in pre-order. The last
        // key issued is the label's.
        let entry = planned
            .member
            .at(&[&[OBJECT_INDEX], &[0, 80, 0, 110, 0, 3]]);
        assert_eq!(
            entry.and_then(|e| e.value.clone()),
            Some(vec![0, 2, 1, 4, 0, 0, 0, 0, 1])
        );
        let tab = planned.member.child(&[TAB_ORDER]).unwrap();
        assert_eq!(
            tab.child(&[0, 3]).unwrap().value.as_ref(),
            Some(&existing_entry)
        );
        assert_eq!(tab.child(&[0, 4]).unwrap().value, entry.unwrap().value);
        // What `place_field` checks on the read-back: a member that lost a
        // stored entry or the edit box's position, or whose object index
        // entry shows another repetition, is not the plan's.
        let as_planned =
            |member: &OwnedNode| object_index_and_tab_order_as_planned(&before, member, &wanted, 4);
        assert!(as_planned(&planned.member));
        let mut dropped = planned.member.clone();
        dropped.remove(&[&[TAB_ORDER], &[0, 3]]);
        assert!(!as_planned(&dropped));
        let mut unpositioned = planned.member.clone();
        unpositioned.remove(&[&[ATTRIBUTES], &[4], &[TAB_POSITION]]);
        assert!(!as_planned(&unpositioned));
        let mut repetition = planned.member.clone();
        repetition.put(
            &[&[OBJECT_INDEX], &[0, 80, 0, 110, 0, 3]],
            OwnedNode::leaf(Vec::new(), vec![0, 2, 1, 4, 0, 0, 0, 0, 2]),
        );
        assert!(!as_planned(&repetition));
        assert!(indexes(&planned.member, 4) && !indexes(&planned.member, 5));
        let last = planned.member.at(&[&[ATTRIBUTES], &[LAST_KEY]]);
        assert_eq!(last.and_then(|n| n.value.clone()), Some(vec![1, 5]));
        // Both new objects end the body's list, after its stored entries;
        // the header's list and the body's other keys are as they were.
        let list = |member: &OwnedNode, part: u8| {
            member
                .at(&[&[PARTS], &[part], &[PART_OBJECTS]])
                .and_then(|n| n.value.clone())
        };
        assert_eq!(
            list(&planned.member, 2),
            Some(vec![
                0, 0, 0, 5, 2, 1, 1, 2, 1, 2, 2, 1, 3, 2, 1, 4, 2, 1, 5
            ])
        );
        assert_eq!(list(&planned.member, 1), list(&before, 1));
        assert_eq!(
            planned.member.at(&[&[PARTS], &[2], &[0x77]]),
            before.at(&[&[PARTS], &[2], &[0x77]])
        );
        assert!(part_lists_extended(&before, &planned.member, &[4, 5]));
        assert!(!part_lists_extended(&before, &planned.member, &[4]));
        assert!(part_lists_extended(
            &planned.member,
            &planned.member,
            &[4, 5]
        ));
        assert_eq!(planned.changes.len(), 8);
        // A layout that issued a larger key: the new keys come after it.
        let mut issued = before.clone();
        issued.put(
            &[&[ATTRIBUTES], &[LAST_KEY]],
            OwnedNode::leaf(Vec::new(), vec![1, 9]),
        );
        assert_eq!(plan(&issued, &wanted).unwrap().field_object, 10);
        // A tab order entry left naming object 4, which is gone: the new
        // edit box does not take its key, so the order names it once.
        let mut stale = before.clone();
        stale.put(
            &[&[TAB_ORDER], &[0, 9]],
            OwnedNode::leaf(Vec::new(), vec![0, 2, 1, 4, 0, 0, 0, 0, 1]),
        );
        assert_eq!(plan(&stale, &wanted).unwrap().field_object, 5);
        // The same in an object index entry nested in a container's entry,
        // as a portal's holds its fields'.
        let mut nested = before.clone();
        nested.put(
            &[&[OBJECT_INDEX], &[0, 1, 0, 1, 0, 0], &[0, 2, 0, 2, 0, 1]],
            OwnedNode::leaf(Vec::new(), vec![0, 2, 1, 4, 0, 0, 0, 0, 1]),
        );
        assert_eq!(plan(&nested, &wanted).unwrap().field_object, 5);

        // Placed: the same request finds its objects; another does not.
        assert_eq!(already_placed(&planned.member, &wanted), Some((4, Some(5))));
        assert_eq!(already_placed(&before, &wanted), None);
        let mut moved = wanted.clone();
        moved.frame.left += 1.0;
        assert_eq!(already_placed(&planned.member, &moved), None);

        // A text on its own (the Text tool): the label's object and record,
        // last on the body's list, with no field reference or index entry.
        let text = wanted.label.clone().unwrap();
        let alone = plan_text(&before, &text).unwrap();
        assert_eq!(alone.text_object, 4);
        let layout = decoded(&alone.member);
        assert_eq!(layout.objects.root, [1, 2, 3, 4]);
        assert!(is_placed_label(layout.objects.get(4).unwrap(), &text));
        for key in [FIELD_INDEX, OBJECT_INDEX, DEFINITION] {
            assert_eq!(alone.member.child(&[key]), before.child(&[key]));
        }
        assert!(part_lists_extended(&before, &alone.member, &[4]));
        let last = alone.member.at(&[&[ATTRIBUTES], &[LAST_KEY]]);
        assert_eq!(last.and_then(|n| n.value.clone()), Some(vec![1, 4]));
        assert_eq!(already_placed_text(&alone.member, &text), Some(4));
        assert_eq!(already_placed_text(&before, &text), None);
    }

    /// A portal of a related occurrence with two fields (#227, operation
    /// 21): on top of the body's objects with its fields as its contents,
    /// as FileMaker Pro's twin stores one (layout-model.md 10.17), found
    /// again by a retry; an unusable request is refused.
    #[test]
    fn a_portal_goes_on_top_with_its_fields_in_its_first_row() {
        let before = layout();
        let related = 0xd0_0002;
        let wanted = PortalPlacement {
            occurrence: related,
            frame: rect(330.0, 50.0, 140.0, 150.0),
            rows: 3,
            fields: vec![FIELD, 0x8001],
        };
        let planned = plan_portal(&before, &wanted, None).unwrap();
        assert_eq!((planned.portal, &planned.fields[..]), (4, &[5, 6][..]));
        assert_eq!((planned.part, planned.stacking), (2, 3));

        let layout = decoded(&planned.member);
        assert_eq!(layout.objects.root, [1, 2, 3, 4]);
        let portal = layout.objects.get(4).unwrap();
        assert_eq!(portal.kind(), ObjectKind::Portal);
        assert_eq!(portal.children, [5, 6]);
        let setup = portal.portal.as_ref().unwrap();
        assert_eq!(
            (setup.table_alias_key, setup.flags, setup.initial_row),
            (1_065_090, 0x10, 1)
        );
        assert_eq!(setup.num_rows, 3.0);
        // Side by side in the first row, 4 points in: two fields of 64.
        let second = layout.objects.get(6).unwrap();
        let spec = second.field_spec().unwrap();
        assert_eq!((spec.table_key, spec.field_key), (1_065_090, 129));
        let bounds = second.bounds.as_ref().unwrap();
        assert_eq!(
            (bounds.top, bounds.left, bounds.bottom, bounds.right),
            (4.0, 72.0, 35.0, 136.0)
        );
        let (stored, _) = stored_objects(&before).unwrap();
        let store = planned
            .member
            .child(&[OBJECTS])
            .unwrap()
            .assembled()
            .unwrap();
        assert_eq!(store[..stored.len()], stored[..]);

        // The portal's reference under no field, and each field's.
        let index = planned.member.child(&[FIELD_INDEX]).unwrap();
        let references = index.child(&uint_segment(related)).unwrap();
        assert_eq!(references.at(&[&[0], &[4]]).unwrap().value, Some(vec![9]));
        assert!(references.at(&[&[0x80, 0x00], &[5]]).is_some());
        assert!(references.at(&[&[0x80, 0x01], &[6]]).is_some());
        // The portal's index entry has no value; its fields' are nested
        // under it, after the portal in pre-order, and in the tab order.
        let entry = planned
            .member
            .at(&[&[OBJECT_INDEX], &[0, 50, 1, 74, 0, 3]])
            .unwrap();
        assert_eq!(entry.value, None);
        let nested = |key: &[u8]| entry.child(key).and_then(|e| e.value.clone());
        assert_eq!(
            nested(&[0, 54, 1, 78, 0, 4]),
            Some(vec![0, 2, 1, 5, 0, 0, 0, 0, 1])
        );
        assert_eq!(
            nested(&[0, 54, 1, 146, 0, 5]),
            Some(vec![0, 2, 1, 6, 0, 0, 0, 0, 1])
        );
        let tab = |order: u8| planned.member.at(&[&[TAB_ORDER], &[0, order]]).cloned();
        assert_eq!(
            tab(2).and_then(|e| e.value),
            Some(vec![0, 2, 1, 6, 0, 0, 0, 0, 1])
        );
        let last = planned.member.at(&[&[ATTRIBUTES], &[LAST_KEY]]);
        assert_eq!(last.and_then(|n| n.value.clone()), Some(vec![1, 6]));
        assert!(part_lists_extended(&before, &planned.member, &[4]));
        for key in [&[DEFINITION][..], &[0x77]] {
            assert_eq!(planned.member.child(key), before.child(key));
        }

        assert_eq!(
            already_placed_portal(&planned.member, &wanted),
            Some((4, vec![5, 6]))
        );
        assert_eq!(already_placed_portal(&before, &wanted), None);
        let mut other_rows = wanted.clone();
        other_rows.rows = 4;
        assert_eq!(already_placed_portal(&planned.member, &other_rows), None);
        // A portal whose options changed since (here its flags, 0x10 to
        // 0x11) is not the one placed.
        let mut store = store.into_owned();
        let at = store
            .windows(4)
            .position(|w| w == [0x18, 0x10, 0x28, 0x01])
            .unwrap();
        store[at + 1] = 0x11;
        let mut changed = planned.member.clone();
        changed.set_child(stored_value(&[OBJECTS], &store));
        assert_eq!(already_placed_portal(&changed, &wanted), None);

        // With a stored table view: a column per field and the empty
        // column renewed, each with Pro's data format record, which the
        // portal's fields get too (#886, layout-model.md 10.17).
        let mut viewed = before.clone();
        viewed.set_child(crate::layout_catalog::table_view(OCCURRENCE).unwrap());
        let placed = plan_portal(&viewed, &wanted, None).unwrap();
        let view = placed.member.child(&[TABLE_VIEW]).unwrap();
        let (_, columns) = stored_objects(view).unwrap();
        assert_eq!(columns.root, [3, 4, 5]);
        let record = |member: &OwnedNode, key: u8| {
            member
                .at(&[&[ATTRIBUTES], &[key], &[FORMAT]])
                .and_then(|n| n.value.clone())
        };
        let pro = Some(crate::layout_catalog::new_column_format());
        for key in [3, 4, 5] {
            assert_eq!(record(view, key), pro, "column {key}");
        }
        for key in [5, 6] {
            assert_eq!(record(&placed.member, key), pro, "field {key}");
        }

        let refused = |change: &dyn Fn(&mut PortalPlacement)| {
            let mut asked = wanted.clone();
            change(&mut asked);
            plan_portal(&before, &asked, None).is_err()
        };
        assert!(refused(&|p| p.occurrence = OCCURRENCE));
        assert!(refused(&|p| p.fields.clear()));
        assert!(refused(&|p| p.rows = 0));
        assert!(refused(&|p| p.rows = 20));
        assert!(refused(&|p| p.frame = rect(330.0, 50.0, 20.0, 150.0)));
        assert!(refused(&|p| p.frame = rect(200.0, 120.0, 140.0, 100.0)));
    }

    /// Operation 22 (#227) on the fixture's header (0 to 40, empty) and body
    /// (40 to 240, objects 1 to 3): parts are keyed again from the top, each
    /// part's subtree moving with it, and the objects of a part that moves
    /// move with it, frames and object index keys, as Pro's twins in
    /// `227-parts/` and `227-parts-objects/` show (layout-model.md 10.18).
    #[test]
    fn parts_are_keyed_again_and_their_objects_move_with_them() {
        let mut before = layout();
        let (field_key, field_value) = object_index_entry(50.0, 110.0, 1, 2).unwrap();
        let portal_key = object_index_entry(150.0, 20.0, 2, 3).unwrap().0;
        before.put(
            &[&[OBJECT_INDEX], &field_key],
            OwnedNode::leaf(Vec::new(), field_value.clone()),
        );
        before.put(
            &[&[OBJECT_INDEX], &portal_key],
            OwnedNode::list(Vec::new(), Vec::new()),
        );
        let part = |member: &OwnedNode, key: u8| {
            let def = member
                .at(&[&[PARTS], &[key], &[DEFINITION]])?
                .value
                .clone()?;
            let part = Part::decode(u64::from(key), &def).ok()?;
            Some((part.part_type, part.top, part.height))
        };
        let top = |member: &OwnedNode, key: u64| {
            decoded(member)
                .objects
                .get(key)
                .and_then(|o| o.bounds.clone())
                .map(|b| b.top)
        };

        // A title header above the header: every part one key lower and
        // 30 points down, the body's objects and list with it.
        let added = PartChange::Add {
            part_type: 0,
            size: 30.0,
        };
        let planned = plan_parts(&before, &added).unwrap().member;
        assert_eq!(part(&planned, 1), Some((0, 0.0, 30.0)));
        assert_eq!(part(&planned, 2), Some((1, 30.0, 40.0)));
        assert_eq!(part(&planned, 3), Some((4, 70.0, 200.0)));
        assert_eq!(
            planned.at(&[&[PARTS], &[3], &[PART_OBJECTS]]),
            before
                .at(&[&[PARTS], &[2], &[PART_OBJECTS]])
                .map(|n| OwnedNode {
                    segment: n.segment.clone(),
                    ..n.clone()
                })
                .as_ref()
        );
        assert!(planned.at(&[&[PARTS], &[3], &[0x77]]).is_some());
        assert_eq!(
            planned.at(&[&[PARTS], &[0]]).and_then(|n| n.value.clone()),
            Some(vec![1, 3])
        );
        assert_eq!(
            (top(&planned, 2), top(&planned, 3)),
            (Some(80.0), Some(180.0))
        );
        let moved = |key: [u8; 6]| planned.at(&[&[OBJECT_INDEX], &key]).cloned();
        assert_eq!(
            moved([0, 80, 0, 110, 0, 1]).and_then(|n| n.value),
            Some(field_value)
        );
        assert!(moved([0, 180, 0, 20, 0, 2]).is_some_and(|n| n.value.is_none()));
        assert!(planned.at(&[&[OBJECT_INDEX], &field_key]).is_none());
        assert!(already_parts(&planned, &added));
        assert!(!already_parts(&before, &added));

        // Deleting the empty header: the body is part 1 again, at the top.
        let deleted = plan_parts(
            &before,
            &PartChange::Delete {
                part: 1,
                part_type: 1,
            },
        )
        .unwrap()
        .member;
        assert_eq!(part(&deleted, 1), Some((4, 0.0, 200.0)));
        assert_eq!(part(&deleted, 2), None);
        assert_eq!(top(&deleted, 2), Some(10.0));

        // Resizing: only the part's size where nothing is below it.
        let resized = PartChange::Resize {
            part: 2,
            part_type: 4,
            size: 180.0,
        };
        let taller = plan_parts(&before, &resized).unwrap().member;
        assert_eq!(part(&taller, 2), Some((4, 40.0, 180.0)));
        assert_eq!(taller.child(&[OBJECTS]), before.child(&[OBJECTS]));

        let refused = |change: PartChange| plan_parts(&before, &change).is_err();
        // A sub-summary, a second header, the body with its objects, a
        // body that would end above the portal (150 to 210).
        assert!(refused(PartChange::Add {
            part_type: 3,
            size: 20.0
        }));
        assert!(refused(PartChange::Add {
            part_type: 1,
            size: 20.0
        }));
        assert!(refused(PartChange::Delete {
            part: 2,
            part_type: 4
        }));
        assert!(refused(PartChange::Resize {
            part: 2,
            part_type: 4,
            size: 150.0
        }));
        assert!(refused(PartChange::Resize {
            part: 2,
            part_type: 4,
            size: 0.5
        }));
        // Layouts it does not re-key: another child among the parts, an
        // index entry naming an object other than the one at its place, an
        // object listed by a part it is not in.
        let footer = PartChange::Add {
            part_type: 7,
            size: 20.0,
        };
        let mut odd = before.clone();
        odd.put(&[&[PARTS], &[3]], OwnedNode::leaf(Vec::new(), vec![1]));
        assert!(plan_parts(&odd, &footer).is_err());
        let mut odd = before.clone();
        let (_, wrong) = object_index_entry(50.0, 110.0, 1, 3).unwrap();
        odd.put(
            &[&[OBJECT_INDEX], &field_key],
            OwnedNode::leaf(Vec::new(), wrong),
        );
        assert!(plan_parts(&odd, &footer).is_err());
        let mut odd = before.clone();
        odd.put(
            &[&[PARTS], &[1], &[PART_OBJECTS]],
            OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 1, 2, 1, 1]),
        );
        odd.put(
            &[&[PARTS], &[2], &[PART_OBJECTS]],
            OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 2, 2, 1, 2, 2, 1, 3]),
        );
        assert!(plan_parts(
            &odd,
            &PartChange::Delete {
                part: 1,
                part_type: 1
            }
        )
        .is_err());
        // Sub-summaries are neither added nor deleted, and a refused kind
        // is never taken as already applied.
        let mut summary = before.clone();
        let mut def = Vec::new();
        om_layout::encode::uint(1, 3, &mut def);
        om_layout::encode::double(4, 240.0, &mut def);
        om_layout::encode::double(5, 20.0, &mut def);
        summary.put(
            &[&[PARTS], &[3], &[DEFINITION]],
            OwnedNode::leaf(Vec::new(), def),
        );
        summary.put(
            &[&[PARTS], &[3], &[PART_OBJECTS]],
            OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 0]),
        );
        assert!(plan_parts(
            &summary,
            &PartChange::Delete {
                part: 3,
                part_type: 3
            }
        )
        .is_err());
        assert!(!already_parts(
            &before,
            &PartChange::Add {
                part_type: 3,
                size: 20.0
            }
        ));
    }

    /// A field placed on a layout that stores a table view, as a new file's
    /// does (#825), becomes its column as Pro's Field tool writes one: the
    /// empty column renewed after it, narrow (#815).
    #[test]
    fn a_placed_field_becomes_a_column_of_the_stored_table_view() {
        let mut before = layout();
        before.set_child(crate::layout_catalog::table_view(OCCURRENCE).unwrap());
        let planned = plan(&before, &placement(rect(110.0, 80.0, 200.0, 20.0))).unwrap();
        let view = planned.member.child(&[TABLE_VIEW]).unwrap();
        let (_, columns) = stored_objects(view).unwrap();
        assert_eq!(columns.root, [3, 4]);
        let object = |key: u64| columns.objects.iter().find(|o| o.id == key).unwrap();
        let spec = object(3).field_spec().unwrap();
        assert_eq!((spec.table_key, spec.field_key), (1_065_089, 128));
        assert_eq!(object(3).flags & encode::EMPTY_COLUMN_FLAG, 0);
        assert_ne!(object(4).flags & encode::EMPTY_COLUMN_FLAG, 0);
        let span = |key: u64| {
            let b = object(key).bounds.clone().unwrap();
            (b.left, b.right)
        };
        assert_eq!((span(3), span(4)), ((0.0, 120.0), (120.0, 136.0)));
        let body = view.at(&[&[PARTS], &[2], &[PART_OBJECTS]]).unwrap();
        assert_eq!(part_list(body.value.as_deref().unwrap()), Some(vec![3, 4]));
        assert_eq!(view.child(&[OBJECT_INDEX]).unwrap().children.len(), 2);
        let tab: Vec<Vec<u8>> = view
            .child(&[TAB_ORDER])
            .unwrap()
            .children
            .iter()
            .map(|t| t.segment.clone())
            .collect();
        assert_eq!(tab, [vec![0, 1], vec![0, 2]]);
        let reference = |path: &[&[u8]]| view.at(&[&[&[FIELD_INDEX][..]], path].concat()).is_some();
        assert!(reference(&[
            &uint_segment(OCCURRENCE),
            &uint_segment(FIELD),
            &[3]
        ]));
        assert!(reference(&[&[0], &[0], &[4]]) && !reference(&[&[0], &[0], &[2]]));
        assert_eq!(last_issued(view).unwrap(), Some(4));
        // Each new column, and the field object, get the stored empty
        // column's data format record, as Pro writes them (#886); the
        // field's column repeats -1, as Pro's.
        let format = |member: &OwnedNode, key: u8| {
            member
                .at(&[&[ATTRIBUTES], &[key], &[FORMAT]])
                .and_then(|n| n.value.clone())
        };
        let pro = Some(crate::layout_catalog::new_column_format());
        assert_eq!(
            (format(view, 3), format(view, 4)),
            (pro.clone(), pro.clone())
        );
        assert_eq!(format(&planned.member, 4), pro);
        let repetitions = object(3).field_obj.as_ref().unwrap().num_of_reps;
        assert_eq!(repetitions, -1);
        // A view OpenMaker made before it wrote the record: Pro's record.
        let mut old = layout();
        let mut bare = crate::layout_catalog::table_view(OCCURRENCE).unwrap();
        bare.remove(&[&[ATTRIBUTES], &[2]]);
        old.set_child(bare);
        let again = plan(&old, &placement(rect(110.0, 80.0, 200.0, 20.0))).unwrap();
        let again_view = again.member.child(&[TABLE_VIEW]).unwrap();
        assert_eq!(format(again_view, 3), pro);
        // A stored record the field-by-field writing does not give back is
        // refused, not copied.
        let mut odd = layout();
        let mut unread = crate::layout_catalog::table_view(OCCURRENCE).unwrap();
        unread.put(
            &[&[ATTRIBUTES], &[2], &[FORMAT]],
            OwnedNode::leaf(vec![FORMAT], vec![0, 14, 9]),
        );
        odd.set_child(unread);
        assert!(matches!(
            plan(&odd, &placement(rect(110.0, 80.0, 200.0, 20.0))),
            Err(Refusal::Unsupported(_))
        ));
        // The layout's own objects are otherwise as without a table view.
        let plain = plan(&layout(), &placement(rect(110.0, 80.0, 200.0, 20.0))).unwrap();
        let mut without = planned.member.clone();
        without.remove(&[&[TABLE_VIEW]]);
        without.remove(&[&[ATTRIBUTES], &[4], &[FORMAT]]);
        assert!(without == plain.member);

        // A second field: the first column's entries are kept as stored,
        // the new column follows it, the empty column is renewed again.
        let mut second = placement(rect(110.0, 120.0, 200.0, 20.0));
        second.field = FIELD + 1;
        second.label = None;
        let again = plan(&planned.member, &second).unwrap();
        let view_again = again.member.child(&[TABLE_VIEW]).unwrap();
        let (_, columns) = stored_objects(view_again).unwrap();
        assert_eq!(columns.root, [3, 5, 6]);
        let index = view_again.child(&[OBJECT_INDEX]).unwrap();
        let first = view
            .child(&[OBJECT_INDEX])
            .unwrap()
            .child(&[0, 24, 0, 0, 0, 0]);
        assert_eq!(index.child(&[0, 24, 0, 0, 0, 0]), first);
        assert!(index.child(&[0, 24, 0, 120, 0, 1]).is_some());
        assert!(index.child(&[0, 24, 0, 240, 0, 2]).is_some());
        let orders = view_again.child(&[TAB_ORDER]).unwrap().children.len();
        assert_eq!(orders, 3);
        assert_eq!(last_issued(view_again).unwrap(), Some(6));

        // An empty column before a field column: refused, nothing placed.
        let mut odd = before.clone();
        let view_node = odd.child_mut(&[TABLE_VIEW]).unwrap();
        let (stored, _) = stored_objects(view_node).unwrap();
        let field_first = encode::append_to_store(
            &stored,
            &[9],
            &[encode::table_column(
                9,
                &rect(0.0, 24.0, 120.0, 16.0),
                1_065_089,
                1,
            )],
        );
        view_node.set_child(stored_value(&[OBJECTS], &field_first));
        let refused = plan(&odd, &placement(rect(110.0, 80.0, 200.0, 20.0)));
        assert!(
            matches!(refused, Err(Refusal::Unsupported(_))),
            "{refused:?}"
        );
    }

    #[test]
    fn exhausted_object_keys_are_refused() {
        let mut before = layout();
        before.put(
            &[&[ATTRIBUTES], &[LAST_KEY]],
            OwnedNode::leaf(Vec::new(), key_string(u64::MAX)),
        );
        assert!(matches!(
            plan(&before, &placement(rect(110.0, 80.0, 200.0, 20.0))),
            Err(Refusal::Unsupported(_))
        ));
    }

    #[test]
    fn advancing_the_object_counter_keeps_unknown_descendants() {
        let mut before = layout();
        let unknown = OwnedNode::list(
            vec![0x77],
            vec![OwnedNode::leaf(vec![0x2a], vec![0xde, 0xad])],
        );
        let mut counter = OwnedNode::leaf(Vec::new(), key_string(9));
        counter.set_child(unknown.clone());
        before.put(&[&[ATTRIBUTES], &[LAST_KEY]], counter);
        let planned = plan(&before, &placement(rect(110.0, 80.0, 200.0, 20.0))).unwrap();
        let counter = planned.member.at(&[&[ATTRIBUTES], &[LAST_KEY]]).unwrap();
        assert_eq!(counter.children, vec![unknown]);
        assert_eq!(last_issued(&planned.member).unwrap(), Some(11));
    }

    #[test]
    fn frames_must_be_in_the_body_beside_simple_objects_only() {
        let before = layout();
        let refused = |frame: Rect| plan(&before, &placement(frame)).unwrap_err();
        // In the header, past the width, empty, over the portal.
        assert!(matches!(
            refused(rect(110.0, 5.0, 100.0, 20.0)),
            Refusal::Invalid(_)
        ));
        assert!(matches!(
            refused(rect(400.0, 80.0, 100.0, 20.0)),
            Refusal::Invalid(_)
        ));
        assert!(matches!(
            refused(rect(110.0, 80.0, 0.0, 20.0)),
            Refusal::Invalid(_)
        ));
        assert!(matches!(
            refused(rect(f64::NAN, 80.0, 10.0, 20.0)),
            Refusal::Invalid(_)
        ));
        assert!(matches!(
            refused(rect(110.0, 160.0, 100.0, 20.0)),
            Refusal::Unsupported(_)
        ));
        // Between whole points, which the object index does not hold.
        assert!(matches!(
            refused(rect(110.5, 80.0, 100.0, 20.0)),
            Refusal::Unsupported(_)
        ));
        // Over the stored field: allowed, beneath the new one.
        let planned = plan(&before, &placement(rect(150.0, 55.0, 100.0, 20.0))).unwrap();
        assert_eq!(planned.overlaps, [2]);
        // Over its own label.
        assert!(matches!(
            refused(rect(30.0, 85.0, 100.0, 20.0)),
            Refusal::Invalid(_)
        ));
        // Another occurrence's field, an unusable label.
        let mut related = placement(rect(110.0, 80.0, 200.0, 20.0));
        related.occurrence = 0xd0_0002;
        assert!(matches!(
            plan(&before, &related),
            Err(Refusal::Unsupported(_))
        ));
        let mut blank = placement(rect(110.0, 80.0, 200.0, 20.0));
        blank.label.as_mut().unwrap().text = "two\nlines".into();
        assert!(matches!(plan(&before, &blank), Err(Refusal::Invalid(_))));
    }

    #[test]
    fn layouts_this_version_does_not_edit_are_refused() {
        let wanted = placement(rect(110.0, 80.0, 200.0, 20.0));
        let mut folder = layout();
        folder.set_child(OwnedNode::leaf(
            vec![MEMBER_FLAGS],
            1u64.to_be_bytes().to_vec(),
        ));
        assert!(matches!(
            plan(&folder, &wanted),
            Err(Refusal::Unsupported(_))
        ));
        let mut unreadable = layout();
        unreadable.set_child(OwnedNode::leaf(vec![OBJECTS], vec![0x0a, 0x05]));
        assert!(matches!(
            plan(&unreadable, &wanted),
            Err(Refusal::Unsupported(_))
        ));
        let mut bodiless = layout();
        bodiless.remove(&[&[PARTS], &[2]]);
        assert!(matches!(
            plan(&bodiless, &wanted),
            Err(Refusal::Unsupported(_))
        ));
        // A body whose list is missing, or counts entries it does not hold.
        for list in [None, Some(vec![0, 0, 0, 4, 2, 1, 1, 2, 1, 2, 2, 1, 3])] {
            let mut unlisted = layout();
            match list {
                None => {
                    unlisted.remove(&[&[PARTS], &[2], &[PART_OBJECTS]]);
                }
                Some(list) => unlisted.put(
                    &[&[PARTS], &[2], &[PART_OBJECTS]],
                    OwnedNode::leaf(Vec::new(), list),
                ),
            }
            assert!(matches!(
                plan(&unlisted, &wanted),
                Err(Refusal::Unsupported(_))
            ));
        }
        // A root naming an object the store does not hold: the new object's
        // pre-order index is not known.
        let mut dangling = layout();
        let (mut store, _) = stored_objects(&dangling).unwrap();
        uint(1, 9, &mut store);
        dangling.set_child(stored_value(&[OBJECTS], &store));
        assert!(matches!(
            plan(&dangling, &wanted),
            Err(Refusal::Unsupported(_))
        ));
        let mut blank = layout();
        blank.remove(&[&[OBJECTS]]);
        blank.remove(&[&[ATTRIBUTES]]);
        blank.put(
            &[&[PARTS], &[2], &[PART_OBJECTS]],
            OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 0]),
        );
        // Without objects; the index still names object 2, whose key is not
        // issued again.
        let planned = plan(&blank, &wanted).unwrap();
        assert_eq!((planned.field_object, planned.stacking), (3, 0));
        assert_eq!(
            decoded(&planned.member).objects.get(3).unwrap().tab_order,
            1
        );
    }

    #[test]
    fn object_index_entries_are_filemakers() {
        // FileMaker Pro 26's save of the edit box OpenMaker placed for
        // issue #166 (object 51 at top 558, left 33, pre-order index 50),
        // and a field object of FileMaker Server's sample file (layout 1,
        // object 201, a two-byte serial key, at top 81, left 713, index 27).
        let (key, value) = object_index_entry(558.0, 33.0, 50, 51).unwrap();
        assert_eq!(hex(&key), "02 2e 00 21 00 32");
        assert_eq!(hex(&value), "00 02 01 33 00 00 00 00 01");
        let (key, value) = object_index_entry(81.0, 713.0, 27, 201).unwrap();
        assert_eq!(hex(&key), "00 51 02 c9 00 1b");
        assert_eq!(hex(&value), "00 03 02 80 49 00 00 00 01");
        // Keys of three serial bytes do not fit the value.
        assert!(object_index_entry(81.0, 713.0, 27, 0x4080).is_err());
    }

    #[test]
    fn long_stores_are_chunked_as_files_hold_them() {
        let value: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let node = stored_value(&[OBJECTS], &value);
        assert_eq!(node.children.len(), 4);
        assert_eq!(node.children[0].value, Some(2500u32.to_be_bytes().to_vec()));
        assert_eq!(node.assembled().unwrap(), value);
        assert_eq!(stored_value(&[OBJECTS], &[1, 2]).value, Some(vec![1, 2]));
    }
}
