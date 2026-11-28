//! OpenMaker's authoring extension (`docs/authoring.md`): one Perform
//! transaction, opcode 240, with which a client replaces a script's steps,
//! adds a field to a table or places a field on a layout on OpenMaker
//! Server, each as a single catalog edit that the host either refuses whole
//! or saves to disk before it answers.
//!
//! It is not a FileMaker transaction. FileMaker Pro saves a script with
//! op60, op82, op61, op13 and op62 on `[17, 5, S]` and `[17, 1]`
//! (scripts.md, "Saving scripts to a host"), but op13's reply is the same
//! empty page whether the host stored the upload, skipped it or failed to
//! save it (schema-path.md 3.3), so a client cannot tell an applied edit
//! from a lost one. Here the host checks the account, the script's locks
//! and its content revision, writes the same modification stamp
//! (schema-path.md 8.1), tells the other clients as the op62 releases
//! would (attention 2, schema-path.md 9), and answers with what happened.
//!
//! A client sends it only to a host that claims the interface with CORBA's
//! `_is_a(`[`REPO_ID`]`)`, as for OpenMaker's file list
//! (`crate::hosted_files`): a host without it answers `false` (FileMaker
//! Server 26.0.3 did for that interface,
//! `docs/hosted-file-list.md`) and never receives the transaction. A host that claims it
//! but answers the transaction with a lone status (3) is treated as not
//! having it.
//!
//! Requests carry the version, the operation, then the operation's fields
//! (popped in that order). Replies end with the status `u16` (popped
//! first); a reply that holds nothing else is a host without the extension.

use om_hbam::{OwnedNode, SlotReader};
use om_rctransaction::{Builder, Pop, Reader, ResultBuilder, ResultReader};

use crate::download::file_header;
use crate::security_catalog::{pop_catalog, push_catalog};

pub use crate::security_catalog::{
    CatalogAccount, CatalogExtendedPrivilege, CatalogPrivilegeSet, SecurityCatalog, MAX_KEY,
    MAX_RECORD_FLAGS,
};
use crate::write::finished;
use crate::{Opcode, PerformError};

/// The interface a host claims with `_is_a` when it answers opcode 240.
pub const REPO_ID: &[u8] = b"IDL:OpenMaker/Authoring:1.0";

/// The extension's version.
pub const VERSION: u16 = 1;

/// The largest new definition one request carries, in its wire form: the
/// request must fit the host's request limit (2 MiB) whole.
pub const MAX_DEFINITION_BYTES: usize = 1 << 20;

/// Statuses. Where FileMaker has an error for the case its number is used.
pub mod status {
    pub const OK: u16 = 0;
    /// The extension, its version or the operation is not available
    /// (FileMaker error 3, "Command is unavailable").
    pub const UNSUPPORTED: u16 = 3;
    /// The request or the new definition is not valid ("Command is
    /// invalid").
    pub const INVALID: u16 = 5;
    /// The account may not change scripts, or is not logged in
    /// ("Insufficient privileges").
    pub const PERMISSION_DENIED: u16 = 9;
    /// Another user holds the script or the catalog ("File or object is in
    /// use").
    pub const IN_USE: u16 = 13;
    /// The file is not open in this session ("File is missing").
    pub const FILE_MISSING: u16 = 100;
    /// No such script ("Script is missing").
    pub const SCRIPT_MISSING: u16 = 104;
    /// The script is not at the expected revision ("Record modification ID
    /// does not match").
    pub const CONFLICT: u16 = 306;
    /// The host could not save the file; nothing changed (800 "Unable to
    /// create file on disk", 809 "Disk/volume is full", 810 "Disk/volume
    /// is locked").
    pub const SAVE_FAILED: [u16; 3] = [800, 809, 810];
    /// The host wrote the change but could not confirm that the file on
    /// disk keeps it: the change may or may not be in the file, which takes
    /// no more changes until it is hosted again (FileMaker's -1, "Unknown
    /// error"). A client reports it as an outcome it cannot know.
    pub const OUTCOME_UNKNOWN: u16 = 0xffff;
    /// The table already has a field of that name ("Name already exists").
    pub const NAME_EXISTS: u16 = 12;
    /// No such table ("Table is missing").
    pub const TABLE_MISSING: u16 = 106;
    /// Another user is changing the schema or a record of the table
    /// ("Database schema is in use by another user").
    pub const SCHEMA_IN_USE: u16 = 303;
    /// The name cannot be used for a field ("The specified name can't be
    /// used").
    pub const NAME_UNUSABLE: u16 = 1300;
    /// No such field ("Field is missing").
    pub const FIELD_MISSING: u16 = 102;
    /// No such layout ("Layout is missing").
    pub const LAYOUT_MISSING: u16 = 105;
    /// Another user holds the layout ("Layout is in use by another user").
    pub const LAYOUT_IN_USE: u16 = 304;
    /// No such value list ("Value list is missing").
    pub const VALUE_LIST_MISSING: u16 = 108;
    /// The host could not read the part of the file the request reads
    /// ("Unable to open file"), as a login to a file whose accounts cannot
    /// be read is answered.
    pub const UNREADABLE: u16 = 802;
}

/// Operation numbers.
const DESCRIBE: u8 = 1;
const REPLACE_SCRIPT_STEPS: u8 = 2;
const CREATE_FIELD: u8 = 3;
const PLACE_FIELD: u8 = 4;
const DESCRIBE_SECURITY: u8 = 5;
const CREATE_LAYOUT: u8 = 6;
const PLACE_BUTTON: u8 = 7;
const PLACE_BUTTON_STEP: u8 = 8;
/// Operation numbers are given out by lane, so no merge order changes one:
/// 7 and 8 buttons, 9 and 10 the script catalog, 11 to 16 Layout mode's
/// object edits.
const SAVE: u8 = 9;
const SET_OBJECT_FRAMES: u8 = 11;
const DELETE_OBJECTS: u8 = 12;
const DUPLICATE_LAYOUT: u8 = 13;
const DELETE_LAYOUT: u8 = 14;
const PLACE_TEXT: u8 = 15;
const LAYOUT_SETUP: u8 = 16;
const LAYOUT_WIDTH: u8 = 17;
const FIELD_CONTROL: u8 = 18;
const PART_OPTIONS: u8 = 19;
const SET_TEXT: u8 = 20;
/// 24 onward: value lists.
const VALUE_LIST_VALUES: u8 = 24;
const PLACE_PORTAL: u8 = 21;
const CHANGE_PARTS: u8 = 22;
/// Layout Setup with fixed page margins: operation 16's request and the
/// margins, in one commit.
const LAYOUT_SETUP_MARGINS: u8 = 23;
/// 28 onward: web viewers.
const PLACE_WEB_VIEWER: u8 = 28;
const SET_OBJECT_ANCHORS: u8 = 30;

/// Bits of [`Described::operations`].
pub const OP_REPLACE_SCRIPT_STEPS: u32 = 1 << REPLACE_SCRIPT_STEPS;
pub const OP_CREATE_FIELD: u32 = 1 << CREATE_FIELD;
pub const OP_PLACE_FIELD: u32 = 1 << PLACE_FIELD;
pub const OP_DESCRIBE_SECURITY: u32 = 1 << DESCRIBE_SECURITY;
pub const OP_CREATE_LAYOUT: u32 = 1 << CREATE_LAYOUT;
pub const OP_PLACE_BUTTON: u32 = 1 << PLACE_BUTTON;
pub const OP_PLACE_BUTTON_STEP: u32 = 1 << PLACE_BUTTON_STEP;

/// The largest step data a Single Step button's request carries, in its
/// wire form.
pub const MAX_STEP_DATA_BYTES: usize = 64 * 1024;

/// The largest stream of a button's script parameter a request carries
/// (each of its formatted and Polish streams).
pub const MAX_PARAMETER_BYTES: usize = 64 * 1024;

/// The most bytes of a web viewer's address a request carries: its
/// formatted and Polish streams together, as the planner bounds them.
pub const MAX_WEB_ADDRESS_BYTES: usize = 64 * 1024;
pub const OP_SAVE: u32 = 1 << SAVE;
pub const OP_SET_OBJECT_FRAMES: u32 = 1 << SET_OBJECT_FRAMES;
pub const OP_DELETE_OBJECTS: u32 = 1 << DELETE_OBJECTS;
pub const OP_DUPLICATE_LAYOUT: u32 = 1 << DUPLICATE_LAYOUT;
pub const OP_DELETE_LAYOUT: u32 = 1 << DELETE_LAYOUT;
pub const OP_PLACE_TEXT: u32 = 1 << PLACE_TEXT;
pub const OP_LAYOUT_SETUP: u32 = 1 << LAYOUT_SETUP;
pub const OP_LAYOUT_WIDTH: u32 = 1 << LAYOUT_WIDTH;
pub const OP_LAYOUT_SETUP_MARGINS: u32 = 1 << LAYOUT_SETUP_MARGINS;
pub const OP_FIELD_CONTROL: u32 = 1 << FIELD_CONTROL;
pub const OP_PART_OPTIONS: u32 = 1 << PART_OPTIONS;
pub const OP_SET_TEXT: u32 = 1 << SET_TEXT;
pub const OP_SET_OBJECT_ANCHORS: u32 = 1 << SET_OBJECT_ANCHORS;
pub const OP_VALUE_LIST_VALUES: u32 = 1 << VALUE_LIST_VALUES;
pub const OP_PLACE_WEB_VIEWER: u32 = 1 << PLACE_WEB_VIEWER;
/// The most items, and characters in all, a value list's values request
/// carries.
pub const MAX_VALUES: usize = 10_000;
pub const MAX_VALUES_CHARS: usize = 10_000;
pub const OP_PLACE_PORTAL: u32 = 1 << PLACE_PORTAL;
pub const OP_CHANGE_PARTS: u32 = 1 << CHANGE_PARTS;
/// The most fields a portal placement carries.
pub const MAX_PORTAL_FIELDS: usize = 20;
/// The most frames, or objects to delete, one request carries.
pub const MAX_OBJECT_FRAMES: usize = 1_000;

