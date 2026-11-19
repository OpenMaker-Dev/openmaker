//! The data contract between OpenMaker's GUI and its data sources.
//!
//! The GUI depends only on this crate's [`DataProvider`] trait and value types,
//! so the UI and the protocol implementation can be built independently. A
//! protocol-backed provider (over a live FileMaker Server) and the [`mock`]
//! provider both implement [`DataProvider`].

pub use om_calc as calc;
pub use om_formula as formula;
pub use om_scripts as scripts;
pub use om_scripts::step_name as script_step_name;
pub use om_scripts::{
    flag, runtime, specs, text_node, Calculation, FieldSpec, Script, ScriptCatalog, ScriptError,
    ScriptKind, ScriptStep, StoredKey,
};
pub use om_sql as sql;
use std::fmt;

mod browse;
pub mod calc_field;
mod calc_triggers;
pub mod choice;
pub mod containers;
pub mod field_catalog;
pub mod field_options;
pub mod file_refs;
pub mod formula_editor;
pub mod found;
pub mod host;
pub mod layout_catalog;
pub mod layout_objects;
pub mod layout_placement;
pub mod library_uses;
pub mod meta;
pub mod new_file;
pub mod quick_find;
pub mod rules;
mod schema;
pub mod schema_draft;
pub mod script_catalog;
pub mod security_catalog;
pub mod security_checksum;
mod step_names;
pub mod summary;
pub mod summary_field;
pub mod value_lists;
pub use browse::{HostBrowser, HostListing, ListingError, ListingFailure};
pub use schema::{
    arrange, unused_name, Arrangement, DatabaseSchema, FieldColumn, OccurrenceBox, OccurrenceShows,
    RelationshipMatch, RelationshipSide, SchemaCapabilities, SchemaChange, SchemaField,
    SchemaFieldKind, SchemaMatch, SchemaOccurrence, SchemaRelationship, SchemaTable,
};

/// A hosted file available on a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedFile {
    pub name: String,
}

/// A layout in an open file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutInfo {
    pub id: u32,
    pub name: String,
}

/// A display-ordered catalog member for layout menus, separate from the
/// navigable layout list used by scripts and Design functions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutMenuEntry {
    pub id: u32,
    pub name: String,
    /// Catalog word `[4, 5, L, f0]` (or its index copy).
    pub flags: u64,
}

/// A field (column) on a layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
}

/// The coarse value type of a field, enough for the GUI to format cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
    Container,
}

/// One record: a row of stringified cell values aligned with a layout's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: i64,
    /// One entry per field; `None` is an empty cell.
    pub values: Vec<Option<String>>,
}

/// Changed fields by their schema names. `None` clears a value. Providers
/// reject unknown fields rather than silently changing another column.
pub type FieldChanges = std::collections::BTreeMap<String, Option<String>>;

/// A script's write of one record (Set Field and the other field steps,
/// committed by Commit Records or by leaving the record): the rules a
/// script's writes follow apply, not a user's (`rules::Entry::script`;
/// script-runtime.md 13.20, 13.21): any normal field and repetition may be
/// set, an auto-entered value that prohibits modification included, and
/// the rules checked as a value is entered apply only to fields validated
/// Always (Set Field checked them, [`DataProvider::check_entry`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptWrite {
    /// The window's layout.
    pub layout: u32,
    /// The base table of a related record (a portal row's, or a related
    /// field's first record); `None` for the layout's own table.
    pub table: Option<u64>,
    /// The record; `None` creates one.
    pub record: Option<i64>,
    /// Cells by id: a field's id for its first repetition, `F * 256 + n`
    /// for repetition n; stored text, `None` empty.
    pub cells: std::collections::BTreeMap<u64, Option<String>>,
    /// The same first repetitions by field name, for providers without
    /// field ids; empty when a related record or a later repetition is
    /// written.
    pub fields: FieldChanges,
    /// The cells of `cells` the user typed (the user saves a record
    /// scripts changed, or a script commits a record the user typed
    /// into): checked by a user's rules as a value is entered, as
    /// [`DataProvider::update_record`] checks them, and against a
    /// related-values list at the commit, unless it skips data entry
    /// validation (#1074); the others by a script's.
    pub typed: std::collections::BTreeSet<u64>,
    /// Commit Records' "Skip data entry validation".
    pub skip_validation: bool,
    /// The session's current privilege set as the step runs: [Full
    /// Access] in a step of a script running with full access, else the
    /// account's. The write's auto-entered and validation formulas read it
    /// as `Get ( CurrentPrivilegeSetName )` (`rules::WriteContext`); `None`
    /// when the caller does not know it, which refuses a write whose formula
    /// reads it.
    pub privilege_set: Option<String>,
    /// The record's `Get ( RecordNumber )` as the window answers it for the
    /// write's auto-entered and validation formulas
    /// (`rules::WriteContext::record_number`), given only where FileMaker's
    /// answer is known (#1038: a row typed into the creation row of a portal
    /// that neither sorts nor filters is its place among the portal's
    /// related records, after those related before the save). `None`
    /// refuses a write whose formula evaluates it.
    pub record_number: Option<u64>,
    /// The preview of this new record its window made before writing it
    /// ([`DataProvider::preview_new_record`], #1047): the write reuses the
    /// serial numbers the preview reserved and the clock and UUID it read,
    /// so a value from unchanged inputs is the value the preview showed.
    /// A preview the provider no longer holds (discarded, used up by the
    /// record's commit, or from before its file reconnected) refuses the
    /// write (3); the window previews the row again. `None` for any other
    /// write.
    pub prepared: Option<PreparedId>,
}

/// A record of the window's that the host does not have as the window
/// holds it (#1063): its own record with its typed cells, a row begun in
/// one of its portals, or a portal row it edited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingRecord {
    /// Its base table, by the window's file's id (as [`ScriptWrite::table`]).
    pub table: u64,
    /// The stored record, `None` for one not created yet.
    pub record: Option<i64>,
    /// Its unsaved cells over the stored ones, by cell as
    /// [`ScriptWrite::cells`]: stored text, `None` empty.
    pub cells: std::collections::BTreeMap<u64, Option<String>>,
    /// The cells of `cells` the user typed, read as their field's type (as
    /// [`ScriptWrite::typed`]).
    pub typed: std::collections::BTreeSet<u64>,
}

/// A new record's values computed before it is written
/// ([`DataProvider::preview_new_record`], #1047), named by its write
/// ([`ScriptWrite::prepared`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreparedId(pub u64);

/// What a new record's rules enter, before it is written (#1047).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRecordPreview {
    /// The id its write names ([`ScriptWrite::prepared`]).
    pub id: PreparedId,
    /// Every cell the record would store with a value other than the
    /// write's own that the record gives alone (auto-entered values, serial
    /// numbers, stored calculations), by cell: a field's id for its first
    /// repetition, `F * 256 + n` for repetition n. A value read from another
    /// record (a lookup, a related field, an aggregate), which the window
    /// may hold unsaved, and every value computed from one is left out
    /// (#1063), as is a formula the preview cannot evaluate yet; the write
    /// computes those, or refuses as it would without a preview.
    pub values: std::collections::BTreeMap<u64, Option<String>>,
}

/// The window's own record as its commit would compute it, before it is
/// committed ([`DataProvider::compute_pending`], #1104).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingValues {
    /// Every cell the record would store, by cell (a field's id for its
    /// first repetition, `F * 256 + n` for repetition n), the window's own
    /// unsaved cells included: stored text, `None` for a cell the window
    /// empties. A cell absent is empty.
    pub values: std::collections::BTreeMap<u64, Option<String>>,
    /// The fields (by id) whose values the commit may not store: one its
    /// rules cannot compute yet, one drawn again (`Random`, the clock or a
    /// UUID, a modification timestamp), one read from another record's
    /// such value, and every field computed from one. Such a field reads as
    /// nothing known, never as its value in `values` or as empty.
    pub unknown: std::collections::BTreeSet<u64>,
}

/// Record operations supported for one layout's underlying table.
///
/// A provider may support editing and creating records before it supports
/// deletion. `editable_fields`, when supplied, is an explicit schema-name
/// allowlist; containers are never exposed as text editors by the GUI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordCapabilities {
    /// Whether existing-record entry should await `record.prepare_edit`.
    /// False keeps ordinary entry synchronous; it grants no recovery proof.
    /// Providers advertise this only where the operation can retain a receipt.
    pub prepare_edit: bool,
    pub create: bool,
    pub update: bool,
    pub delete: bool,
    pub editable_fields: Option<std::collections::BTreeSet<String>>,
    /// The fields of related records Browse may edit, by the table
    /// occurrence they are reached through (one relationship from the
    /// layout's) and field id: those the occurrence's table's own rules
    /// let a user edit. An occurrence missing here takes no edits.
    pub related: std::collections::BTreeMap<u64, std::collections::BTreeSet<u64>>,
    /// The fields Browse would let the user edit but the account's
    /// privilege set refuses, by schema name, with why. FileMaker Pro lets
    /// the user enter them and answers typing with the refusal's alert.
    pub refused: std::collections::BTreeMap<String, EditRefusal>,
    /// The table's global fields, by schema name. A record the set may not
    /// edit does not refuse them: their value is the session's, which a
    /// write stores before it checks the record's limits (#690).
    pub globals: std::collections::BTreeSet<String>,
}

/// FileMaker Pro's alert for a change the account's privilege set does not
/// allow (#137's reading of FileMaker Pro 26.0.3), with error 200.
pub const PRIVILEGE_DENIED: &str =
    "Your access privileges do not allow you to perform this action.";

/// FileMaker Pro's alert for a change to a field the account's privilege
/// set gives view-only access (#137's reading), with error 201.
pub const FIELD_NOT_MODIFIABLE: &str =
    "This action cannot be performed because this field is not modifiable.";

/// Why the account's privilege set refuses a change the user types into a
/// field (#690).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditRefusal {
    /// [Read-Only Access], a set that may not edit the table's records or
    /// this record, or the layout's records view only: error 200.
    Privileges,
    /// A field the set gives view-only access: error 201.
    ViewOnlyField,
}

impl EditRefusal {
    /// FileMaker Pro's alert text.
    pub fn message(self) -> &'static str {
        match self {
            Self::Privileges => PRIVILEGE_DENIED,
            Self::ViewOnlyField => FIELD_NOT_MODIFIABLE,
        }
    }
}

