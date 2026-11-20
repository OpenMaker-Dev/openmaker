//! Runs the [`DataProvider`] on its own thread so provider calls (which may
//! block on the network) never stall the egui update loop.
//!
//! The UI sends [`Request`]s tagged with an epoch and drains [`Response`]s each
//! frame. The worker asks egui to repaint after every reply so the UI wakes up
//! without polling. A panicking provider is contained and reported as a
//! [`DataError`] instead of taking the app down.
//!
//! While it waits for requests, and while scripts run, the worker asks the
//! provider whether the open file's connection was lost (its keepalive
//! notices while the window is idle) and tells the UI once, with
//! [`Reply::ConnectionLost`], waking it; errors of calls made after the
//! loss carry [`DataError::lost`].

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::licensing::{LicenseKey, LicenseStore};
use crate::session::PageKey;

use std::sync::Arc;

use om_data::host::{CalcData, CalcNeed, CalcSchema};
use om_data::value_lists::ValueListCatalog;
use om_data::{
    Connection, DataError, DataProvider, DatabaseSchema, FieldChanges, HostBrowser, HostListing,
    LayoutGeometry, LayoutInfo, ListingError, ListingFailure, LocalFile, LocalOpen, Record,
    RecordCapabilities, RecordPage, RenderLayout, SchemaChange, Script, ScriptCatalog,
};

/// A provider call the UI wants made.
#[derive(Debug)]
pub(crate) enum Request {
    /// List a host's files with the provider, which has no
    /// [`HostBrowser`] ([`Worker::list_files`]).
    ListFiles {
        token: u64,
        host: String,
        port: u16,
    },
    Open {
        connection: Connection,
        use_saved_license: bool,
    },
    /// Open a file on this computer. No client license is read or sent.
    OpenLocal(LocalFile),
    /// Open a file on this computer asking for an account, whatever its
    /// File Options say (`DataProvider::open_local_asking`); answered as
    /// [`Request::OpenLocal`] is.
    OpenLocalAsking(std::path::PathBuf),
    LicenseStatus,
    SaveLicense(LicenseKey),
    RemoveLicense,
    Layouts,
    /// The file's scripts: for the Script Workspace that asked when
    /// `workspace` names its request, else for the file's windows only.
    Scripts {
        workspace: Option<u64>,
    },
    /// The Script Workspace's request `workspace` (each of its requests
    /// has its own, which the reply names).
    SaveScript {
        workspace: u64,
        script: Script,
    },
    SaveScriptCatalog {
        workspace: u64,
        catalog: ScriptCatalog,
    },
    /// Open Manage Database: begin a schema edit (`begin_schema_edit`).
    BeginSchemaEdit,
    /// Stage a change on the open edit.
    ChangeSchema(SchemaChange),
    /// Manage Database's OK: save the staged changes.
    CommitSchema,
    /// Manage Database's Cancel: drop them and release the edit.
    CancelSchema,
    /// Layout mode's Save Layout of a field placed with the Field tool.
    PlaceLayoutField(om_data::layout_placement::LayoutFieldPlacement),
    /// Save Layout: move or resize objects
    /// (`DataProvider::set_layout_object_frames`).
    SetObjectFrames(om_data::layout_objects::LayoutObjectFrames),
    /// Save Layout: delete objects (`DataProvider::delete_layout_objects`).
    DeleteObjects(om_data::layout_objects::LayoutObjectDeletion),
    /// New Layout/Report: add a layout (`DataProvider::create_layout`).
    CreateLayout(om_data::layout_catalog::LayoutRequest),
    /// Duplicate Layout: copy a layout (`DataProvider::duplicate_layout`).
    DuplicateLayout(u32),
    /// Delete Layout: delete a layout (`DataProvider::delete_layout`).
    DeleteLayout(u32),
    LayoutSetup(om_data::layout_catalog::LayoutSetupChange),
    /// Edit…: a custom value list as Edit Value List opens it
    /// (`DataProvider::custom_value_list`).
    CustomValueList(u64),
    /// Edit Value List's OK (`DataProvider::set_value_list_values`).
    SetValueListValues(om_data::value_lists::ValuesChange),
    /// Part Setup's Change: a part's page options
    /// (`DataProvider::set_part_options`).
    SetPartOptions(om_data::layout_placement::PartOptionsChange),
    /// Part Setup's Create and Delete, and a dragged part boundary
    /// (`DataProvider::set_layout_parts`).
    SetLayoutParts(om_data::layout_placement::PartsChange),
    SetLayoutWidth(om_data::layout_placement::LayoutWidthChange),
    /// Save Layout: show a field object with another control
    /// (`DataProvider::set_field_control`).
    SetFieldControl(om_data::layout_objects::FieldControlChange),
    /// Save Layout: a text object's new words
    /// (`DataProvider::set_layout_text`).
    SetLayoutText(om_data::layout_placement::TextChange),
    PlaceLayoutButton(om_data::layout_placement::LayoutButtonPlacement),
    /// Save Layout: place a portal (`DataProvider::place_layout_portal`).
    PlaceLayoutPortal(om_data::layout_placement::LayoutPortalPlacement),
    /// Save Layout: place a text on its own
    /// (`DataProvider::place_layout_text`).
    PlaceLayoutText(om_data::layout_placement::LayoutTextPlacement),
    SecurityCatalog {
        token: u64,
    },
    /// File > Manage > Value Lists' catalog, for the dialog's request
    /// `token`.
    ValueLists {
        token: u64,
    },
    RenderLayout {
        layout_id: u32,
    },
    LayoutGeometry {
        layout_id: u32,
    },
    RecordCapabilities {
        layout_id: u32,
    },
    /// Where the window starts on the layout's table: the record a local
    /// file was closed on (`DataProvider::take_saved_position`). Asked
    /// before the layout's other requests, so the window is on it before
    /// its triggers run.
    SavedPosition {
        layout_id: u32,
    },
    Records(PageKey),
    PrepareRecordEdit(u64, om_data::RecordEditRequest),
    UpdateRecord {
        layout_id: u32,
        record_id: i64,
        values: FieldChanges,
    },
    DeleteRecord {
        layout_id: u32,
        record_id: i64,
    },
    /// The file's calculation schema.
    CalcSchema,
    /// Data calculations found missing, stopped when `cancel` is (#987).
    CalcData {
        needs: Vec<CalcNeed>,
        cancel: om_data::host::CalcCancel,
    },
    /// Value list `list`'s values as the host lists them, from field
    /// `field` of base table `table` (`DataProvider::host_field_values`).
    HostFieldValues {
        list: u64,
        table: u64,
        field: u64,
        /// `Calculations::take_list_wants`' epoch, returned with the reply.
        epoch: u64,
    },
    /// Build a field's value list from committed data (`list_build`,
    /// #745), reading through the provider: never in paint.
    BuildValueList {
        /// The list, its ticket (returned with the reply), and the window's
        /// date and calculation session (`Calculations::take_list_builds`).
        build: Box<crate::calc::ListBuild>,
        /// The window's reads (#987): stopped, the list is not answered.
        cancel: om_data::host::CalcCancel,
    },
    /// Run a script on this thread (`scripting`).
    Script(Box<crate::scripting::ScriptJob>),
    /// The user's answer to a script's prompt (or pause).
    Answer {
        prompt: u64,
        answer: crate::scripting::Answer,
    },
    /// Open the window's file again after its connection was lost.
    Reconnect(Box<ReconnectJob>),
    /// The window's new record a reconnect could not make again (#152):
    /// try again (`retry`) or discard it.
    Recreate {
        retry: bool,
    },
    /// Open a related file that refused the window's account with another
    /// (`DataProvider::sign_in_related`, #214).
    SignInRelated(Box<SignInJob>),
    /// A new password for the account the local file's Login refused with
    /// 211, then the file opened with it (`DataProvider::change_due_password`,
    /// #157); answered as [`Request::OpenLocal`] is.
    ChangeDuePassword(Box<NewPassword>),
    /// File > Change Password…: the open file's account's password from
    /// the old to the new one (`DataProvider::change_password`, #830).
    ChangePassword(Box<PasswordPair>),
    /// Settle the window's partly applied save (#1006): check it again,
    /// resume it, discard what it did not apply, or only report it.
    SettleSave(Settle),
    /// Read these records again by layout and id, after a reconnect
    /// (`DataProvider::record_by_id`), and these related records by base
    /// table and id, the window's related edits of the records held
    /// ([`read_related`], #152).
    Recheck {
        records: Vec<(u32, i64)>,
        related: Vec<(u64, i64)>,
    },
    /// The window began unsaved edits of these records: their saves answer
    /// to the versions it showed (`DataProvider::retain_record`).
    Retain(Vec<(u32, i64, Option<u64>)>),
    /// The window keeps no edits of these records any more
    /// (`DataProvider::release_record`).
    ReleaseRecords(Vec<(u32, i64)>),
    /// The window closes showing the layout with the record current: a
    /// local file keeps its found set (`DataProvider::keep_found_set`).
    KeepFoundSet {
        layout_id: u32,
        record: Option<i64>,
    },
    Close,
}