/// The longest field name and comment a request carries, in characters
/// (FileMaker's limit for names; OpenMaker's for comments).
pub const MAX_FIELD_NAME_CHARS: usize = 100;
pub const MAX_COMMENT_CHARS: usize = 10_000;
/// The longest label a placement carries, in characters.
pub const MAX_LABEL_CHARS: usize = 1_000;
/// The longest layout name a request carries, in characters (FileMaker's
/// limit for names).
pub const MAX_LAYOUT_NAME_CHARS: usize = 100;

/// Revisions and the keys of a modification stamp (schema-path.md 8.1)
/// are defined once, in `om_hbam::design_paths`, and the host checks and
/// reports them with the same functions as its clients. A revision here is
/// an edit token: the whole member (or catalog), stored passwords
/// included, without modification stamps ([`content_revision`] for a
/// script, [`layout_revision`] for a layout, [`catalog_revision`] for
/// anything else). [`legacy_content_revision`] and
/// [`legacy_catalog_revision`] are the tokens an older client computes,
/// which a host accepts and answers in.
pub use om_hbam::design_paths::{
    catalog_revision, is_stamp, legacy_catalog_revision,
    legacy_script_form_revision as legacy_content_revision, Revision, MOD_ACCOUNT, MOD_COUNT,
    MOD_TIME, MOD_USER,
};

/// A script member's `[17, 5, S]` edit token (`om_hbam::design_paths`).
pub fn content_revision(member: &OwnedNode) -> Revision {
    om_hbam::design_paths::script_form_revision(member)
}

/// A layout member's `[4, 5, L]` edit token (`om_hbam::design_paths`).
pub fn layout_revision(member: &OwnedNode) -> Revision {
    om_hbam::design_paths::catalog_revision(member)
}

/// A request.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// What the host offers this session's account.
    Describe,
    ReplaceScriptSteps(ReplaceScriptSteps),
    CreateField(CreateField),
    PlaceField(PlaceField),
    PlaceButton(PlaceButton),
    PlaceButtonStep(PlaceButtonStep),
    /// The file's accounts, privilege sets and extended privileges, without
    /// any password or other credential ([`SecurityCatalog`]).
    DescribeSecurity,
    CreateLayout(CreateLayout),
    SetObjectFrames(SetObjectFrames),
    DeleteObjects(DeleteObjects),
    PlaceText(PlaceText),
    DuplicateLayout(DuplicateLayout),
    DeleteLayout(DeleteLayout),
    LayoutSetup(LayoutSetup),
    LayoutWidth(LayoutWidth),
    FieldControl(FieldControl),
    PartOptions(PartOptions),
    SetText(SetText),
    SetObjectAnchors(SetObjectAnchors),
    ValueListValues(ValueListValues),
    PlacePortal(PlacePortal),
    ChangeParts(ChangeParts),
    PlaceWebViewer(PlaceWebViewer),
    /// Write the file to disk now if it has changes the disk lacks, and
    /// answer [`Reply::Saved`] only when the disk holds them all. A client
    /// that changed the file with FileMaker's op13, whose reply says
    /// nothing about the disk (schema-path.md 3.3), asks this to know.
    Save,
    /// A version or operation this side does not know.
    Unknown {
        version: u16,
        operation: u8,
    },
}

/// What a request asks for, read before its parameters
/// ([`request_operation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Describe,
    ReplaceScriptSteps,
    CreateField,
    PlaceField,
    PlaceButton,
    PlaceButtonStep,
    DescribeSecurity,
    CreateLayout,
    SetObjectFrames,
    DeleteObjects,
    PlaceText,
    DuplicateLayout,
    DeleteLayout,
    LayoutSetup,
    LayoutWidth,
    LayoutSetupMargins,
    FieldControl,
    PartOptions,
    SetText,
    SetObjectAnchors,
    ValueListValues,
    PlaceWebViewer,
    PlacePortal,
    ChangeParts,
    Save,
    /// A version or operation this side does not know: the request parses
    /// as [`Request::Unknown`].
    Unknown {
        version: u16,
        operation: u8,
    },
}

/// Replace script `script`'s step list `04` and step data `05` with those
/// of `definition`, if the script is at revision `expected`. Every other
/// child of the stored definition must be in `definition` as it is stored;
/// the host writes the stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceScriptSteps {
    pub script: u64,
    pub expected: Revision,
    /// The whole new definition node `[17, 5, S]`.
    pub definition: OwnedNode,
}

/// Add a normal field with stored values of type `data_type` (1 text, 2
/// number, 3 date, 4 time, 5 timestamp; schema-path.md 5.5) named `name`
/// to table `table` (its catalog id, the big-endian reading of its key), if
/// the table's fields `[T, 3]` are at revision `expected`
/// ([`catalog_revision`]). The host allocates the field's id and writes
/// its definition, name keys, display position and stamps; the field has
/// FileMaker's defaults otherwise (no auto-enter or validation, indexed as
/// needed, one repetition). An empty `comment` is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateField {
    pub table: u64,
    pub expected: Revision,
    pub data_type: u8,
    pub name: String,
    pub comment: String,
}

/// A frame in layout coordinates (points): top, left, bottom, right, as
/// `FMLayoutPB.Rect` stores them.
pub type Frame = [f64; 4];

/// Place an edit box showing field `field` (its catalog id) of occurrence
/// `occurrence` (the layout's own) at `frame` on layout `layout`, with a
/// text label `label` (its text and frame) beside it if given, on top of
/// the layout's objects, if the layout member `[4, 5, L]` is at revision
/// `expected` ([`catalog_revision`]). The host allocates the objects' keys
/// and UUIDs and writes the modification stamp (`docs/authoring.md`,
/// "Fields on layouts").
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceField {
    pub layout: u64,
    pub expected: Revision,
    /// The expected base table of `occurrence`, checked under the host's
    /// store lock, including retries of an already applied placement.
    pub table: u64,
    pub occurrence: u64,
    pub field: u64,
    pub frame: Frame,
    pub label: Option<(String, Frame)>,
}

/// Place a portal at `frame` on layout `layout`'s body showing `rows` rows
/// of occurrence `occurrence` (its catalog id; one related to the layout's
/// own), with `fields` (catalog ids of fields of that occurrence's table
/// `table`) side by side in its first row, on top of the layout's objects,
/// if the layout member `[4, 5, L]` is at revision `expected`
/// ([`catalog_revision`]). The host allocates the keys and UUIDs and writes
/// the stamp (`docs/authoring.md`, "Portals on layouts"). The reply is
/// [`Reply::Placed`] with the portal's key as its `field_object`; its
/// fields take the keys after it, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacePortal {
    pub layout: u64,
    pub expected: Revision,
    /// The expected base table of `occurrence`, checked under the host's
    /// store lock, as [`PlaceField::table`].
    pub table: u64,
    pub occurrence: u64,
    pub frame: Frame,
    pub rows: u32,
    pub fields: Vec<u64>,
}

/// Place a text object holding `text` (one line) at `frame` on layout
/// `layout`'s body, on top of the layout's objects, as the Text tool places
/// one, if the layout member `[4, 5, L]` is at revision `expected`
/// ([`catalog_revision`]). The host allocates the object's key and UUIDs
/// and writes the stamp (`docs/authoring.md`, "Texts on layouts"). The
/// reply is [`Reply::Placed`], the text's key as its `field_object`.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceText {
    pub layout: u64,
    pub expected: Revision,
    pub frame: Frame,
    pub text: String,
}

/// Add a Form layout named `name` showing records of table occurrence
/// `occurrence` (its catalog id), with one empty body part, in the theme and
/// width of layout `like` (0: the first layout in the display order with a
/// theme), if the layout catalog's index `[4, 1]` is at revision `expected`
/// ([`catalog_revision`]). The host allocates the layout's id and writes
/// its member `[4, 5, L]`, the index's entries, the stamps and `uuid` as the
/// layout's UUID `d8` (`docs/authoring.md`, "New layouts"; layout-model.md
/// 10.6). The UUID is the request's identity: a layout of the file that
/// already has it was made by this request, whose answer was lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateLayout {
    pub expected: Revision,
    pub occurrence: u64,
    pub like: u64,
    pub name: String,
    pub uuid: [u8; 16],
}

/// Copy layout `layout` as FileMaker Pro's Duplicate Layout does, if the
/// layout catalog's index `[4, 1]` is at revision `expected`
/// ([`catalog_revision`]): the host allocates the copy's id, writes its
/// member, the index's entries, the library's use counts, the stamps and
/// `uuid` as the copy's UUID `d8` (`docs/authoring.md`, "Duplicating
/// layouts"; layout-model.md 10.9). The UUID is the request's identity, as
/// for [`CreateLayout`]. The reply is [`Reply::LayoutCreated`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateLayout {
    pub expected: Revision,
    pub layout: u64,
    pub uuid: [u8; 16],
}

/// Delete layout `layout` as FileMaker Pro's Delete Layout does, if the
/// layout catalog's index `[4, 1]` is at revision `expected`
/// ([`catalog_revision`]): the host removes its member and index entries,
/// releases the library uses its objects held and writes the stamps
/// (`docs/authoring.md`, "Deleting layouts"; layout-model.md 10.10). The
/// reply is [`Reply::Deleted`] with the index's revision; a layout already
/// gone at another revision was deleted by an earlier sending (`changed`
/// false), as layout ids are not issued again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteLayout {
    pub expected: Revision,
    pub layout: u64,
}