/// A related file that refused the account and password the file was
/// opened with ([`DataProvider::related_sign_ins`], #214).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelatedSignIn {
    /// The file's name, as its data source names it.
    pub file: String,
    /// The opened file's table occurrences that show its tables: a layout
    /// using one needs the file.
    pub occurrences: std::collections::BTreeSet<u64>,
}

/// What of a page the account's custom privilege set gives no access
/// (docs/re/session-identity.md 2.5): records it may not view (their values
/// arrive withheld) and fields without access, by schema name. FileMaker
/// Pro shows `<No Access>` in them (#137's reading); their values are empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoAccess {
    pub records: std::collections::BTreeSet<i64>,
    pub fields: std::collections::BTreeSet<String>,
    /// The records the account may view but not edit (`Get ( RecordAccess )`
    /// 1): its set's edit access, an edit formula not true on them, or the
    /// layout's records view only.
    pub view_only: std::collections::BTreeSet<i64>,
    /// Whether the account's set was judged at all (`None` set access:
    /// nothing is known, and `Get ( RecordAccess )` keeps the set's answer).
    pub judged: bool,
    /// The layout gives its records no access (session-identity.md 2.2):
    /// every field shows `<No Access>` through it, while `Get (
    /// RecordAccess )` keeps the table's answer (#707's reading).
    pub layout: bool,
}

impl NoAccess {
    /// Whether field `field` of record `record` shows `<No Access>`.
    pub fn hides(&self, record: i64, field: &str) -> bool {
        self.layout || self.records.contains(&record) || self.fields.contains(field)
    }

    /// `Get ( RecordAccess )` for record `record` (session-identity.md
    /// 2.4): 0 when it may not be viewed, 1 when it may not be edited, 2
    /// otherwise; `None` when the set was not judged.
    pub fn record_access(&self, record: i64) -> Option<i64> {
        if !self.judged {
            None
        } else if self.records.contains(&record) {
            Some(0)
        } else if self.view_only.contains(&record) {
            Some(1)
        } else {
            Some(2)
        }
    }
}

impl RecordCapabilities {
    pub fn can_edit(&self, field: &Field) -> bool {
        self.update
            && field.kind != FieldKind::Container
            && self
                .editable_fields
                .as_ref()
                .map_or(true, |names| names.contains(&field.name))
    }

    /// Why typing into field `field` of a record is refused with an alert:
    /// `None` when the user may change it, or when Browse does not let the
    /// user enter it at all. `record_access` is the record's
    /// `Get ( RecordAccess )` ([`NoAccess::record_access`]): a record the
    /// set may not view (0, FileMaker Pro's reading on #137) or not edit
    /// (1, but not in a global field) refuses before the field's own
    /// access (OpenMaker's order; FileMaker Pro's is unmeasured).
    pub fn typing_refusal(&self, field: &Field, record_access: Option<i64>) -> Option<EditRefusal> {
        let refused = self.refused.get(&field.name).copied();
        if field.kind == FieldKind::Container || (refused.is_none() && !self.can_edit(field)) {
            return None;
        }
        match record_access {
            Some(0) => Some(EditRefusal::Privileges),
            Some(1) if !self.globals.contains(&field.name) => Some(EditRefusal::Privileges),
            _ => refused,
        }
    }

    /// Whether Browse may edit field `field` of the related records of
    /// `occurrence` ([`Self::related`]).
    pub fn can_edit_related(&self, occurrence: u64, field: u64) -> bool {
        self.related
            .get(&occurrence)
            .is_some_and(|fields| fields.contains(&field))
    }
}

/// A page of records for a layout, with the field list and total count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordPage {
    /// The immutable host versions of these exact rows, keyed by record id.
    /// Providers without versioned writes leave this empty. A draft must use
    /// the version on its accepted page, never a newer background read.
    pub versions: std::collections::BTreeMap<i64, u64>,
    pub fields: Vec<Field>,
    pub records: Vec<Record>,
    pub total: u64,
}

/// A layout's geometry: where its parts and objects sit, in points, enough to
/// draw the layout as boxes before its styles are known.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutGeometry {
    /// Layout width in points.
    pub width: f64,
    /// Layout height in points: the bottom of the lowest part.
    pub height: f64,
    /// Parts, top to bottom.
    pub parts: Vec<PartGeom>,
    /// Objects in stacking order, back-most first; a container comes before
    /// the objects inside it.
    pub objects: Vec<ObjectGeom>,
    /// The revision of the layout as this geometry was read from it
    /// ([`layout_placement::Revision`]), when the provider can place fields
    /// on it: a placement made on this geometry names it, and the host
    /// refuses the placement if the layout changed since. `None` when the
    /// provider cannot tell; such a layout is not edited.
    pub revision: Option<layout_placement::Revision>,
}

/// One layout part: a band across the layout.
#[derive(Clone, Debug, PartialEq)]
pub struct PartGeom {
    /// The part type for display, e.g. "Header" or "Body".
    pub kind_name: String,
    /// Top edge in layout coordinates (points).
    pub top: f64,
    /// Height in points.
    pub height: f64,
}

/// One layout object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectGeom {
    /// The object type for display, e.g. "Field" or "Text".
    pub kind_name: String,
    /// Bounds in layout coordinates (points). Absolute: a contained object's
    /// bounds are already offset by its containers'.
    pub rect: GeomRect,
    /// For a field object, the name of the field it shows, when known.
    pub field_name: Option<String>,
    /// The object's key in the layout's object store (0 when it is not a
    /// stored object).
    pub key: u64,
    /// Whether Layout mode can move and resize it
    /// ([`layout_objects::movable`]): a top-level object without contents.
    pub movable: bool,
    /// Whether Layout mode can delete it ([`layout_objects::deletable`]):
    /// a movable one that shows no image from the file's library.
    pub deletable: bool,
}

/// A rectangle by its edges, in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GeomRect {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

impl GeomRect {
    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }
}

/// A decoded file layout and the field names used by this provider's record
/// pages. Bindings are keyed by layout object id; unresolved related fields
/// are deliberately absent, rather than displaying values from another table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderLayout {
    pub layout: om_layout::Layout,
    pub field_bindings: std::collections::BTreeMap<u64, String>,
}

/// Credentials and target for a connection. Its `Debug` form leaves the
/// password out.
#[derive(Clone)]
pub struct Connection {
    pub host: String,
    pub port: u16,
    pub file: String,
    pub account: String,
    pub password: String,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("file", &self.file)
            .field("account", &self.account)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// A `.fmp12` file on this computer for the client to open itself
/// ([`DataProvider::open_local`]). Its `Debug` form leaves the password
/// out, and the password is erased when it is dropped.
#[derive(Clone)]
pub struct LocalFile {
    pub path: std::path::PathBuf,
    /// The account and password to log in with; `None` to learn whether
    /// the file can be opened and which account to suggest
    /// ([`LocalOpen::NeedsLogin`]).
    pub credentials: Option<(String, zeroize::Zeroizing<String>)>,
}

impl fmt::Debug for LocalFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalFile")
            .field("path", &self.path)
            .field(
                "account",
                &self.credentials.as_ref().map(|(account, _)| account),
            )
            .finish_non_exhaustive()
    }
}

/// What [`DataProvider::open_local`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalOpen {
    /// The file is open; `read_only` says why no change to it is saved,
    /// when none is (its permissions, its folder, its other names, no
    /// locks).
    Opened { read_only: Option<String> },
    /// The file needs an account name and password: open it again with
    /// credentials. `account` is the name to suggest.
    NeedsLogin { account: String },
}

/// A window's new record a reconnect held again on the new connection
/// (`DataProvider::take_recreated_records`, #152). Its host record id is
/// new, reserved by the new connection: the lost one's reservation went
/// with it. Its values are the ones it had: its cells, its creation clock
/// and UUID and what they computed, and its serial numbers, none computed
/// or taken again.
#[derive(Clone, Debug)]
pub struct RecreatedRecord {
    pub window: u32,
    /// Its base table.
    pub table: u64,
    /// The id the lost connection had reserved.
    pub old: u64,
    /// The id it has now, or why it could not be held again.
    pub new: Result<u64, DataError>,
}

/// An error from a data provider, surfaced to the GUI as a message.
#[derive(Clone, Debug)]
pub struct DataError {
    pub message: String,
    /// The FileMaker error code a script step reports for it
    /// (`Get ( LastError )`), when the provider knows one.
    pub code: Option<i32>,
    /// The open file's connection to the host is gone
    /// ([`DataProvider::connection_lost`]): nothing more can be read or
    /// saved until the file is opened again.
    pub lost: bool,
    /// For a commit that failed: what it did in each file whose records it
    /// held, in the order it reached them (#1006); empty for any other
    /// error, or when the provider does not say. Unless every file
    /// committed the commit failed, and with a file
    /// [`FileOutcome::Committed`] or [`FileOutcome::Unknown`] it may be
    /// partly applied. The window then keeps every change it made and
    /// sends none again by itself.
    pub commit: Vec<FileCommit>,
}

/// What a commit did in one file whose records it held (#1006).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOutcome {
    /// No commit (op70) of its records was sent, so the file is as it was:
    /// an upload before it (op14) goes into the connection's temporary area
    /// only, which changes no record (write-path.md 3.3).
    NotSubmitted,
    /// Its records were committed and read back as written.
    Committed,
    /// Its records were sent, then something failed: what its host kept is
    /// unknown.
    Unknown,
    /// Sent with an unknown outcome, then read back as before the commit
    /// under locks its transaction still held: proven not applied, so a
    /// resumed save sends it again (#1006).
    NotApplied,
    /// Sent with an unknown outcome, and its read-back settles nothing: its
    /// records hold neither what the commit wrote nor what was there
    /// before, cannot be read, or are no longer locked. Only discarding
    /// the save settles it.
    Unresolved,
}

/// One file's part of a failed commit ([`DataError::commit`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCommit {
    /// The file, as the provider tells files apart: the same id for the
    /// same file in every commit of the provider's, whatever its name.
    /// `None` when the provider cannot tell.
    pub participant: Option<ParticipantId>,
    /// The file as the user knows it, for messages only: two hosts can hold
    /// files of one name, numbered alike.
    pub file: String,
    pub outcome: FileOutcome,
}

/// A file a provider's commits write, by an id the provider gives it
/// ([`FileCommit::participant`], #1006): opaque, and the same for the file
/// while the provider is open. It is not a host's id for the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParticipantId(pub u64);