/// The user's reconnect of a file whose connection was lost. The file is
/// the window's; the worker reconnects it where it was opened, with the
/// account it was opened with.
pub(crate) struct ReconnectJob {
    /// Typed for this attempt only; never kept or shown.
    pub password: zeroize::Zeroizing<String>,
    pub use_saved_license: bool,
    /// The records to read back once the file is open again, by layout
    /// and id: those the file's windows hold unsaved edits of.
    pub records: Vec<(u32, u32, i64)>,
    /// Each window's current record, by window, layout and id, to find
    /// in the window's found set.
    pub current: Vec<(u32, u32, i64)>,
    /// The related records the windows hold unsaved edits of, by window,
    /// base table and id, to read back on the new session (#152).
    pub related: Vec<(u32, u64, i64)>,
    /// Other files whose windows hold changes a reconnect cannot keep, as
    /// the windows know them: they are not reopened with this one and
    /// stay lost for their own Reconnect (#152).
    pub keep_lost: Vec<u64>,
}

/// The old and new passwords typed for [`Request::ChangePassword`]. Its
/// `Debug` leaves them out.
pub(crate) struct PasswordPair {
    pub old: zeroize::Zeroizing<String>,
    pub new: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for PasswordPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PasswordPair(<redacted>)")
    }
}

/// A new password typed for [`Request::ChangeDuePassword`]. Its `Debug`
/// leaves it out.
pub(crate) struct NewPassword(pub zeroize::Zeroizing<String>);

impl std::fmt::Debug for NewPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NewPassword(<redacted>)")
    }
}

/// The account a related file that refused the window's is to be opened
/// with ([`Request::SignInRelated`]).
pub(crate) struct SignInJob {
    pub file: String,
    pub account: String,
    /// Typed for this attempt only; never kept or shown.
    pub password: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for SignInJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInJob")
            .field("file", &self.file)
            .field("account", &self.account)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl std::fmt::Debug for ReconnectJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconnectJob")
            .field("password", &"<redacted>")
            .field("use_saved_license", &self.use_saved_license)
            .field("records", &self.records)
            .field("current", &self.current)
            .finish()
    }
}

/// What the window asks of its partly applied save ([`Request::SettleSave`],
/// #1006): the provider's typed operations, which own its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settle {
    /// Read back each file whose outcome is unknown
    /// (`DataProvider::reconcile_save`).
    Check,
    /// Send only the work not applied (`DataProvider::resume_save`).
    Resume,
    /// Drop what was not applied (`DataProvider::discard_save`).
    Discard,
    /// Only report it (`DataProvider::unsettled_save`).
    Report,
}

/// One owning window's checked record, qualified by window, layout and id.
pub(crate) type ReconnectRecord = ((u32, u32, i64), Result<RecordPage, DataError>);

/// A file opened again ([`Request::Reconnect`]).
#[derive(Debug)]
pub(crate) struct Reconnected {
    pub host_version: Option<String>,
    /// Each record asked for, as the host holds it now (no record when it
    /// is gone).
    pub records: Vec<ReconnectRecord>,
    /// Where each window's current record is in its found set now.
    pub positions: Vec<(u32, Result<Option<u64>, DataError>)>,
    /// The files open again, each with its account's privileges: the
    /// window's own, and the others of the lost connection that reopened on
    /// the new one (#1011). A file not here stays lost.
    pub recovered: Vec<(u64, Option<om_data::host::Privileges>)>,
    /// Each related record asked for, as the host holds it now (`None`
    /// when it is gone); its save answers to this version (#152).
    pub related: Vec<RelatedReadBack>,
    /// The files open again whose save the connection was lost in sent
    /// their hosts no record data (`FileState::unsent_save`, #152): their
    /// windows keep its changes, to save again.
    pub unsent: Vec<u64>,
    /// Each window's new record the reconnect held again
    /// (`DataProvider::take_recreated_records`, #152), by window and the
    /// lost connection's id: its id now, or why it could not be.
    pub recreated: Vec<(u32, u64, i64, Result<i64, String>)>,
    /// The windows whose record sessions the reconnect carried to the new
    /// connection (`DataProvider::carried_record_sessions`): any other
    /// window's record session went with the lost one.
    pub carried: Vec<u32>,
}

/// A related record read back after a reconnect: by window, base table and
/// id, and its values (`None`: the host no longer has it).
pub(crate) type RelatedReadBack = (
    (u32, u64, i64),
    Result<Option<std::sync::Arc<om_data::host::RecordValues>>, DataError>,
);

/// A related record read again (Check Again): by base table and id.
pub(crate) type RelatedRead = (
    (u64, i64),
    Result<Option<std::sync::Arc<om_data::host::RecordValues>>, DataError>,
);

/// Related record `record` of base table `table` as the host holds it now
/// (`None` when it is gone), read for the provider's selected window; a
/// save of it answers to the version read now
/// (`DataProvider::retain_related_record`), as a record read back by id
/// does (#152).
pub(crate) fn read_related(
    provider: &mut dyn DataProvider,
    table: u64,
    record: i64,
) -> Result<Option<std::sync::Arc<om_data::host::RecordValues>>, DataError> {
    let id = u64::try_from(record).map_err(|_| DataError::new("not a record id"))?;
    let need = om_data::host::CalcNeed::Record { table, record: id };
    let read =
        contain(provider, |p| p.calc_data(std::slice::from_ref(&need))).and_then(|data| {
            data.records
                .get(&(table, id))
                .cloned()
                .ok_or_else(|| DataError::new("the host sent nothing for it"))
        })?;
    if let Some(version) = read.as_ref().and_then(|values| values.mod_count) {
        provider.release_related_record(table, record);
        provider.retain_related_record(table, record, version);
    }
    Ok(read)
}

/// A file the provider opened.
#[derive(Debug)]
pub(crate) struct Opened {
    /// `Get ( HostApplicationVersion )`, when the file is hosted.
    pub host_version: Option<String>,
    pub privileges: Option<om_data::host::Privileges>,
    /// The account logged in, when the provider knows it: the one a local
    /// file opened with when it did not ask.
    pub account: Option<String>,
}

/// A host's files as listed for the Hosts dialog, with the token of the
/// listing they answer.
#[derive(Debug)]
pub(crate) struct Listed {
    pub token: u64,
    pub result: Result<HostListing, ListingError>,
}

/// The outcome of one [`Request`].
#[derive(Debug)]
pub(crate) enum Reply {
    RecordEditPrepared(
        u64,
        om_data::RecordEditRequest,
        Result<om_data::EditPreparation, DataError>,
    ),