/// Show field object `object` of layout `layout` with display type
/// `control` (layout-model.md 5.4) and value list `value_list` (`None`
/// keeps the stored one), if the layout's member is at revision `expected`
/// ([`catalog_revision`]) (`docs/authoring.md`, "Field controls"). The
/// reply is [`Reply::Framed`] with the member's revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldControl {
    pub expected: Revision,
    pub layout: u64,
    pub object: u64,
    pub control: u32,
    pub value_list: Option<u64>,
}

/// Show text object `object` of layout `layout` with the words `text`, if
/// the layout's member is at revision `expected` ([`catalog_revision`])
/// (`docs/authoring.md`, "Texts on layouts"). The reply is
/// [`Reply::Framed`] with the member's revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetText {
    pub expected: Revision,
    pub layout: u64,
    pub object: u64,
    pub text: String,
}

/// Set one object's stored autosizing anchors at the expected layout revision.
/// The reply is [`Reply::Framed`] with the member's revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetObjectAnchors {
    pub expected: Revision,
    pub layout: u64,
    pub object: u64,
    /// The four autosizing bits (28 through 31 of LayoutObject.flags).
    pub flags: u32,
}

/// Set Part Setup's options for part `part` (its key, `[4, 5, L, 03, P]`)
/// of layout `layout`: its `PartDef` flags, page break after every
/// `page_break_after` occurrences (0: off) and, for a sub-summary, the field
/// it is sorted by (`None` keeps the stored one), as catalog ids of the
/// occurrence, its base table and the field, if the layout's member is at
/// revision
/// `expected` ([`catalog_revision`]) (`docs/authoring.md`, "Part
/// options"). The reply is [`Reply::Framed`] with the member's revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartOptions {
    pub expected: Revision,
    pub layout: u64,
    pub part: u64,
    pub flags: u64,
    pub page_break_after: u32,
    pub break_field: Option<(u64, u64, u64)>,
}

/// What [`ChangeParts`] does to a layout's parts.
pub const PARTS_ADD: u8 = 1;
pub const PARTS_DELETE: u8 = 2;
pub const PARTS_RESIZE: u8 = 3;

/// Add, delete or resize a part of layout `layout`, as Part Setup's Create
/// and Delete and a dragged part boundary do, if the layout member
/// `[4, 5, L]` is at revision `expected` ([`catalog_revision`]): `action`
/// is [`PARTS_ADD`] (a part of type `part_type`, `size` points high, where
/// FileMaker's order puts it), [`PARTS_DELETE`] (part `part`, of type
/// `part_type`) or [`PARTS_RESIZE`] (part `part`, of type `part_type`, made
/// `size` points high). Parts are keyed again from the top and the objects
/// of the parts that move go with them (`docs/authoring.md`, "Parts"). The
/// reply is [`Reply::Framed`] with the member's revision.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeParts {
    pub expected: Revision,
    pub layout: u64,
    pub action: u8,
    pub part: u64,
    pub part_type: u32,
    pub size: f64,
}

/// Set layout `layout`'s width (`LayoutDef.baseWidth`) to `width` points,
/// if its member `[4, 5, L]` is at revision `expected`
/// ([`catalog_revision`]); its definition's other bytes stay as stored
/// (`docs/authoring.md`, "Layout width"). The reply is [`Reply::Framed`]
/// with the member's revision.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutWidth {
    pub expected: Revision,
    pub layout: u64,
    pub width: f64,
}

/// Set custom value list `list`'s values to `values` (one item per entry,
/// `-` for a divider) as Edit Value List does, if its member `[21, 5, V]`
/// is at revision `expected` ([`catalog_revision`]; `docs/authoring.md`,
/// "Value lists"; file-metadata.md 6.5). The reply is [`Reply::Framed`]
/// with the member's revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueListValues {
    pub expected: Revision,
    pub list: u64,
    pub values: Vec<String>,
}

/// Set layout `layout`'s name, view flags `f1` and whether the layout
/// menus leave it out (`hidden`) as Layout Setup does, if its member
/// `[4, 5, L]` is at revision `expected` ([`catalog_revision`]): `flags`
/// may differ from the stored ones only in the bits Layout Setup sets
/// (`docs/authoring.md`, "Layout Setup"; layout-model.md 10.11). With
/// `margins`, also its fixed page margins (`LayoutDef.customMargins`, in
/// points: top, left, bottom, right), in the same commit: operation 23,
/// else 16. The reply is [`Reply::Framed`] with the member's revision.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutSetup {
    pub expected: Revision,
    pub layout: u64,
    pub name: String,
    pub flags: u64,
    pub hidden: bool,
    pub margins: Option<[f64; 4]>,
}

/// Move or resize objects of layout `layout`: each object (its key) to its
/// frame, if the layout member `[4, 5, L]` is at revision `expected`
/// ([`catalog_revision`]). The host plans the frames into the layout as
/// stored and writes the layout's stamp (`docs/authoring.md`, "Moving
/// objects"; layout-model.md 10.7).
#[derive(Clone, Debug, PartialEq)]
pub struct SetObjectFrames {
    pub layout: u64,
    pub expected: Revision,
    pub frames: Vec<(u64, Frame)>,
}

/// Delete objects of layout `layout` (their keys), if the layout member
/// `[4, 5, L]` is at revision `expected` ([`catalog_revision`]). The host
/// plans the deletion into the layout as stored and writes the layout's
/// stamp (`docs/authoring.md`, "Deleting objects"; layout-model.md 10.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteObjects {
    pub layout: u64,
    pub expected: Revision,
    pub objects: Vec<u64>,
}

/// Place a web viewer at `frame` on layout `layout`'s body, on top of the
/// layout's objects, as the Web Viewer tool and Web Viewer Setup place one
/// (#896), with Setup's options `flags` (`externalObj.flags`) and a
/// custom web address (a calculation's formatted stream, empty for none,
/// and its Polish stream), if the layout member `[4, 5, L]` is at revision
/// `expected`. The host checks the options and the address, allocates the
/// viewer's key and UUID and writes the stamp (`docs/authoring.md`, "Web
/// viewers on layouts"). The reply is [`Reply::Placed`], the viewer's key
/// as its `field_object`.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceWebViewer {
    pub layout: u64,
    pub expected: Revision,
    pub frame: Frame,
    pub flags: u32,
    pub formatted: Vec<u8>,
    pub polish: Vec<u8>,
}

/// Place a button on layout `layout`'s body that performs script
/// `script`, with label `label` and an optional parameter (a step
/// calculation's formatted and Polish streams, empty for none), if the
/// layout is at revision `expected` (#226; `docs/authoring.md`, "Buttons
/// on layouts"). The host checks the script and the parameter, allocates
/// the object's key and UUIDs and writes the stamp. The reply is
/// [`Reply::Placed`], the button's key as its `field_object`.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceButton {
    pub layout: u64,
    pub expected: Revision,
    pub script: u64,
    pub frame: Frame,
    pub label: String,
    pub parameter: Option<(Vec<u8>, Vec<u8>)>,
}

/// Place a button on layout `layout`'s body whose action is one script
/// step (Button Setup's Single Step), with label `label`, if the layout is
/// at revision `expected` (`docs/authoring.md`, "Buttons on layouts"). The
/// step is its descriptor as `LayoutObject.proto`'s `ScriptStep` stores it
/// and its data subtree (the button's attribute `11`), as the Script
/// Workspace stores a step. The reply is [`Reply::Placed`], as for
/// [`PlaceButton`].
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceButtonStep {
    pub layout: u64,
    pub expected: Revision,
    pub frame: Frame,
    pub label: String,
    pub id: u16,
    pub value: u16,
    pub flags: u32,
    pub reserved: [u8; 14],
    pub data: Option<OwnedNode>,
}

/// A node as the only child of a root, in the wire form.
fn node_wire(node: &OwnedNode) -> Option<Vec<u8>> {
    OwnedNode::list(Vec::new(), vec![node.clone()]).to_wire()
}

/// The only child of the root of the wire slot list `bytes`, read in one
/// pass within `memory` bytes of memory (`om_hbam::tree::read_owned`):
/// `Ok(None)` when its tree would take more, an error when the bytes are not
/// one node.
fn wire_node(bytes: &[u8], memory: usize) -> Result<Option<OwnedNode>, PerformError> {
    match om_hbam::tree::read_owned(SlotReader::new(bytes), memory) {
        Ok(mut root) if root.children.len() == 1 => Ok(root.children.pop()),
        Err(om_hbam::Error::TooLarge { .. }) => Ok(None),
        _ => Err(PerformError::Truncated),
    }
}