impl DataError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            lost: false,
            commit: Vec::new(),
        }
    }

    /// An error with the FileMaker error code a script step reports.
    pub fn with_code(code: i32, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code),
            lost: false,
            commit: Vec::new(),
        }
    }

    /// The connection to the host was lost.
    pub fn connection_lost(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            lost: true,
            commit: Vec::new(),
        }
    }
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DataError {}

/// A reading of the record data a provider's writes had sent its hosts
/// ([`DataProvider::submission_mark`], #152). Two readings around a save
/// tell whether it sent any ([`SubmissionMark::until`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubmissionMark(u64);

impl SubmissionMark {
    /// A reading of a count that only grows by each request that may
    /// carry or commit record data.
    pub fn new(sent: u64) -> Self {
        Self(sent)
    }

    /// What the provider sent between this reading and `later`, a reading
    /// of the same provider on the same thread. `None` (no reading) is
    /// [`Submitted::Possibly`].
    pub fn until(self, later: Option<SubmissionMark>) -> Submitted {
        match later {
            Some(later) if later.0 == self.0 => Submitted::Nothing,
            _ => Submitted::Possibly,
        }
    }
}

/// What a save sent its hosts between two [`SubmissionMark`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submitted {
    /// No request that may carry or commit record data went to any host,
    /// so a save that failed in between left every record there as it
    /// was.
    Nothing,
    /// One may have gone, or the provider cannot tell: what a failed
    /// save left on a host is unknown.
    Possibly,
}

/// The result of a script started by Perform Script on Server with Callback
/// (script-runtime.md 11.4). The callback script runs as an OnPSOSCallback
/// trigger with `Get ( ScriptResult )` = `result` and `Get ( LastError )`
/// preset to `error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerScriptCallback {
    /// The id given to [`DataProvider::perform_script_on_server_with_callback`].
    pub callback: u32,
    pub error: runtime::ErrorCode,
    pub result: String,
}

/// The Closes queued for a provider that it has not handled yet
/// ([`DataProvider::close_requests`]). The window counts one as it queues
/// Close; the provider's thread counts it handled after
/// [`DataProvider::close`]. Clones share the count.
#[derive(Clone, Debug, Default)]
pub struct CloseRequests(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl CloseRequests {
    /// A Close was queued.
    pub fn queued(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// A queued Close was handled.
    pub fn handled(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        let mut count = self.0.load(SeqCst);
        while count > 0 {
            match self.0.compare_exchange(count, count - 1, SeqCst, SeqCst) {
                Ok(_) => break,
                Err(now) => count = now,
            }
        }
    }

    /// Whether a Close waits: work that may take long stops early.
    pub fn pending(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst) > 0
    }
}

/// The opened record page accepted before ordinary attachment work starts.
/// A later worker read cannot substitute for this RID/version.
#[derive(Clone, Copy, Debug)]
pub struct ContainerEditBaseline {
    /// The accepted layout of the opened file.
    pub layout: u32,
    /// The accepted existing record's id.
    pub record: i64,
    /// The version shown by that accepted page; missing proof is not inferred.
    pub version: Option<u64>,
}

/// Prepare ordinary existing-row recovery before accepting any changed cell.
/// Operation id: `record.prepare_edit`; label: Prepare Record Edit.
/// It neither writes values nor acquires container references.
#[derive(Clone, Debug)]
pub struct RecordEditRequest {
    /// Caller-accepted layout, RID and version, captured before the first edit.
    pub baseline: ContainerEditBaseline,
    /// Actual upcoming cells (field id, or F * 256 + repetition).
    pub cells: std::collections::BTreeSet<u64>,
    /// First-repetition field names for callers without decoded cell ids.
    pub fields: std::collections::BTreeSet<String>,
}

/// Recovery availability is separate from permission to make an ordinary edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditPreparation {
    /// Original row/version and own rules are retained before this mutation.
    Prepared,
    /// This provider/session cannot prove recovery. Ordinary entry keeps its
    /// existing behavior; a later upload must not invent a baseline.
    Unavailable,
}

/// A source of FileMaker data for the GUI.
///
/// Implementations are synchronous and single-threaded; the GUI calls them off
/// the UI thread. Methods return [`DataError`] with a human-readable message.
pub trait DataProvider: Send {
    /// Whether connections can use a FileMaker client license. The GUI stores
    /// the key separately from connection settings in the OS credential store.
    fn supports_client_license(&self) -> bool {
        false
    }