    Files(Listed),
    Opened(Result<Opened, DataError>),
    /// A local file opened, or asks for an account.
    OpenedLocal(Result<(LocalOpen, Opened), DataError>),
    License(Result<bool, DataError>),
    Layouts(Result<(Vec<LayoutInfo>, Vec<om_data::LayoutMenuEntry>), DataError>),
    LayoutFieldPlaced(Result<om_data::layout_placement::PlacedLayoutField, DataError>),
    LayoutCreated(Result<om_data::layout_catalog::CreatedLayout, DataError>),
    LayoutDeleted(Result<om_data::layout_catalog::RemovedLayout, DataError>),
    LayoutSetUp(Result<om_data::layout_objects::FramedObjects, DataError>),
    CustomValueList(u64, Result<om_data::value_lists::CustomList, DataError>),
    ValueListValuesSet(Result<om_data::layout_objects::FramedObjects, DataError>),
    /// The answer to the part options sent, with them, so the dialog that
    /// sent them knows its own.
    PartOptionsSet(
        om_data::layout_placement::PartOptionsChange,
        Result<om_data::layout_objects::FramedObjects, DataError>,
    ),
    /// The answer to the parts change sent, with it.
    LayoutPartsSet(
        om_data::layout_placement::PartsChange,
        Result<om_data::layout_objects::FramedObjects, DataError>,
    ),
    LayoutWidthSet(Result<om_data::layout_objects::FramedObjects, DataError>),
    FieldControlSet(Result<om_data::layout_objects::FramedObjects, DataError>),
    LayoutTextSet(Result<om_data::layout_objects::FramedObjects, DataError>),
    LayoutButtonPlaced(Result<om_data::layout_placement::PlacedLayoutButton, DataError>),
    LayoutPortalPlaced(Result<om_data::layout_placement::PlacedLayoutPortal, DataError>),
    LayoutTextPlaced(Result<om_data::layout_placement::PlacedLayoutText, DataError>),
    ObjectsFramed(Result<om_data::layout_objects::FramedObjects, DataError>),
    ObjectsDeleted(Result<om_data::layout_objects::DeletedObjects, DataError>),
    RenderLayout {
        layout_id: u32,
        result: Box<Result<Option<RenderLayout>, DataError>>,
    },
    LayoutGeometry {
        layout_id: u32,
        result: Result<Option<LayoutGeometry>, DataError>,
    },
    RecordCapabilities {
        layout_id: u32,
        result: Result<RecordCapabilities, DataError>,
    },
    SavedPosition {
        layout_id: u32,
        position: Option<u64>,
    },
    Records {
        key: PageKey,
        result: Result<RecordPage, DataError>,
        /// The found set's size and sort, read with the page
        /// (`DataProvider::records_with_summary`).
        found: Option<om_data::found::FoundSet>,
        /// What of the page the account may not see
        /// (`DataProvider::no_access`).
        no_access: om_data::NoAccess,
        /// The provider's calculation data generation after the read
        /// (`DataProvider::calc_generation`): unchanged, what the window's
        /// calculations read stays valid (#1042).
        generation: Option<u64>,
    },
    Updated {
        layout_id: u32,
        record_id: i64,
        result: Result<Record, DataError>,
    },
    Deleted {
        layout_id: u32,
        record_id: i64,
        result: Result<(), DataError>,
    },
    Scripts {
        workspace: Option<u64>,
        result: Result<ScriptCatalog, DataError>,
    },
    ScriptSaved {
        workspace: u64,
        result: Result<Script, DataError>,
    },
    ScriptCatalogSaved {
        workspace: u64,
        result: Result<ScriptCatalog, DataError>,
    },
    Schema {
        result: Result<DatabaseSchema, DataError>,
        changed: bool,
    },
    Security {
        token: u64,
        result: Result<om_data::security_catalog::SecurityCatalog, DataError>,
    },
    ValueLists {
        token: u64,
        result: Result<ValueListCatalog, DataError>,
    },
    /// The calculation schema, and what to tell the user about the file's
    /// related files: the warnings, and the files that refused the account
    /// for another to be asked for (`DataProvider::take_related_sign_ins`).
    CalcSchema {
        result: Result<Option<Arc<CalcSchema>>, DataError>,
        warnings: Vec<String>,
        sign_ins: Vec<om_data::RelatedSignIn>,
    },
    /// The file's calculation schema grew while the provider answered
    /// `then`: related files a layout or calculation read opened on demand
    /// ([`DataProvider::take_grown_schema`]). The file's windows take the
    /// schema, keeping what they read (no id changed), before `then`.
    Grown {
        grown: GrownSchema,
        then: Box<Reply>,
    },
    /// The answer to [`Request::ChangePassword`].
    PasswordChanged(Result<(), DataError>),
    /// The answer to [`Request::SignInRelated`] for related file `file`.
    RelatedSignIn {
        file: String,
        result: Result<(), DataError>,
    },
    CalcData {
        needs: Vec<CalcNeed>,
        result: Box<Result<CalcData, DataError>>,
    },
    HostFieldValues {
        list: u64,
        epoch: u64,
        result: Result<Option<Vec<String>>, DataError>,
    },
    /// [`Request::BuildValueList`]'s list (`crate::list_build::Built`):
    /// `None` when there is nothing to show yet, with when to ask again
    /// when a related host could not be reached.
    ValueListBuilt {
        list: u64,
        ticket: u64,
        result: Option<Result<Arc<om_data::choice::Choices>, om_data::choice::ChoiceError>>,
        retry_after: Option<std::time::Duration>,
    },
    /// What a running script tells the window.
    Script(crate::scripting::ScriptEvent),
    /// The open file's connection to the host was lost.
    ConnectionLost(DataError),
    /// What the host's administrator sent the file's session, read while
    /// idle ([`DataProvider::take_host_messages`]), oldest first.
    HostMessages(Vec<om_data::HostMessage>),
    Reconnected(Box<Result<Reconnected, DataError>>),
    /// What [`Request::Recreate`] did: the record's old and new id, and
    /// its place in the window's found set, when it was made again; `None`
    /// when it was discarded.
    Recreated(Result<Option<(i64, i64, Option<u64>)>, DataError>),
    /// Each record [`Request::Recheck`] asked for, as the host holds it.
    Rechecked {
        records: Vec<((u32, i64), Result<RecordPage, DataError>)>,
        related: Vec<RelatedRead>,
    },
    /// What [`Request::SettleSave`] did, and the window's partly applied
    /// save after it (`None`: none is left).
    SaveSettled {
        settle: Settle,
        result: Result<(), DataError>,
        report: Option<Vec<om_data::FileCommit>>,
    },
    Released,
    Closed,
}

impl Reply {
    /// The reply to the request itself, inside any [`Reply::Grown`] the
    /// worker wrapped it in.
    pub(crate) fn answered(&self) -> &Reply {
        match self {
            Reply::Grown { then, .. } => then.answered(),
            reply => reply,
        }
    }

    /// The provider's error in this reply, if it failed.
    pub(crate) fn error(&self) -> Option<&DataError> {
        match self {
            Reply::Opened(r) => r.as_ref().err(),
            Reply::OpenedLocal(r) => r.as_ref().err(),
            Reply::License(r) => r.as_ref().err(),
            Reply::Layouts(r) => r.as_ref().err(),
            Reply::LayoutFieldPlaced(r) => r.as_ref().err(),
            Reply::LayoutCreated(r) => r.as_ref().err(),
            Reply::LayoutDeleted(r) => r.as_ref().err(),
            Reply::LayoutSetUp(r) => r.as_ref().err(),
            Reply::CustomValueList(_, r) => r.as_ref().err(),
            Reply::ValueListValuesSet(r) => r.as_ref().err(),
            Reply::PartOptionsSet(_, r) => r.as_ref().err(),
            Reply::LayoutPartsSet(_, r) => r.as_ref().err(),
            Reply::LayoutWidthSet(r) => r.as_ref().err(),
            Reply::FieldControlSet(r) => r.as_ref().err(),
            Reply::LayoutTextSet(r) => r.as_ref().err(),
            Reply::LayoutButtonPlaced(r) => r.as_ref().err(),
            Reply::LayoutPortalPlaced(r) => r.as_ref().err(),
            Reply::LayoutTextPlaced(r) => r.as_ref().err(),
            Reply::ObjectsFramed(r) => r.as_ref().err(),
            Reply::ObjectsDeleted(r) => r.as_ref().err(),
            Reply::RenderLayout { result, .. } => result.as_ref().as_ref().err(),
            Reply::LayoutGeometry { result, .. } => result.as_ref().err(),
            Reply::RecordCapabilities { result, .. } => result.as_ref().err(),
            Reply::Records { result, .. } => result.as_ref().err(),
            Reply::Updated { result, .. } => result.as_ref().err(),
            Reply::Deleted { result, .. } => result.as_ref().err(),
            Reply::Scripts { result, .. } | Reply::ScriptCatalogSaved { result, .. } => {
                result.as_ref().err()
            }
            Reply::ScriptSaved { result, .. } => result.as_ref().err(),
            Reply::Schema { result, .. } => result.as_ref().err(),
            Reply::Security { result, .. } => result.as_ref().err(),
            Reply::ValueLists { result, .. } => result.as_ref().err(),
            Reply::CalcSchema { result, .. } => result.as_ref().err(),
            Reply::Grown { then, .. } => then.error(),
            Reply::RelatedSignIn { result, .. } => result.as_ref().err(),
            Reply::PasswordChanged(result) => result.as_ref().err(),
            Reply::CalcData { result, .. } => result.as_ref().as_ref().err(),
            Reply::HostFieldValues { result, .. } => result.as_ref().err(),
            // A list that could not be built says so in its control.
            Reply::ValueListBuilt { .. } => None,
            Reply::RecordEditPrepared(_, _, result) => result.as_ref().err(),
            Reply::ConnectionLost(error) => Some(error),
            Reply::HostMessages(_) => None,
            Reply::Reconnected(result) => result.as_ref().as_ref().err(),
            Reply::Recreated(result) => result.as_ref().err(),
            // Each record's read carries its own error.
            Reply::Rechecked { .. } | Reply::Released | Reply::SavedPosition { .. } => None,
            Reply::SaveSettled { result, .. } => result.as_ref().err(),
            // A listing's failure is not the open file's.
            Reply::Files(_) | Reply::Script(_) | Reply::Closed => None,
        }
    }
}