/// Build the transaction. Fails when the definition does not encode or is
/// larger than [`MAX_DEFINITION_BYTES`].
pub fn build_request(
    transaction_id: u32,
    file_id: u32,
    request: &Request,
) -> Result<Vec<u8>, String> {
    let header = file_header(Opcode::OpenMakerAuthoring, transaction_id, file_id);
    let b = Builder::with_header(header);
    let (b, operation, version) = match request {
        Request::Describe => (b, DESCRIBE, VERSION),
        Request::DescribeSecurity => (b, DESCRIBE_SECURITY, VERSION),
        Request::Save => (b, SAVE, VERSION),
        Request::ReplaceScriptSteps(replace) => {
            let wire = node_wire(&replace.definition)
                .ok_or("The script definition has a key or value too large to send.")?;
            if wire.len() > MAX_DEFINITION_BYTES {
                return Err(format!(
                    "The script definition is {} KB; one request carries at most {} KB.",
                    wire.len().div_ceil(1024),
                    MAX_DEFINITION_BYTES / 1024
                ));
            }
            let b = b
                .push_u64(replace.script)
                .push_raw(&replace.expected)
                .push_raw(&wire)
                .push_u32(wire.len() as u32);
            (b, REPLACE_SCRIPT_STEPS, VERSION)
        }
        Request::CreateField(create) => {
            if create.name.chars().count() > MAX_FIELD_NAME_CHARS
                || create.comment.chars().count() > MAX_COMMENT_CHARS
            {
                return Err(format!(
                    "A field name holds at most {MAX_FIELD_NAME_CHARS} characters and a comment \
                     {MAX_COMMENT_CHARS}."
                ));
            }
            let b = b
                .push_string(&create.comment)
                .push_string(&create.name)
                .push_u8(create.data_type)
                .push_raw(&create.expected)
                .push_u64(create.table);
            (b, CREATE_FIELD, VERSION)
        }
        Request::PlaceField(place) => {
            let push_frame = |b: Builder, frame: &Frame| {
                frame
                    .iter()
                    .rev()
                    .fold(b, |b, edge| b.push_u64(edge.to_bits()))
            };
            let b = match &place.label {
                Some((text, frame)) => {
                    if text.chars().count() > MAX_LABEL_CHARS {
                        return Err(format!(
                            "A label holds at most {MAX_LABEL_CHARS} characters."
                        ));
                    }
                    push_frame(b, frame).push_string(text).push_u8(1)
                }
                None => b.push_u8(0),
            };
            let b = push_frame(b, &place.frame)
                .push_u64(place.field)
                .push_u64(place.occurrence)
                .push_u64(place.table)
                .push_raw(&place.expected)
                .push_u64(place.layout);
            (b, PLACE_FIELD, VERSION)
        }
        Request::FieldControl(set) => {
            let b = b
                .push_bool(set.value_list.is_some())
                .push_u64(set.value_list.unwrap_or(0))
                .push_u32(set.control)
                .push_u64(set.object)
                .push_raw(&set.expected)
                .push_u64(set.layout);
            (b, FIELD_CONTROL, VERSION)
        }
        Request::ValueListValues(set) => {
            // Each item on its own, so its boundaries survive; an item is
            // one line.
            if set.values.iter().any(|v| v.contains(['\r', '\n'])) {
                return Err("a value list's item holds a return".into());
            }
            let chars: usize = set.values.iter().map(|v| v.chars().count()).sum();
            if set.values.len() > MAX_VALUES || chars > MAX_VALUES_CHARS {
                return Err(format!(
                    "the values are over {MAX_VALUES} items or {MAX_VALUES_CHARS} characters"
                ));
            }
            let mut b = b;
            for value in set.values.iter().rev() {
                b = b.push_string(value);
            }
            let b = b
                .push_u32(u32::try_from(set.values.len()).expect("bounded"))
                .push_raw(&set.expected)
                .push_u64(set.list);
            (b, VALUE_LIST_VALUES, VERSION)
        }
        Request::PlacePortal(place) => {
            if place.fields.is_empty() || place.fields.len() > MAX_PORTAL_FIELDS {
                return Err(format!(
                    "A portal is placed with 1 to {MAX_PORTAL_FIELDS} fields."
                ));
            }
            let b = place
                .fields
                .iter()
                .rev()
                .fold(b, |b, field| b.push_u64(*field))
                .push_u32(place.fields.len() as u32)
                .push_u32(place.rows);
            let b = place
                .frame
                .iter()
                .rev()
                .fold(b, |b, edge| b.push_u64(edge.to_bits()))
                .push_u64(place.occurrence)
                .push_u64(place.table)
                .push_raw(&place.expected)
                .push_u64(place.layout);
            (b, PLACE_PORTAL, VERSION)
        }
        Request::SetObjectAnchors(set) => {
            let b = b
                .push_u32(set.flags)
                .push_u64(set.object)
                .push_raw(&set.expected)
                .push_u64(set.layout);
            (b, SET_OBJECT_ANCHORS, VERSION)
        }
        Request::SetText(set) => {
            let b = b
                .push_string(&set.text)
                .push_u64(set.object)
                .push_raw(&set.expected)
                .push_u64(set.layout);
            (b, SET_TEXT, VERSION)
        }
        Request::ChangeParts(change) => {
            let b = b
                .push_u64(change.size.to_bits())
                .push_u32(change.part_type)
                .push_u64(change.part)
                .push_u8(change.action)
                .push_raw(&change.expected)
                .push_u64(change.layout);
            (b, CHANGE_PARTS, VERSION)
        }
        Request::PartOptions(set) => {
            let (occurrence, table, field) = set.break_field.unwrap_or((0, 0, 0));
            let b = b
                .push_bool(set.break_field.is_some())
                .push_u64(field)
                .push_u64(table)
                .push_u64(occurrence)
                .push_u32(set.page_break_after)
                .push_u64(set.flags)
                .push_u64(set.part)
                .push_raw(&set.expected)
                .push_u64(set.layout);
            (b, PART_OPTIONS, VERSION)
        }
        Request::LayoutWidth(set) => {
            let b = b
                .push_u64(set.width.to_bits())
                .push_raw(&set.expected)
                .push_u64(set.layout);
            (b, LAYOUT_WIDTH, VERSION)
        }
        Request::LayoutSetup(setup) => {
            let (b, operation) = match setup.margins {
                Some(margins) => (
                    margins
                        .iter()
                        .rev()
                        .fold(b, |b, edge| b.push_u64(edge.to_bits())),
                    LAYOUT_SETUP_MARGINS,
                ),
                None => (b, LAYOUT_SETUP),
            };
            let b = b
                .push_bool(setup.hidden)
                .push_string(&setup.name)
                .push_u64(setup.flags)
                .push_raw(&setup.expected)
                .push_u64(setup.layout);
            (b, operation, VERSION)
        }
        Request::DeleteLayout(delete) => {
            let b = b.push_u64(delete.layout).push_raw(&delete.expected);
            (b, DELETE_LAYOUT, VERSION)
        }
        Request::DuplicateLayout(duplicate) => {
            let b = b
                .push_raw(&duplicate.uuid)
                .push_u64(duplicate.layout)
                .push_raw(&duplicate.expected);
            (b, DUPLICATE_LAYOUT, VERSION)
        }
        Request::PlaceText(place) => {
            if place.text.chars().count() > MAX_LABEL_CHARS {
                return Err(format!(
                    "A text holds at most {MAX_LABEL_CHARS} characters."
                ));
            }
            let b = place
                .frame
                .iter()
                .rev()
                .fold(b.push_string(&place.text), |b, edge| {
                    b.push_u64(edge.to_bits())
                })
                .push_raw(&place.expected)
                .push_u64(place.layout);
            (b, PLACE_TEXT, VERSION)
        }
        Request::CreateLayout(create) => {
            if create.name.chars().count() > MAX_LAYOUT_NAME_CHARS {
                return Err(format!(
                    "A layout name holds at most {MAX_LAYOUT_NAME_CHARS} characters."
                ));
            }
            let b = b
                .push_raw(&create.uuid)
                .push_string(&create.name)
                .push_u64(create.like)
                .push_u64(create.occurrence)
                .push_raw(&create.expected);
            (b, CREATE_LAYOUT, VERSION)
        }
        Request::SetObjectFrames(set) => {
            if set.frames.is_empty() || set.frames.len() > MAX_OBJECT_FRAMES {
                return Err(format!("A request moves 1 to {MAX_OBJECT_FRAMES} objects."));
            }
            let mut b = b;
            for (object, frame) in set.frames.iter().rev() {
                b = frame
                    .iter()
                    .rev()
                    .fold(b, |b, edge| b.push_u64(edge.to_bits()))
                    .push_u64(*object);
            }
            let b = b
                .push_u32(set.frames.len() as u32)
                .push_raw(&set.expected)
                .push_u64(set.layout);
            (b, SET_OBJECT_FRAMES, VERSION)
        }
        Request::DeleteObjects(delete) => {
            if delete.objects.is_empty() || delete.objects.len() > MAX_OBJECT_FRAMES {
                return Err(format!(
                    "A request deletes 1 to {MAX_OBJECT_FRAMES} objects."
                ));
            }
            let b = delete
                .objects
                .iter()
                .rev()
                .fold(b, |b, object| b.push_u64(*object))
                .push_u32(delete.objects.len() as u32)
                .push_raw(&delete.expected)
                .push_u64(delete.layout);
            (b, DELETE_OBJECTS, VERSION)
        }
        Request::PlaceButton(place) => {
            if place.label.chars().count() > MAX_LABEL_CHARS {
                return Err(format!(
                    "A label holds at most {MAX_LABEL_CHARS} characters."
                ));
            }
            let b = match &place.parameter {
                Some((formatted, polish)) => {
                    if formatted.len() > MAX_PARAMETER_BYTES || polish.len() > MAX_PARAMETER_BYTES {
                        return Err(format!(
                            "A script parameter's calculation is at most {} KB.",
                            MAX_PARAMETER_BYTES / 1024
                        ));
                    }
                    b.push_raw(polish)
                        .push_u32(polish.len() as u32)
                        .push_raw(formatted)
                        .push_u32(formatted.len() as u32)
                        .push_u8(1)
                }
                None => b.push_u8(0),
            };
            let b = place
                .frame
                .iter()
                .rev()
                .fold(b.push_string(&place.label), |b, edge| {
                    b.push_u64(edge.to_bits())
                })
                .push_u64(place.script)
                .push_raw(&place.expected)
                .push_u64(place.layout);
            (b, PLACE_BUTTON, VERSION)
        }
        Request::PlaceWebViewer(place) => {
            if place.formatted.len() + place.polish.len() > MAX_WEB_ADDRESS_BYTES {
                return Err(format!(
                    "A web address's calculation is at most {} KB.",
                    MAX_WEB_ADDRESS_BYTES / 1024
                ));
            }
            let b = place
                .frame
                .iter()
                .rev()
                .fold(
                    b.push_raw(&place.polish)
                        .push_u32(place.polish.len() as u32)
                        .push_raw(&place.formatted)
                        .push_u32(place.formatted.len() as u32)
                        .push_u32(place.flags),
                    |b, edge| b.push_u64(edge.to_bits()),
                )
                .push_raw(&place.expected)
                .push_u64(place.layout);
            (b, PLACE_WEB_VIEWER, VERSION)
        }
        Request::PlaceButtonStep(place) => {
            if place.label.chars().count() > MAX_LABEL_CHARS {
                return Err(format!(
                    "A label holds at most {MAX_LABEL_CHARS} characters."
                ));
            }
            let wire = match &place.data {
                Some(data) => node_wire(data).ok_or("The step's data does not encode.")?,
                None => Vec::new(),
            };
            if wire.len() > MAX_STEP_DATA_BYTES {
                return Err(format!(
                    "A button's step data is at most {} KB.",
                    MAX_STEP_DATA_BYTES / 1024
                ));
            }
            let b = b
                .push_raw(&wire)
                .push_u32(wire.len() as u32)
                .push_raw(&place.reserved)
                .push_u32(place.flags)
                .push_u16(place.value)
                .push_u16(place.id);
            let b = place
                .frame
                .iter()
                .rev()
                .fold(b.push_string(&place.label), |b, edge| {
                    b.push_u64(edge.to_bits())
                })
                .push_raw(&place.expected)
                .push_u64(place.layout);
            (b, PLACE_BUTTON_STEP, VERSION)
        }
        Request::Unknown { version, operation } => (b, *operation, *version),
    };
    Ok(b.push_u8(operation).push_u16(version).finish())
}