    /// Select the client license for the next connection; `None` clears it.
    fn set_client_license(&mut self, _license: Option<&str>) -> Result<(), DataError> {
        Err(DataError::new(
            "This provider does not use a client license.",
        ))
    }

    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(ScriptCatalog::default())
    }
    /// Catalog member `id` with its steps, to run it: a script, folder or
    /// separator, `None` when the file has none. A provider that can read
    /// one member alone does, as FileMaker Pro 26 reads the script a file
    /// trigger runs (#1053), rather than every script's steps; by default
    /// it is taken from [`DataProvider::scripts`].
    fn script(&mut self, id: u64) -> Result<Option<Script>, DataError> {
        Ok(self.scripts()?.scripts.into_iter().find(|s| s.id == id))
    }
    /// The scripts for the Script Workspace: as [`DataProvider::scripts`],
    /// and the catalog its changes are made on, which
    /// [`DataProvider::save_script_catalog`] refuses to save over once
    /// another client changed it.
    fn scripts_for_editing(&mut self) -> Result<ScriptCatalog, DataError> {
        self.scripts()
    }
    fn save_script(&mut self, _script: Script) -> Result<Script, DataError> {
        Err(DataError::new("This provider cannot save scripts."))
    }
    /// Store the script catalog's members as `catalog` lists them: their
    /// order, names, options and folders, with new members added and
    /// missing ones deleted (the Scripts pane commits these at once, as
    /// FileMaker does). Steps are saved with [`DataProvider::save_script`];
    /// the answer is the catalog as stored.
    fn save_script_catalog(&mut self, _catalog: ScriptCatalog) -> Result<ScriptCatalog, DataError> {
        Err(DataError::new("This provider cannot save scripts."))
    }

    /// Perform Script on Server (script-runtime.md 11) in the open file: the
    /// host runs the script in a session of its own with this connection's
    /// account. The result's error is the script's final `Get ( LastError )`
    /// or the host's refusal (812 when it runs too many scripts); its value
    /// is the Exit Script result when the call waited. A script host passes
    /// the step's call through (`Network::perform_script_on_server`); the
    /// records the script changed should be read again afterwards.
    fn perform_script_on_server(
        &mut self,
        _call: &runtime::ServerScriptCall,
    ) -> Result<runtime::ServerScriptResult, DataError> {
        Err(DataError::new(
            "This provider cannot perform scripts on a server.",
        ))
    }

    /// Perform Script on Server with Callback (script-runtime.md 11.4):
    /// start the script without waiting. Its result comes back from
    /// [`DataProvider::server_script_results`] tagged with `callback`, an
    /// id the caller chose to find its callback script by. Returns the
    /// host's answer to starting it (0, or a refusal such as 812).
    fn perform_script_on_server_with_callback(
        &mut self,
        _call: &runtime::ServerScriptCall,
        _callback: u32,
    ) -> Result<runtime::ErrorCode, DataError> {
        Err(DataError::new(
            "This provider cannot perform scripts on a server.",
        ))
    }

    /// Change the found set of a layout's table (finds, omits, sorts;
    /// [`found`]). [`DataProvider::records`] pages through the found set
    /// afterwards. A find that matches nothing leaves an empty found set
    /// and fails with code 401.
    fn change_found_set(
        &mut self,
        _layout_id: u32,
        _change: found::FoundSetChange,
    ) -> Result<found::FoundSet, DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot find or sort records.",
        ))
    }

    /// Each window of a file has a found set of its own (Claris "Opening
    /// multiple windows per file"): the calls that follow ([`Self::records`],
    /// [`Self::change_found_set`], found-set calculation data) use window
    /// `window`'s found sets. A window's record session
    /// ([`Self::begin_record_commit`]) goes with it too: another window's
    /// writes are not held in it. A provider without windows shares one
    /// found set per table.
    fn use_window(&mut self, _window: u32) {}

    /// New Window: window `to` starts with a copy of window `from`'s found
    /// sets.
    fn copy_window(&mut self, _from: u32, _to: u32) {}

    /// Close Window: window `window`'s found sets are gone, and its record
    /// session, if one is open, ends as Revert ends it.
    fn drop_window(&mut self, _window: u32) {}

    /// The values of value list `id` (its catalog id) in the list's order
    /// (file-metadata.md 6.4): custom values with their `-` dividers, or a
    /// field's distinct values in index order. Sorts by a value list read
    /// it.
    /// Set repetition `repetition` of global field `field` of `table` to
    /// `text` (its stored form) for this session only: FileMaker keeps a
    /// client's global values in a hosted file for its session, starting
    /// from the values the file was stored with (Claris "Global fields").
    /// Calculations read it from then on. A provider that cannot is error
    /// 201, "Field cannot be modified".
    fn set_global(
        &mut self,
        _table: u64,
        _field: u64,
        _repetition: u32,
        _text: &str,
    ) -> Result<(), DataError> {
        Err(DataError::with_code(
            201,
            "This connection cannot set global fields.",
        ))
    }

    /// Enter a user's `text` into a global field, applying the supported
    /// type's entry parsing before changing this session's value. Unlike
    /// [`Self::set_global`], this is not a script's permissive assignment.
    /// The native provider supports the defined repetitions of plain Date,
    /// Time and Timestamp data globals; unsupported targets/providers refuse with error 3.
    fn set_typed_global(
        &mut self,
        _table: u64,
        _field: u64,
        _repetition: u32,
        _text: &str,
    ) -> Result<(), DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This connection cannot enter typed global values.",
        ))
    }

    fn value_list(&mut self, _id: u64) -> Result<Vec<String>, DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot read value lists.",
        ))
    }

    /// The distinct values of field `field` of base table `table` as the
    /// host lists them for a value list: one CreateValueListEx
    /// (query-program.md 8.7), in index order, each with the text the host
    /// sends. `None` when the answer is not known to be the whole list (the
    /// field has no index, the host cut the list, a key may stand for
    /// several values, or the reply could not be read), or when the provider
    /// asks no host: the caller then reads the values another way.
    fn host_field_values(
        &mut self,
        _table: u64,
        _field: u64,
    ) -> Result<Option<Vec<String>>, DataError> {
        Ok(None)
    }

    /// The distinct values of a field of the layout's table in index order
    /// (its value index): the list Insert from Index offers.
    fn index_values(&mut self, _layout_id: u32, _field: &str) -> Result<Vec<String>, DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot read field indexes.",
        ))
    }

    /// Set Next Serial Value: the next value field `field` of the table
    /// of occurrence `occurrence` auto-enters (op72 with `kReplace`,
    /// write-path.md 3.2).
    fn set_next_serial_value(
        &mut self,
        _occurrence: u64,
        _field: u64,
        _value: &str,
    ) -> Result<(), DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot set serial numbers.",
        ))
    }

    /// Open Transaction (script-runtime.md 8.1): until
    /// [`DataProvider::commit_transaction`] or
    /// [`DataProvider::revert_transaction`], record creations, edits and
    /// deletions are held by the provider (and their records locked)
    /// rather than written, and record pages show them.
    fn begin_transaction(
        &mut self,
        _options: runtime::TransactionOptions,
    ) -> Result<(), DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider has no transactions.",
        ))
    }

    /// The window's record session: its commit of a record with its related
    /// records (the user's save, Commit Records), or a record New Record
    /// made, held from then until it is committed
    /// ([`Self::commit_record_session`]) or reverted
    /// ([`Self::revert_transaction`]), as FileMaker Pro holds it under a
    /// transaction of its own (write-path.md 1.1, 1.2). A transaction
    /// [`Self::begin_transaction`] would open with default options, which a
    /// provider may keep apart from a script's Open Transaction (#1006),
    /// and with its window ([`Self::use_window`]).
    fn begin_record_commit(&mut self) -> Result<(), DataError> {
        self.begin_transaction(runtime::TransactionOptions::default())
    }

    /// Commit the window's record session as
    /// [`Self::commit_transaction`] does, except that a refusal before
    /// anything is sent (a rule, a validation) keeps the session open and
    /// its records held, as FileMaker Pro keeps a record open whose commit
    /// was refused. A provider that cannot keep it ends it.
    fn commit_record_session(&mut self, skip_validation: bool) -> Result<(), DataError> {
        self.commit_transaction(skip_validation)
    }

    /// Whether the current window's record session is open
    /// ([`Self::begin_record_commit`]).
    fn record_session_open(&self) -> bool {
        false
    }

    /// The current window's save that may be partly applied (#1006): what
    /// it did in each file whose records it held, one or more of them
    /// [`FileOutcome::Committed`] or [`FileOutcome::Unknown`]. The provider
    /// keeps it, sending nothing again by itself, and the window takes no
    /// changes until it is settled: Revert Record
    /// ([`Self::revert_record_session`]) discards what was not saved, while
    /// the files already saved keep it. `None` when there is none.
    fn unsettled_save(&mut self) -> Option<Vec<FileCommit>> {
        None
    }

    /// Every window's save that may be partly applied, by window, each as
    /// [`Self::unsettled_save`] reports the current window's (#1006): what a
    /// script left, for the windows to freeze when it ends. The default:
    /// none.
    fn unsettled_saves(&mut self) -> Vec<(u32, Vec<FileCommit>)> {
        Vec::new()
    }

    /// Check the current window's unsettled save again (#1006): read back
    /// each file whose outcome is unknown, against the values its commit
    /// computed and what was there before. It becomes
    /// [`FileOutcome::Committed`], [`FileOutcome::NotApplied`] (proven, so
    /// [`Self::resume_save`] sends it) or [`FileOutcome::Unresolved`].
    /// Nothing is sent but reads. The report; a save then applied in every
    /// file is complete.
    fn reconcile_save(&mut self) -> Result<Vec<FileCommit>, DataError> {
        Err(DataError::with_code(
            3,
            "This provider keeps no partly saved records.",
        ))
    }

    /// Resume the current window's unsettled save: send its work not
    /// applied, and only that, in its order, from the values its commit
    /// computed. Nothing is computed again, and no record is created again.
    /// Refused (3) while a file's outcome is unknown or unresolved. A
    /// failure keeps the save, its report updated (the error's
    /// [`DataError::commit`]).
    fn resume_save(&mut self) -> Result<(), DataError> {
        Err(DataError::with_code(
            3,
            "This provider keeps no partly saved records.",
        ))
    }

    /// Discard what the current window's unsettled save did not apply, as
    /// Revert Record does ([`Self::revert_record_session`]): the files that
    /// saved keep it, and nothing is deleted or undone. Refused (3) without
    /// one.
    fn discard_save(&mut self) -> Result<(), DataError> {
        Err(DataError::with_code(
            3,
            "This window has no partly saved record to settle.",
        ))
    }

    /// Whether a transaction is open, a record session or a script's: the
    /// writes made now are held in it, not saved.
    fn transaction_open(&self) -> bool {
        self.record_session_open()
    }

    /// Whether the current window's record session holds its records:
    /// open, or set aside while another transaction is open (a script's
    /// Open Transaction in another window, which the window's writes would
    /// join). A set-aside session is open again once that transaction ends;
    /// meanwhile reads show its records, and the window's commit enters it
    /// ([`Self::enter_record_session`]).
    fn record_session_held(&self) -> bool {
        self.record_session_open()
    }

    /// Make the current window's set-aside record session the open one for
    /// the window's commit, the other transaction set aside until
    /// [`Self::leave_record_session`]: the window's record is outside that
    /// transaction, as FileMaker keeps other windows' changes
    /// (script-runtime.md 8.5). Whether it was entered.
    fn enter_record_session(&mut self) -> bool {
        false
    }

    /// Undo [`Self::enter_record_session`]: the session, if still open, is
    /// set aside again, and the other transaction is open again.
    fn leave_record_session(&mut self) {}

    /// Revert the current window's record session as
    /// [`Self::revert_transaction`] does, also one set aside
    /// ([`Self::record_session_held`]): another transaction open stays
    /// open. Without one, nothing.
    fn revert_record_session(&mut self) -> Result<(), DataError> {
        if self.record_session_open() {
            self.revert_transaction()
        } else {
            Ok(())
        }
    }

    /// Keep what the window's open record session holds before the
    /// window's commit stages its changes in it, so that a commit that
    /// fails after staging some can drop them
    /// ([`Self::rollback_record_session`]) and its retry stages them once.
    fn mark_record_session(&mut self) {}

    /// Put the window's open record session back as
    /// [`Self::mark_record_session`] kept it: what was staged since is
    /// dropped. Records it locked or created since stay locked under the
    /// session, their ids used, until it ends. Without a mark, nothing.
    fn rollback_record_session(&mut self) {}

    /// Commit Transaction (8.2): apply the record rules to every held
    /// record and write them all together; on a failure nothing is
    /// written, the transaction is reverted and its error returned.
    fn commit_transaction(&mut self, _skip_validation: bool) -> Result<(), DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider has no transactions.",
        ))
    }

    /// Revert Transaction (8.3): drop every held change and release the
    /// records. Reserved record ids are not given back.
    fn revert_transaction(&mut self) -> Result<(), DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider has no transactions.",
        ))
    }

    /// The records the open transaction created and still holds, as (base
    /// table, record id), whichever window made them: what
    /// [`Self::revert_transaction`] removes. None from a provider that
    /// does not tell.
    fn transaction_created(&self) -> Vec<(u64, i64)> {
        Vec::new()
    }

    /// The results of scripts started with a callback, waiting up to `wait`
    /// for the first; oldest first.
    fn server_script_results(
        &mut self,
        _wait: std::time::Duration,
    ) -> Result<Vec<ServerScriptCallback>, DataError> {
        Ok(Vec::new())
    }

    /// Authenticated account facts for the open file, when available.
    fn account_info(&self) -> host::AccountInfo {
        host::AccountInfo::default()
    }

    /// The authenticated account's root-file catalog metadata. Unknown on
    /// providers without a security catalog; never an authorization decision.
    fn account_privileges(&self) -> Option<host::Privileges> {
        None
    }

    /// The catalog's [Full Access] set, for an elevated script step.
    fn full_access_privileges(&self) -> Option<host::Privileges> {
        None
    }

    /// Whether this provider implements record creation, editing and deletion.
    fn can_write_records(&self) -> bool {
        false
    }

    /// What of `page`, a page of layout `layout_id`, the account may not
    /// see ([`NoAccess`]); nothing for providers without privilege sets.
    fn no_access(&mut self, _layout_id: u32, _page: &RecordPage) -> NoAccess {
        NoAccess::default()
    }

    /// `Get ( RecordAccess )` for record `record_id` through layout
    /// `layout_id` ([`NoAccess::record_access`]); `None` when unknown.
    fn record_access(&mut self, _layout_id: u32, _record_id: i64) -> Option<i64> {
        None
    }

    /// The operations and editable fields available for this layout. Override
    /// this for providers whose supported operations vary by table or field.
    fn record_capabilities(&mut self, _layout_id: u32) -> Result<RecordCapabilities, DataError> {
        let writable = self.can_write_records();
        Ok(RecordCapabilities {
            create: writable,
            update: writable,
            delete: writable,
            ..Default::default()
        })
    }

    /// Create a record. An error carrying a code guarantees no record was
    /// created: callers may release references acquired for the attempt.
    /// Failures after a possible commit must have no code. If only
    /// post-write calculations fail, return the committed record instead.
    fn create_record(
        &mut self,
        _layout_id: u32,
        _values: FieldChanges,
    ) -> Result<Record, DataError> {
        Err(DataError::new(
            "This provider does not support creating records.",
        ))
    }

    fn update_record(
        &mut self,
        _layout_id: u32,
        _record_id: i64,
        _values: FieldChanges,
    ) -> Result<Record, DataError> {
        Err(DataError::new(
            "This provider does not support editing records.",
        ))
    }

    fn delete_record(&mut self, _layout_id: u32, _record_id: i64) -> Result<(), DataError> {
        Err(DataError::new(
            "This provider does not support deleting records.",
        ))
    }

    /// Refuse a commit through layout `layout_id` whose records the
    /// account may not change (docs/re/session-identity.md 2.2), before any
    /// of its writes: a deleted portal row's record names no layout of its
    /// own.
    fn require_records_through(&self, _layout_id: u32) -> Result<(), DataError> {
        Ok(())
    }

    /// Delete record `record` of base table `table` (Delete Portal Row's
    /// related record), as [`DataProvider::delete_record`] deletes the
    /// layout's; inside a transaction the deletion is held.
    fn delete_related_record(&mut self, _table: u64, _record: i64) -> Result<(), DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot delete related records.",
        ))
    }

    /// The values the rules of the new record `write` describes would
    /// enter, computed before it is written (#1047): what a window shows of
    /// a row the user began, and what its triggers read, before the commit.
    /// Nothing is written (no op71, op14, op70 or op103), but the serial
    /// numbers generated on creation are reserved now (op72) and stay used
    /// whatever follows. The write naming the preview
    /// ([`ScriptWrite::prepared`]) reuses them, and the clock and UUID read
    /// now. A `write` naming an earlier preview (`ScriptWrite::prepared`)
    /// computes that one again over the cells it gives now (a trigger
    /// changed one, a vetoed commit is retried), with what it drew and
    /// under the same id, reserving nothing. `Ok(None)`: the provider
    /// cannot preview, `write` is no new record, or a value the user typed
    /// does not read as its field's type. The record's validation is left
    /// to its write.
    fn preview_new_record(
        &mut self,
        _write: &ScriptWrite,
    ) -> Result<Option<NewRecordPreview>, DataError> {
        Ok(None)
    }

    /// [`Self::preview_new_record`] over the window's records it has not
    /// saved (#1063): `pending` names every one but the written row (its
    /// own record's typed cells, the other rows begun in its portals, the
    /// portal rows it edited), and the preview reads related records and
    /// lookups through them as the window holds them, instead of leaving
    /// out every value read from another record. A provider that cannot
    /// read them so previews as [`Self::preview_new_record`] does.
    fn preview_new_record_over(
        &mut self,
        write: &ScriptWrite,
        pending: &[PendingRecord],
    ) -> Result<Option<NewRecordPreview>, DataError> {
        let _ = pending;
        self.preview_new_record(write)
    }

    /// The window's own record `write` names (its record, or none for one
    /// not created yet), with its unsaved cells, as its commit would compute
    /// it: its auto-entered values, lookups and stored calculations over
    /// those cells (#1104). Related records and lookups are read through
    /// the window's other unsaved records `others`, as
    /// [`Self::preview_new_record_over`] reads them. Nothing is sent or
    /// reserved. `None` when the provider cannot compute it (another file's
    /// table, a typed value not of its field's type, records it cannot
    /// read as the window holds them).
    fn compute_pending(
        &mut self,
        _write: &ScriptWrite,
        _others: &[PendingRecord],
    ) -> Result<Option<PendingValues>, DataError> {
        Ok(None)
    }

    /// Forget preview `id`, whose record will not be written (the row was
    /// reverted, its window closed) or was saved. Its reserved serial
    /// numbers stay used, a gap, as a reverted record's are; an unknown id
    /// is ignored.
    fn discard_prepared(&mut self, _id: PreparedId) {}

    /// `record.prepare_edit`: availability and preparation for ordinary existing
    /// record edits. Call before changing the first cell, and before adding a
    /// field to the original footprint. Find/global/no-op edits need no receipt.
    /// `RecordCapabilities::prepare_edit` advertises whether Browse must await
    /// this result; providers without this operation leave it false.
    /// An unavailable proof preserves normal editing semantics. A genuine stale
    /// accepted version/lock failure is an error and the caller accepts no edit.
    fn prepare_record_edit(
        &mut self,
        _request: &RecordEditRequest,
    ) -> Result<EditPreparation, DataError> {
        Ok(EditPreparation::Unavailable)
    }

    /// Commit a script's write ([`ScriptWrite`]); inside a transaction the
    /// record is held as [`DataProvider::update_record`] holds it. A
    /// provider without rules of its own writes the layout's fields by
    /// name, as a user's edit; related records and later repetitions are
    /// then error 3.
    fn write_record(&mut self, write: &ScriptWrite) -> Result<Record, DataError> {
        if write.table.is_some() || write.cells.len() > write.fields.len() {
            return Err(DataError::with_code(
                found::code::UNSUPPORTED,
                "This provider cannot write related records or repetitions.",
            ));
        }
        match write.record {
            Some(id) => self.update_record(write.layout, id, write.fields.clone()),
            None => self.create_record(write.layout, write.fields.clone()),
        }
    }

    /// Set Field's own checks of `text` (the value as the step gives it)
    /// entered into `cell` of `table` (a field's id, or `F * 256 + n`):
    /// the rules FileMaker checks as a value is entered (maximum
    /// characters 511, strict data types 500-502, range 503, value list
    /// 506) for a field validated Always, as `rules::RecordPlan::check_entry`
    /// answers them; the field then keeps its value (script-runtime.md
    /// 13.20, 13.21). The open transaction's "Skip data entry validation"
    /// applies. A provider without rules accepts every value.
    fn check_entry(&mut self, _table: u64, _cell: u64, _text: &str) -> Result<(), DataError> {
        Ok(())
    }

    /// Whether this provider implements the import write with import entry
    /// rules. Ordinary record editing alone does not imply this capability.
    fn supports_record_import(&self) -> bool {
        false
    }

    /// Import Records' write of one row (script-steps.md 11.3,
    /// "Implementation notes (import/export)"): `cells` (a field's id for
    /// its first repetition, `F * 256 + n` for repetition n; stored text,
    /// `None` empty) into a new record (`record` `None`) or record `record`
    /// of the layout's table, with the import's entry options
    /// ([`rules::Entry::import`]): any normal field of any type, "only
    /// during data entry" validation not checked, auto-enter only when
    /// `auto_enter` and never over a value the row sets. Inside a
    /// transaction the row is held as those rules left it, and Commit
    /// Transaction writes it so. Returns the record's id.
    fn import_record(
        &mut self,
        _layout_id: u32,
        _record: Option<i64>,
        _cells: &std::collections::BTreeMap<u64, Option<String>>,
        _auto_enter: bool,
    ) -> Result<i64, DataError> {
        Err(DataError::new(
            "This provider does not support importing records.",
        ))
    }

    /// `container.prepare_stored_copy` (Prepare Stored Container Copy): prepare
    /// recovery before a stored reference's first acquisition. Baseline is the
    /// actual accepted destination row; field is its first-repetition
    /// destination, and value retains the
    /// actual source-cell provenance. No count or record write is sent here.
    /// Unavailable preserves ordinary copying without a recovery promise.
    fn prepare_stored_container_edit(
        &mut self,
        _baseline: ContainerEditBaseline,
        _field: &str,
        _value: &calc::Container,
    ) -> Result<EditPreparation, DataError> {
        Ok(EditPreparation::Unavailable)
    }

    /// Prepare an ordinary existing record for attachment entry before acquiring
    /// any container reference. `version` is from the exact accepted record page,
    /// never a later read. Returns true when an ordinary record session now owns
    /// its same-RID edit and can retain acknowledged uploads across reconnect.
    /// False leaves ordinary container entry unchanged without promising recovery.
    /// Missing/stale accepted versions are refused by a versioned provider.
    fn prepare_container_edit(
        &mut self,
        _layout_id: u32,
        _record_id: i64,
        _version: Option<u64>,
        _field: &str,
    ) -> Result<bool, DataError> {
        Ok(false)
    }

    /// Prepare the exact accepted existing Related record before an attachment
    /// acquisition. False promises no recovery; a prepared Root's group refuses
    /// missing/stale versions rather than replacing them with later observations.
    /// `root` supplies its accepted page BEFORE the first Related acquisition
    /// when Related is entered first. It creates no Root field or ownership;
    /// recovery remains unavailable until the later Root upload is acknowledged.
    fn prepare_related_container_edit(
        &mut self,
        _occurrence: u64,
        _table: u64,
        _record: i64,
        _version: Option<u64>,
        _field: u64,
        _root: Option<ContainerEditBaseline>,
    ) -> Result<bool, DataError> {
        Ok(false)
    }

    /// Put container data into field `field` of the layout's table
    /// (containers.md 3, 5.1): the reference text the field then holds,
    /// with one reference to its library member held for it: the data
    /// stored with op88, or op87 +1 for a member of the same table's
    /// library. `None` for a value without data, which the field holds as
    /// its text (script oracle s34).
    fn container_reference(
        &mut self,
        _layout_id: u32,
        _field: &str,
        _value: &calc::Container,
    ) -> Result<Option<String>, DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot store container data.",
        ))
    }

    /// Add `delta` to the reference count of the member a container field's
    /// text `reference` names in the layout's table library (op87, 5.1): +1
    /// for a value copied to another record, -1 for one an edit set and
    /// then replaced or reverted. Text that names no member is left alone.
    fn adjust_container(
        &mut self,
        _layout_id: u32,
        _reference: &str,
        _delta: i32,
    ) -> Result<(), DataError> {
        Ok(())
    }

    /// Acknowledged draft references owned by this window's transaction or
    /// retained recovery receipt, in client table ids. Revert and Commit settle
    /// these themselves; callers must not decrement the same acquisitions.
    fn owned_container_drafts(&self) -> std::collections::BTreeMap<(u64, String), u32> {
        Default::default()
    }

    /// [`Self::container_reference`] for field `field` of base table
    /// `table`, a related record's container (a portal row's): the data
    /// goes into that table's library.
    fn related_container_reference(
        &mut self,
        _table: u64,
        _field: u64,
        _value: &calc::Container,
    ) -> Result<Option<String>, DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot store related container data.",
        ))
    }

    /// [`Self::adjust_container`] in base table `table`'s library.
    fn adjust_related_container(
        &mut self,
        _table: u64,
        _reference: &str,
        _delta: i32,
    ) -> Result<(), DataError> {
        Ok(())
    }

    fn database_schema(&mut self) -> Result<DatabaseSchema, DataError> {
        Err(DataError::new(
            "This provider cannot read the database schema.",
        ))
    }

    /// Open Manage Database: the schema as it can be edited, with what may
    /// be saved in its capabilities. A provider that saves to a host takes
    /// FileMaker's schema lock here, so a second user opening Manage
    /// Database is refused (error 303) until this edit is committed or
    /// cancelled.
    fn begin_schema_edit(&mut self) -> Result<DatabaseSchema, DataError> {
        self.database_schema()
    }

    /// Stage one change on the open edit and return the schema with it.
    /// Nothing is saved until [`DataProvider::commit_schema_changes`].
    fn apply_schema_change(&mut self, _change: SchemaChange) -> Result<DatabaseSchema, DataError> {
        Err(DataError::new(
            "This provider cannot change the database schema.",
        ))
    }

    /// Save the staged changes (Manage Database's OK) and end the edit;
    /// returns the schema as saved.
    fn commit_schema_changes(&mut self) -> Result<DatabaseSchema, DataError> {
        Err(DataError::new(
            "This provider cannot change the database schema.",
        ))
    }

    /// Drop the staged changes and end the edit (Manage Database's
    /// Cancel), releasing what the edit holds.
    fn cancel_schema_changes(&mut self) -> Result<(), DataError> {
        Ok(())
    }

    /// Place a field on a layout ([`layout_placement`]), as the host's
    /// authoring extension does for `om-author`'s `place_field`: planned
    /// against the layout as stored, then saved by the host, which checks
    /// the account, the layout's lock and the revision and writes nothing
    /// in part. Afterwards this provider's layouts are those the host
    /// stores. A provider that cannot edit layouts refuses (code 3).
    fn place_layout_field(
        &mut self,
        _place: layout_placement::LayoutFieldPlacement,
    ) -> Result<layout_placement::PlacedLayoutField, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot edit layouts.",
        ))
    }

    /// Move or resize objects of a layout ([`layout_objects`]), as the
    /// host's authoring extension does for `om-author`'s
    /// `set_object_frames`: planned against the layout as stored, then
    /// saved by the host, which checks the account, the layout's lock and
    /// the revision and writes nothing in part. Afterwards this provider's
    /// layouts are those the host stores. A provider that cannot edit
    /// layouts refuses (code 3).
    fn set_layout_object_frames(
        &mut self,
        _change: layout_objects::LayoutObjectFrames,
    ) -> Result<layout_objects::FramedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot edit layouts.",
        ))
    }

    /// Delete objects of a layout ([`layout_objects`]), as the host's
    /// authoring extension does for `om-author`'s `delete_objects`: planned
    /// against the layout as stored, then saved by the host, which checks
    /// the account, the layout's lock and the revision and writes nothing
    /// in part. Afterwards this provider's layouts are those the host
    /// stores. A provider that cannot edit layouts refuses (code 3).
    fn delete_layout_objects(
        &mut self,
        _change: layout_objects::LayoutObjectDeletion,
    ) -> Result<layout_objects::DeletedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot edit layouts.",
        ))
    }

    /// Add a Form layout with an empty body part ([`layout_catalog`]), as
    /// the host's authoring extension does for `om-author`'s
    /// `create_layout`: the host checks the account, the name, the
    /// occurrence and the catalog's revision, plans the layout into the
    /// catalog as stored and saves it whole or not at all. Afterwards this
    /// provider's layouts, its layout list and its calculation schema are
    /// those the host stores. A provider that cannot add layouts refuses
    /// (code 3).
    fn create_layout(
        &mut self,
        _request: layout_catalog::LayoutRequest,
    ) -> Result<layout_catalog::CreatedLayout, DataError> {
        Err(DataError::with_code(3, "This provider cannot add layouts."))
    }

    /// Copy layout `layout` as FileMaker Pro's Duplicate Layout does
    /// ([`layout_catalog::plan_duplicate`]), as the host's authoring
    /// extension does for `om-author`'s `duplicate_layout`: the host checks
    /// the account, the layout and the catalog's revision and saves the
    /// copy, its index entries and the library's use counts whole or not at
    /// all. Afterwards this provider's layouts are those the host stores. A
    /// provider that cannot add layouts refuses (code 3).
    fn duplicate_layout(
        &mut self,
        _layout: u32,
    ) -> Result<layout_catalog::CreatedLayout, DataError> {
        Err(DataError::with_code(3, "This provider cannot add layouts."))
    }

    /// Delete layout `layout` as FileMaker Pro's Delete Layout does
    /// ([`layout_catalog::plan_delete`]), as the host's authoring extension
    /// does for `om-author`'s `delete_layout`: the host checks the account,
    /// the layout, its lock and the catalog's revision and removes the
    /// layout, its index entries and its library uses whole or not at all.
    /// Afterwards this provider's layouts are those the host stores. A
    /// provider that cannot change layouts refuses (code 3).
    /// Set a layout's name and views as Layout Setup does
    /// ([`layout_catalog::plan_setup`]), as the host's authoring extension
    /// does for `om-author`'s `layout_setup`: the host checks the account,
    /// the layout, its lock and its revision and writes its member and the
    /// index whole or not at all. Afterwards this provider's layouts are
    /// those the host stores. A provider that cannot change layouts refuses
    /// (code 3).
    /// Set a layout's width ([`layout_placement::plan_width`]), as the
    /// host's authoring extension does for `om-author`'s
    /// `set_layout_width`: the host checks the account, the layout, its lock
    /// and its revision. Afterwards this provider's layouts are those the
    /// host stores. A provider that cannot change layouts refuses (code 3).
    /// Show a field object with another control
    /// ([`layout_objects::plan_control`]), as the host's authoring extension
    /// does for `om-author`'s `set_field_control`: the host checks the
    /// account, the layout, its lock and its revision, the field and the
    /// value list. Afterwards this provider's layouts are those the host
    /// stores. A provider that cannot change layouts refuses (code 3).
    fn set_field_control(
        &mut self,
        _change: layout_objects::FieldControlChange,
    ) -> Result<layout_objects::FramedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change layouts.",
        ))
    }

    /// A text object's words, as FileMaker Pro's Layout mode retypes them
    /// (`layout_placement::plan_words`, authoring operation 20): saved by the
    /// host, behind the layout's exclusive lock, if the layout is at the
    /// revision the change was made on.
    fn set_layout_text(
        &mut self,
        _change: layout_placement::TextChange,
    ) -> Result<layout_objects::FramedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change layouts.",
        ))
    }

    /// Part Setup's options for one part of a layout, as FileMaker Pro's
    /// Part Definition sets them (`layout_placement::plan_part_options`,
    /// authoring operation 19): saved by the host, behind the layout's
    /// exclusive lock, if the layout is at the revision the change was made
    /// on.
    fn set_part_options(
        &mut self,
        _change: layout_placement::PartOptionsChange,
    ) -> Result<layout_objects::FramedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change layouts.",
        ))
    }

    /// Add, delete or resize a part of a layout, as FileMaker Pro's Part
    /// Setup and a dragged part boundary do (`layout_placement::plan_parts`,
    /// authoring operation 22): saved by the host, behind the layout's
    /// exclusive lock, if the layout is at the revision the change was made
    /// on.
    fn set_layout_parts(
        &mut self,
        _change: layout_placement::PartsChange,
    ) -> Result<layout_objects::FramedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change layouts.",
        ))
    }

    fn set_layout_width(
        &mut self,
        _change: layout_placement::LayoutWidthChange,
    ) -> Result<layout_objects::FramedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change layouts.",
        ))
    }

    fn set_layout_setup(
        &mut self,
        _change: layout_catalog::LayoutSetupChange,
    ) -> Result<layout_objects::FramedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change layouts.",
        ))
    }

    fn delete_layout(&mut self, _layout: u32) -> Result<layout_catalog::RemovedLayout, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot delete layouts.",
        ))
    }

    /// Place a text on a layout's body on its own, as the Text tool does
    /// ([`layout_placement::plan_text`]), as
    /// [`DataProvider::place_layout_field`] places a field: planned against
    /// the layout as stored, then saved by the host. Afterwards this
    /// provider's layouts are those the host stores. A provider that cannot
    /// edit layouts refuses (code 3).
    fn place_layout_text(
        &mut self,
        _place: layout_placement::LayoutTextPlacement,
    ) -> Result<layout_placement::PlacedLayoutText, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot edit layouts.",
        ))
    }

    /// Place a portal on a layout's body, as FileMaker Pro's Portal tool,
    /// Portal Setup and Add Fields to Portal do
    /// ([`layout_placement::plan_portal`], authoring operation 21), as
    /// [`DataProvider::place_layout_field`] places a field: planned against
    /// the layout as stored, then saved by the host. A provider that cannot
    /// edit layouts refuses (code 3).
    fn place_layout_portal(
        &mut self,
        _place: layout_placement::LayoutPortalPlacement,
    ) -> Result<layout_placement::PlacedLayoutPortal, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot edit layouts.",
        ))
    }

    /// Place a button that performs a script on a layout
    /// ([`layout_placement::plan_button`], #226), as
    /// [`DataProvider::place_layout_field`] places a field: planned against
    /// the layout as stored, then saved by the host, which checks the
    /// script, the account, the layout's lock and the revision. Afterwards
    /// this provider's layouts are those the host stores. A provider that
    /// cannot edit layouts refuses (code 3).
    fn place_layout_button(
        &mut self,
        _place: layout_placement::LayoutButtonPlacement,
    ) -> Result<layout_placement::PlacedLayoutButton, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot edit layouts.",
        ))
    }

    /// The open file's accounts, privilege sets and extended privileges
    /// (Manage Security), with no password or other credential. Refused
    /// unless the signed-in account is the file's own, in its built-in
    /// [Full Access] set.
    fn security_catalog(&mut self) -> Result<security_catalog::SecurityCatalog, DataError> {
        Err(DataError::new(
            "This provider cannot read the file's security.",
        ))
    }
    /// The open file's schema for calculations (fields, relationships,
    /// custom functions), `None` when this provider cannot describe it.
    fn calc_schema(&mut self) -> Result<Option<std::sync::Arc<host::CalcSchema>>, DataError> {
        Ok(None)
    }

    /// Refresh Window: invalidate cached record reads without committing or
    /// reverting edits, changing found sets/sorts, or reloading the schema.
    /// `flush_joins` also discards relationship query results (including
    /// lookup candidates). `flush_external` concerns related ODBC data only,
    /// not hosted FileMaker peer files. A provider without ODBC sources
    /// succeeds harmlessly; an unsupported ODBC backend must report its limit.
    ///
    /// The default suits uncached providers. Providers retaining calculation
    /// data must override this and advance their cache generation.
    fn refresh_window(
        &mut self,
        _flush_joins: bool,
        _flush_external: bool,
    ) -> Result<(), DataError> {
        Ok(())
    }

    /// Refresh Object/Portal's calculation inputs without a write. A portal
    /// supplies (source occurrence, source record, target occurrence) in the
    /// hydrated schema's namespace. Return the new cache generation when the
    /// provider has one so the requesting window can retain unrelated results.
    fn refresh_object(&mut self, join: Option<(u64, u64, u64)>) -> Result<Option<u64>, DataError> {
        self.refresh_window(join.is_some(), false)?;
        Ok(None)
    }

    /// The generation of the data this provider reads for calculations
    /// ([`host::CalcData::generation`]) after its last call. A record page
    /// read under the generation a window's calculation data has leaves
    /// that data valid, so the window keeps it (#1042); `None`, the
    /// default, makes every page read drop it.
    fn calc_generation(&self) -> Option<u64> {
        None
    }

    /// Data for calculations a client evaluates: each need, with the
    /// records a related set names.
    fn calc_data(&mut self, _needs: &[host::CalcNeed]) -> Result<host::CalcData, DataError> {
        Err(DataError::new(
            "This provider does not supply data for calculations.",
        ))
    }

    /// [`DataProvider::calc_data`] for a window that may stop needing it
    /// (#987): once `cancel` is cancelled, the provider stops between
    /// bounded network operations and answers
    /// [`host::CalcData::superseded`], nothing partial, without falling
    /// back to reading need by need. A provider that cannot stop answers in
    /// full (the default).
    fn calc_data_for(
        &mut self,
        needs: &[host::CalcNeed],
        cancel: &host::CalcCancel,
    ) -> Result<host::CalcData, DataError> {
        if cancel.cancelled() {
            return Ok(host::CalcData::superseded());
        }
        self.calc_data(needs)
    }

    /// Explicit sample providers identify themselves in the window chrome.
    fn is_sample(&self) -> bool {
        false
    }

    /// `Get ( HostApplicationVersion )`: the host's product and version as
    /// it names them when the connection is made (`Server 26.0.2`, README
    /// F14; the Claris reference's `Server 26.0.1`). `None` for a file that
    /// is not hosted or a provider that does not know.
    fn host_application_version(&self) -> Option<String> {
        None
    }

    /// The file's styled layout for Browse rendering, when available.
    fn render_layout(&mut self, _layout_id: u32) -> Result<Option<RenderLayout>, DataError> {
        Ok(None)
    }

    /// List hosted files on a server (before opening one). May be empty if the
    /// provider is already scoped to a single file.
    fn list_files(&mut self, host: &str, port: u16) -> Result<Vec<HostedFile>, DataError>;

    /// Lists hosts' files without this provider: on another thread, over
    /// connections of the browser's own, so a listing neither waits for nor
    /// touches the open file. `None` (the default): list with
    /// [`DataProvider::list_files`].
    fn host_browser(&self) -> Option<std::sync::Arc<dyn HostBrowser>> {
        None
    }

    /// The stored external FileMaker sources of this file, in their native ids.
    fn database_sources(&mut self) -> Result<Vec<file_refs::FileDataSource>, DataError> {
        Ok(Vec::new())
    }

    /// The open file's value lists, as File > Manage > Value Lists and the
    /// authoring service's `get_value_lists` show them ([`value_lists`]).
    /// A provider that cannot read them says so (error 3) rather than
    /// answering an empty catalog.
    fn value_list_catalog(&mut self) -> Result<value_lists::ValueListCatalog, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot read the file's value lists.",
        ))
    }

    /// Custom value list `list` as Edit Value List opens it: its name,
    /// values and the revision of its member as stored now (#869).
    /// Refused for a list whose values do not come from custom values.
    fn custom_value_list(&mut self, _list: u64) -> Result<value_lists::CustomList, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change value lists.",
        ))
    }

    /// Edit Value List's OK (`value_lists::plan_values`, authoring
    /// operation 24): the list's custom values, saved by the host behind
    /// the catalog's locks if the list is at the revision the dialog opened
    /// on. Afterwards this provider's value lists are those the host
    /// stores. A provider that cannot change value lists refuses (code 3).
    fn set_value_list_values(
        &mut self,
        _change: value_lists::ValuesChange,
    ) -> Result<layout_objects::FramedObjects, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change value lists.",
        ))
    }

    /// Open a separate desktop file session on this provider's current host,
    /// or for a file on this computer beside it ([`Self::local_peer_path`]).
    /// A file on another host or port is named `fmnet:/host:port/Name`
    /// ([`file_refs::fmnet_name`]): it is offered this file's account once,
    /// and after a refusal there opens only with an account typed for it
    /// ([`Self::open_peer_as`]); [`Self::peer_refused`] then asks for one.
    /// Implementations inherit its connection identity, license and credentials;
    /// they must not replace this provider or reuse its transaction/window state.
    fn open_peer(&mut self, _file: &str) -> Result<Box<dyn DataProvider>, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot open another database.",
        ))
    }

    /// [`Self::open_peer`] with `account` and `password` instead of the
    /// ones this file was opened with: a sign-in to a file that refused
    /// them (#214). One Login, none for a pair it refused before in this
    /// session.
    fn open_peer_as(
        &mut self,
        _file: &str,
        _account: &str,
        _password: &str,
    ) -> Result<Box<dyn DataProvider>, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot open a file with another account.",
        ))
    }

    /// Whether `file`, as [`Self::open_peer`] names it, refused `credentials`
    /// (or, without them, the account and password this file was opened
    /// with) in this session: its open failed with that refusal.
    fn peer_refused(&self, _file: &str, _credentials: Option<(&str, &str)>) -> bool {
        false
    }

    /// Whether this file's relationships keep `file` open as an external
    /// data source. Desktop Close File refuses those sources before saves.
    fn references_peer(&mut self, _file: &str) -> Result<bool, DataError> {
        Ok(false)
    }

    /// The path (links followed) of the open file, when it is one this
    /// provider opened from this computer ([`Self::open_local`]): what
    /// tells two open files apart, whatever names reached them.
    fn local_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Where `file`, a plain file name a data source or Open File gives,
    /// is when the open file is one on this computer: beside it, in its
    /// folder, as [`Self::open_peer`] opens it. `None` for a hosted file
    /// (peers are on its host) or a name that is not plain.
    fn local_peer_path(&self, _file: &str) -> Option<std::path::PathBuf> {
        None
    }

    /// The authenticated account of this file's independent session.
    fn session_account(&self) -> Option<String> {
        None
    }

    /// The connected endpoint's port, for same-host file path validation.
    fn server_port(&self) -> u16 {
        5003
    }

    /// Open and authenticate to a file.
    fn open(&mut self, connection: &Connection) -> Result<(), DataError>;

    /// Open the file this provider had open again after its connection
    /// was lost, as the user's reconnect asks: a fresh connection, login
    /// and catalog, as [`Self::open`] makes, keeping what the client holds
    /// of the file's windows (their found sets and sorts) where the
    /// provider has any. Nothing of the lost session is replayed (no
    /// transaction, lock or write). A failure leaves no file open; a later
    /// attempt may still carry the windows' found sets.
    ///
    /// The password serves this attempt only: an implementation keeps no
    /// copy of it once this returns, whatever [`Self::open`] keeps for the
    /// file's later logins (related files are opened within the attempt;
    /// a file a script opens afterwards needs the file opened again).
    fn reopen(&mut self, connection: &Connection) -> Result<(), DataError> {
        self.open(connection)
    }

    /// Record `record_id` of the layout's table as the host holds it now,
    /// whatever the window's found set: a page of that one record, its
    /// values as [`Self::records_with_calculations`] gives them with no
    /// calculations, and no record when the table no longer has it. A
    /// reconnected window reads the records it holds unsaved edits of
    /// with it, to compare them with what the edits started from.
    ///
    /// A provider with record versions keeps the version this read for the
    /// selected window ([`Self::use_window`]) as the one its save is checked against, whatever is read of the
    /// record meanwhile (pages, scripts), until the record is written, read
    /// by id again, or released ([`Self::release_record`]): no later read
    /// can stand in for the check of the edits.
    fn record_by_id(&mut self, _layout_id: u32, _record_id: i64) -> Result<RecordPage, DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot read a record by its id.",
        ))
    }

    /// The selected window began unsaved edits of record `record_id`: a
    /// provider with record versions keeps `version` from the exact page
    /// the window accepted and displayed as the one their save is checked against, as
    /// [`Self::record_by_id`] keeps its own, whatever is read of the record
    /// meanwhile, until the record is written, read by id again or released
    /// ([`Self::release_record`]). Another window's reads or another
    /// client's changes then refuse the save (FileMaker's 306) rather than
    /// let it replace values the window never showed.
    /// A versioned provider refuses saving a draft with no accepted version
    /// until the record has been read back and the user has checked it.
    fn retain_record(&mut self, _layout_id: u32, _record_id: i64, _version: Option<u64>) {}

    /// Give up only the selected window's version [`Self::record_by_id`] or
    /// [`Self::retain_record`] kept for record `record_id`: later reads of
    /// the record set the version a save is checked against again. The
    /// window releases a record whose edits it no longer keeps.
    fn release_record(&mut self, _layout_id: u32, _record_id: i64) {}

    /// [`Self::retain_record`] for record `record` of base table `table`, a
    /// related record the window's user edited in Browse (a portal row's,
    /// or a related field's first record): its save, held in a transaction
    /// or not, is checked against `version`, the modification count the
    /// window showed, until [`Self::release_related_record`]. A version the
    /// window already keeps for the record stays; `true` when this one is
    /// kept instead.
    fn retain_related_record(&mut self, _table: u64, _record: i64, _version: u64) -> bool {
        false
    }

    /// Give up what [`Self::retain_related_record`] kept.
    fn release_related_record(&mut self, _table: u64, _record: i64) {}

    /// The window closes showing the layout with `record` current: a local
    /// file keeps the window's found set of the layout's table and that
    /// record, when the window wrote a record of the file
    /// (file-metadata.md 11). Nothing is told; the default keeps nothing.
    fn keep_found_set(&mut self, _layout_id: u32, _record: Option<i64>) {}

    /// Where the record a local file was closed on is in the window's found
    /// set of the layout's table (zero-based): FileMaker Pro saves it with
    /// the found set (file-metadata.md 11). Asking restores the table's
    /// saved found set when the window has not read the table yet; the
    /// position is given once per window and table, `None` otherwise.
    fn take_saved_position(&mut self, _layout_id: u32) -> Option<u64> {
        None
    }

    /// Where record `record_id` is in the window's found set of the
    /// layout's table (zero-based), `None` when it is not there (omitted
    /// or deleted since). A reconnected window finds the record it showed
    /// with it.
    fn record_position(
        &mut self,
        _layout_id: u32,
        _record_id: i64,
    ) -> Result<Option<u64>, DataError> {
        Err(DataError::with_code(
            found::code::UNSUPPORTED,
            "This provider cannot locate a record in the found set.",
        ))
    }

    /// Reopen the same local file after a lost session, preserving its owned
    /// engine/file lock and the windows' found sets. Credentials serve only
    /// this attempt; no client license is read or sent.
    /// After another file of this session reconnected the connection this
    /// file lost, reopen this file on that new connection: only when this
    /// file is on the same host and port and was opened with the same
    /// account and password, which it uses again, so the user types
    /// nothing more (#1011 after a loss). `Ok(true)` reopened, keeping its
    /// windows' found sets as [`Self::reopen`] does; `Ok(false)` when there
    /// is no such connection (another host or account, nothing
    /// reconnected): the file stays lost for its own Reconnect. A refusal
    /// is an error, and the file stays lost; nothing is tried again.
    fn reopen_on_successor(&mut self) -> Result<bool, DataError> {
        Ok(false)
    }

    fn reopen_local(&mut self, _file: &LocalFile) -> Result<(), DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot reconnect a local file.",
        ))
    }

    /// Whether the provider opens `.fmp12` files on this computer itself,
    /// without a server ([`DataProvider::open_local`]).
    fn supports_local_files(&self) -> bool {
        false
    }

    /// Open a file on this computer, as [`DataProvider::open`] opens a
    /// hosted one: the file is authenticated against its own accounts, and
    /// changes are saved to it. No client license is used. Without
    /// credentials the answer is [`LocalOpen::NeedsLogin`] once the file
    /// has been read; wrong credentials are an error with FileMaker's code
    /// 212, an account the provider does not admit (3, 9) another, and
    /// either way the file waits for another account.
    fn open_local(&mut self, _file: &LocalFile) -> Result<LocalOpen, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot open files on this computer.",
        ))
    }

    /// The layouts in the open file.
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError>;

    /// Full menu catalog, including folder delimiters and separators.
    fn layout_menu(&mut self) -> Result<Vec<LayoutMenuEntry>, DataError> {
        self.layouts().map(|layouts| {
            layouts
                .into_iter()
                .map(|l| LayoutMenuEntry {
                    id: l.id,
                    name: l.name,
                    flags: 0,
                })
                .collect()
        })
    }

    /// A page of records for a layout.
    fn records(&mut self, layout_id: u32, offset: u64, limit: u32)
        -> Result<RecordPage, DataError>;

    /// A page's stored values, evaluating only the named calculated,
    /// summary or global fields. An empty list leaves those values for
    /// the client's calculation host to read on demand, so opening a form
    /// does not summarize the entire found set before showing a record.
    /// Providers without a separate calculation source may return their
    /// usual complete page.
    fn records_with_calculations(
        &mut self,
        layout_id: u32,
        offset: u64,
        limit: u32,
        _fields: &[String],
    ) -> Result<RecordPage, DataError> {
        self.records(layout_id, offset, limit)
    }

    /// [`Self::records_with_calculations`], with the size of the window's
    /// found set of the layout's table and whether it is sorted, as read
    /// with the page: the status toolbar's Found or Total. `None` when the
    /// provider does not tell.
    fn records_with_summary(
        &mut self,
        layout_id: u32,
        offset: u64,
        limit: u32,
        fields: &[String],
    ) -> Result<(RecordPage, Option<found::FoundSet>), DataError> {
        Ok((
            self.records_with_calculations(layout_id, offset, limit, fields)?,
            None,
        ))
    }

    /// A layout's geometry, for drawing it. `Ok(None)` when the provider
    /// cannot describe the layout; the GUI then builds a generic view from
    /// the field list. The default has no geometry for any layout.
    fn layout_geometry(&mut self, _layout_id: u32) -> Result<Option<LayoutGeometry>, DataError> {
        Ok(None)
    }

    /// Why the open file's connection to the host was lost, once it is (an
    /// error with [`DataError::lost`] set); `None` while it works or when no
    /// file is open. Requests on a lost connection fail without reaching the
    /// host. Cheap: callers poll it while idle.
    fn connection_lost(&self) -> Option<DataError> {
        None
    }

    /// The windows whose record sessions a reconnect of this file carries
    /// to the new connection (#152): new records with no submitted data,
    /// container reference or commit begun, including a provider-verified
    /// group of related scalar drafts after an all-file no-data refusal.
    /// Related groups revalidate every file and existing version before
    /// reserving any new id. Their new records are held under ids reserved
    /// then, their values as they were ([`Self::take_recreated_records`]).
    /// A window with a new record not among them loses it with the
    /// connection. The default: none.
    fn carried_record_sessions(&self) -> Vec<u32> {
        Vec::new()
    }

    /// What the last reconnect (or [`Self::recreate_record`]) did with the
    /// new records it carried ([`Self::carried_record_sessions`]), each
    /// once: the id it holds the record under now, or why it could not. A
    /// window's records that could not all be held again are kept as they
    /// were, under the lost connection's ids, its values shown and its
    /// writes refused (3), until [`Self::recreate_record`] or
    /// [`Self::discard_unrecreated`]; a later reconnect carries them too.
    fn take_recreated_records(&mut self) -> Vec<RecreatedRecord> {
        Vec::new()
    }

    /// Whether `record` of related table `table` is a new record the open
    /// record session holds, or set aside when the commit that reserved it
    /// was refused before any record data (#1095): the window's next write
    /// of that row names it, rather than make another under a new id. The
    /// default: none is.
    fn related_record_reserved(&self, table: u64, record: i64) -> bool {
        let _ = (table, record);
        false
    }

    /// Whether preview `id` (`preview_new_record`) outlives a reconnect,
    /// what it issued (serial numbers, clock, UUID) kept for its record's
    /// save (#152). A row begun with one that does not would be computed
    /// anew: a reconnect does not carry its window's new record. The
    /// default: none does.
    fn preview_survives_reconnect(&self, id: PreparedId) -> bool {
        let _ = id;
        false
    }

    /// Try again to hold the current window's kept new records on the
    /// connection (`take_recreated_records` says what came of it): op71
    /// reserves new ids, and nothing is computed or reserved again.
    /// Refused (3) without any.
    fn recreate_record(&mut self) -> Result<(), DataError> {
        Err(DataError::with_code(
            3,
            "This window has no new record to make again.",
        ))
    }

    /// Discard the current window's kept new records: they were never
    /// submitted, so nothing on the host changes; their serial numbers
    /// stay used, a gap. Refused (3) without any.
    fn discard_unrecreated(&mut self) -> Result<(), DataError> {
        Err(DataError::with_code(
            3,
            "This window has no new record to discard.",
        ))
    }

    /// A reading of the record data this provider's writes have sent its
    /// hosts so far, through every file and connection they reach,
    /// another file's included (#152). Two readings on the same thread
    /// around a save tell whether it sent any ([`SubmissionMark::until`]).
    /// `None` (the default): the provider cannot tell, and a save that
    /// failed is taken to have reached its host.
    fn submission_mark(&self) -> Option<SubmissionMark> {
        None
    }

    /// What the user should be told about the open file's related files
    /// (another file's tables its occurrences show): the files that could
    /// not be opened and why, and tables they cannot read. Their fields then
    /// read as missing. Known once [`Self::calc_schema`] answered; none by
    /// default.
    fn related_file_warnings(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether the calculation schema grew since this was last asked: a
    /// provider opening related files when something first reads them
    /// opened some, and [`Self::calc_schema`] now binds more occurrences.
    /// Ids the earlier schema gave stay; what was read under them stays
    /// valid. `false` by default.
    fn take_grown_schema(&mut self) -> bool {
        false
    }

    /// Change the password of the account this file is open with, from
    /// `old` to `new` (File > Change Password…, #830), as FileMaker's
    /// `SetPassword` does: a local file's own host writes it. Refused with
    /// FileMaker's codes (212 for a wrong old password, 206, 208, 209, 11;
    /// 3 for a file on a host).
    fn change_password(&mut self, _old: &str, _new: &str) -> Result<(), DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change a password.",
        ))
    }

    /// [`Self::open_local`] without credentials, asking for an account even
    /// when the file's File Options "Log in using" names one: as FileMaker
    /// Pro asks when Option (macOS) or Shift (Windows) is held at open
    /// (local-files.md 3.1).
    fn open_local_asking(&mut self, path: &std::path::Path) -> Result<LocalOpen, DataError> {
        self.open_local(&LocalFile {
            path: path.to_path_buf(),
            credentials: None,
        })
    }

    /// Set a new password for the account a local file's Login just
    /// refused because its password must be changed first (211), and open
    /// the file with it (#157): the file's own host writes it as FileMaker
    /// does. Refused with FileMaker's codes (206, 208, 209, 11), the
    /// account still waiting for another; without such a refusal, 3.
    fn change_due_password(&mut self, _new: &str) -> Result<LocalOpen, DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot change a password.",
        ))
    }

    /// The related files that refused the account and password the file
    /// was opened with and may be opened with another (#214). Known once
    /// [`Self::calc_schema`] answered; none by default.
    fn related_sign_ins(&self) -> Vec<RelatedSignIn> {
        Vec::new()
    }

    /// Open related file `file`, named by [`Self::related_sign_ins`],
    /// with `account` and `password` (#214): one Login, or none when it
    /// refused that pair before in this session. On success the
    /// calculation schema is built again with the file's tables
    /// ([`Self::calc_schema`] answers it); a refusal is the file's own (212
    /// on a host).
    fn sign_in_related(
        &mut self,
        _file: &str,
        _account: &str,
        _password: &str,
    ) -> Result<(), DataError> {
        Err(DataError::with_code(
            3,
            "This provider cannot open a related file with another account.",
        ))
    }

    /// The Closes queued for this provider and not handled yet: work of
    /// the provider that may take long, such as opening a file's related
    /// files, stops early when one waits, instead of holding the
    /// provider's thread until it ends. `None` (the default) for a provider
    /// without such work.
    fn close_requests(&self) -> Option<CloseRequests> {
        None
    }

    /// Close the connection. Idempotent.
    fn close(&mut self);
}

pub mod mock;
mod mock_calc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connections_debug_form_leaves_the_password_out() {
        let connection = Connection {
            host: "127.0.0.1".into(),
            port: 5003,
            file: "Tasks.fmp12".into(),
            account: "Admin".into(),
            password: "correct horse".into(),
        };
        let shown = format!("{connection:?}");
        assert!(!shown.contains("correct horse"), "{shown}");
        assert!(
            shown.contains("Admin") && shown.contains("<redacted>"),
            "{shown}"
        );
    }
}