/// A reply with the epoch of the request that produced it. The UI bumps its
/// epoch when it abandons a session, so stale replies can be ignored.
#[derive(Debug)]
pub(crate) struct Response {
    pub epoch: u64,
    /// The window whose request it answers (a script's events: the window
    /// the script works on).
    pub window: u32,
    pub reply: Reply,
}

pub(crate) struct Job {
    pub epoch: u64,
    /// The window that asks: the provider uses its found sets.
    pub window: u32,
    pub request: Request,
    pub ctx: egui::Context,
}

/// The UI-side handle to the provider thread.
pub(crate) struct Worker {
    jobs: Option<Sender<Job>>,
    responses: Receiver<Response>,
    /// Esc / Cmd-period while a script runs (Allow User Abort).
    abort: Arc<AtomicBool>,
    /// The provider's queued Closes, counted as they are sent so its long
    /// work (opening related files) can stop before it gets to them.
    closes: Option<om_data::CloseRequests>,
    /// Lists hosts' files on threads of their own, apart from the provider
    /// and its open file ([`DataProvider::host_browser`]).
    browser: Option<Arc<dyn HostBrowser>>,
    /// The listings those threads finished.
    listed: (Sender<Listed>, Receiver<Listed>),
}

/// Why [`Worker::poll`] could not return a response.
pub(crate) enum PollError {
    /// Nothing is ready yet.
    Empty,
    /// The provider thread is gone; no further replies will arrive.
    Stopped,
}

impl Worker {
    /// Hold requests without executing them, to check UI dispatch ordering.
    #[cfg(test)]
    pub(crate) fn capture_jobs() -> (Self, Receiver<Job>) {
        let (jobs, received) = mpsc::channel();
        let (_, responses) = mpsc::channel();
        (
            Self {
                jobs: Some(jobs),
                responses,
                abort: Arc::new(AtomicBool::new(false)),
                closes: None,
                browser: None,
                listed: mpsc::channel(),
            },
            received,
        )
    }

    /// Move `provider` onto a new thread.
    ///
    /// If the thread cannot be spawned the worker is created stopped, and every
    /// [`Worker::send`] reports an error.
    #[cfg(test)]
    pub fn spawn(provider: Box<dyn DataProvider>) -> Self {
        Self::with_license_store(provider, Box::<crate::licensing::OsLicenseStore>::default())
    }

    #[cfg(test)]
    pub fn with_license_store(
        provider: Box<dyn DataProvider>,
        store: Box<dyn LicenseStore>,
    ) -> Self {
        Self::with_print_dispatcher(provider, store, crate::printing::default_dispatcher())
    }