/// Parse the transaction (the host side). A version or operation it does
/// not know is [`Request::Unknown`], to be answered
/// [`status::UNSUPPORTED`].
pub fn parse_request(transaction: &[u8]) -> Result<Request, PerformError> {
    parse_request_within(transaction, usize::MAX)?.ok_or(PerformError::Truncated)
}

/// [`parse_request`], a new definition read into its tree in one pass
/// within `memory` bytes of memory, as `om_hbam::tree::read_owned` counts
/// it. `Ok(None)` when its tree would take more: a slot list of short keys
/// takes tens of times its size as a tree, so a host passes the memory it
/// reserved for the request and refuses the request past it.
pub fn parse_request_within(
    transaction: &[u8],
    memory: usize,
) -> Result<Option<Request>, PerformError> {
    let (mut r, operation) = request_header(transaction)?;
    let request = match operation {
        Operation::Unknown { version, operation } => {
            return Ok(Some(Request::Unknown { version, operation }))
        }
        Operation::Describe => Request::Describe,
        Operation::DescribeSecurity => Request::DescribeSecurity,
        Operation::Save => Request::Save,
        Operation::ReplaceScriptSteps => {
            let len = r.pop_u32()? as usize;
            let wire = r.pop_raw(len)?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let script = r.pop_u64()?;
            let Some(definition) = wire_node(wire, memory)? else {
                return Ok(None);
            };
            Request::ReplaceScriptSteps(ReplaceScriptSteps {
                script,
                expected,
                definition,
            })
        }
        Operation::CreateField => {
            let table = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let data_type = r.pop_u8()?;
            let name = r.pop_string()?;
            let comment = r.pop_string()?;
            Request::CreateField(CreateField {
                table,
                expected,
                data_type,
                name,
                comment,
            })
        }
        Operation::PlaceField => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let table = r.pop_u64()?;
            let occurrence = r.pop_u64()?;
            let field = r.pop_u64()?;
            let frame = pop_frame(&mut r)?;
            let label = match r.pop_u8()? {
                0 => None,
                _ => {
                    let text = r.pop_string()?;
                    Some((text, pop_frame(&mut r)?))
                }
            };
            Request::PlaceField(PlaceField {
                layout,
                expected,
                table,
                occurrence,
                field,
                frame,
                label,
            })
        }
        Operation::SetObjectAnchors => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let object = r.pop_u64()?;
            let flags = r.pop_u32()?;
            Request::SetObjectAnchors(SetObjectAnchors {
                expected,
                layout,
                object,
                flags,
            })
        }
        Operation::FieldControl => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let object = r.pop_u64()?;
            let control = r.pop_u32()?;
            let list = r.pop_u64()?;
            let value_list = r.pop_bool()?.then_some(list);
            Request::FieldControl(FieldControl {
                expected,
                layout,
                object,
                control,
                value_list,
            })
        }
        Operation::ValueListValues => {
            let list = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let count = r.pop_u32()? as usize;
            if count > MAX_VALUES {
                return Err(PerformError::Truncated);
            }
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(r.pop_string()?);
            }
            Request::ValueListValues(ValueListValues {
                expected,
                list,
                values,
            })
        }
        Operation::PlacePortal => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let table = r.pop_u64()?;
            let occurrence = r.pop_u64()?;
            let frame = pop_frame(&mut r)?;
            let rows = r.pop_u32()?;
            let count = r.pop_u32()? as usize;
            if count == 0 || count > MAX_PORTAL_FIELDS {
                return Err(PerformError::Truncated);
            }
            let mut fields = Vec::with_capacity(count);
            for _ in 0..count {
                fields.push(r.pop_u64()?);
            }
            Request::PlacePortal(PlacePortal {
                layout,
                expected,
                table,
                occurrence,
                frame,
                rows,
                fields,
            })
        }
        Operation::SetText => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let object = r.pop_u64()?;
            let text = r.pop_string()?;
            Request::SetText(SetText {
                expected,
                layout,
                object,
                text,
            })
        }
        Operation::ChangeParts => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let action = r.pop_u8()?;
            let part = r.pop_u64()?;
            let part_type = r.pop_u32()?;
            let size = f64::from_bits(r.pop_u64()?);
            Request::ChangeParts(ChangeParts {
                expected,
                layout,
                action,
                part,
                part_type,
                size,
            })
        }
        Operation::PartOptions => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let part = r.pop_u64()?;
            let flags = r.pop_u64()?;
            let page_break_after = r.pop_u32()?;
            let occurrence = r.pop_u64()?;
            let table = r.pop_u64()?;
            let field = r.pop_u64()?;
            let break_field = r.pop_bool()?.then_some((occurrence, table, field));
            Request::PartOptions(PartOptions {
                expected,
                layout,
                part,
                flags,
                page_break_after,
                break_field,
            })
        }
        Operation::LayoutWidth => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let width = f64::from_bits(r.pop_u64()?);
            Request::LayoutWidth(LayoutWidth {
                expected,
                layout,
                width,
            })
        }
        Operation::LayoutSetup | Operation::LayoutSetupMargins => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let flags = r.pop_u64()?;
            let name = r.pop_string()?;
            let hidden = r.pop_bool()?;
            let margins = match operation {
                Operation::LayoutSetupMargins => Some(pop_frame(&mut r)?),
                _ => None,
            };
            Request::LayoutSetup(LayoutSetup {
                expected,
                layout,
                name,
                flags,
                hidden,
                margins,
            })
        }
        Operation::DeleteLayout => {
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let layout = r.pop_u64()?;
            Request::DeleteLayout(DeleteLayout { expected, layout })
        }
        Operation::DuplicateLayout => {
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let layout = r.pop_u64()?;
            let uuid: [u8; 16] = r.pop_raw(16)?.try_into().expect("16 bytes");
            Request::DuplicateLayout(DuplicateLayout {
                expected,
                layout,
                uuid,
            })
        }
        Operation::PlaceText => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let frame = pop_frame(&mut r)?;
            let text = r.pop_string()?;
            Request::PlaceText(PlaceText {
                layout,
                expected,
                frame,
                text,
            })
        }
        Operation::SetObjectFrames => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let count = r.pop_u32()? as usize;
            if count == 0 || count > MAX_OBJECT_FRAMES {
                return Err(PerformError::Truncated);
            }
            let mut frames = Vec::with_capacity(count);
            for _ in 0..count {
                let object = r.pop_u64()?;
                frames.push((object, pop_frame(&mut r)?));
            }
            Request::SetObjectFrames(SetObjectFrames {
                layout,
                expected,
                frames,
            })
        }
        Operation::DeleteObjects => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let count = r.pop_u32()? as usize;
            if count == 0 || count > MAX_OBJECT_FRAMES {
                return Err(PerformError::Truncated);
            }
            let mut objects = Vec::with_capacity(count);
            for _ in 0..count {
                objects.push(r.pop_u64()?);
            }
            Request::DeleteObjects(DeleteObjects {
                layout,
                expected,
                objects,
            })
        }
        Operation::CreateLayout => {
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let occurrence = r.pop_u64()?;
            let like = r.pop_u64()?;
            let name = r.pop_string()?;
            let uuid: [u8; 16] = r.pop_raw(16)?.try_into().expect("16 bytes");
            Request::CreateLayout(CreateLayout {
                expected,
                occurrence,
                like,
                name,
                uuid,
            })
        }
        Operation::PlaceButton => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let script = r.pop_u64()?;
            let frame = pop_frame(&mut r)?;
            let label = r.pop_string()?;
            let parameter = match r.pop_u8()? {
                0 => None,
                _ => {
                    let mut stream = || -> Result<Vec<u8>, PerformError> {
                        let len = r.pop_u32()? as usize;
                        if len > MAX_PARAMETER_BYTES {
                            return Err(PerformError::Truncated);
                        }
                        Ok(r.pop_raw(len)?.to_vec())
                    };
                    let formatted = stream()?;
                    let polish = stream()?;
                    Some((formatted, polish))
                }
            };
            Request::PlaceButton(PlaceButton {
                layout,
                expected,
                script,
                frame,
                label,
                parameter,
            })
        }
        Operation::PlaceWebViewer => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let frame = pop_frame(&mut r)?;
            let flags = r.pop_u32()?;
            // Both streams within one bound, before either is read.
            let mut left = MAX_WEB_ADDRESS_BYTES;
            let mut stream = || -> Result<Vec<u8>, PerformError> {
                let len = r.pop_u32()? as usize;
                if len > left {
                    return Err(PerformError::Truncated);
                }
                left -= len;
                Ok(r.pop_raw(len)?.to_vec())
            };
            let formatted = stream()?;
            let polish = stream()?;
            Request::PlaceWebViewer(PlaceWebViewer {
                layout,
                expected,
                frame,
                flags,
                formatted,
                polish,
            })
        }
        Operation::PlaceButtonStep => {
            let layout = r.pop_u64()?;
            let expected: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
            let frame = pop_frame(&mut r)?;
            let label = r.pop_string()?;
            let id = r.pop_u16()?;
            let value = r.pop_u16()?;
            let flags = r.pop_u32()?;
            let reserved: [u8; 14] = r.pop_raw(14)?.try_into().expect("14 bytes");
            let len = r.pop_u32()? as usize;
            if len > MAX_STEP_DATA_BYTES {
                return Err(PerformError::Truncated);
            }
            let wire = r.pop_raw(len)?;
            let data = if wire.is_empty() {
                None
            } else {
                let Some(data) = wire_node(wire, memory)? else {
                    return Ok(None);
                };
                Some(data)
            };
            Request::PlaceButtonStep(PlaceButtonStep {
                layout,
                expected,
                frame,
                label,
                id,
                value,
                flags,
                reserved,
                data,
            })
        }
    };
    finished(&r)?;
    Ok(Some(request))
}

/// The operation the request `transaction` asks for, read without its
/// parameters: a host checks what the operation needs of its caller before
/// it reads them, and reads a definition, tens of times its size as a
/// tree, only for a caller that may send it. An error only when the
/// transaction does not hold a version and an operation.
pub fn request_operation(transaction: &[u8]) -> Result<Operation, PerformError> {
    request_header(transaction).map(|(_, operation)| operation)
}

/// The request's version and operation, and the reader at its parameters.
fn request_header(transaction: &[u8]) -> Result<(Reader<'_>, Operation), PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let version = r.pop_u16()?;
    let operation = r.pop_u8()?;
    let known = match (version, operation) {
        (VERSION, DESCRIBE) => Operation::Describe,
        (VERSION, REPLACE_SCRIPT_STEPS) => Operation::ReplaceScriptSteps,
        (VERSION, CREATE_FIELD) => Operation::CreateField,
        (VERSION, PLACE_FIELD) => Operation::PlaceField,
        (VERSION, PLACE_BUTTON) => Operation::PlaceButton,
        (VERSION, PLACE_BUTTON_STEP) => Operation::PlaceButtonStep,
        (VERSION, PLACE_WEB_VIEWER) => Operation::PlaceWebViewer,
        (VERSION, DESCRIBE_SECURITY) => Operation::DescribeSecurity,
        (VERSION, CREATE_LAYOUT) => Operation::CreateLayout,
        (VERSION, SET_OBJECT_FRAMES) => Operation::SetObjectFrames,
        (VERSION, DELETE_OBJECTS) => Operation::DeleteObjects,
        (VERSION, PLACE_TEXT) => Operation::PlaceText,
        (VERSION, DUPLICATE_LAYOUT) => Operation::DuplicateLayout,
        (VERSION, DELETE_LAYOUT) => Operation::DeleteLayout,
        (VERSION, LAYOUT_SETUP) => Operation::LayoutSetup,
        (VERSION, LAYOUT_WIDTH) => Operation::LayoutWidth,
        (VERSION, LAYOUT_SETUP_MARGINS) => Operation::LayoutSetupMargins,
        (VERSION, FIELD_CONTROL) => Operation::FieldControl,
        (VERSION, PART_OPTIONS) => Operation::PartOptions,
        (VERSION, CHANGE_PARTS) => Operation::ChangeParts,
        (VERSION, SET_TEXT) => Operation::SetText,
        (VERSION, SET_OBJECT_ANCHORS) => Operation::SetObjectAnchors,
        (VERSION, VALUE_LIST_VALUES) => Operation::ValueListValues,
        (VERSION, PLACE_PORTAL) => Operation::PlacePortal,
        (VERSION, SAVE) => Operation::Save,
        _ => Operation::Unknown { version, operation },
    };
    Ok((r, known))
}

fn pop_frame(r: &mut Reader<'_>) -> Result<Frame, PerformError> {
    let mut frame = [0.0; 4];
    for edge in &mut frame {
        *edge = f64::from_bits(r.pop_u64()?);
    }
    Ok(frame)
}

/// What the host offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Described {
    /// The operations it implements ([`OP_REPLACE_SCRIPT_STEPS`],
    /// [`OP_CREATE_FIELD`], [`OP_PLACE_FIELD`]).
    pub operations: u32,
    /// Whether this session's account may author: change scripts, add
    /// fields and place them on layouts.
    pub can_author: bool,
    /// The largest definition it takes, in its wire form.
    pub max_definition_bytes: u32,
}

/// A replacement the host made, or found already made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replaced {
    /// False when the script already had the new steps: nothing was
    /// written.
    pub changed: bool,
    /// The stored definition's content revision.
    pub revision: Revision,
}

/// A field the host added, or found already there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    /// False when the table already had this field (the same name, kind,
    /// type, storage and comment) and was not at the expected revision: an
    /// earlier request added it, so nothing was written.
    pub changed: bool,
    /// The field's catalog id, the big-endian reading of its key.
    pub field: u64,
    /// The table's fields' revision as stored ([`catalog_revision`]).
    pub revision: Revision,
}

/// A placement the host made, or found already made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// False when the layout already had these objects (the same field at
    /// the same frame, and the same label) and was not at the expected
    /// revision: an earlier request placed them, so nothing was written.
    pub changed: bool,
    /// The field object's key, and the label object's (0 for none).
    pub field_object: u64,
    pub label_object: u64,
    /// The layout member's revision as stored ([`catalog_revision`]).
    pub revision: Revision,
}

/// A layout the host added, or found already there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutCreated {
    /// False when a layout of the file has the request's UUID: an earlier
    /// sending of the request added it, so nothing was written.
    pub changed: bool,
    /// The layout's id, the big-endian reading of its key.
    pub layout: u64,
    /// The layout catalog index's revision as stored ([`catalog_revision`]).
    pub revision: Revision,
}

/// Frames the host set, or found already set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Framed {
    /// False when every object was already at its frame and the layout was
    /// not at the expected revision: an earlier request set them, so
    /// nothing was written.
    pub changed: bool,
    /// The layout member's revision as stored ([`catalog_revision`]).
    pub revision: Revision,
}

/// Objects the host deleted, or found already deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deleted {
    /// False when none of the objects was on the layout and the layout was
    /// not at the expected revision: an earlier request deleted them, so
    /// nothing was written.
    pub changed: bool,
    /// The layout member's revision as stored ([`catalog_revision`]).
    pub revision: Revision,
}

/// A reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Described(Described),
    Replaced(Replaced),
    Created(Created),
    Placed(Placed),
    LayoutCreated(LayoutCreated),
    Framed(Framed),
    Deleted(Deleted),
    /// The file's security catalog, answering
    /// [`Request::DescribeSecurity`].
    Security(SecurityCatalog),
    /// The file on disk holds every change, answering [`Request::Save`].
    Saved,
    /// Refused with `status`; nothing changed. A conflict names the
    /// script's current revision.
    Refused {
        status: u16,
        message: String,
        current: Option<Revision>,
    },
    /// A host without the extension (a reply of a lone status).
    NotSupported {
        status: u16,
    },
}

/// Build a reply (the host side). A [`Reply::Security`] catalog holding a
/// key or record flags too long for its wire form
/// ([`MAX_KEY`], [`MAX_RECORD_FLAGS`]) is answered as status 3 instead: the
/// host leaves such records out before it builds the reply.
pub fn build_reply(reply: &Reply) -> Vec<u8> {
    let b = ResultBuilder::new();
    let (b, message, code) = match reply {
        Reply::Security(catalog) => match push_catalog(b, catalog) {
            Some(b) => (b, "", status::OK),
            None => {
                return build_reply(&Reply::Refused {
                    status: status::UNSUPPORTED,
                    message: "The security catalog has a record this version cannot send.".into(),
                    current: None,
                })
            }
        },
        Reply::Described(d) => (
            b.push_u32(d.max_definition_bytes)
                .push_bool(d.can_author)
                .push_u32(d.operations),
            "",
            status::OK,
        ),
        Reply::Replaced(r) => (b.push_raw(&r.revision).push_bool(r.changed), "", status::OK),
        Reply::Saved => (b, "", status::OK),
        Reply::Created(c) => (
            b.push_raw(&c.revision)
                .push_u64(c.field)
                .push_bool(c.changed),
            "",
            status::OK,
        ),
        Reply::Placed(p) => (
            b.push_raw(&p.revision)
                .push_u64(p.label_object)
                .push_u64(p.field_object)
                .push_bool(p.changed),
            "",
            status::OK,
        ),
        Reply::Framed(f) => (b.push_raw(&f.revision).push_bool(f.changed), "", status::OK),
        Reply::Deleted(d) => (b.push_raw(&d.revision).push_bool(d.changed), "", status::OK),
        Reply::LayoutCreated(c) => (
            b.push_raw(&c.revision)
                .push_u64(c.layout)
                .push_bool(c.changed),
            "",
            status::OK,
        ),
        Reply::Refused {
            status,
            message,
            current,
        } => {
            let b = match current {
                Some(revision) => b.push_raw(revision).push_bool(true),
                None => b.push_bool(false),
            };
            (b, message.as_str(), *status)
        }
        Reply::NotSupported { status } => return ResultBuilder::new().push_u16(*status).finish(),
    };
    b.push_string(message)
        .push_u16(VERSION)
        .push_u16(code)
        .finish()
}