    pub(crate) fn with_print_dispatcher(
        provider: Box<dyn DataProvider>,
        store: Box<dyn LicenseStore>,
        dispatcher: Arc<dyn crate::printing::Dispatcher>,
    ) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (resp_tx, resp_rx) = mpsc::channel::<Response>();
        let abort = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&abort);
        let closes = provider.close_requests();
        let browser = provider.host_browser();
        let spawned = thread::Builder::new()
            .name("om-provider".into())
            .spawn(move || serve(provider, store, &job_rx, &resp_tx, flag, dispatcher));
        Self {
            jobs: spawned.ok().map(|_| job_tx),
            responses: resp_rx,
            abort,
            closes,
            browser,
            listed: mpsc::channel(),
        }
    }

    /// List `host`'s files for the Hosts dialog, as listing `token`. With a
    /// [`HostBrowser`] the listing runs on a thread of its own, over a
    /// connection of its own, and [`Worker::poll_listed`] returns it;
    /// setting `canceled` stops it. Otherwise the provider lists them in
    /// turn, for window `window`, and the reply is a [`Reply::Files`].
    #[allow(clippy::too_many_arguments)]
    pub fn list_files(
        &self,
        ctx: &egui::Context,
        epoch: u64,
        window: u32,
        token: u64,
        host: String,
        port: u16,
        canceled: Arc<AtomicBool>,
    ) -> Result<(), DataError> {
        let Some(browser) = self.browser.clone() else {
            return self.send(ctx, epoch, window, Request::ListFiles { token, host, port });
        };
        let listed = self.listed.0.clone();
        let ctx = ctx.clone();
        thread::Builder::new()
            .name("om-host-browser".into())
            .spawn(move || {
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    browser.browse(&host, port, &canceled)
                }))
                .unwrap_or_else(|payload| {
                    Err(ListingError::new(
                        ListingFailure::Other,
                        format!(
                            "Listing files failed unexpectedly: {}",
                            panic_message(payload.as_ref())
                        ),
                    ))
                });
                if listed.send(Listed { token, result }).is_ok() {
                    ctx.request_repaint();
                }
            })
            .map(|_| ())
            .map_err(|e| DataError::new(format!("Could not start listing files: {e}")))
    }

    /// Take the next listing a browser thread finished, without blocking.
    pub fn poll_listed(&self) -> Option<Listed> {
        self.listed.1.try_recv().ok()
    }

    /// Ask the running script to stop (Esc, Cmd-period).
    pub fn abort_script(&self) {
        self.abort.store(true, Ordering::SeqCst);
    }

    /// Queue window `window`'s request. Requests run in order, one at a
    /// time.
    pub fn send(
        &self,
        ctx: &egui::Context,
        epoch: u64,
        window: u32,
        request: Request,
    ) -> Result<(), DataError> {
        if let (Request::Close, Some(closes)) = (&request, &self.closes) {
            closes.queued();
        }
        let is_close = matches!(request, Request::Close);
        let job = Job {
            epoch,
            window,
            request,
            ctx: ctx.clone(),
        };
        let result = self
            .jobs
            .as_ref()
            .ok_or_else(stopped_error)
            .and_then(|jobs| jobs.send(job).map_err(|_| stopped_error()));
        if result.is_err() {
            if let (true, Some(closes)) = (is_close, &self.closes) {
                closes.handled();
            }
        }
        result
    }

    /// Whether a queued Close has not been handled yet.
    pub fn closes_pending(&self) -> bool {
        self.closes
            .as_ref()
            .is_some_and(om_data::CloseRequests::pending)
    }

    /// Take the next finished response without blocking.
    pub fn poll(&self) -> Result<Response, PollError> {
        self.responses.try_recv().map_err(|err| match err {
            TryRecvError::Empty => PollError::Empty,
            TryRecvError::Disconnected => PollError::Stopped,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(closes) = &self.closes {
            closes.queued();
        }
    }
}

pub(crate) fn stopped_error() -> DataError {
    DataError::new("The data provider has stopped; restart OpenMaker to reconnect.")
}

/// How often an idle worker, or a script waiting for the user, asks the
/// provider whether the connection was lost.
pub(crate) const CONNECTION_POLL: Duration = Duration::from_secs(1);

/// The provider thread's loop. Ends when the UI drops its [`Worker`].
/// Scripts run here too (`scripting`): the session serves requests that
/// arrive while a script runs.
fn serve(
    mut provider: Box<dyn DataProvider>,
    mut store: Box<dyn LicenseStore>,
    jobs: &Receiver<Job>,
    responses: &Sender<Response>,
    abort: Arc<AtomicBool>,
    dispatcher: Arc<dyn crate::printing::Dispatcher>,
) {
    let mut scripts = crate::scripting::ScriptSession::new(abort);
    scripts.set_print_dispatcher(Arc::clone(&dispatcher));
    let mut files = crate::scripting::databases::OpenFiles::default();
    // The epoch and the egui context of the UI's last request: a loss
    // noticed while idle is told as of it, and wakes that context.
    let (mut epoch, mut ctx) = (0, None::<egui::Context>);
    // When the hosts' calls were last read between requests: once a second
    // at most, so that a request does not wait for that read after the one
    // before it (each file's read waits for up to its first message).
    let mut host_messages_read = std::time::Instant::now();
    loop {
        let mut wait = scripts
            .next_wake(&files)
            .map_or(CONNECTION_POLL, |wait| wait.min(CONNECTION_POLL));
        // The next read of the hosts' calls is due a second after the last,
        // not a second after the last request.
        if ctx.is_some() {
            wait = wait.min(CONNECTION_POLL.saturating_sub(host_messages_read.elapsed()));
        }
        let job = match jobs.recv_timeout(wait) {
            Ok(job) => Some(job),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Some(job) = &job {
            epoch = job.epoch;
            ctx = Some(job.ctx.clone());
        }
        let mut io = crate::scripting::session::Io {
            provider: &mut provider,
            files: &mut files,
            store: store.as_mut(),
            jobs,
            responses,
        };
        let mut panicked = panic::catch_unwind(AssertUnwindSafe(|| match job {
            Some(job) => scripts.handle(&mut io, job),
            None => scripts.wake(&mut io),
        }))
        .is_err();
        // A script or provider panic ends the session's running scripts,
        // not the worker, nor the session's files and windows: the windows
        // the scripts worked in show what they left and say they stopped,
        // and every window goes on being answered by its file (#1247).
        // Before, the session and its files were dropped: the message went
        // to no window, the app kept its script running, and no window but
        // the first was answered.
        while panicked {
            scripts.abandon(&mut io);
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
            // A request the scripts deferred (a file opened or closed
            // meanwhile) is still served.
            panicked = match scripts.take_deferred() {
                Some(job) => {
                    panic::catch_unwind(AssertUnwindSafe(|| scripts.handle(&mut io, job))).is_err()
                }
                None => false,
            };
        }
        if let Some(ctx) = &ctx {
            scripts.notice_loss(provider.as_ref(), responses, epoch, ctx, &mut files);
            if host_messages_read.elapsed() >= CONNECTION_POLL {
                scripts.tell_host_messages(provider.as_mut(), responses, epoch, ctx, &mut files);
                host_messages_read = std::time::Instant::now();
            }
        }
    }
    files.close_all();
    // Best effort: a panic here has nowhere useful to go.
    let _ = contain(provider.as_mut(), |p| {
        p.close();
        Ok(())
    });
}

/// What the window needs of a file the provider just opened.
fn opened(provider: &dyn DataProvider) -> Opened {
    Opened {
        host_version: provider.host_application_version(),
        privileges: provider.account_privileges(),
        account: provider.session_account(),
    }
}

/// Apply the same optional-license policy to opening and reconnecting hosts.
/// A key-store failure does not block an OpenMaker Server connection.
pub(crate) fn with_client_license<T>(
    provider: &mut dyn DataProvider,
    store: &mut dyn LicenseStore,
    use_saved_license: bool,
    open: impl FnOnce(&mut dyn DataProvider) -> Result<T, DataError>,
) -> Result<T, DataError> {
    let mut unreadable = None;
    if provider.supports_client_license() {
        let key = if use_saved_license {
            store.load().unwrap_or_else(|error| {
                unreadable = Some(error);
                None
            })
        } else {
            None
        };
        provider.set_client_license(key.as_ref().map(LicenseKey::expose))?;
    }
    open(provider).map_err(|mut error| {
        if let Some(store) = unreadable {
            error.message = format!("{} The saved license was not sent: {store}", error.message);
        }
        error
    })
}

/// The calculation schema a provider grew while answering a request
/// ([`Reply::Grown`]), with what it says about related files then.
#[derive(Debug)]
pub(crate) struct GrownSchema {
    pub result: Result<Option<Arc<CalcSchema>>, DataError>,
    pub warnings: Vec<String>,
    pub sign_ins: Vec<om_data::RelatedSignIn>,
}

/// The answer to `request`, which the file's windows take after any
/// schema the provider grew answering it ([`Reply::Grown`]).
pub(crate) fn handle(
    provider: &mut dyn DataProvider,
    store: &mut dyn LicenseStore,
    request: Request,
) -> Reply {
    let reply = answer(provider, store, request);
    // The request is answered: a panic after it never loses the answer
    // (#313). One asking whether the schema grew is a schema read that
    // failed, so the file's calculations stop rather than go on with a
    // schema that may be stale.
    let grew = panic::catch_unwind(AssertUnwindSafe(|| provider.take_grown_schema()));
    let grown = match grew {
        Ok(false) => return reply,
        Ok(true) => GrownSchema {
            result: contain(provider, |p| p.calc_schema()),
            warnings: panic::catch_unwind(AssertUnwindSafe(|| provider.related_file_warnings()))
                .unwrap_or_default(),
            sign_ins: panic::catch_unwind(AssertUnwindSafe(|| provider.related_sign_ins()))
                .unwrap_or_default(),
        },
        Err(payload) => GrownSchema {
            result: Err(panicked(payload.as_ref())),
            warnings: Vec::new(),
            sign_ins: Vec::new(),
        },
    };
    Reply::Grown {
        grown,
        then: Box::new(reply),
    }
}

/// [`handle`] in window `window`, the window the request is about: the
/// provider acts on it. A panic while it switches there leaves a save
/// (Commit Record's update, a deletion) with its own failure reply, as a
/// failed provider call answers it, so the window does not wait for good
/// (#313). Any other request panics on as before: its reply is not one the
/// window can take as a plain failure (a save's settling, a page and its
/// access mask, a cancellation), so the scripts are stopped instead.
pub(crate) fn handle_in(
    provider: &mut dyn DataProvider,
    store: &mut dyn LicenseStore,
    window: u32,
    request: Request,
) -> Reply {
    match panic::catch_unwind(AssertUnwindSafe(|| provider.use_window(window))) {
        Ok(()) => handle(provider, store, request),
        Err(payload) => {
            // The loss probe is best effort: a second panic here must not
            // lose the save's reply either.
            let lost =
                panic::catch_unwind(AssertUnwindSafe(|| provider.connection_lost().is_some()))
                    .unwrap_or(false);
            let error = || {
                let mut error = panicked(payload.as_ref());
                error.lost |= lost;
                error
            };
            match request {
                Request::UpdateRecord {
                    layout_id,
                    record_id,
                    ..
                } => Reply::Updated {
                    layout_id,
                    record_id,
                    result: Err(error()),
                },
                Request::DeleteRecord {
                    layout_id,
                    record_id,
                } => Reply::Deleted {
                    layout_id,
                    record_id,
                    result: Err(error()),
                },
                _ => panic::resume_unwind(payload),
            }
        }
    }
}

/// Whether `provider`, the one that just answered a request, holds its
/// changes in a script transaction (open, not apart in a record session):
/// asked before switching back to the scripts' window, which may be
/// another file's provider. A panic asking counts as held.
pub(crate) fn holds_in_transaction(provider: &dyn DataProvider) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        provider.transaction_open() && !provider.record_session_open()
    }))
    .unwrap_or(true)
}

/// `reply`, once switching the provider back to the scripts' window after
/// it panicked: the scripts stop, and their cleanup reverts a script
/// transaction still open (`ScriptSession::revert_abandoned`). A save the
/// answering provider `held` in such a transaction
/// ([`holds_in_transaction`]) is then answered as failed, also inside a
/// [`Reply::Grown`], so the window keeps its draft, rather than as kept
/// (#313).
pub(crate) fn unkept_save(held: bool, reply: Reply) -> Reply {
    if !held {
        return reply;
    }
    let unkept = || {
        DataError::new(
            "The scripts stopped when the data provider failed unexpectedly, and the open \
             transaction that held this change was reverted: it was not saved.",
        )
    };
    match reply {
        Reply::Grown { grown, then } => Reply::Grown {
            grown,
            then: Box::new(unkept_save(held, *then)),
        },
        Reply::Updated {
            layout_id,
            record_id,
            result: Ok(_),
        } => Reply::Updated {
            layout_id,
            record_id,
            result: Err(unkept()),
        },
        Reply::Deleted {
            layout_id,
            record_id,
            result: Ok(()),
        } => Reply::Deleted {
            layout_id,
            record_id,
            result: Err(unkept()),
        },
        reply => reply,
    }
}