/// Parse the reply to `request` (the client side).
pub fn parse_reply(request: &Request, result: &[u8]) -> Result<Reply, PerformError> {
    let mut r = ResultReader::new(result);
    let code = r.pop_u16()?;
    if r.remaining() == 0 {
        return Ok(Reply::NotSupported { status: code });
    }
    let _version = r.pop_u16()?;
    let message = r.pop_string()?;
    let reply = if code == status::OK {
        match request {
            Request::Describe => {
                let operations = r.pop_u32()?;
                let can_author = r.pop_bool()?;
                let max_definition_bytes = r.pop_u32()?;
                Reply::Described(Described {
                    operations,
                    can_author,
                    max_definition_bytes,
                })
            }
            Request::ReplaceScriptSteps(_) => {
                let changed = r.pop_bool()?;
                let revision: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
                Reply::Replaced(Replaced { changed, revision })
            }
            Request::CreateField(_) => {
                let changed = r.pop_bool()?;
                let field = r.pop_u64()?;
                let revision: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
                Reply::Created(Created {
                    changed,
                    field,
                    revision,
                })
            }
            Request::PlaceField(_)
            | Request::PlaceButton(_)
            | Request::PlaceButtonStep(_)
            | Request::PlaceText(_)
            | Request::PlacePortal(_)
            | Request::PlaceWebViewer(_) => {
                let changed = r.pop_bool()?;
                let field_object = r.pop_u64()?;
                let label_object = r.pop_u64()?;
                let revision: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
                Reply::Placed(Placed {
                    changed,
                    field_object,
                    label_object,
                    revision,
                })
            }
            Request::SetObjectFrames(_)
            | Request::LayoutSetup(_)
            | Request::LayoutWidth(_)
            | Request::FieldControl(_)
            | Request::PartOptions(_)
            | Request::ChangeParts(_)
            | Request::SetText(_)
            | Request::SetObjectAnchors(_)
            | Request::ValueListValues(_) => {
                let changed = r.pop_bool()?;
                let revision: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
                Reply::Framed(Framed { changed, revision })
            }
            Request::DeleteObjects(_) | Request::DeleteLayout(_) => {
                let changed = r.pop_bool()?;
                let revision: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
                Reply::Deleted(Deleted { changed, revision })
            }
            Request::CreateLayout(_) | Request::DuplicateLayout(_) => {
                let changed = r.pop_bool()?;
                let layout = r.pop_u64()?;
                let revision: Revision = r.pop_raw(32)?.try_into().expect("32 bytes");
                Reply::LayoutCreated(LayoutCreated {
                    changed,
                    layout,
                    revision,
                })
            }
            Request::DescribeSecurity => Reply::Security(pop_catalog(&mut r)?),
            Request::Save => Reply::Saved,
            Request::Unknown { .. } => return Err(PerformError::Truncated),
        }
    } else {
        let current = if r.pop_bool()? {
            Some(r.pop_raw(32)?.try_into().expect("32 bytes"))
        } else {
            None
        };
        Reply::Refused {
            status: code,
            message,
            current,
        }
    };
    if r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(steps: &[u8]) -> OwnedNode {
        OwnedNode::list(
            vec![0x80, 0x0d],
            vec![
                OwnedNode::leaf(vec![4], steps.to_vec()),
                OwnedNode::leaf(vec![0x10], b"name".to_vec()),
                OwnedNode::leaf(vec![0xfc], vec![1, 1]),
            ],
        )
    }

    #[test]
    fn operation_bits_are_distinct_and_a_layout_host_does_not_offer_save() {
        let bits = [
            OP_REPLACE_SCRIPT_STEPS,
            OP_CREATE_FIELD,
            OP_PLACE_FIELD,
            OP_DESCRIBE_SECURITY,
            OP_CREATE_LAYOUT,
            OP_SAVE,
        ];
        for (i, a) in bits.iter().enumerate() {
            for b in &bits[i + 1..] {
                assert_eq!(a & b, 0, "{a:#x} {b:#x}");
            }
        }
        // A host that offers creating layouts (operation 6) and not Save
        // (9): a client asking for Save's bit is told it is unavailable.
        let layouts_only = OP_REPLACE_SCRIPT_STEPS
            | OP_CREATE_FIELD
            | OP_PLACE_FIELD
            | OP_DESCRIBE_SECURITY
            | OP_CREATE_LAYOUT;
        assert_eq!(layouts_only & OP_SAVE, 0);
        assert_eq!(OP_SAVE, 1 << 9);
        assert_eq!(
            request_operation(&build_request(9, 1, &Request::Save).unwrap()),
            Ok(Operation::Save)
        );
    }

    #[test]
    fn requests_and_replies_round_trip() {
        let replace = Request::ReplaceScriptSteps(ReplaceScriptSteps {
            script: 141,
            expected: [7; 32],
            definition: definition(&[1; 28]),
        });
        let create = Request::CreateField(CreateField {
            table: 0x8001,
            expected: [5; 32],
            data_type: 3,
            name: "Due date".into(),
            comment: "When it is due".into(),
        });
        let place = Request::PlaceField(PlaceField {
            layout: 3,
            expected: [8; 32],
            table: 0x8001,
            occurrence: 0xd0_0001,
            field: 0x8000,
            frame: [80.0, 110.5, 100.0, 310.25],
            label: Some(("Follow up".into(), [80.0, 20.0, 100.0, 100.0])),
        });
        let unlabeled = Request::PlaceField(PlaceField {
            layout: 3,
            expected: [8; 32],
            table: 0x8001,
            occurrence: 0xd0_0001,
            field: 7,
            frame: [1.0, 2.0, 3.0, 4.0],
            label: None,
        });
        let layout = Request::CreateLayout(CreateLayout {
            expected: [3; 32],
            occurrence: 0xd0_0002,
            like: 3,
            name: "Invoices – Form".into(),
            uuid: [0xab; 16],
        });
        let frames = Request::SetObjectFrames(SetObjectFrames {
            layout: 3,
            expected: [4; 32],
            frames: vec![(1, [60.0, 140.0, 82.0, 300.5]), (12, [1.0, 2.0, 3.0, 4.0])],
        });
        let deletion = Request::DeleteObjects(DeleteObjects {
            layout: 3,
            expected: [6; 32],
            objects: vec![12, 0x4000],
        });
        let button = Request::PlaceButton(PlaceButton {
            layout: 3,
            expected: [8; 32],
            script: 0x8003,
            frame: [80.0, 110.0, 104.0, 200.0],
            label: "Print".into(),
            parameter: Some((vec![1, 2, 3], vec![0x13, 1, b'x' ^ 0x5a, 0])),
        });
        let plain_button = Request::PlaceButton(PlaceButton {
            layout: 3,
            expected: [8; 32],
            script: 2,
            frame: [1.0, 2.0, 3.0, 4.0],
            label: "Go".into(),
            parameter: None,
        });
        let step_button = Request::PlaceButtonStep(PlaceButtonStep {
            layout: 3,
            expected: [8; 32],
            frame: [80.0, 110.0, 104.0, 200.0],
            label: "Finish".into(),
            id: 76,
            value: 0,
            flags: 0x4000,
            reserved: [0; 14],
            data: Some(OwnedNode::list(
                vec![0x11],
                vec![OwnedNode::list(
                    vec![0, 0],
                    vec![OwnedNode::leaf(vec![0x10], vec![1; 14])],
                )],
            )),
        });
        let plain_step = Request::PlaceButtonStep(PlaceButtonStep {
            layout: 3,
            expected: [8; 32],
            frame: [1.0, 2.0, 3.0, 4.0],
            label: "New".into(),
            id: 7,
            value: 0,
            flags: 0,
            reserved: [0; 14],
            data: None,
        });
        let control = Request::FieldControl(FieldControl {
            expected: [9; 32],
            layout: 3,
            object: 37,
            control: 2,
            value_list: Some(4),
        });
        let portal = Request::PlacePortal(PlacePortal {
            layout: 3,
            expected: [8; 32],
            table: 0x81,
            occurrence: 0xd0_0003,
            frame: [158.0, 134.0, 433.0, 743.5],
            rows: 7,
            fields: vec![2, 0x8001],
        });
        let anchors = Request::SetObjectAnchors(SetObjectAnchors {
            expected: [12; 32],
            layout: 3,
            object: 5,
            flags: 0xc0000000,
        });
        let words = Request::SetText(SetText {
            expected: [2; 32],
            layout: 3,
            object: 5,
            text: "Given Name – é".into(),
        });
        let parts = Request::ChangeParts(ChangeParts {
            expected: [6; 32],
            layout: 3,
            action: PARTS_RESIZE,
            part: 2,
            part_type: 4,
            size: 135.0,
        });
        let part = Request::PartOptions(PartOptions {
            expected: [3; 32],
            layout: 3,
            part: 4,
            flags: 0x441,
            page_break_after: 2,
            break_field: Some((0xd0_0002, 0x8001, 4)),
        });
        let width = Request::LayoutWidth(LayoutWidth {
            expected: [7; 32],
            layout: 3,
            width: 900.5,
        });
        let margins = Request::LayoutSetup(LayoutSetup {
            expected: [8; 32],
            layout: 3,
            name: "Contact Details".into(),
            flags: 0x10f3_0a49,
            hidden: false,
            margins: Some([50.0, 70.0, 60.0, 80.5]),
        });
        let setup = Request::LayoutSetup(LayoutSetup {
            expected: [6; 32],
            layout: 3,
            name: "Contact Details – 2".into(),
            flags: 0x10f3_0a09,
            hidden: true,
            margins: None,
        });
        let gone = Request::DeleteLayout(DeleteLayout {
            expected: [4; 32],
            layout: 2,
        });
        let duplicate = Request::DuplicateLayout(DuplicateLayout {
            expected: [5; 32],
            layout: 3,
            uuid: [0xcd; 16],
        });
        let text = Request::PlaceText(PlaceText {
            layout: 3,
            expected: [9; 32],
            frame: [20.0, 30.0, 40.0, 130.0],
            text: "Due – soon".into(),
        });
        let values = Request::ValueListValues(ValueListValues {
            expected: [7; 32],
            list: 1,
            values: ["Photo", "-", "Ünïcode – item", ""]
                .map(String::from)
                .to_vec(),
        });
        // Each item as given, none at all included; an item is one line.
        let no_values = Request::ValueListValues(ValueListValues {
            expected: [7; 32],
            list: 1,
            values: Vec::new(),
        });
        let returns = Request::ValueListValues(ValueListValues {
            expected: [7; 32],
            list: 1,
            values: vec!["Two\rlines".into()],
        });
        assert!(build_request(9, 1, &returns).is_err());
        let viewer = Request::PlaceWebViewer(PlaceWebViewer {
            layout: 3,
            expected: [8; 32],
            frame: [300.0, 40.0, 500.0, 440.0],
            flags: 0x800d,
            formatted: vec![0x13, 1, b'x' ^ 0x5a, 0],
            polish: vec![0x13, 1, b'x' ^ 0x5a],
        });
        for request in [
            anchors,
            viewer,
            values,
            no_values,
            parts.clone(),
            portal.clone(),
            words.clone(),
            part.clone(),
            control.clone(),
            width.clone(),
            setup.clone(),
            gone.clone(),
            duplicate.clone(),
            text.clone(),
            deletion.clone(),
            frames.clone(),
            Request::Describe,
            Request::DescribeSecurity,
            Request::Save,
            replace.clone(),
            create.clone(),
            place.clone(),
            unlabeled,
            layout.clone(),
            button.clone(),
            plain_button,
            step_button,
            plain_step,
        ] {
            let transaction = build_request(9, 1, &request).unwrap();
            assert_eq!(transaction[0], 240);
            assert_eq!(parse_request(&transaction).unwrap(), request);
        }
        let unknown = Request::Unknown {
            version: 2,
            operation: DESCRIBE,
        };
        let transaction = build_request(9, 1, &unknown).unwrap();
        assert_eq!(parse_request(&transaction).unwrap(), unknown);

        let replies = [
            (Request::Save, Reply::Saved),
            (
                Request::Save,
                Reply::Refused {
                    status: status::SAVE_FAILED[0],
                    message: "not written".into(),
                    current: None,
                },
            ),
            (
                Request::Describe,
                Reply::Described(Described {
                    operations: OP_REPLACE_SCRIPT_STEPS,
                    can_author: true,
                    max_definition_bytes: MAX_DEFINITION_BYTES as u32,
                }),
            ),
            (
                replace.clone(),
                Reply::Replaced(Replaced {
                    changed: true,
                    revision: [3; 32],
                }),
            ),
            (
                replace.clone(),
                Reply::Refused {
                    status: status::CONFLICT,
                    message: "changed".into(),
                    current: Some([4; 32]),
                },
            ),
            (
                replace,
                Reply::Refused {
                    status: status::PERMISSION_DENIED,
                    message: "no".into(),
                    current: None,
                },
            ),
            (
                create.clone(),
                Reply::Created(Created {
                    changed: true,
                    field: 0x8000,
                    revision: [6; 32],
                }),
            ),
            (
                create,
                Reply::Refused {
                    status: status::NAME_EXISTS,
                    message: "taken".into(),
                    current: None,
                },
            ),
            (
                place.clone(),
                Reply::Placed(Placed {
                    changed: true,
                    field_object: 4,
                    label_object: 5,
                    revision: [9; 32],
                }),
            ),
            (
                place,
                Reply::Refused {
                    status: status::LAYOUT_IN_USE,
                    message: "in Layout mode".into(),
                    current: None,
                },
            ),
            (
                frames,
                Reply::Framed(Framed {
                    changed: true,
                    revision: [5; 32],
                }),
            ),
            (
                deletion,
                Reply::Deleted(Deleted {
                    changed: false,
                    revision: [7; 32],
                }),
            ),
            (
                control,
                Reply::Framed(Framed {
                    changed: true,
                    revision: [9; 32],
                }),
            ),
            (
                width,
                Reply::Framed(Framed {
                    changed: true,
                    revision: [8; 32],
                }),
            ),
            (
                margins,
                Reply::Framed(Framed {
                    changed: true,
                    revision: [3; 32],
                }),
            ),
            (
                setup,
                Reply::Framed(Framed {
                    changed: false,
                    revision: [2; 32],
                }),
            ),
            (
                gone,
                Reply::Deleted(Deleted {
                    changed: true,
                    revision: [3; 32],
                }),
            ),
            (
                duplicate,
                Reply::LayoutCreated(LayoutCreated {
                    changed: false,
                    layout: 5,
                    revision: [2; 32],
                }),
            ),
            (
                text,
                Reply::Placed(Placed {
                    changed: true,
                    field_object: 52,
                    label_object: 0,
                    revision: [7; 32],
                }),
            ),
            (
                layout,
                Reply::LayoutCreated(LayoutCreated {
                    changed: true,
                    layout: 5,
                    revision: [2; 32],
                }),
            ),
            (
                Request::DescribeSecurity,
                Reply::Security(SecurityCatalog {
                    accounts: vec![
                        CatalogAccount {
                            key: vec![1],
                            name: None,
                            kind: Some(0),
                            active: Some(false),
                            must_change_password: Some(false),
                            privilege_set: Some(vec![3]),
                        },
                        CatalogAccount {
                            key: vec![2],
                            name: Some("Admin".into()),
                            kind: None,
                            active: None,
                            must_change_password: None,
                            privilege_set: None,
                        },
                    ],
                    privilege_sets: vec![CatalogPrivilegeSet {
                        key: vec![1],
                        name: Some("[Full Access]".into()),
                        record_flags: Some(vec![0x55, 3]),
                        password_days: Some(0),
                    }],
                    extended_privileges: vec![CatalogExtendedPrivilege {
                        key: vec![0x80, 0x84],
                        keyword: Some("fmapp".into()),
                        sets: vec![vec![1], vec![2]],
                    }],
                }),
            ),
        ];
        for (request, reply) in replies {
            assert_eq!(parse_reply(&request, &build_reply(&reply)).unwrap(), reply);
        }
        // A host without the extension answers a lone status 3.
        let legacy = ResultBuilder::new().push_u16(3).finish();
        assert_eq!(
            parse_reply(&Request::Describe, &legacy).unwrap(),
            Reply::NotSupported { status: 3 }
        );
    }

    #[test]
    fn stamp_keys_are_two_byte_segments_as_captured() {
        // schema-path.md 3.4 (live:s1/a_um16): `0e fc01 09 <"OpenMaker">`.
        let user = OwnedNode::leaf(MOD_USER.to_vec(), om_scsu::encode_scsu("OpenMaker"));
        let wire = OwnedNode::list(Vec::new(), vec![user]).to_wire().unwrap();
        assert_eq!(wire[..5], [0x80, 0x0e, 0xfc, 0x01, 0x09]);
    }

    /// A host reads a request's operation without its definition, and a
    /// new definition into its tree within the memory it gives: a
    /// definition of many short keys, tens of times its size as a tree, is
    /// not read within a few times its size, and within enough it reads as
    /// `parse_request` reads it (#385).
    #[test]
    fn definitions_are_read_within_a_memory_limit() {
        let data = (1..=4_000u64)
            .map(|k| OwnedNode::leaf(om_hbam::serial::encode(k), Vec::new()))
            .collect();
        let mut short_keys = definition(&[1; 28]);
        short_keys.set_child(OwnedNode::list(vec![5], data));
        let request = Request::ReplaceScriptSteps(ReplaceScriptSteps {
            script: 1,
            expected: [0; 32],
            definition: short_keys,
        });
        let txn = build_request(1, 1, &request).unwrap();
        assert_eq!(request_operation(&txn), Ok(Operation::ReplaceScriptSteps));
        assert_eq!(parse_request_within(&txn, 4 * txn.len()), Ok(None));
        assert_eq!(
            parse_request_within(&txn, usize::MAX),
            Ok(Some(request.clone()))
        );
        assert_eq!(parse_request(&txn), Ok(request));
    }

    #[test]
    fn oversized_definitions_are_refused_before_sending() {
        // As a long script stores its step data: one subtree per step.
        let data = (1..=50_000u64)
            .map(|k| OwnedNode::leaf(om_hbam::serial::encode(k), vec![0; 24]))
            .collect();
        let mut big = definition(&[1; 28]);
        big.set_child(OwnedNode::list(vec![5], data));
        let request = Request::ReplaceScriptSteps(ReplaceScriptSteps {
            script: 1,
            expected: [0; 32],
            definition: big,
        });
        assert!(build_request(1, 1, &request)
            .unwrap_err()
            .contains("at most"));
    }
}