fn answer(
    provider: &mut dyn DataProvider,
    store: &mut dyn LicenseStore,
    request: Request,
) -> Reply {
    match request {
        Request::ListFiles { token, host, port } => Reply::Files(Listed {
            token,
            result: contain_apart(provider, |p| p.list_files(&host, port))
                .map(HostListing::of_files)
                .map_err(ListingError::from),
        }),
        Request::LicenseStatus => {
            Reply::License(contain(provider, |_| store.load().map(|key| key.is_some())))
        }
        Request::SaveLicense(key) => {
            Reply::License(contain(provider, |_| store.save(&key).map(|()| true)))
        }
        Request::RemoveLicense => {
            Reply::License(contain(provider, |_| store.remove().map(|()| false)))
        }
        Request::Open {
            connection,
            use_saved_license,
        } => Reply::Opened(contain(provider, |provider| {
            with_client_license(provider, store, use_saved_license, |provider| {
                provider.open(&connection)
            })?;
            Ok(opened(provider))
        })),
        Request::OpenLocalAsking(path) => Reply::OpenedLocal(contain(provider, |provider| {
            provider
                .open_local_asking(&path)
                .map(|open| (open, opened(provider)))
        })),
        Request::OpenLocal(file) => Reply::OpenedLocal(contain(provider, |provider| {
            provider
                .open_local(&file)
                .map(|open| (open, opened(provider)))
        })),
        Request::ChangePassword(pair) => Reply::PasswordChanged(contain(provider, |provider| {
            provider.change_password(&pair.old, &pair.new)
        })),
        Request::ChangeDuePassword(new) => Reply::OpenedLocal(contain(provider, |provider| {
            provider
                .change_due_password(&new.0)
                .map(|open| (open, opened(provider)))
        })),
        Request::Layouts => {
            Reply::Layouts(contain(provider, |p| Ok((p.layouts()?, p.layout_menu()?))))
        }
        Request::Scripts { workspace } => Reply::Scripts {
            workspace,
            result: contain(provider, |p| match workspace {
                Some(_) => p.scripts_for_editing(),
                None => p.scripts(),
            }),
        },
        Request::SaveScript { workspace, script } => Reply::ScriptSaved {
            workspace,
            result: contain(provider, |p| p.save_script(script)),
        },
        Request::SaveScriptCatalog { workspace, catalog } => Reply::ScriptCatalogSaved {
            workspace,
            result: contain(provider, |p| p.save_script_catalog(catalog)),
        },
        Request::BeginSchemaEdit => Reply::Schema {
            result: contain(provider, |p| p.begin_schema_edit()),
            changed: false,
        },
        Request::ChangeSchema(change) => Reply::Schema {
            result: contain(provider, |p| p.apply_schema_change(change)),
            changed: false,
        },
        Request::CommitSchema => Reply::Schema {
            result: contain(provider, |p| p.commit_schema_changes()),
            changed: true,
        },
        Request::CancelSchema => {
            let _ = contain(provider, |p| p.cancel_schema_changes());
            Reply::Released
        }
        Request::PlaceLayoutField(place) => {
            Reply::LayoutFieldPlaced(contain(provider, |p| p.place_layout_field(place)))
        }
        Request::CreateLayout(request) => {
            Reply::LayoutCreated(contain(provider, |p| p.create_layout(request)))
        }
        Request::DuplicateLayout(layout) => {
            Reply::LayoutCreated(contain(provider, |p| p.duplicate_layout(layout)))
        }
        Request::DeleteLayout(layout) => {
            Reply::LayoutDeleted(contain(provider, |p| p.delete_layout(layout)))
        }
        Request::LayoutSetup(change) => {
            Reply::LayoutSetUp(contain(provider, |p| p.set_layout_setup(change)))
        }
        Request::SetLayoutParts(change) => {
            let sent = change.clone();
            Reply::LayoutPartsSet(sent, contain(provider, |p| p.set_layout_parts(change)))
        }
        Request::CustomValueList(list) => {
            Reply::CustomValueList(list, contain(provider, |p| p.custom_value_list(list)))
        }
        Request::SetValueListValues(change) => {
            Reply::ValueListValuesSet(contain(provider, |p| p.set_value_list_values(change)))
        }
        Request::SetPartOptions(change) => {
            Reply::PartOptionsSet(change, contain(provider, |p| p.set_part_options(change)))
        }
        Request::SetLayoutWidth(change) => {
            Reply::LayoutWidthSet(contain(provider, |p| p.set_layout_width(change)))
        }
        Request::SetFieldControl(change) => {
            Reply::FieldControlSet(contain(provider, |p| p.set_field_control(change)))
        }
        Request::SetLayoutText(change) => {
            Reply::LayoutTextSet(contain(provider, |p| p.set_layout_text(change)))
        }
        Request::PlaceLayoutButton(place) => {
            Reply::LayoutButtonPlaced(contain(provider, |p| p.place_layout_button(place)))
        }
        Request::PlaceLayoutPortal(place) => {
            Reply::LayoutPortalPlaced(contain(provider, |p| p.place_layout_portal(place)))
        }
        Request::PlaceLayoutText(place) => {
            Reply::LayoutTextPlaced(contain(provider, |p| p.place_layout_text(place)))
        }
        Request::SetObjectFrames(change) => {
            Reply::ObjectsFramed(contain(provider, |p| p.set_layout_object_frames(change)))
        }
        Request::DeleteObjects(change) => {
            Reply::ObjectsDeleted(contain(provider, |p| p.delete_layout_objects(change)))
        }
        Request::SecurityCatalog { token } => Reply::Security {
            token,
            result: contain(provider, |p| p.security_catalog()),
        },
        Request::ValueLists { token } => Reply::ValueLists {
            token,
            result: contain(provider, |p| p.value_list_catalog()),
        },
        Request::RenderLayout { layout_id } => Reply::RenderLayout {
            layout_id,
            result: Box::new(contain(provider, |p| p.render_layout(layout_id))),
        },
        Request::LayoutGeometry { layout_id } => Reply::LayoutGeometry {
            layout_id,
            result: contain(provider, |p| p.layout_geometry(layout_id)),
        },
        Request::RecordCapabilities { layout_id } => Reply::RecordCapabilities {
            layout_id,
            result: contain(provider, |p| p.record_capabilities(layout_id)),
        },
        Request::SavedPosition { layout_id } => Reply::SavedPosition {
            layout_id,
            position: contain(provider, |p| Ok(p.take_saved_position(layout_id)))
                .ok()
                .flatten(),
        },
        Request::Records(key) => {
            let read = contain(provider, |p| {
                p.records_with_summary(key.layout_id, key.offset, key.limit, &[])
            });
            let (result, found) = match read {
                Ok((page, found)) => (Ok(page), found),
                Err(error) => (Err(error), None),
            };
            let no_access = match &result {
                Ok(page) => provider.no_access(key.layout_id, page),
                Err(_) => om_data::NoAccess::default(),
            };
            Reply::Records {
                key,
                result,
                found,
                no_access,
                generation: provider.calc_generation(),
            }
        }
        Request::UpdateRecord {
            layout_id,
            record_id,
            values,
        } => Reply::Updated {
            layout_id,
            record_id,
            result: contain(provider, |p| p.update_record(layout_id, record_id, values)),
        },
        Request::DeleteRecord {
            layout_id,
            record_id,
        } => Reply::Deleted {
            layout_id,
            record_id,
            result: contain(provider, |p| p.delete_record(layout_id, record_id)),
        },
        Request::CalcSchema => Reply::CalcSchema {
            result: contain(provider, |p| p.calc_schema()),
            warnings: provider.related_file_warnings(),
            sign_ins: provider.related_sign_ins(),
        },
        Request::SignInRelated(job) => Reply::RelatedSignIn {
            file: job.file.clone(),
            result: contain(provider, |p| {
                p.sign_in_related(&job.file, &job.account, &job.password)
            }),
        },
        Request::CalcData { needs, cancel } => {
            let result = contain(provider, |p| p.calc_data_for(&needs, &cancel));
            Reply::CalcData {
                needs,
                result: Box::new(result),
            }
        }
        Request::HostFieldValues {
            list,
            table,
            field,
            epoch,
        } => Reply::HostFieldValues {
            list,
            epoch,
            result: contain(provider, |p| p.host_field_values(table, field)),
        },
        Request::BuildValueList { build, cancel } => {
            let (today, info) = (build.today, &build.info);
            let built = contain(provider, |p| {
                Ok(crate::list_build::build(
                    p,
                    build.list,
                    (today, info),
                    &cancel,
                ))
            })
            .unwrap_or_else(|error| crate::list_build::Built {
                result: Some(Err(crate::list_build::failed(&error))),
                retry_after: None,
            });
            Reply::ValueListBuilt {
                list: build.list,
                ticket: build.ticket,
                result: built.result,
                retry_after: built.retry_after,
            }
        }
        // The script session handles these itself.
        Request::Script(_) | Request::Answer { .. } => Reply::Closed,
        Request::SettleSave(settle) => {
            let result = contain(provider, |p| match settle {
                Settle::Check => p.reconcile_save().map(|_| ()),
                Settle::Resume => p.resume_save(),
                Settle::Discard => p.discard_save(),
                Settle::Report => Ok(()),
            });
            Reply::SaveSettled {
                settle,
                result,
                report: provider.unsettled_save(),
            }
        }
        Request::Recheck { records, related } => Reply::Rechecked {
            records: records
                .into_iter()
                .map(|(layout, record)| {
                    let read = contain(provider, |p| p.record_by_id(layout, record));
                    ((layout, record), read)
                })
                .collect(),
            related: related
                .into_iter()
                .map(|(table, record)| ((table, record), read_related(provider, table, record)))
                .collect(),
        },
        Request::PrepareRecordEdit(token, request) => {
            // Retain the caller's accepted page before a delayed read/reply can
            // change the UI's page. Unavailable providers still need this pin.
            let prepared = contain(provider, |p| {
                p.retain_record(
                    request.baseline.layout,
                    request.baseline.record,
                    request.baseline.version,
                );
                p.prepare_record_edit(&request)
            });
            Reply::RecordEditPrepared(token, request, prepared)
        }
        Request::Retain(records) => {
            for (layout, record, version) in records {
                let _ = contain(provider, |p| {
                    p.retain_record(layout, record, version);
                    Ok(())
                });
            }
            Reply::Released
        }
        Request::KeepFoundSet { layout_id, record } => {
            let _ = contain(provider, |p| {
                p.keep_found_set(layout_id, record);
                Ok(())
            });
            Reply::Released
        }
        Request::ReleaseRecords(records) => {
            for (layout, record) in records {
                let _ = contain(provider, |p| {
                    p.release_record(layout, record);
                    Ok(())
                });
            }
            Reply::Released
        }
        // Only an idle session reconnects (`ScriptSession::handle`).
        Request::Reconnect(_) => Reply::Reconnected(Box::new(Err(DataError::new(
            "A script is running. Reconnect when it has ended.",
        )))),
        Request::Recreate { .. } => Reply::Recreated(Err(DataError::new(
            "A script is running. Try again when it has ended.",
        ))),
        Request::Close => {
            let _ = contain(provider, |p| {
                p.close();
                Ok(())
            });
            if let Some(closes) = provider.close_requests() {
                closes.handled();
            }
            Reply::Closed
        }
    }
}

/// Run a provider call, turning a panic into a [`DataError`]. An error
/// after the connection was lost says so ([`DataError::lost`]).
pub(crate) fn contain<T>(
    provider: &mut dyn DataProvider,
    call: impl FnOnce(&mut dyn DataProvider) -> Result<T, DataError>,
) -> Result<T, DataError> {
    contain_apart(provider, call).map_err(|mut error| {
        error.lost |= provider.connection_lost().is_some();
        error
    })
}

/// [`contain`] for a call that is not about the open file (listing a
/// host's files): its error never says the file's connection was lost.
fn contain_apart<T>(
    provider: &mut dyn DataProvider,
    call: impl FnOnce(&mut dyn DataProvider) -> Result<T, DataError>,
) -> Result<T, DataError> {
    panic::catch_unwind(AssertUnwindSafe(|| call(&mut *provider)))
        .unwrap_or_else(|payload| Err(panicked(payload.as_ref())))
}

/// The error a provider panic becomes.
fn panicked(payload: &(dyn Any + Send)) -> DataError {
    DataError::new(format!(
        "The data provider failed unexpectedly: {}",
        panic_message(payload)
    ))
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "unknown panic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::mock::MockProvider;
    use om_data::HostedFile;
    use std::time::{Duration, Instant};

    /// Panics listing files; once `grown` is set, on the next question
    /// whether the schema grew; switching windows while `windows` is.
    #[derive(Default)]
    struct PanickingProvider {
        grown: Arc<AtomicBool>,
        windows: Arc<AtomicBool>,
        /// The next loss probe panics.
        lost: Arc<AtomicBool>,
        /// A script's transaction is open.
        held: Arc<AtomicBool>,
    }

    impl DataProvider for PanickingProvider {
        fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
            panic!("list_files exploded")
        }
        fn use_window(&mut self, _: u32) {
            if self.windows.load(Ordering::SeqCst) {
                panic!("use_window exploded")
            }
        }
        fn take_grown_schema(&mut self) -> bool {
            if self.grown.swap(false, Ordering::SeqCst) {
                panic!("take_grown_schema exploded")
            }
            false
        }
        fn connection_lost(&self) -> Option<DataError> {
            if self.lost.swap(false, Ordering::SeqCst) {
                panic!("connection_lost exploded")
            }
            None
        }
        fn transaction_open(&self) -> bool {
            self.held.load(Ordering::SeqCst)
        }
        fn open(&mut self, _: &Connection) -> Result<(), DataError> {
            Ok(())
        }
        fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
            Ok(Vec::new())
        }
        fn records(&mut self, _: u32, _: u64, _: u32) -> Result<RecordPage, DataError> {
            Err(DataError::new("no records"))
        }
        fn close(&mut self) {}
    }

    fn wait(worker: &Worker) -> Response {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match worker.poll() {
                Ok(response) => return response,
                Err(PollError::Stopped) => panic!("worker stopped"),
                Err(PollError::Empty) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(PollError::Empty) => panic!("timed out waiting for the worker"),
            }
        }
    }

    /// The mock with transactions whose commit finds the connection lost,
    /// and one script: Set Error Capture On, Open Transaction, Pause,
    /// Commit Transaction.
    struct LosingCommit {
        mock: MockProvider,
        lost: bool,
    }

    impl DataProvider for LosingCommit {
        fn list_files(&mut self, host: &str, port: u16) -> Result<Vec<HostedFile>, DataError> {
            self.mock.list_files(host, port)
        }
        fn open(&mut self, connection: &Connection) -> Result<(), DataError> {
            self.mock.open(connection)
        }
        fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
            self.mock.layouts()
        }
        fn records(
            &mut self,
            layout: u32,
            offset: u64,
            limit: u32,
        ) -> Result<RecordPage, DataError> {
            self.mock.records(layout, offset, limit)
        }
        fn update_record(
            &mut self,
            layout: u32,
            record: i64,
            values: FieldChanges,
        ) -> Result<Record, DataError> {
            self.mock.update_record(layout, record, values)
        }
        fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
            let mut capture = om_data::ScriptStep::new(86);
            capture.flags |= om_data::flag::TURN_ON;
            let (mut open, mut commit) =
                (om_data::ScriptStep::new(205), om_data::ScriptStep::new(206));
            (open.value, commit.value) = (3, 3);
            let steps = vec![capture, open, om_data::ScriptStep::new(62), commit];
            Ok(ScriptCatalog {
                scripts: vec![Script {
                    id: 1,
                    name: "Commit".into(),
                    steps,
                    ..Default::default()
                }],
                ..Default::default()
            })
        }
        fn begin_transaction(
            &mut self,
            _: om_data::runtime::TransactionOptions,
        ) -> Result<(), DataError> {
            Ok(())
        }
        fn commit_transaction(&mut self, _: bool) -> Result<(), DataError> {
            self.lost = true;
            Err(DataError::new("io: Broken pipe (os error 32)"))
        }
        fn revert_transaction(&mut self) -> Result<(), DataError> {
            Ok(())
        }
        fn connection_lost(&self) -> Option<DataError> {
            self.lost
                .then(|| DataError::connection_lost("The connection to the host was lost."))
        }
        fn close(&mut self) {
            self.mock.close();
        }
    }

    /// A Commit Transaction the connection is lost in: the UI hears of the
    /// loss before any window the failed commit left, and that window
    /// still holds the record's edit (whether the host has it is unknown).
    #[test]
    fn a_commit_lost_with_the_connection_is_told_first_and_keeps_the_edit() {
        use crate::scripting::{Answer, ScriptEvent, ScriptJob, Start, WindowState};
        let ctx = egui::Context::default();
        let worker = Worker::spawn(Box::new(LosingCommit {
            mock: MockProvider::new(),
            lost: false,
        }));
        let connection = Connection {
            host: "fms.example.com".into(),
            port: 5003,
            file: "Contacts.fmp12".into(),
            account: "admin".into(),
            password: "secret".into(),
        };
        let open = Request::Open {
            connection,
            use_saved_license: false,
        };
        worker.send(&ctx, 1, 1, open).unwrap();
        assert!(matches!(wait(&worker).reply, Reply::Opened(Ok(_))));
        let window = WindowState {
            layout: Some(1),
            record: Some(1),
            ..WindowState::default()
        };
        let job = ScriptJob {
            start: Start::Script { id: 1 },
            window: window.clone(),
            others: Vec::new(),
            context: Default::default(),
        };
        worker
            .send(&ctx, 1, 1, Request::Script(Box::new(job)))
            .unwrap();
        while !matches!(
            wait(&worker).reply,
            Reply::Script(ScriptEvent::Paused { .. })
        ) {}

        // The user edits the record in the transaction and continues.
        let mut edited = window;
        edited
            .drafts
            .insert("First Name".into(), Some("Edited".into()));
        let answer = Answer::Resume(Box::new(edited));
        worker
            .send(&ctx, 1, 1, Request::Answer { prompt: 0, answer })
            .unwrap();
        let mut told = false;
        let finished = loop {
            match wait(&worker).reply {
                Reply::ConnectionLost(_) => told = true,
                Reply::Script(ScriptEvent::Window(_)) => {
                    assert!(told, "a window went out before the loss")
                }
                Reply::Script(ScriptEvent::Finished { window, .. }) => break window,
                _ => {}
            }
        };
        assert!(told, "the loss was never told");
        assert_eq!(
            finished.state.drafts.get("First Name"),
            Some(&Some("Edited".to_owned())),
            "the edit was dropped"
        );
    }

    struct HydratingProvider {
        closes: om_data::CloseRequests,
        started: Sender<()>,
        release: Receiver<()>,
    }

    impl DataProvider for HydratingProvider {
        fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
            Ok(Vec::new())
        }
        fn open(&mut self, _: &Connection) -> Result<(), DataError> {
            Ok(())
        }
        fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
            Ok(Vec::new())
        }
        fn records(&mut self, _: u32, _: u64, _: u32) -> Result<RecordPage, DataError> {
            Err(DataError::new("no records"))
        }
        fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
            self.started.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            if self.closes.pending() {
                Err(DataError::new("cancelled"))
            } else {
                Ok(None)
            }
        }
        fn close_requests(&self) -> Option<om_data::CloseRequests> {
            Some(self.closes.clone())
        }
        fn close(&mut self) {}
    }

    #[test]
    fn queuing_close_cancels_hydration_before_the_worker_handles_it() {
        let (started, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let closes = om_data::CloseRequests::default();
        let worker = Worker::spawn(Box::new(HydratingProvider {
            closes: closes.clone(),
            started,
            release: resume,
        }));
        let ctx = egui::Context::default();
        worker.send(&ctx, 1, 1, Request::CalcSchema).unwrap();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.send(&ctx, 2, 1, Request::Close).unwrap();
        assert!(
            closes.pending(),
            "Close must signal while the worker is blocked"
        );
        release.send(()).unwrap();
        assert!(matches!(
            wait(&worker).reply,
            Reply::CalcSchema { result: Err(_), .. }
        ));
        assert!(matches!(wait(&worker).reply, Reply::Closed));
        assert!(
            !closes.pending(),
            "a later open must not inherit cancellation"
        );
    }

    #[test]
    fn replies_carry_the_request_epoch() {
        let ctx = egui::Context::default();
        let worker = Worker::spawn(Box::new(MockProvider::new()));
        worker.send(&ctx, 7, 1, Request::Layouts).unwrap();
        let response = wait(&worker);
        assert_eq!(response.epoch, 7);
        // The mock refuses layouts before open.
        assert!(matches!(response.reply, Reply::Layouts(Err(_))));
    }

    #[test]
    fn a_panicking_provider_becomes_an_error_and_the_worker_survives() {
        let ctx = egui::Context::default();
        let (grown, windows, lost) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let worker = Worker::spawn(Box::new(PanickingProvider {
            grown: Arc::clone(&grown),
            windows: Arc::clone(&windows),
            lost: Arc::clone(&lost),
            ..PanickingProvider::default()
        }));
        let request = Request::ListFiles {
            token: 7,
            host: "h".into(),
            port: 1,
        };
        worker.send(&ctx, 1, 1, request).unwrap();
        match wait(&worker).reply {
            Reply::Files(Listed {
                token: 7,
                result: Err(err),
            }) => {
                assert_eq!(err.failure, ListingFailure::Other);
                assert!(err.message.contains("list_files exploded"));
            }
            other => panic!("unexpected reply {other:?}"),
        }
        worker.send(&ctx, 1, 1, Request::Layouts).unwrap();
        assert!(matches!(wait(&worker).reply, Reply::Layouts(Ok(_))));

        // A panic after the provider answered still sends the answer, with
        // the schema read failed (#313).
        grown.store(true, Ordering::SeqCst);
        worker.send(&ctx, 1, 1, Request::Layouts).unwrap();
        let reply = wait(&worker).reply;
        // What the request itself was answered, for a session that keeps
        // an open's endpoint.
        assert!(matches!(reply.answered(), Reply::Layouts(Ok(_))));
        match reply {
            Reply::Grown { grown, then } => {
                let error = grown.result.unwrap_err();
                assert!(
                    error.message.contains("take_grown_schema exploded"),
                    "{error:?}"
                );
                assert!(matches!(*then, Reply::Layouts(Ok(_))));
            }
            other => panic!("unexpected reply {other:?}"),
        }
        // One switching to a save's window is the save's own failure, which
        // ends the window's wait for it, also when the loss probe that
        // follows panics too.
        windows.store(true, Ordering::SeqCst);
        lost.store(true, Ordering::SeqCst);
        let update = Request::UpdateRecord {
            layout_id: 1,
            record_id: 2,
            values: FieldChanges::new(),
        };
        worker.send(&ctx, 1, 1, update).unwrap();
        match wait(&worker).reply {
            Reply::Updated {
                layout_id: 1,
                record_id: 2,
                result: Err(err),
            } => assert!(err.message.contains("use_window exploded"), "{err:?}"),
            other => panic!("unexpected reply {other:?}"),
        }
    }

    /// A switch back that panics after a save stops the scripts, whose
    /// cleanup reverts a script transaction still open: a save it held is
    /// answered as not kept, one outside a transaction as kept (#313).
    #[test]
    fn a_save_a_reverted_transaction_held_is_not_answered_as_kept() {
        let held = Arc::new(AtomicBool::new(true));
        let provider = PanickingProvider {
            held: Arc::clone(&held),
            ..PanickingProvider::default()
        };
        let deleted = || Reply::Deleted {
            layout_id: 1,
            record_id: 2,
            result: Ok(()),
        };
        assert!(holds_in_transaction(&provider));
        match unkept_save(true, deleted()) {
            Reply::Deleted {
                result: Err(err), ..
            } => {
                assert!(err.message.contains("not saved"), "{err:?}")
            }
            other => panic!("unexpected reply {other:?}"),
        }
        // Inside the schema the worker grew answering it, too.
        let grown = Reply::Grown {
            grown: GrownSchema {
                result: Err(DataError::new("schema")),
                warnings: Vec::new(),
                sign_ins: Vec::new(),
            },
            then: Box::new(deleted()),
        };
        assert!(matches!(
            unkept_save(true, grown).answered(),
            Reply::Deleted { result: Err(_), .. }
        ));
        // A provider with no script transaction kept the save.
        held.store(false, Ordering::SeqCst);
        assert!(!holds_in_transaction(&provider));
        assert!(matches!(
            unkept_save(false, deleted()),
            Reply::Deleted { result: Ok(()), .. }
        ));
    }
}
