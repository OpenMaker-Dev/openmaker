//! The authoring service. Each command opens the file over a new
//! authenticated connection (`om_provider::NativeProvider`, the desktop
//! client's), does its work and closes it; `apply` then reads the script
//! back over another new connection. Nothing is cached between commands,
//! so every answer is the host's current state, and the CLI and the MCP
//! server behave the same.
//!
//! Development discovery reads metadata only: the open downloads the
//! schema and layout catalogs, scripts come from the script catalog, and
//! names from the calculation schema, which opens the file's related files
//! as the desktop client does. One command reads records: `list_choices`,
//! which reads a field control's value-list items through the provider's
//! calculation host (`om_data::choice`), as the desktop's controls offer
//! them. `choose_value_list_item` needs a desktop window, which this
//! service has not: it answers `no_window` and changes nothing.
//!
//! New fields are planned with the host's own code
//! (`om_data::field_catalog`) against the table's fields as stored, then
//! added by the host, which checks and allocates again under its locks;
//! `create_field` reads the table's fields back over a new connection.
//! Fields are placed on layouts the same way (`om_data::layout_placement`
//! against the layout's catalog member as stored); `place_field` reads the
//! layout back over a new connection and decodes it as the desktop client
//! does.
//!
//! `run_script` asks the host to run a script with Perform Script on Server
//! (`DataProvider::perform_script_on_server`, as the desktop client's step
//! does) and waits for it to end. The host runs it in a session of its own
//! with the configured account's privileges; this service only resolves the
//! script and reports the host's answer.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use om_data::choice::{self as value_choice, ChoiceError, ChoiceTarget, Unavailable};
use om_data::field_catalog::{self, NewField, Refusal, ScalarType};
use om_data::host::{CalcSchema, LayoutKind};
use om_data::layout_catalog;
use om_data::layout_move;
use om_data::layout_objects::{self, ObjectFrame};
use om_data::layout_placement::{self, Label, Placement};
use om_data::security_catalog::{AccountKind, RecordAccess, EDITING_UNSUPPORTED};
use om_data::value_lists::{CatalogList, FieldSource, ListSource, OccurrenceSource};
use om_data::{
    DataProvider, DatabaseSchema, FieldKind, GeomRect, SchemaField, SchemaFieldKind, Script,
    ScriptKind,
};
use om_hbam::design_paths::{self, is_stamp, Kind, Revision};
use om_hbam::slot::uint_segment;
use om_hbam::OwnedNode;
use om_layout::{LayoutObject, ObjectKind, PartKind, Rect};
use om_protocol::authoring::{self as wire, status};
use om_provider::{Authoring, AuthoringError, NativeProvider, ScriptDefinitions};
use om_scripts::display::{Lookup, StepNames, MAX_PASTE_BYTES, MAX_PASTE_STEPS};
use serde_json::{json, Value};

use crate::api::{
    self, ApplyParams, ArrangeObjectsParams, AuthorError, ChangePartsParams, ChoiceParams,
    ChooseParams, Command, CompletionParams, CreateFieldParams, CreateFileParams,
    CreateLayoutParams, DeleteLayoutParams, DeleteObjectsParams, DuplicateLayoutParams, EditParams,
    ErrorKind, FieldControlParams, FieldParams, FieldType, FrameParams, InspectParams,
    LayoutParams, LayoutSetupParams, LayoutWidthParams, MoveObjectsParams, ObjectFramesParams,
    OtherParams, Part, PartOptionsParams, PlaceFieldParams, PlacePortalParams, PlaceTextParams,
    PlaceWebViewerParams, PlacementParams, RunScriptParams, ScriptParams, TextParams,
    ValueListParams, ValueListValuesParams, API_VERSION,
};
use crate::catalog::{self, CommandSpec, Grant, Needs};
use crate::refs::{self, reference};
use crate::source::{self, Compiled};
use crate::target::{forget_password, Target};

/// The name of the text form scripts are given and taken in.
pub const SOURCE_FORMAT: &str = "openmaker.steps-text/v1";

/// The authoring service for one hosted file.
pub struct Service {
    target: Target,
}

/// An open file, closed when dropped.
pub(crate) struct Opened(NativeProvider);

impl Deref for Opened {
    type Target = NativeProvider;
    fn deref(&self) -> &NativeProvider {
        &self.0
    }
}

impl DerefMut for Opened {
    fn deref_mut(&mut self) -> &mut NativeProvider {
        &mut self.0
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// A script ready to edit: the file open, its scripts as stored and its
/// names.
struct Loaded {
    opened: Opened,
    definitions: ScriptDefinitions,
    names: Arc<CalcSchema>,
    id: u64,
}

impl Loaded {
    fn script(&self) -> &Script {
        self.definitions
            .catalog
            .scripts
            .iter()
            .find(|s| s.id == self.id)
            .expect("a resolved script")
    }

    fn definition(&self) -> &OwnedNode {
        &self.definitions.definitions[&self.id]
    }

    fn names(&self) -> &dyn StepNames {
        self.names.as_ref()
    }
}

/// A table ready for a new field: the file open, and the table's fields as
/// stored with their revision.
struct TableFields {
    opened: Opened,
    id: u64,
    name: String,
    catalog: Option<OwnedNode>,
    revision: Revision,
}

/// The revision of a table's fields `[T, 3]` (none: an empty catalog), as
/// the host computes it: the general form over the whole catalog
/// (`om_hbam::design_paths`).
fn fields_revision(catalog: Option<&OwnedNode>) -> Revision {
    match catalog {
        Some(catalog) => design_paths::catalog_revision(catalog),
        None => design_paths::catalog_revision(&OwnedNode::list(vec![3], Vec::new())),
    }
}

/// A script's revision, the edit token of its member `[17, 5, S]`.
fn script_revision(definition: &OwnedNode) -> Revision {
    design_paths::edit_token(Some(Kind::Script), definition)
}

/// A layout's revision, the edit token of its member `[4, 5, L]`.
fn layout_revision(member: &OwnedNode) -> Revision {
    design_paths::edit_token(Some(Kind::Layout), member)
}

fn scalar(field_type: FieldType) -> ScalarType {
    match field_type {
        FieldType::Text => ScalarType::Text,
        FieldType::Number => ScalarType::Number,
        FieldType::Date => ScalarType::Date,
        FieldType::Time => ScalarType::Time,
        FieldType::Timestamp => ScalarType::Timestamp,
    }
}

fn new_field(name: &str, field_type: FieldType, comment: &Option<String>) -> NewField {
    NewField {
        name: name.to_owned(),
        data_type: scalar(field_type),
        comment: comment.clone().filter(|c| !c.is_empty()),
    }
}

fn type_name(data_type: FieldKind) -> &'static str {
    match data_type {
        FieldKind::Text => "text",
        FieldKind::Number => "number",
        FieldKind::Date => "date",
        FieldKind::Time => "time",
        FieldKind::Timestamp => "timestamp",
        FieldKind::Container => "container",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A refusal of a planned field as an error.
fn refusal_error(refusal: Refusal, table: u64, file: &str) -> AuthorError {
    let message = format!("{refusal} Nothing was written.");
    match refusal {
        Refusal::Name(_) => AuthorError::new(ErrorKind::InvalidName, message),
        Refusal::Comment(_) => AuthorError::new(ErrorKind::InvalidRequest, message),
        Refusal::Exists { field, name } => AuthorError::new(ErrorKind::DuplicateName, message)
            .with_details(json!({
                "field": reference("field", format!("{table}.{field}"), file),
                "name": name,
            })),
        Refusal::Unsupported(_) => AuthorError::new(ErrorKind::Unsupported, message),
    }
}

/// A revision as text.
pub fn revision_text(revision: &Revision) -> String {
    design_paths::revision_text(revision)
}

/// A revision from text.
fn parse_revision(text: &str) -> Result<Revision, AuthorError> {
    design_paths::parse_revision(text).ok_or_else(|| {
        AuthorError::new(
            ErrorKind::InvalidRequest,
            format!("“{text}” is not a revision such as get_script returns (sha256:…)."),
        )
    })
}

/// The keys of a definition every edit keeps as stored: all but the step
/// list, the step data and the modification stamp.
fn preserved_keys(definition: &OwnedNode) -> Vec<String> {
    definition
        .children
        .iter()
        .filter(|c| !matches!(c.segment.as_slice(), [4] | [5]) && !is_stamp(&c.segment))
        .map(|c| c.segment.iter().map(|b| format!("{b:02x}")).collect())
        .collect()
}

fn kind_name(kind: ScriptKind) -> &'static str {
    match kind {
        ScriptKind::Script => "script",
        ScriptKind::Folder => "folder",
        ScriptKind::FolderEnd => "folder_end",
        ScriptKind::Separator => "separator",
    }
}

/// Whether and why not an authoring operation can be done.
fn availability(authoring: &Authoring) -> Value {
    match authoring {
        Authoring::Available {
            max_definition_bytes,
        } => json!({ "available": true, "max_definition_bytes": max_definition_bytes }),
        Authoring::Denied(reason) => {
            json!({ "available": false, "kind": "permission_denied", "reason": reason })
        }
        Authoring::Unavailable(reason) => {
            json!({ "available": false, "kind": "unsupported", "reason": reason })
        }
    }
}

/// Whether and why not fields can be added, and their types.
fn field_availability(authoring: &Authoring) -> Value {
    let mut value = availability(authoring);
    let object = value.as_object_mut().expect("an object");
    object.remove("max_definition_bytes");
    object.insert("types".into(), json!(ScalarType::ALL.map(ScalarType::name)));
    value
}

/// Whether and why not fields can be placed on layouts, and the forms that
/// can be placed.
fn placement_availability(authoring: &Authoring) -> Value {
    let mut value = availability(authoring);
    let object = value.as_object_mut().expect("an object");
    object.remove("max_definition_bytes");
    object.insert(
        "forms".into(),
        json!({
            "objects": ["edit_box", "text_label"],
            "fields": "the layout's own table's fields, not containers; one repetition",
            "parts": ["body"],
            "layouts": "fixed (not responsive) layouts whose objects OpenMaker reads",
            "stacking": "on top of every stored object",
            "not_over": ["group", "portal", "tab control", "slide control", "popover",
                         "button bar"],
        }),
    );
    value
}

/// Whether and why not layouts can be added, and the kind made.
fn new_layout_availability(authoring: &Authoring) -> Value {
    let mut value = availability(authoring);
    let object = value.as_object_mut().expect("an object");
    object.remove("max_definition_bytes");
    object.insert(
        "forms".into(),
        json!({
            "layouts": "a Form layout for a computer: one empty body part, no objects",
            "theme": "the theme and width of a layout of the file",
        }),
    );
    value
}

/// Whether and why not the host grants `grant`, with what it covers.
fn grant_availability(grant: Grant, authoring: &Authoring) -> Value {
    match grant {
        Grant::ScriptSteps => availability(authoring),
        Grant::Fields => field_availability(authoring),
        Grant::Layouts => placement_availability(authoring),
        Grant::NewLayouts => new_layout_availability(authoring),
        Grant::LayoutObjects
        | Grant::ObjectDeletion
        | Grant::Texts
        | Grant::LayoutCopies
        | Grant::LayoutDeletion
        | Grant::LayoutSetup
        | Grant::LayoutWidths
        | Grant::FieldControls
        | Grant::PartOptions
        | Grant::TextEditing
        | Grant::WebViewers
        | Grant::ValueLists => availability(authoring),
    }
}

/// An authoring operation that is not available as an error; `Ok` when it
/// is.
fn require(authoring: Authoring) -> Result<(), AuthorError> {
    match authoring {
        Authoring::Available { .. } => Ok(()),
        Authoring::Denied(reason) => Err(AuthorError::new(ErrorKind::PermissionDenied, reason)),
        Authoring::Unavailable(reason) => Err(AuthorError::new(ErrorKind::Unsupported, reason)),
    }
}

/// A command that needs a desktop window ([`Needs::Window`]), which this
/// service has not.
fn window_unavailable() -> Value {
    let error = ChoiceError::Unavailable(Unavailable::NoWindow);
    json!({
        "available": false,
        "kind": "unsupported",
        "unavailable": Unavailable::NoWindow.name(),
        "reason": error.message(),
    })
}

/// A value-list choice's error as the commands report it, with
/// FileMaker's error in `details.code`.
fn choice_error(error: ChoiceError) -> AuthorError {
    let kind = match &error {
        ChoiceError::NoControl | ChoiceError::NoValueList | ChoiceError::TargetGone => {
            ErrorKind::NotFound
        }
        ChoiceError::Refused(value_choice::Refusal::ReadOnly) => ErrorKind::PermissionDenied,
        ChoiceError::NotAnItem => ErrorKind::InvalidRequest,
        ChoiceError::Failed(_) => ErrorKind::Connection,
        ChoiceError::Unavailable(_)
        | ChoiceError::NotAMenuOrList
        | ChoiceError::ListUnsupported(_)
        | ChoiceError::Pending
        | ChoiceError::TooLarge
        | ChoiceError::OtherValues
        | ChoiceError::NoCommand(_)
        | ChoiceError::Refused(_) => ErrorKind::Unsupported,
    };
    let mut details = json!({ "code": error.code() });
    if let ChoiceError::Unavailable(what) = error {
        details["unavailable"] = json!(what.name());
    }
    AuthorError::new(kind, error.message()).with_details(details)
}

/// Why reading the file's security failed, as the commands report it.
/// Nothing is written by it, so nothing is said about writing.
fn security_error(error: AuthoringError) -> AuthorError {
    match error {
        AuthoringError::Refused {
            status: code,
            message,
            ..
        } => {
            let kind = match code {
                status::PERMISSION_DENIED => ErrorKind::PermissionDenied,
                status::FILE_MISSING => ErrorKind::NotFound,
                _ => ErrorKind::Unsupported,
            };
            AuthorError::new(kind, message).with_details(json!({ "status": code }))
        }
        AuthoringError::Unavailable(reason) => AuthorError::new(ErrorKind::Unsupported, reason),
        AuthoringError::NotSent(reason) | AuthoringError::OutcomeUnknown(reason) => {
            AuthorError::new(ErrorKind::Connection, reason)
        }
    }
}

/// A value-list catalog member as `get_value_lists` gives it: `id` is the
/// workspace's id (as `inspect` and `list_choices` give it), `filemaker_id`
/// the id FileMaker shows.
fn value_list_json(list: &CatalogList, file: &str) -> Value {
    let occurrence = |o: &OccurrenceSource| {
        json!({
            "ref": reference("occurrence", o.id, file),
            "name": o.name,
        })
    };
    let field = |f: &FieldSource| {
        json!({
            "occurrence": f.occurrence.as_ref().map(occurrence),
            "field_id": f.field,
            "field_name": f.field_name,
            "other_file": f.other_file,
            "repetition": f.repetition,
            "label": f.label(),
        })
    };
    let mut out = json!({
        "ref": reference("value_list", list.id, file),
        "id": list.id,
        "filemaker_id": list.shown_id,
        "name": list.name,
    });
    match &list.content {
        Err(unreadable) => {
            out["source"] = json!("unreadable");
            out["unreadable"] = json!({
                "reason": unreadable.code(),
                "message": unreadable.message(),
            });
        }
        Ok(definition) => {
            match &definition.source {
                ListSource::Custom { values } => {
                    out["source"] = json!("custom");
                    out["values"] = json!(values);
                }
                ListSource::Field {
                    primary,
                    secondary,
                    related_from,
                    sort_by_second,
                    second_only,
                } => {
                    out["source"] = json!("field");
                    out["field"] = field(primary);
                    out["second_field"] = json!(secondary.as_ref().map(field));
                    out["related_from"] = json!(related_from.as_ref().map(occurrence));
                    out["sort_by_second_field"] = json!(sort_by_second);
                    out["show_second_field_only"] = json!(second_only);
                }
                ListSource::External {
                    file: source,
                    list: key,
                } => {
                    out["source"] = json!("external");
                    out["file_reference"] = json!(source);
                    out["list_key"] = json!(key);
                }
                ListSource::Calculation => out["source"] = json!("calculation"),
            }
            out["sort_language"] = json!(definition
                .sort_language
                .as_ref()
                .map(|l| json!({"id": l.id, "name": l.name})));
            out["unknown_options"] = json!(definition.unknown_options);
        }
    }
    out
}

fn not_in_this_version(what: &str) -> Value {
    json!({
        "available": false,
        "kind": "unsupported",
        "reason": format!("{what} is not in {API_VERSION}."),
    })
}

fn invalid_source(diagnostics: Vec<source::Diagnostic>) -> AuthorError {
    let count = diagnostics.len();
    AuthorError::new(
        ErrorKind::InvalidSource,
        format!(
            "The text does not compile ({count} problem{}); nothing was written.",
            if count == 1 { "" } else { "s" }
        ),
    )
    .with_details(json!({ "diagnostics": diagnostics }))
}

/// The response to a command that needs no connection ([`Needs::Nothing`]),
/// or `None` for one that does: the CLI answers these without a host.
pub fn handle_offline(command: &Command) -> Option<Value> {
    match command {
        Command::Commands => Some(api::response(command.name(), Ok(catalog::describe()))),
        Command::CreateFile(params) => Some(api::response(command.name(), create_file(params))),
        Command::CompareDesigns(params) => Some(api::response(
            command.name(),
            crate::designs::compare_designs(params),
        )),
        _ => None,
    }
}

/// `create_file`: the new file's path, its name and the account that opens
/// it (`om_data::new_file::create`).
fn create_file(params: &CreateFileParams) -> Result<Value, AuthorError> {
    use om_data::new_file::{create, CreateError};
    match create(std::path::Path::new(&params.path)) {
        Ok(created) => Ok(json!({
            "path": created.path.display().to_string(),
            "name": created.name,
            "account": created.account,
            "password": "",
        })),
        Err(error @ CreateError::Exists(_)) => Err(AuthorError::new(
            ErrorKind::DuplicateName,
            error.to_string(),
        )),
        Err(error @ CreateError::Name(_)) => {
            Err(AuthorError::new(ErrorKind::InvalidName, error.to_string()))
        }
        Err(error @ CreateError::Write(_)) => {
            Err(AuthorError::new(ErrorKind::SaveFailed, error.to_string()))
        }
    }
}

/// A layout ready for a placement: the file open, the layout's catalog
/// member as stored with its revision, and the file's schema.
struct LoadedLayout {
    opened: Opened,
    id: u64,
    name: String,
    member: OwnedNode,
    /// The layout catalog's index `[4, 1]`, from the same download as
    /// `member`: its entry holds the layout's words when the member does not
    /// (`layout_catalog::layout_word`, #977).
    index: Option<OwnedNode>,
    revision: Revision,
    /// The occurrence it shows records of, and that occurrence's table of
    /// this file.
    occurrence: Option<u64>,
    table: Option<u64>,
    schema: DatabaseSchema,
}

/// A frame as the API gives it.
fn frame_json(frame: &Rect) -> Value {
    json!({
        "left": frame.left,
        "top": frame.top,
        "width": frame.width(),
        "height": frame.height(),
    })
}

fn layout_edit_refusal(refusal: layout_placement::Refusal) -> AuthorError {
    let kind = match refusal {
        layout_placement::Refusal::Invalid(_) => ErrorKind::InvalidRequest,
        layout_placement::Refusal::Unsupported(_) => ErrorKind::Unsupported,
    };
    AuthorError::new(kind, refusal.to_string())
}

fn object_frame_conflict(layout: &str, current: &Revision, expected: &Revision) -> AuthorError {
    AuthorError::new(
        ErrorKind::Conflict,
        format!(
            "{layout} changed since the revision the objects were moved on; nothing was written."
        ),
    )
    .with_details(json!({
        "current_revision": revision_text(current),
        "expected_revision": revision_text(expected),
    }))
}

fn rect(frame: &FrameParams) -> Rect {
    Rect {
        top: frame.top,
        left: frame.left,
        bottom: frame.top + frame.height,
        right: frame.left + frame.width,
        other: Vec::new(),
    }
}

fn wire_frame(frame: &Rect) -> wire::Frame {
    [frame.top, frame.left, frame.bottom, frame.right]
}

fn object_kind(object: &LayoutObject) -> String {
    let kind = match object.kind() {
        ObjectKind::Field => "field",
        ObjectKind::Text => "text",
        ObjectKind::Graphic => "graphic",
        ObjectKind::Line => "line",
        ObjectKind::Rectangle => "rectangle",
        ObjectKind::RoundedRectangle => "rounded_rectangle",
        ObjectKind::Oval => "oval",
        ObjectKind::Group => "group",
        ObjectKind::Portal => "portal",
        ObjectKind::Button => "button",
        ObjectKind::TabControl => "tab_control",
        ObjectKind::TabPanel => "tab_panel",
        ObjectKind::External if object.web_viewer().is_some() => "web_viewer",
        ObjectKind::External => "external",
        ObjectKind::PopoverButton => "popover_button",
        ObjectKind::Popover => "popover",
        ObjectKind::SlideControl => "slide_control",
        ObjectKind::SlidePanel => "slide_panel",
        ObjectKind::ButtonBar => "button_bar",
        ObjectKind::Unknown(t) => return format!("unknown_{t}"),
    };
    kind.to_owned()
}

fn part_kind(kind: PartKind) -> &'static str {
    match kind {
        PartKind::TitleHeader => "title_header",
        PartKind::Header => "header",
        PartKind::LeadingGrandSummary => "leading_grand_summary",
        PartKind::LeadingSubSummary => "leading_sub_summary",
        PartKind::Body => "body",
        PartKind::TrailingSubSummary => "trailing_sub_summary",
        PartKind::TrailingGrandSummary => "trailing_grand_summary",
        PartKind::Footer => "footer",
        PartKind::TitleFooter => "title_footer",
        PartKind::TopNavigation => "top_navigation",
        PartKind::BottomNavigation => "bottom_navigation",
        PartKind::Other(_) => "other",
    }
}

/// A UUID as `GetLayoutObjectAttribute` shows it (8-4-4-4-12, upper case).
fn uuid_text(uuid: &[u8; 16]) -> String {
    let hex: String = uuid.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// A planning refusal as an error.
fn placement_refusal(refusal: layout_placement::Refusal) -> AuthorError {
    let message = format!("{refusal} Nothing was written.");
    match refusal {
        layout_placement::Refusal::Invalid(_) => {
            AuthorError::new(ErrorKind::InvalidRequest, message)
        }
        layout_placement::Refusal::Unsupported(_) => {
            AuthorError::new(ErrorKind::Unsupported, message)
        }
    }
}

/// Whether every key below `old` is in `new` as it is in `old`: `new` only
/// gained entries.
fn only_gained(old: &OwnedNode, new: &OwnedNode) -> bool {
    old.value == new.value
        && old.children.iter().all(|child| {
            new.child(&child.segment)
                .is_some_and(|found| only_gained(child, found))
        })
}

/// The layout member's keys an edit keeps as stored: all but the field
/// reference index `01`, the parts `03` (whose lists of objects gain the
/// new ones; checked by `layout_placement::part_lists_extended`), the
/// object attributes `05`, the object store `07`, the object index `0a`
/// and the tab order `15` (which gain the edit box; checked by
/// `layout_placement::object_index_and_tab_order_as_planned`), the table
/// view `0d` and the modification stamp.
fn kept_layout_keys(member: &OwnedNode) -> Vec<&OwnedNode> {
    member
        .children
        .iter()
        .filter(|c| {
            !matches!(
                c.segment.as_slice(),
                [1] | [3] | [5] | [7] | [0x0a] | [0x0d] | [0x15]
            ) && !is_stamp(&c.segment)
        })
        .collect()
}

impl Service {
    pub fn new(target: Target) -> Self {
        Self { target }
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The response to a request in JSON (`api::parse_request`).
    pub fn handle_json(&mut self, request: &str) -> Value {
        match api::parse_request(request) {
            Ok(command) => self.handle(&command),
            Err(error) => api::response("", Err(error)),
        }
    }

    /// The response to `command`.
    pub fn handle(&mut self, command: &Command) -> Value {
        api::response(command.name(), self.run(command))
    }

    /// Run `command`.
    pub fn run(&mut self, command: &Command) -> Result<Value, AuthorError> {
        match command {
            Command::Commands => Ok(catalog::describe()),
            Command::CreateFile(params) => create_file(params),
            Command::CompareDesigns(params) => crate::designs::compare_designs(params),
            Command::Capabilities => self.capabilities(),
            Command::Inspect(params) => self.inspect(params),
            Command::GetScript(params) => self.get_script(params),
            Command::Validate(params) => self.validate(params),
            Command::Plan(params) => self.plan(params),
            Command::Apply(params) => self.apply(params),
            Command::PlanField(params) => self.plan_field(params),
            Command::CreateField(params) => self.create_field(params),
            Command::GetLayout(params) => self.get_layout(params),
            Command::PlanFieldPlacement(params) => self.plan_field_placement(params),
            Command::PlaceField(params) => self.place_field(params),
            Command::CreateLayout(params) => self.create_layout(params),
            Command::SetObjectFrames(params) => self.set_object_frames(params),
            Command::ArrangeObjects(params) => self.arrange_objects(params),
            Command::MoveObjects(params) => self.move_objects(params),
            Command::DeleteObjects(params) => self.delete_objects(params),
            Command::PlaceText(params) => self.place_text(params),
            Command::PlaceWebViewer(params) => self.place_web_viewer(params),
            Command::PlacePortal(params) => self.place_portal(params),
            Command::DuplicateLayout(params) => self.duplicate_layout(params),
            Command::DeleteLayout(params) => self.delete_layout(params),
            Command::LayoutSetup(params) => self.layout_setup(params),
            Command::SetLayoutWidth(params) => self.set_layout_width(params),
            Command::SetFieldControl(params) => self.set_field_control(params),
            Command::SetPartOptions(params) => self.set_part_options(params),
            Command::ChangeParts(params) => self.change_parts(params),
            Command::SetText(params) => self.set_text(params),
            Command::RunScript(params) => self.run_script(params),
            Command::ListChoices(params) => self.list_choices(params),
            Command::ChooseValueListItem(params) => self.choose_value_list_item(params),
            Command::EnterOtherValue(params) => self.enter_other_value(params),
            Command::GetValueListValues(params) => self.get_value_list_values(params),
            Command::SetValueListValues(params) => self.set_value_list_values(params),
            Command::GetSecurity => self.get_security(),
            Command::GetValueLists(_) => self.get_value_lists(),
            Command::CompleteCalculation(params) => self.complete_calculation(params),
            Command::FindUses(params) => self.find_uses(params),
            Command::BrokenReferences(params) => self.broken_references(params),
            Command::UnusedObjects(params) => self.unused_objects(params),
            Command::ScriptCalls(params) => self.script_calls(params),
        }
    }

    /// The file's name as references give it.
    pub(crate) fn file(&self) -> String {
        om_data::file_refs::hosted_name(&self.target.file)
    }

    pub(crate) fn open(&self) -> Result<Opened, AuthorError> {
        let mut provider = NativeProvider::new(self.target.tls.clone());
        let mut connection = self.target.connection();
        let opened = provider.open_metadata(&connection);
        forget_password(&mut connection);
        opened.map_err(|e| {
            let hint = if e.code == Some(om_provider::CLIENT_LICENSE_REFUSED) {
                ". Check OPENMAKER_LICENSE_KEY, OPENMAKER_LICENSE_FILE, or license_file in the author config."
            } else {
                ""
            };
            AuthorError::new(
                ErrorKind::Connection,
                format!(
                    "Could not open {} on {}:{} as {}: {e}{hint}",
                    self.target.file, self.target.host, self.target.port, self.target.account
                ),
            )
        })?;
        Ok(Opened(provider))
    }

    pub(crate) fn read_error(e: om_data::DataError) -> AuthorError {
        AuthorError::new(ErrorKind::Connection, e.to_string())
    }

    /// Whether the host grants `grant` to this connection.
    fn granted(opened: &mut Opened, grant: Grant) -> Result<Authoring, AuthorError> {
        match grant {
            Grant::ScriptSteps => opened.script_authoring(),
            Grant::Fields => opened.field_authoring(),
            Grant::Layouts => opened.layout_authoring(),
            Grant::NewLayouts => opened.layout_creation(),
            Grant::LayoutObjects => opened.object_editing(),
            Grant::ObjectDeletion => opened.object_deletion(),
            Grant::Texts => opened.text_placement(),
            Grant::LayoutCopies => opened.layout_duplication(),
            Grant::LayoutDeletion => opened.layout_deletion(),
            Grant::LayoutSetup => opened.layout_setting(),
            Grant::LayoutWidths => opened.layout_widths(),
            Grant::FieldControls => opened.field_controls(),
            Grant::PartOptions => opened.part_options(),
            Grant::TextEditing => opened.text_editing(),
            Grant::WebViewers => opened.web_viewer_placement(),
            Grant::ValueLists => opened.value_list_editing(),
        }
        .map_err(Self::read_error)
    }

    /// Whether `spec` is available on this connection, as the commands
    /// that report it give it: a command the host need not grant always is.
    fn available(opened: &mut Opened, spec: &CommandSpec) -> Result<Value, AuthorError> {
        Ok(match spec.needs {
            Needs::Grant(grant) => grant_availability(grant, &Self::granted(opened, grant)?),
            Needs::Nothing | Needs::File => json!({ "available": true }),
            Needs::Window => window_unavailable(),
        })
    }

    /// `spec`'s grant as an error when the host does not give it; nothing is
    /// sent then.
    fn require_grant(opened: &mut Opened, spec: &CommandSpec) -> Result<(), AuthorError> {
        match spec.needs {
            Needs::Grant(grant) => require(Self::granted(opened, grant)?),
            Needs::Nothing | Needs::File => Ok(()),
            Needs::Window => Err(choice_error(ChoiceError::Unavailable(
                Unavailable::NoWindow,
            ))),
        }
    }

    /// Open the file and find the script `target` names.
    fn load(&self, target: &ScriptParams) -> Result<Loaded, AuthorError> {
        let mut opened = self.open()?;
        let definitions = opened.script_definitions().map_err(Self::read_error)?;
        let names = opened
            .calc_schema()
            .map_err(Self::read_error)?
            .ok_or_else(|| {
                AuthorError::new(
                    ErrorKind::Unsupported,
                    "The file's names are not available.",
                )
            })?;
        let id = self.resolve(&definitions, target)?;
        Ok(Loaded {
            opened,
            definitions,
            names,
            id,
        })
    }

    fn resolve(
        &self,
        definitions: &ScriptDefinitions,
        target: &ScriptParams,
    ) -> Result<u64, AuthorError> {
        let file = self.file();
        let scripts = || {
            definitions
                .catalog
                .scripts
                .iter()
                .filter(|s| s.kind == ScriptKind::Script)
        };
        let id = match (&target.script, &target.script_name) {
            (Some(reference), None) => refs::script_id(reference, &file)?,
            (None, Some(name)) => {
                match Lookup::by_name(scripts().map(|s| (s.id, s.name.as_str())), name) {
                    Lookup::One(id) => id,
                    Lookup::Missing => {
                        return Err(AuthorError::new(
                            ErrorKind::NotFound,
                            format!("{file} has no script named “{name}”."),
                        ))
                    }
                    Lookup::Ambiguous => {
                        let candidates: Vec<String> = scripts()
                            .filter(|s| s.name.to_lowercase() == name.to_lowercase())
                            .map(|s| reference("script", s.id, &file))
                            .collect();
                        return Err(AuthorError::new(
                            ErrorKind::AmbiguousReference,
                            format!(
                                "Several scripts of {file} are named “{name}”; name one by its \
                                 reference."
                            ),
                        )
                        .with_details(json!({ "candidates": candidates })));
                    }
                }
            }
            _ => {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    "Name the script with either `script` (a reference from inspect) or \
                     `script_name`.",
                ))
            }
        };
        let script = definitions
            .catalog
            .scripts
            .iter()
            .find(|s| s.id == id)
            .filter(|_| definitions.definitions.contains_key(&id))
            .ok_or_else(|| {
                AuthorError::new(ErrorKind::NotFound, format!("{file} has no script {id}."))
            })?;
        if script.kind != ScriptKind::Script {
            return Err(AuthorError::new(
                ErrorKind::InvalidRequest,
                format!(
                    "Member {id} of {file}'s script catalog is a {}, which has no steps.",
                    kind_name(script.kind).replace('_', " ")
                ),
            ));
        }
        Ok(id)
    }

    /// The folders each catalog member is in, by id.
    fn folders(definitions: &ScriptDefinitions) -> std::collections::BTreeMap<u64, Vec<String>> {
        let mut path: Vec<String> = Vec::new();
        let mut out = std::collections::BTreeMap::new();
        for script in &definitions.catalog.scripts {
            match script.kind {
                ScriptKind::FolderEnd => {
                    path.pop();
                }
                kind => {
                    out.insert(script.id, path.clone());
                    if kind == ScriptKind::Folder {
                        path.push(script.name.clone());
                    }
                }
            }
        }
        out
    }

    fn script_json(&self, script: &Script, definitions: &ScriptDefinitions) -> Value {
        let folder = Self::folders(definitions)
            .remove(&script.id)
            .unwrap_or_default();
        let mut value = json!({
            "ref": reference("script", script.id, &self.file()),
            "id": script.id,
            "name": script.name,
            "kind": kind_name(script.kind),
            "folder": folder,
            "in_menu": script.in_scripts_menu(),
        });
        if script.kind == ScriptKind::Script {
            value["full_access"] = json!(script.runs_with_full_access());
            value["steps"] = json!(script.steps.len());
            if let Some(definition) = definitions.definitions.get(&script.id) {
                value["revision"] = json!(revision_text(&script_revision(definition)));
            }
        }
        value
    }

    /// The file's security catalog (`om_data::security_catalog`), as the
    /// host decodes it: never a password or other credential.
    fn get_security(&mut self) -> Result<Value, AuthorError> {
        let file = self.file();
        let mut opened = self.open()?;
        let catalog = opened.security().map_err(security_error)?;
        let set_name = |key: &Option<Vec<u8>>| {
            key.as_deref()
                .and_then(|key| catalog.set(key))
                .and_then(|set| set.name.clone())
        };
        let accounts: Vec<Value> = catalog
            .accounts
            .iter()
            .map(|account| {
                json!({
                    "key": hex(&account.key),
                    "name": account.display_name(),
                    "guest": account.is_guest(),
                    "type": match account.kind {
                        Some(AccountKind::FileMakerFile) => json!("filemaker_file"),
                        Some(AccountKind::Other(kind)) => json!({ "other": kind }),
                        None => Value::Null,
                    },
                    "active": account.active,
                    "must_change_password": account.must_change_password,
                    "privilege_set": account.privilege_set.as_deref().map(hex),
                    "privilege_set_name": set_name(&account.privilege_set),
                })
            })
            .collect();
        let sets: Vec<Value> = catalog
            .privilege_sets
            .iter()
            .map(|set| {
                let access = set.record_access();
                json!({
                    "key": hex(&set.key),
                    "name": set.name,
                    "record_flags": set.record_flags.as_deref().map(hex),
                    "record_access": (access != RecordAccess::Unknown).then(|| access.label()),
                    "password_days": set.password_days,
                    "accounts": catalog
                        .accounts_in(&set.key)
                        .map(|a| a.display_name())
                        .collect::<Vec<_>>(),
                    "extended_privileges": catalog
                        .extended_of(&set.key)
                        .map(|e| e.keyword.as_deref())
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let extended: Vec<Value> = catalog
            .extended_privileges
            .iter()
            .map(|privilege| {
                json!({
                    "key": hex(&privilege.key),
                    "keyword": privilege.keyword,
                    "privilege_sets": privilege
                        .sets
                        .iter()
                        .map(|key| json!({ "key": hex(key), "name": set_name(&Some(key.clone())) }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({
            "file": file,
            "accounts": accounts,
            "privilege_sets": sets,
            "extended_privileges": extended,
            "editable": false,
            "not_editable_reason": EDITING_UNSUPPORTED,
            "not_decoded": "File access, privilege set descriptions, and layout, field, script, \
                            value list and menu access are not decoded (issue #137).",
        }))
    }

    fn capabilities(&mut self) -> Result<Value, AuthorError> {
        let mut opened = self.open()?;
        let product = opened.host_application_version().unwrap_or_default();
        // What this version does not do, then each command's operation.
        let mut operations = json!({
            "create_script": not_in_this_version("Creating scripts"),
            "rename_script": not_in_this_version("Renaming scripts"),
            "delete_script": not_in_this_version("Deleting scripts"),
            "reorder_scripts": not_in_this_version("Moving scripts and folders"),
            "script_options": not_in_this_version(
                "Changing a script's menu or full-access options"
            ),
            "schema": not_in_this_version(
                "Creating tables, changing or deleting fields, calculation, summary, \
                 container and global fields, field options and relationships"
            ),
            "layouts": not_in_this_version(
                "Changing layouts other than adding a Form layout with an empty body and \
                 placing a field (with an optional text label) in a layout's body"
            ),
            "security": {
                "available": false,
                "kind": "unsupported",
                "reason": EDITING_UNSUPPORTED,
            },
            "value_lists": not_in_this_version("Creating, changing or deleting value lists"),
        });
        for spec in catalog::ALL {
            if let Some(operation) = spec.operation {
                operations[operation] = Self::available(&mut opened, spec)?;
            }
        }
        // Reading the file's security needs the host's operation and an
        // account of the file in its [Full Access] set, which only the host
        // can tell: ask it.
        if let Some(operation) = catalog::GET_SECURITY.operation {
            operations[operation] = match opened.security() {
                Ok(_) => json!({ "available": true }),
                Err(error) => {
                    let error = security_error(error);
                    json!({ "available": false, "kind": error.kind, "reason": error.message })
                }
            };
        }
        let max_definition = catalog::APPLY
            .operation
            .and_then(|apply| operations[apply].get("max_definition_bytes"))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(json!({
            "api_version": API_VERSION,
            "source_format": SOURCE_FORMAT,
            "host": {
                "host": self.target.host,
                "port": self.target.port,
                "product": product,
            },
            "file": self.file(),
            "account": self.target.account,
            "operations": operations,
            "limits": {
                "max_source_bytes": MAX_PASTE_BYTES,
                "max_steps": MAX_PASTE_STEPS,
                "max_definition_bytes": max_definition,
                "max_field_name_chars": field_catalog::MAX_NAME_CHARS,
                "max_comment_chars": field_catalog::MAX_COMMENT_CHARS,
                "max_label_chars": layout_placement::MAX_LABEL_CHARS,
                "max_coordinate": layout_placement::MAX_COORDINATE,
            },
            "records": "read only by list_choices: the records a value list's items come from",
        }))
    }

    /// The items field control `params.object` offers on record
    /// `params.record` (`om_data::choice`).
    fn list_choices(&mut self, params: &ChoiceParams) -> Result<Value, AuthorError> {
        let file = self.file();
        let (layout, object) = refs::layout_object_id(&params.object, &file)?;
        let mut opened = self.open()?;
        let listed = opened
            .value_list_choices(ChoiceTarget {
                layout,
                object,
                record: params.record,
            })
            .map_err(choice_error)?;
        let items: Vec<Value> = listed
            .choices
            .items
            .iter()
            .map(|item| {
                json!({
                    "value": item.value,
                    "label": item.label,
                    "divider": listed.choices.is_divider(item),
                })
            })
            .collect();
        Ok(json!({
            "object": params.object,
            "record": params.record,
            "value_list": {
                "ref": reference("value_list", listed.list.id, &file),
                "name": listed.list.name,
            },
            "items": items,
        }))
    }

    /// Choosing an item needs a desktop window's focus, draft and triggers
    /// (`om_data::choice::choose`), which this service has not: the typed
    /// `no_window`, after checking the reference names this file. Nothing
    /// is read or changed.
    fn choose_value_list_item(&mut self, params: &ChooseParams) -> Result<Value, AuthorError> {
        refs::layout_object_id(&params.object, &self.file())?;
        Err(choice_error(ChoiceError::Unavailable(
            Unavailable::NoWindow,
        )))
    }

    /// A custom value list as Edit Value List opens it (#869): its name,
    /// values and revision, read now through the provider.
    fn get_value_list_values(&mut self, params: &ValueListParams) -> Result<Value, AuthorError> {
        let file = self.file();
        let list = refs::value_list_id(&params.value_list, &file)?;
        let mut opened = self.open()?;
        let custom = opened.custom_value_list(list).map_err(Self::read_error)?;
        Ok(json!({
            "value_list": reference("value_list", list, &file),
            "name": custom.name,
            "values": custom.values,
            "revision": revision_text(&custom.revision),
        }))
    }

    /// Edit Value List's OK (#869): the list's values through the host's
    /// authoring operation 24, then read back over a new connection.
    fn set_value_list_values(
        &mut self,
        params: &ValueListValuesParams,
    ) -> Result<Value, AuthorError> {
        let file = self.file();
        let list = refs::value_list_id(&params.value_list, &file)?;
        let expected = parse_revision(&params.expected_revision)?;
        let list_ref = reference("value_list", list, &file);
        let mut opened = self.open()?;
        Self::require_grant(&mut opened, &catalog::SET_VALUE_LIST_VALUES)?;
        let set = opened.value_list_values_on_host(wire::ValueListValues {
            expected,
            list,
            values: params.values.clone(),
        });
        drop(opened);
        let label = list_ref.clone();
        let set = set.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The values of {list_ref} were sent but no answer came back \
                         ({reason}), so they may or may not have been saved. Get the list \
                         again, or repeat the same set_value_list_values: it reports \
                         already_applied if they are there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let fresh = self
            .open()
            .and_then(|mut fresh| fresh.custom_value_list(list).map_err(Self::read_error));
        let verified = match fresh {
            Ok(fresh) => {
                if fresh.revision != set.revision || fresh.values != params.values {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported the values of {list_ref} saved, but a new \
                             connection reads others."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&set.revision),
                        "found_revision": revision_text(&fresh.revision),
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the values saved, but reading them back failed: {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if set.changed { "set" } else { "already_applied" },
            "value_list": list_ref,
            "values": params.values,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&set.revision),
            "verified": verified,
        }))
    }

    /// Other… needs a desktop window too (`om_data::choice::other_value`):
    /// the typed `no_window`, after checking the reference names this file.
    fn enter_other_value(&mut self, params: &OtherParams) -> Result<Value, AuthorError> {
        refs::layout_object_id(&params.object, &self.file())?;
        Err(choice_error(ChoiceError::Unavailable(
            Unavailable::NoWindow,
        )))
    }

    /// The file's value-list catalog (`om_data::value_lists`), the one
    /// File > Manage > Value Lists shows, through the provider's
    /// `value_list_catalog`.
    fn get_value_lists(&mut self) -> Result<Value, AuthorError> {
        let file = self.file();
        let mut opened = self.open()?;
        let catalog = opened.value_list_catalog().map_err(Self::read_error)?;
        let lists: Vec<Value> = catalog
            .lists
            .iter()
            .map(|list| value_list_json(list, &file))
            .collect();
        Ok(json!({
            "file": file,
            "editable": catalog.read_only.is_none(),
            "read_only": catalog.read_only.map(|reason| json!({
                "reason": reason.code(),
                "message": reason.message(),
            })),
            "value_lists": lists,
        }))
    }

    /// The names completing the one typed at `params.cursor`
    /// (`om_data::formula_editor::completion`), from the file's names as
    /// the host gives them. Positions are characters of `params.text`.
    fn complete_calculation(&mut self, params: &CompletionParams) -> Result<Value, AuthorError> {
        const MAX_TEXT_BYTES: usize = 1_000_000;
        const MAX_LIMIT: usize = 1000;
        let invalid = |message: String| AuthorError::new(ErrorKind::InvalidRequest, message);
        if params.text.len() > MAX_TEXT_BYTES {
            return Err(invalid(format!(
                "The text is {} bytes; at most {MAX_TEXT_BYTES} are completed.",
                params.text.len()
            )));
        }
        let limit = params.limit.unwrap_or(50);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(invalid(format!("limit must be 1 to {MAX_LIMIT}.")));
        }
        let length = params.text.chars().count();
        let cursor = params.cursor.unwrap_or(length);
        if cursor > length {
            return Err(invalid(format!(
                "cursor {cursor} is past the end of the text ({length} characters)."
            ))
            .with_details(json!({ "cursor": cursor, "length": length })));
        }
        let file = self.file();
        let wanted = match (&params.occurrence, &params.occurrence_name) {
            (Some(_), Some(_)) => {
                return Err(invalid(
                    "Name the occurrence with either `occurrence` or `occurrence_name`.".into(),
                ))
            }
            (Some(r), None) => Some(refs::occurrence_id(r, &file)?),
            _ => None,
        };
        let mut opened = self.open()?;
        let schema = opened.calc_schema().map_err(Self::read_error)?;
        // The context: the named occurrence, else the first, as the dialog
        // opens a new calculation. Without the file's names there is none.
        let context = match (schema.as_deref(), wanted, &params.occurrence_name) {
            (None, _, _) => None,
            (Some(s), Some(id), _) => Some(s.occurrence(id).ok_or_else(|| {
                AuthorError::new(
                    ErrorKind::NotFound,
                    format!("{file} has no occurrence {id}."),
                )
            })?),
            (Some(s), None, Some(name)) => Some(s.occurrence_named(name).ok_or_else(|| {
                AuthorError::new(
                    ErrorKind::NotFound,
                    format!("{file} has no occurrence named “{name}”."),
                )
            })?),
            (Some(s), None, None) => s.occurrences_in_order().first().copied(),
        };
        let scope = om_data::formula_editor::Scope {
            occurrence: context.map(|o| o.id),
            unqualified: false,
        };
        let completion =
            om_data::formula_editor::complete(&params.text, cursor, scope, schema.as_deref())
                .map_err(|e| {
                    invalid(format!("cursor {} is past the end of the text.", e.cursor))
                })?;
        let candidates: Vec<Value> = completion
            .candidates
            .iter()
            .take(limit)
            .map(|c| {
                let at = completion.replace.start;
                let mut value = json!({
                    "kind": c.kind.as_str(),
                    "name": c.name,
                    "label": c.label,
                    "insert": c.insertion.text,
                    "selection_after": {
                        "start": at + c.insertion.select.start,
                        "end": at + c.insertion.select.end,
                    },
                    "category": c.category,
                });
                if let Some((occurrence, field)) = c.field {
                    value["field"] = json!(schema
                        .as_deref()
                        .and_then(|s| { completion_field_reference(s, occurrence, field, &file) }));
                    value["occurrence"] = json!(schema
                        .as_deref()
                        .map(|s| { completion_occurrence_reference(s, occurrence, &file) }));
                }
                value
            })
            .collect();
        let source = |unavailable: Option<om_data::formula_editor::Unavailable>| match unavailable {
            None => json!({ "available": true }),
            Some(reason) => json!({ "available": false, "reason": reason.as_str() }),
        };
        Ok(json!({
            "units": "characters",
            "length": length,
            "cursor": cursor,
            "context": context.map(|o| json!({
                "ref": schema.as_deref().map(|s| {
                    completion_occurrence_reference(s, o.id, &file)
                }),
                "name": o.name,
            })),
            "prefix": completion.prefix,
            "replace": { "start": completion.replace.start, "end": completion.replace.end },
            "refused": completion.refused.map(|r| r.as_str()),
            "candidates": candidates,
            "total": completion.candidates.len(),
            "truncated": completion.candidates.len() > limit,
            "sources": {
                "functions": { "available": true },
                "fields": source(completion.fields),
                "custom_functions": source(completion.custom_functions),
            },
        }))
    }

    fn inspect(&mut self, params: &InspectParams) -> Result<Value, AuthorError> {
        let wanted = |part: Part| {
            params
                .include
                .as_ref()
                .is_none_or(|parts| parts.contains(&part))
        };
        let file = self.file();
        let mut opened = self.open()?;
        let mut out = json!({ "file": file });
        if wanted(Part::Scripts) {
            let definitions = opened.script_definitions().map_err(Self::read_error)?;
            let scripts: Vec<Value> = definitions
                .catalog
                .scripts
                .iter()
                .map(|s| self.script_json(s, &definitions))
                .collect();
            out["scripts"] = json!(scripts);
        }
        if wanted(Part::Tables) || wanted(Part::Occurrences) {
            let snapshot = opened
                .database_schema_snapshot()
                .map_err(Self::read_error)?;
            let schema = snapshot.schema;
            let catalogs = snapshot.field_catalogs;
            if wanted(Part::Tables) {
                let tables: Vec<Value> = schema
                    .tables
                    .iter()
                    .map(|t| {
                        let fields: Vec<Value> = t
                            .fields
                            .iter()
                            .map(|f| {
                                json!({
                                    "ref": reference("field", format!("{}.{}", t.id, f.id), &file),
                                    "id": f.id,
                                    "name": f.name,
                                    "kind": match f.kind {
                                        SchemaFieldKind::Normal => "normal",
                                        SchemaFieldKind::Calculation => "calculation",
                                        SchemaFieldKind::Summary => "summary",
                                        SchemaFieldKind::Unknown(_) => "unknown",
                                    },
                                    "type": type_name(f.data_type),
                                })
                            })
                            .collect();
                        json!({
                            "ref": reference("table", t.id, &file),
                            "id": t.id,
                            "name": t.name,
                            "fields_revision": revision_text(&fields_revision(catalogs.get(&t.id))),
                            "fields": fields,
                        })
                    })
                    .collect();
                out["tables"] = json!(tables);
            }
            if wanted(Part::Occurrences) {
                let names = opened.calc_schema().map_err(Self::read_error)?;
                let occurrences: Vec<Value> = schema
                    .occurrences
                    .iter()
                    .map(|o| {
                        let mut value = json!({
                            "ref": reference("occurrence", o.id, &file),
                            "id": o.id,
                            "name": o.name,
                            "table": o.table_id.map(|t| reference("table", t, &file)),
                        });
                        if o.table_id.is_none() {
                            // Another file's table: named by its data source,
                            // never by a table of this file.
                            value["data_source"] = json!(names
                                .as_ref()
                                .and_then(|n| n.external_occurrence(o.id))
                                .map(|e| e.source.clone()));
                        }
                        value
                    })
                    .collect();
                out["occurrences"] = json!(occurrences);
            }
        }
        if wanted(Part::Layouts) || wanted(Part::ValueLists) {
            let names = opened
                .calc_schema()
                .map_err(Self::read_error)?
                .ok_or_else(|| {
                    AuthorError::new(
                        ErrorKind::Unsupported,
                        "The file's names are not available.",
                    )
                })?;
            if wanted(Part::Layouts) {
                let layouts: Vec<Value> = names
                    .layouts()
                    .iter()
                    .map(|l| {
                        json!({
                            "ref": reference("layout", l.id, &file),
                            "id": l.id,
                            "name": l.name,
                            "kind": match l.kind {
                                LayoutKind::Layout => "layout",
                                LayoutKind::Folder => "folder",
                                LayoutKind::FolderEnd => "folder_end",
                            },
                            "occurrence": l.occurrence.map(|o| reference("occurrence", o, &file)),
                        })
                    })
                    .collect();
                out["layouts"] = json!(layouts);
                let index = opened.layout_index().map_err(Self::read_error)?;
                out["layouts_revision"] = json!(revision_text(&layouts_revision(index.as_ref())));
            }
            if wanted(Part::ValueLists) {
                let lists: Vec<Value> = names
                    .value_lists()
                    .iter()
                    .map(|l| {
                        json!({
                            "ref": reference("value_list", l.id, &file),
                            "id": l.id,
                            "name": l.name,
                        })
                    })
                    .collect();
                out["value_lists"] = json!(lists);
            }
        }
        if wanted(Part::DataSources) {
            let sources: Vec<Value> = opened
                .database_sources()
                .map_err(Self::read_error)?
                .iter()
                .map(|s| {
                    json!({
                        "ref": reference("data_source", s.id, &file),
                        "id": s.id,
                        "name": s.name,
                    })
                })
                .collect();
            out["data_sources"] = json!(sources);
        }
        let warnings = opened.related_file_warnings().to_vec();
        if !warnings.is_empty() {
            out["related_file_warnings"] = json!(warnings);
        }
        Ok(out)
    }

    fn get_script(&mut self, params: &ScriptParams) -> Result<Value, AuthorError> {
        let mut loaded = self.load(params)?;
        let script = loaded.script();
        let (text, steps) = source::script_text(&script.steps, loaded.names());
        let count = |fidelity: &str| steps.iter().filter(|s| s.fidelity == fidelity).count();
        let mut out = json!({
            "script": self.script_json(script, &loaded.definitions),
            "revision": revision_text(&script_revision(loaded.definition())),
            "source_format": SOURCE_FORMAT,
            "source": text,
            "steps": steps,
            "opaque": {
                "steps_with_stored_options": count("extras"),
                "steps_stored_whole": count("raw"),
                "unreferenced_step_data": script.unreferenced_data.len(),
                "kept_keys": preserved_keys(loaded.definition()),
            },
        });
        out["apply"] = Self::available(&mut loaded.opened, &catalog::APPLY)?;
        Ok(out)
    }

    /// Compile `source` against the loaded script.
    fn compile(loaded: &Loaded, source: &str) -> Result<Compiled, AuthorError> {
        source::compile(loaded.script(), loaded.definition(), loaded.names(), source)
            .map_err(invalid_source)
    }

    /// A conflict: the script is at `current`, not `expected`.
    fn conflict(&self, id: u64, current: &Revision, expected: &Revision) -> AuthorError {
        AuthorError::new(
            ErrorKind::Conflict,
            format!(
                "{} changed since the revision the text was edited from; nothing was written.",
                reference("script", id, &self.file())
            ),
        )
        .with_details(json!({
            "current_revision": revision_text(current),
            "expected_revision": revision_text(expected),
        }))
    }

    fn validate(&mut self, params: &EditParams) -> Result<Value, AuthorError> {
        let loaded = self.load(&params.target())?;
        let current = script_revision(loaded.definition());
        let script = self.script_json(loaded.script(), &loaded.definitions);
        match source::compile(
            loaded.script(),
            loaded.definition(),
            loaded.names(),
            &params.source,
        ) {
            Ok(compiled) => {
                let after = script_revision(&compiled.definition);
                if let Some(expected) = &params.expected_revision {
                    let expected = parse_revision(expected)?;
                    if expected != current && after != current {
                        return Err(self.conflict(loaded.id, &current, &expected));
                    }
                }
                Ok(json!({
                    "valid": true,
                    "script": script,
                    "base_revision": revision_text(&current),
                    "after_revision": revision_text(&after),
                    "steps": {
                        "total": compiled.script.steps.len(),
                        "kept": compiled.kept,
                        "new": compiled.new,
                        "deleted": compiled.deleted,
                    },
                }))
            }
            Err(diagnostics) => Ok(json!({
                "valid": false,
                "script": script,
                "base_revision": revision_text(&current),
                "diagnostics": diagnostics,
            })),
        }
    }

    fn plan(&mut self, params: &EditParams) -> Result<Value, AuthorError> {
        let mut loaded = self.load(&params.target())?;
        let compiled = Self::compile(&loaded, &params.source)?;
        let current = script_revision(loaded.definition());
        let after = script_revision(&compiled.definition);
        let expected = params
            .expected_revision
            .as_deref()
            .map(parse_revision)
            .transpose()?;
        if let Some(expected) = &expected {
            if *expected != current && after != current {
                return Err(self.conflict(loaded.id, &current, expected));
            }
        }
        let names = loaded.names();
        let (before_text, _) = source::script_text(&loaded.script().steps, names);
        let (after_text, _) = source::script_text(&compiled.script.steps, names);
        let label = reference("script", loaded.id, &self.file());
        let diff = source::unified_diff(
            &before_text,
            &after_text,
            &format!("{label} {}", revision_text(&current)),
            &format!("{label} {}", revision_text(&after)),
        );
        let normalized =
            after_text.trim_end_matches('\n') != params.source.trim_end_matches(['\n', '\r']);
        let matches_later_revision = after == current && expected.is_some_and(|e| e != current);
        let native_save_unconfirmed = matches_later_revision
            && loaded
                .opened
                .uses_native_script_save()
                .map_err(Self::read_error)?;
        let mut out = json!({
            "script": self.script_json(loaded.script(), &loaded.definitions),
            "base_revision": revision_text(&current),
            "after_revision": revision_text(&after),
            "changed": after != current,
            "already_applied": matches_later_revision && !native_save_unconfirmed,
            "steps": {
                "before": loaded.script().steps.len(),
                "after": compiled.script.steps.len(),
                "kept": compiled.kept,
                "new": compiled.new,
                "deleted": compiled.deleted,
            },
            "diff": diff,
            "after_source": after_text,
            "normalized": normalized,
            "kept_keys": preserved_keys(loaded.definition()),
        });
        if native_save_unconfirmed {
            out["native_save_unconfirmed"] = json!(true);
        }
        out["apply"] = Self::available(&mut loaded.opened, &catalog::APPLY)?;
        Ok(out)
    }

    fn apply(&mut self, params: &ApplyParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let expected_after = params
            .expected_after_revision
            .as_deref()
            .map(parse_revision)
            .transpose()?;
        let mut loaded = self.load(&params.target())?;
        let compiled = Self::compile(&loaded, &params.source)?;
        let id = loaded.id;
        let label = reference("script", id, &self.file());
        let current = script_revision(loaded.definition());
        let after = script_revision(&compiled.definition);
        if let Some(planned) = expected_after.filter(|planned| *planned != after) {
            return Err(AuthorError::new(
                ErrorKind::Conflict,
                format!(
                    "The text now compiles to another result than the plan did (the file's names \
                     may have changed); nothing was written to {label}."
                ),
            )
            .with_details(json!({
                "after_revision": revision_text(&after),
                "expected_after_revision": revision_text(&planned),
            })));
        }
        let steps = json!({
            "before": loaded.script().steps.len(),
            "after": compiled.script.steps.len(),
            "kept": compiled.kept,
            "new": compiled.new,
            "deleted": compiled.deleted,
        });
        if after == current {
            if expected != current {
                let reason = match loaded.opened.uses_native_script_save() {
                    Ok(false) => None,
                    Ok(true) => Some("The earlier native index upload cannot be verified.".into()),
                    Err(error) => Some(format!(
                        "The host's script save path could not be confirmed ({}).",
                        error.message
                    )),
                };
                if let Some(reason) = reason {
                    return Err(AuthorError::new(
                        ErrorKind::OutcomeUnknown,
                        format!("The member for {label} has the requested steps. {reason}"),
                    )
                    .with_details(json!({
                        "previous_revision": revision_text(&expected),
                        "after_revision": revision_text(&after),
                        "member_content_matches": true,
                        "index_verified": false,
                    })));
                }
            }
            return Ok(json!({
                "status": if expected == current { "unchanged" } else { "already_applied" },
                "script": label,
                "revision": revision_text(&current),
                "steps": steps,
            }));
        }
        if expected != current {
            return Err(self.conflict(id, &current, &expected));
        }
        Self::require_grant(&mut loaded.opened, &catalog::APPLY)?;
        let base = loaded.definition().clone();
        // The step save the desktop's Script Workspace makes too.
        let replaced = loaded
            .opened
            .save_script_steps(&compiled.script, &base, expected);
        drop(loaded);
        let replaced = replaced.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The edit of {label} was sent but could not be confirmed ({reason}). A \
                         fresh script read can compare the member with after_revision and \
                         previous_revision. A native save also needs its index upload confirmed."
                    ),
                )
                .with_details(json!({
                    "previous_revision": revision_text(&current),
                    "after_revision": revision_text(&after),
                }))
            })
        })?;
        let verified = self.verify(id, &after, &base)?;
        Ok(json!({
            "status": if replaced.changed { "applied" } else { "already_applied" },
            "script": label,
            "previous_revision": revision_text(&current),
            "revision": revision_text(&replaced.revision),
            "steps": steps,
            "verified": verified,
        }))
    }

    /// Read script `id` back over a new connection: it must be at revision
    /// `after`, with every key but its steps and stamp as in `base`.
    fn verify(&self, id: u64, after: &Revision, base: &OwnedNode) -> Result<Value, AuthorError> {
        let unverified = |e: AuthorError| {
            json!({
                "fresh_connection": false,
                "reason": format!("The host reported the edit saved, but reading it back failed: {}", e.message),
            })
        };
        let mut fresh = match self.open() {
            Ok(fresh) => fresh,
            Err(e) => return Ok(unverified(e)),
        };
        let definitions = match fresh.script_definitions() {
            Ok(definitions) => definitions,
            Err(e) => return Ok(unverified(Self::read_error(e))),
        };
        let label = reference("script", id, &self.file());
        let Some(definition) = definitions.definitions.get(&id) else {
            return Err(AuthorError::new(
                ErrorKind::VerificationFailed,
                format!("The host reported {label} saved, but a new connection does not find it."),
            ));
        };
        let found = script_revision(definition);
        let kept = preserved_keys(base);
        let kept_as_stored = preserved_keys(definition) == kept
            && base
                .children
                .iter()
                .filter(|c| !matches!(c.segment.as_slice(), [4] | [5]) && !is_stamp(&c.segment))
                .all(|c| definition.child(&c.segment) == Some(c));
        if found != *after || !kept_as_stored {
            return Err(AuthorError::new(
                ErrorKind::VerificationFailed,
                format!(
                    "The host reported {label} saved, but a new connection reads another \
                     definition."
                ),
            )
            .with_details(json!({
                "expected_revision": revision_text(after),
                "found_revision": revision_text(&found),
                "other_keys_kept": kept_as_stored,
            })));
        }
        Ok(json!({
            "fresh_connection": true,
            "revision": revision_text(&found),
            "kept_keys": kept,
        }))
    }

    /// Open the file and read the fields of the table `table` or
    /// `table_name` names.
    fn load_table(
        &self,
        table: &Option<String>,
        table_name: &Option<String>,
    ) -> Result<TableFields, AuthorError> {
        let file = self.file();
        let mut opened = self.open()?;
        let schema = opened.database_schema().map_err(Self::read_error)?;
        let tables = || schema.tables.iter().map(|t| (t.id, t.name.as_str()));
        let id = match (table, table_name) {
            (Some(reference), None) => refs::table_id(reference, &file)?,
            (None, Some(name)) => match Lookup::by_name(tables(), name) {
                Lookup::One(id) => id,
                Lookup::Missing => {
                    return Err(AuthorError::new(
                        ErrorKind::NotFound,
                        format!("{file} has no table named “{name}”."),
                    ))
                }
                Lookup::Ambiguous => {
                    let candidates: Vec<String> = tables()
                        .filter(|(_, n)| n.to_lowercase() == name.to_lowercase())
                        .map(|(id, _)| reference("table", id, &file))
                        .collect();
                    return Err(AuthorError::new(
                        ErrorKind::AmbiguousReference,
                        format!(
                            "Several tables of {file} are named “{name}”; name one by its \
                             reference."
                        ),
                    )
                    .with_details(json!({ "candidates": candidates })));
                }
            },
            _ => {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    "Name the table with either `table` (a reference from inspect) or \
                     `table_name`.",
                ))
            }
        };
        let name = tables()
            .find(|(t, _)| *t == id)
            .map(|(_, name)| name.to_owned())
            .ok_or_else(|| {
                AuthorError::new(ErrorKind::NotFound, format!("{file} has no table {id}."))
            })?;
        let catalog = opened.field_catalog(id).map_err(Self::read_error)?;
        let revision = fields_revision(catalog.as_ref());
        Ok(TableFields {
            opened,
            id,
            name,
            catalog,
            revision,
        })
    }

    fn table_json(&self, table: &TableFields) -> Value {
        json!({
            "ref": reference("table", table.id, &self.file()),
            "id": table.id,
            "name": table.name,
        })
    }

    fn field_ref(&self, table: u64, field: u64) -> String {
        reference("field", format!("{table}.{field}"), &self.file())
    }

    /// The field of `table` that `field` would make, already there: its id.
    fn made(table: &TableFields, field: &NewField) -> Option<u64> {
        let (id, member) = field_catalog::same_name(table.catalog.as_ref()?, &field.name)?;
        field_catalog::is_made_by(member, field).then_some(id)
    }

    /// A conflict: the table's fields are at `current`, not `expected`.
    fn fields_conflict(&self, table: &TableFields, expected: &Revision) -> AuthorError {
        AuthorError::new(
            ErrorKind::Conflict,
            format!(
                "The fields of {} changed since the revision the field was planned against; \
                 nothing was written.",
                reference("table", table.id, &self.file())
            ),
        )
        .with_details(json!({
            "current_revision": revision_text(&table.revision),
            "expected_revision": revision_text(expected),
        }))
    }

    fn plan_field(&mut self, params: &FieldParams) -> Result<Value, AuthorError> {
        let expected = params
            .expected_revision
            .as_deref()
            .map(parse_revision)
            .transpose()?;
        let mut table = self.load_table(&params.table, &params.table_name)?;
        let field = new_field(&params.name, params.field_type, &params.comment);
        let file = self.file();
        let create = Self::available(&mut table.opened, &catalog::CREATE_FIELD)?;
        let stale = expected.is_some_and(|e| e != table.revision);
        if let Some(id) = Self::made(&table, &field).filter(|_| stale) {
            return Ok(json!({
                "table": self.table_json(&table),
                "base_revision": revision_text(&table.revision),
                "already_applied": true,
                "field": { "ref": self.field_ref(table.id, id), "id": id, "name": field.name },
                "create": create,
            }));
        }
        if let Some(expected) = expected.filter(|_| stale) {
            return Err(self.fields_conflict(&table, &expected));
        }
        let planned = field_catalog::plan(table.catalog.as_ref(), &field)
            .map_err(|r| refusal_error(r, table.id, &file))?;
        let changes: Vec<Value> = planned
            .changes
            .iter()
            .map(|(key, holds)| json!({ "key": key, "holds": holds }))
            .collect();
        Ok(json!({
            "table": self.table_json(&table),
            "base_revision": revision_text(&table.revision),
            "already_applied": false,
            "field": {
                "ref": self.field_ref(table.id, planned.field),
                "id": planned.field,
                "name": field.name,
                "type": field.data_type.name(),
                "kind": "normal",
                "storage": "stored",
                "comment": field.comment,
                "auto_enter": null,
                "validation": null,
                "indexing": "as needed",
                "repetitions": 1,
                "definition": hex(&planned.definition),
                "position": planned.position,
            },
            "id_allocation": "The host allocates the id when it adds the field: the next after \
                              every id the table issued, this one unless the table changes first.",
            "changes": changes,
            "create": create,
        }))
    }

    fn create_field(&mut self, params: &CreateFieldParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let mut table = self.load_table(&params.table, &params.table_name)?;
        let field = new_field(&params.name, params.field_type, &params.comment);
        let file = self.file();
        let table_id = table.id;
        let table_ref = reference("table", table_id, &file);
        let field_json = |id: u64| {
            json!({
                "ref": self.field_ref(table_id, id),
                "id": id,
                "name": field.name,
                "type": field.data_type.name(),
            })
        };
        if expected != table.revision {
            // An earlier create whose answer was lost: nothing to write.
            if let Some(id) = Self::made(&table, &field) {
                return Ok(json!({
                    "status": "already_applied",
                    "table": table_ref,
                    "field": field_json(id),
                    "revision": revision_text(&table.revision),
                }));
            }
            return Err(self.fields_conflict(&table, &expected));
        }
        field_catalog::plan(table.catalog.as_ref(), &field)
            .map_err(|r| refusal_error(r, table.id, &file))?;
        Self::require_grant(&mut table.opened, &catalog::CREATE_FIELD)?;
        let base = table.catalog.clone();
        let created = table.opened.create_field(wire::CreateField {
            table: table_id,
            expected,
            data_type: field.data_type.code(),
            name: field.name.clone(),
            comment: field.comment.clone().unwrap_or_default(),
        });
        drop(table);
        let label = format!("the fields of {table_ref}");
        let created = created.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The new field “{}” was sent but no answer came back ({reason}), so it \
                         may or may not have been added. Inspect {table_ref} again, or repeat \
                         the same create_field: it reports already_applied if the field is \
                         there.",
                        field.name
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let verified = self.verify_field(table_id, &created, &field, base.as_ref())?;
        Ok(json!({
            "status": if created.changed { "created" } else { "already_applied" },
            "table": table_ref,
            "field": field_json(created.field),
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&created.revision),
            "verified": verified,
        }))
    }

    /// Read table `table`'s fields back over a new connection: they must be
    /// at the revision the host reported, with the new field as `field`
    /// makes it, listed in the schema, and every field of `base` as it was.
    fn verify_field(
        &self,
        table: u64,
        created: &wire::Created,
        field: &NewField,
        base: Option<&OwnedNode>,
    ) -> Result<Value, AuthorError> {
        let unverified = |e: AuthorError| {
            json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the field added, but reading it back failed: {}",
                    e.message
                ),
            })
        };
        let mut fresh = match self.open() {
            Ok(fresh) => fresh,
            Err(e) => return Ok(unverified(e)),
        };
        let catalog = match fresh.field_catalog(table) {
            Ok(catalog) => catalog,
            Err(e) => return Ok(unverified(Self::read_error(e))),
        };
        let schema = match fresh.database_schema() {
            Ok(schema) => schema,
            Err(e) => return Ok(unverified(Self::read_error(e))),
        };
        let found = fields_revision(catalog.as_ref());
        let member = catalog
            .as_ref()
            .and_then(|c| c.at(&[&[5], &uint_segment(created.field)]));
        let made = member.is_some_and(|m| field_catalog::is_made_by(m, field));
        let listed = schema
            .tables
            .iter()
            .find(|t| t.id == table)
            .and_then(|t| t.fields.iter().find(|f| f.id == created.field))
            .is_some_and(|f| f.name == field.name && f.kind == SchemaFieldKind::Normal);
        let others = base.map(field_catalog::members).unwrap_or_default();
        let kept = others
            .iter()
            .all(|old| catalog.as_ref().and_then(|c| c.at(&[&[5], &old.segment])) == Some(old));
        if found != created.revision || !made || !listed || !kept {
            return Err(AuthorError::new(
                ErrorKind::VerificationFailed,
                format!(
                    "The host reported {} added, but a new connection reads other fields.",
                    self.field_ref(table, created.field)
                ),
            )
            .with_details(json!({
                "expected_revision": revision_text(&created.revision),
                "found_revision": revision_text(&found),
                "field_as_planned": made,
                "listed_in_schema": listed,
                "other_fields_kept": kept,
            })));
        }
        Ok(json!({
            "fresh_connection": true,
            "revision": revision_text(&found),
            "listed_in_schema": true,
            "other_fields_kept": others.len(),
        }))
    }
}

impl Service {
    /// Arrange objects relative to one another through the guarded frame writer.
    fn arrange_objects(&mut self, params: &ArrangeObjectsParams) -> Result<Value, AuthorError> {
        use om_data::layout_arrange::{arrange, Arrangement};
        let arrangement = Arrangement::from_id(&params.arrangement).ok_or_else(|| {
            let ids: Vec<&str> = Arrangement::ALL.iter().map(|a| a.id()).collect();
            AuthorError::new(
                ErrorKind::InvalidRequest,
                format!(
                    "“{}” is no arrangement; the arrangements are {}.",
                    params.arrangement,
                    ids.join(", ")
                ),
            )
        })?;
        if params.objects.len() < arrangement.minimum() {
            return Err(AuthorError::new(
                ErrorKind::InvalidRequest,
                format!(
                    "{} needs at least {} objects.",
                    arrangement.id(),
                    arrangement.minimum()
                ),
            ));
        }
        let expected = parse_revision(&params.expected_revision)?;
        let layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let layout_ref = self.layout_ref(layout.id);
        let frames = self.movable_layout_frames(&layout, &params.objects)?;
        let changed = arrange(&frames, arrangement).map_err(layout_edit_refusal)?;
        if changed.is_empty() {
            // Arranged so already: at the expected revision nothing is to
            // change; at another, as after a request whose answer was lost,
            // the objects are as asked (set_object_frames' already_applied).
            return Ok(json!({
                "status": if expected == layout.revision { "unchanged" } else { "already_applied" },
                "layout": layout_ref,
                "arrangement": arrangement.id(),
                "revision": revision_text(&layout.revision),
            }));
        }
        let mut result =
            self.write_object_frames(layout.id, &changed, &params.expected_revision)?;
        if let Some(fields) = result.as_object_mut() {
            fields.insert("arrangement".into(), json!(arrangement.id()));
        }
        Ok(result)
    }

    fn move_objects(&mut self, params: &MoveObjectsParams) -> Result<Value, AuthorError> {
        let max = layout_objects::MAX_FRAMES;
        if !(1..=max).contains(&params.objects.len()) {
            return Err(AuthorError::new(
                ErrorKind::InvalidRequest,
                format!("A request moves 1 to {max} objects."),
            ));
        }
        let expected = parse_revision(&params.expected_revision)?;
        let layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let layout_ref = self.layout_ref(layout.id);
        // A retry with an old base must not compute the relative offset
        // again from frames that may already include the first move.
        if expected != layout.revision {
            return Err(object_frame_conflict(
                &layout_ref,
                &layout.revision,
                &expected,
            ));
        }
        let mut objects = self.movable_layout_frames(&layout, &params.objects)?;
        let plan = layout_move::plan(
            objects.iter().map(|object| GeomRect {
                top: object.frame.top,
                left: object.frame.left,
                bottom: object.frame.bottom,
                right: object.frame.right,
            }),
            layout_move::Delta {
                dx: params.dx,
                dy: params.dy,
            },
        )
        .map_err(layout_edit_refusal)?;
        if plan.delta == layout_move::Delta::default() {
            return Ok(json!({
                "status": "unchanged",
                "layout": layout_ref,
                "revision": revision_text(&layout.revision),
            }));
        }
        for (object, frame) in objects.iter_mut().zip(plan.frames) {
            object.frame.top = frame.top;
            object.frame.left = frame.left;
            object.frame.bottom = frame.bottom;
            object.frame.right = frame.right;
        }
        let id = layout.id;
        drop(layout);
        self.write_object_frames(id, &objects, &params.expected_revision)
    }

    fn movable_layout_frames(
        &self,
        layout: &LoadedLayout,
        objects: &[String],
    ) -> Result<Vec<ObjectFrame>, AuthorError> {
        let file = self.file();
        let layout_ref = self.layout_ref(layout.id);
        let (_, store) =
            layout_placement::stored_objects(&layout.member).map_err(layout_edit_refusal)?;
        let decoded = layout_placement::decode(&layout.member).ok_or_else(|| {
            AuthorError::new(
                ErrorKind::Unsupported,
                format!("OpenMaker cannot read {layout_ref}'s objects."),
            )
        })?;
        let mut frames = Vec::with_capacity(objects.len());
        for object in objects {
            let (on, key) = refs::layout_object_id(object, &file)?;
            let stored = decoded.objects.get(key).filter(|_| on == layout.id);
            let Some(bounds) = stored.and_then(|o| o.bounds.clone()) else {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    format!("{object} is not an object of {layout_ref} with a frame."),
                ));
            };
            if frames.iter().any(|f: &ObjectFrame| f.object == key) {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    format!("{object} is named twice."),
                ));
            }
            // Every object, the ones that stay included: one that cannot be
            // moved must not anchor the others.
            layout_objects::movable(&store, key).map_err(layout_edit_refusal)?;
            frames.push(ObjectFrame {
                object: key,
                frame: bounds,
            });
        }
        Ok(frames)
    }

    fn write_object_frames(
        &mut self,
        layout: u64,
        frames: &[ObjectFrame],
        expected_revision: &str,
    ) -> Result<Value, AuthorError> {
        self.set_object_frames(&ObjectFramesParams {
            layout: Some(self.layout_ref(layout)),
            layout_name: None,
            objects: frames
                .iter()
                .map(|f| crate::api::ObjectFrameParams {
                    object: self.object_ref(layout, f.object),
                    frame: FrameParams {
                        left: f.frame.left,
                        top: f.frame.top,
                        width: f.frame.right - f.frame.left,
                        height: f.frame.bottom - f.frame.top,
                    },
                })
                .collect(),
            expected_revision: expected_revision.into(),
        })
    }

    fn set_object_frames(&mut self, params: &ObjectFramesParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let file = self.file();
        let mut layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let layout_ref = self.layout_ref(layout.id);
        let mut frames = Vec::with_capacity(params.objects.len());
        for object in &params.objects {
            let (on, key) = refs::layout_object_id(&object.object, &file)?;
            if on != layout.id {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    format!("{} is not an object of {layout_ref}.", object.object),
                ));
            }
            frames.push(ObjectFrame {
                object: key,
                frame: rect(&object.frame),
            });
        }
        let objects: Vec<String> = frames
            .iter()
            .map(|f| self.object_ref(layout.id, f.object))
            .collect();
        if expected != layout.revision {
            // An earlier request whose answer was lost: nothing to write.
            if layout_objects::already_framed(&layout.member, &frames) {
                return Ok(json!({
                    "status": "already_applied",
                    "layout": layout_ref,
                    "objects": objects,
                    "revision": revision_text(&layout.revision),
                }));
            }
            return Err(object_frame_conflict(
                &layout_ref,
                &layout.revision,
                &expected,
            ));
        }
        layout_objects::plan(&layout.member, &frames).map_err(layout_edit_refusal)?;
        Self::require_grant(&mut layout.opened, &catalog::SET_OBJECT_FRAMES)?;
        let edges = |r: &Rect| [r.top, r.left, r.bottom, r.right];
        let framed = layout.opened.set_object_frames(wire::SetObjectFrames {
            layout: layout.id,
            expected,
            frames: frames.iter().map(|f| (f.object, edges(&f.frame))).collect(),
        });
        drop(layout);
        let label = layout_ref.clone();
        let framed = framed.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The new frames were sent but no answer came back ({reason}), so they may \
                         or may not have been saved. Get {layout_ref} again to check the objects' \
                         current frames."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        // Read back over a new connection: the layout at the revision the
        // host reported, each object at its frame.
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh) => {
                let at = layout_objects::already_framed(&fresh.member, &frames);
                if fresh.revision != framed.revision || !at {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported the objects of {layout_ref} moved, but a new \
                             connection reads other frames."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&framed.revision),
                        "found_revision": revision_text(&fresh.revision),
                        "objects_at_frames": at,
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the objects moved, but reading the layout back failed: {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if framed.changed { "moved" } else { "already_applied" },
            "layout": layout_ref,
            "objects": objects,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&framed.revision),
            "verified": verified,
        }))
    }

    /// Place a text on a layout's body on its own
    /// (`om_data::layout_placement::plan_text`) through the host's authoring
    /// extension, then read the layout back over a new connection.
    fn place_text(&mut self, params: &PlaceTextParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        layout_placement::check_label(&params.text).map_err(placement_refusal)?;
        let mut layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let text = layout_placement::Label {
            text: params.text.clone(),
            frame: rect(&params.frame),
        };
        let layout_id = layout.id;
        let layout_ref = self.layout_ref(layout_id);
        if expected != layout.revision {
            // A lost answer's text may be there; the host decides.
            if layout_placement::already_placed_text(&layout.member, &text).is_none() {
                return Err(self.layout_conflict(&layout, &expected));
            }
        } else {
            layout_placement::plan_text(&layout.member, &text).map_err(placement_refusal)?;
        }
        Self::require_grant(&mut layout.opened, &catalog::PLACE_TEXT)?;
        let placed = layout.opened.place_text(wire::PlaceText {
            layout: layout_id,
            expected,
            frame: wire_frame(&text.frame),
            text: text.text.clone(),
        });
        drop(layout);
        let label = layout_ref.clone();
        let placed = placed.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The text was sent but no answer came back ({reason}), so it may or may \
                         not have been placed. Get {layout_ref} again, or repeat the same \
                         place_text: it reports already_applied if it is there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let object = self.object_ref(layout_id, placed.field_object);
        // Read back over a new connection: the layout at the revision the
        // host reported, the text on top as placed.
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh) => {
                let there = layout_placement::already_placed_text(&fresh.member, &text)
                    == Some(placed.field_object);
                if fresh.revision != placed.revision || !there {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported the text placed on {layout_ref}, but a new \
                             connection reads another revision or does not find it on top."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&placed.revision),
                        "found_revision": revision_text(&fresh.revision),
                        "text_on_top": there,
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the text placed, but reading the layout back failed: {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if placed.changed { "placed" } else { "already_applied" },
            "layout": layout_ref,
            "object": object,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&placed.revision),
            "verified": verified,
        }))
    }

    /// Place a web viewer on a layout's body (#896,
    /// `layout_placement::plan_web_viewer`): its address compiled against
    /// the file's names in the layout's occurrence, its options as
    /// FileMaker Pro stores them, checked against the layout as stored,
    /// sent, and read back over a new connection.
    fn place_web_viewer(&mut self, params: &PlaceWebViewerParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let mut setup = layout_placement::WebViewerSetup::DEFAULT;
        for (name, on) in params.options.iter().flatten() {
            let option = match name.as_str() {
                "allow_interaction" => &mut setup.interaction,
                "display_in_find_mode" => &mut setup.find_mode,
                "display_progress_bar" => &mut setup.progress_bar,
                "display_status_messages" => &mut setup.status_messages,
                "encode_url" => &mut setup.encode_url,
                "allow_javascript_scripts" => &mut setup.javascript,
                other => {
                    return Err(AuthorError::new(
                        ErrorKind::InvalidRequest,
                        format!(
                            "Web Viewer Setup has no option {other:?}: allow_interaction, \
                             display_in_find_mode, display_progress_bar, \
                             display_status_messages, encode_url or allow_javascript_scripts."
                        ),
                    ))
                }
            };
            *option = *on;
        }
        let flags = setup.flags().ok_or_else(|| {
            AuthorError::new(
                ErrorKind::Unsupported,
                "OpenMaker does not write this combination of Web Viewer Setup's options yet: \
                 what FileMaker Pro stores for it is not measured. The defaults, the defaults \
                 with one option changed, and interaction, Find mode and JavaScript scripts on \
                 with the rest off (with or without the progress bar), are written.",
            )
        })?;
        let mut layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let schema = layout
            .opened
            .calc_schema()
            .map_err(Self::read_error)?
            .ok_or_else(|| {
                AuthorError::new(
                    ErrorKind::Unsupported,
                    "The file's names are not available, so the web address cannot be compiled.",
                )
            })?;
        let (formatted, polish) =
            layout_placement::compile_address(&params.address, &schema, layout.occurrence)
                .map_err(placement_refusal)?;
        let viewer = layout_placement::WebViewerPlacement {
            frame: rect(&params.frame),
            flags,
            formatted,
            polish,
        };
        let layout_id = layout.id;
        let layout_ref = self.layout_ref(layout_id);
        if expected != layout.revision {
            // A lost answer's viewer may be there; the host decides.
            if layout_placement::already_placed_web_viewer(&layout.member, &viewer).is_none() {
                return Err(self.layout_conflict(&layout, &expected));
            }
        } else {
            layout_placement::plan_web_viewer(&layout.member, &viewer, schema.names())
                .map_err(placement_refusal)?;
        }
        Self::require_grant(&mut layout.opened, &catalog::PLACE_WEB_VIEWER)?;
        let placed = layout.opened.place_web_viewer(wire::PlaceWebViewer {
            layout: layout_id,
            expected,
            frame: wire_frame(&viewer.frame),
            flags: viewer.flags,
            formatted: viewer.formatted.clone(),
            polish: viewer.polish.clone(),
        });
        drop(layout);
        let label = layout_ref.clone();
        let placed = placed.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The web viewer was sent but no answer came back ({reason}), so it may \
                         or may not have been placed. Get {layout_ref} again, or repeat the same \
                         place_web_viewer: it reports already_applied if it is there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let object = self.object_ref(layout_id, placed.field_object);
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh) => {
                let there = layout_placement::already_placed_web_viewer(&fresh.member, &viewer)
                    == Some(placed.field_object);
                if fresh.revision != placed.revision || !there {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported the web viewer placed on {layout_ref}, but a new \
                             connection reads another revision or does not find it on top."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&placed.revision),
                        "found_revision": revision_text(&fresh.revision),
                        "web_viewer_on_top": there,
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the web viewer placed, but reading the layout back \
                     failed: {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if placed.changed { "placed" } else { "already_applied" },
            "layout": layout_ref,
            "object": object,
            "options": format!("{flags:#06x}"),
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&placed.revision),
            "verified": verified,
        }))
    }

    /// Place a portal on a layout's body (operation 21,
    /// `layout_placement::plan_portal`): its occurrence and fields found in
    /// the layout's schema, checked against the layout as stored, sent, and
    /// read back over a new connection.
    fn place_portal(&mut self, params: &PlacePortalParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let mut layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let file = self.file();
        let schema = &layout.schema;
        let occurrences = || schema.occurrences.iter().map(|o| (o.id, o.name.as_str()));
        let occurrence = match (&params.occurrence, &params.occurrence_name) {
            (Some(reference), None) => refs::occurrence_id(reference, &file)?,
            (None, Some(name)) => match Lookup::by_name(occurrences(), name) {
                Lookup::One(id) => id,
                Lookup::Missing => {
                    return Err(AuthorError::new(
                        ErrorKind::NotFound,
                        format!("{file} has no table occurrence named “{name}”."),
                    ))
                }
                Lookup::Ambiguous => {
                    return Err(AuthorError::new(
                        ErrorKind::AmbiguousReference,
                        format!(
                            "Several table occurrences of {file} are named “{name}”; name one \
                             by its reference."
                        ),
                    ))
                }
            },
            _ => {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    "Name the portal's table occurrence with either `occurrence` (a reference \
                     from inspect) or `occurrence_name`.",
                ))
            }
        };
        let table = schema
            .occurrences
            .iter()
            .find(|o| o.id == occurrence)
            .and_then(|o| o.table_id)
            .ok_or_else(|| {
                AuthorError::new(
                    ErrorKind::NotFound,
                    format!(
                        "{file} has no table occurrence {occurrence} showing one of its tables."
                    ),
                )
            })?;
        let table_fields = schema
            .tables
            .iter()
            .find(|t| t.id == table)
            .map(|t| t.fields.as_slice())
            .unwrap_or_default();
        let mut fields = Vec::with_capacity(params.field_names.len());
        for name in &params.field_names {
            let items = table_fields.iter().map(|f| (f.id, f.name.as_str()));
            fields.push(match Lookup::by_name(items, name) {
                Lookup::One(id) => id,
                Lookup::Missing => {
                    return Err(AuthorError::new(
                        ErrorKind::NotFound,
                        format!(
                            "{} has no field named “{name}”.",
                            reference("table", table, &file)
                        ),
                    ))
                }
                Lookup::Ambiguous => {
                    return Err(AuthorError::new(
                        ErrorKind::AmbiguousReference,
                        format!("Several fields are named “{name}”."),
                    ))
                }
            });
        }
        let placement = layout_placement::PortalPlacement {
            occurrence,
            frame: rect(&params.frame),
            rows: params.rows,
            fields,
        };
        let layout_id = layout.id;
        let layout_ref = self.layout_ref(layout_id);
        if expected != layout.revision {
            // A lost answer's portal may be there; the host decides.
            if layout_placement::already_placed_portal(&layout.member, &placement).is_none() {
                return Err(self.layout_conflict(&layout, &expected));
            }
        } else {
            layout_placement::plan_portal(&layout.member, &placement, None)
                .map_err(placement_refusal)?;
        }
        Self::require_grant(&mut layout.opened, &catalog::PLACE_PORTAL)?;
        let placed = layout.opened.place_portal(wire::PlacePortal {
            layout: layout_id,
            expected,
            table,
            occurrence,
            frame: wire_frame(&placement.frame),
            rows: placement.rows,
            fields: placement.fields.clone(),
        });
        drop(layout);
        let label = layout_ref.clone();
        let placed = placed.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The portal was sent but no answer came back ({reason}), so it may or \
                         may not have been placed. Get {layout_ref} again, or repeat the same \
                         place_portal: it reports already_applied if it is there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        // Read back over a new connection: the layout at the revision the
        // host reported, the portal on top with its fields as placed.
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh) => {
                let found = layout_placement::already_placed_portal(&fresh.member, &placement);
                let there = found.as_ref().map(|(portal, _)| *portal) == Some(placed.field_object);
                if fresh.revision != placed.revision || !there {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported the portal placed on {layout_ref}, but a new \
                             connection reads another revision or does not find it on top."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&placed.revision),
                        "found_revision": revision_text(&fresh.revision),
                        "portal_on_top": there,
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the portal placed, but reading the layout back failed: {}",
                    e.message
                ),
            }),
        };
        let portal = placed.field_object;
        let fields: Vec<String> = (1..=placement.fields.len() as u64)
            .map(|i| self.object_ref(layout_id, portal + i))
            .collect();
        Ok(json!({
            "status": if placed.changed { "placed" } else { "already_applied" },
            "layout": layout_ref,
            "portal": self.object_ref(layout_id, portal),
            "fields": fields,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&placed.revision),
            "verified": verified,
        }))
    }

    /// Delete objects of a layout (`om_data::layout_objects`) through the
    /// host's authoring extension, then read the layout back over a new
    /// connection.
    fn delete_objects(&mut self, params: &DeleteObjectsParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let file = self.file();
        let mut layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let layout_ref = self.layout_ref(layout.id);
        let mut keys = Vec::with_capacity(params.objects.len());
        for object in &params.objects {
            let (on, key) = refs::layout_object_id(object, &file)?;
            if on != layout.id {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    format!("{object} is not an object of {layout_ref}."),
                ));
            }
            keys.push(key);
        }
        let objects: Vec<String> = keys
            .iter()
            .map(|key| self.object_ref(layout.id, *key))
            .collect();
        if expected != layout.revision {
            // An earlier request whose answer was lost: nothing to write.
            if layout_objects::already_deleted(&layout.member, &keys) {
                return Ok(json!({
                    "status": "already_applied",
                    "layout": layout_ref,
                    "objects": objects,
                    "revision": revision_text(&layout.revision),
                }));
            }
            return Err(AuthorError::new(
                ErrorKind::Conflict,
                format!(
                    "{layout_ref} changed since the revision the objects were deleted on; \
                     nothing was written."
                ),
            )
            .with_details(json!({
                "current_revision": revision_text(&layout.revision),
                "expected_revision": revision_text(&expected),
            })));
        }
        layout_objects::plan_deletion(&layout.member, &keys).map_err(|refusal| {
            let kind = match refusal {
                layout_placement::Refusal::Invalid(_) => ErrorKind::InvalidRequest,
                layout_placement::Refusal::Unsupported(_) => ErrorKind::Unsupported,
            };
            AuthorError::new(kind, refusal.to_string())
        })?;
        Self::require_grant(&mut layout.opened, &catalog::DELETE_OBJECTS)?;
        let deleted = layout.opened.delete_objects(wire::DeleteObjects {
            layout: layout.id,
            expected,
            objects: keys.clone(),
        });
        drop(layout);
        let label = layout_ref.clone();
        let deleted = deleted.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The deletion was sent but no answer came back ({reason}), so it may or \
                         may not have been saved. Get {layout_ref} again, or repeat the same \
                         delete_objects: it reports already_applied if the objects are gone."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        // Read back over a new connection: the layout at the revision the
        // host reported, without the objects.
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh) => {
                let gone = layout_objects::already_deleted(&fresh.member, &keys);
                if fresh.revision != deleted.revision || !gone {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported the objects of {layout_ref} deleted, but a new \
                             connection reads them or another revision."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&deleted.revision),
                        "found_revision": revision_text(&fresh.revision),
                        "objects_gone": gone,
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the objects deleted, but reading the layout back failed: {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if deleted.changed { "deleted" } else { "already_applied" },
            "layout": layout_ref,
            "objects": objects,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&deleted.revision),
            "verified": verified,
        }))
    }

    /// Add a Form layout with an empty body (`om_data::layout_catalog`)
    /// through the host's authoring extension, then read it back over a new
    /// connection.
    fn create_layout(&mut self, params: &CreateLayoutParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        layout_catalog::check_name(&params.name)
            .map_err(|refusal| AuthorError::new(ErrorKind::InvalidName, refusal.to_string()))?;
        let file = self.file();
        let like = match (&params.layout, &params.layout_name) {
            (None, None) => None,
            _ => Some(
                self.load_layout(&LayoutParams {
                    layout: params.layout.clone(),
                    layout_name: params.layout_name.clone(),
                })?
                .id,
            ),
        };
        let mut opened = self.open()?;
        let schema = opened.database_schema().map_err(Self::read_error)?;
        let occurrences = || schema.occurrences.iter().map(|o| (o.id, o.name.as_str()));
        let occurrence = match (&params.occurrence, &params.occurrence_name) {
            (Some(reference), None) => refs::occurrence_id(reference, &file)?,
            (None, Some(name)) => match Lookup::by_name(occurrences(), name) {
                Lookup::One(id) => id,
                Lookup::Missing => {
                    return Err(AuthorError::new(
                        ErrorKind::NotFound,
                        format!("{file} has no table occurrence named “{name}”."),
                    ))
                }
                Lookup::Ambiguous => {
                    return Err(AuthorError::new(
                        ErrorKind::AmbiguousReference,
                        format!(
                            "Several table occurrences of {file} are named “{name}”; name one \
                             by its reference."
                        ),
                    ))
                }
            },
            _ => {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    "Name the table occurrence with either `occurrence` (a reference from \
                     inspect) or `occurrence_name`.",
                ))
            }
        };
        if !schema.occurrences.iter().any(|o| o.id == occurrence) {
            return Err(AuthorError::new(
                ErrorKind::NotFound,
                format!("{file} has no table occurrence {occurrence}."),
            ));
        }
        Self::require_grant(&mut opened, &catalog::CREATE_LAYOUT)?;
        let created = opened.create_layout_on_host(wire::CreateLayout {
            expected,
            occurrence,
            like: like.unwrap_or(0),
            name: params.name.clone(),
            uuid: layout_request_uuid(&expected, occurrence, like.unwrap_or(0), &params.name),
        });
        drop(opened);
        let label = format!("the layouts of {file}");
        let created = created.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The new layout “{}” was sent but no answer came back ({reason}), so it \
                         may or may not have been added. Inspect the file again, or repeat the \
                         same create_layout: it reports already_applied if the layout is there.",
                        params.name
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let verified = self.verify_layout(&created, &params.name, occurrence)?;
        Ok(json!({
            "status": if created.changed { "created" } else { "already_applied" },
            "layout": {
                "ref": self.layout_ref(created.layout),
                "id": created.layout,
                "name": params.name,
            },
            "occurrence": reference("occurrence", occurrence, &file),
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&created.revision),
            "verified": verified,
        }))
    }

    /// Delete a layout as FileMaker Pro's Delete Layout does
    /// (`om_data::layout_catalog::plan_delete`) through the host's
    /// authoring extension, then read the layout list back over a new
    /// connection.
    fn delete_layout(&mut self, params: &DeleteLayoutParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        // A layout named by its reference is sent even when it is gone: a
        // repeat after a lost answer finds it deleted, which the host
        // tells (layout ids are not issued again). A name must name a
        // layout that is there.
        let (id, mut opened) = match (&params.layout, &params.layout_name) {
            (Some(reference), None) => (refs::layout_id(reference, &self.file())?, self.open()?),
            _ => {
                let layout = self.load_layout(&LayoutParams {
                    layout: params.layout.clone(),
                    layout_name: params.layout_name.clone(),
                })?;
                (layout.id, layout.opened)
            }
        };
        let layout_ref = self.layout_ref(id);
        Self::require_grant(&mut opened, &catalog::DELETE_LAYOUT)?;
        let deleted = opened.delete_layout_on_host(wire::DeleteLayout {
            expected,
            layout: id,
        });
        drop(opened);
        let label = format!("the layouts of {}", self.file());
        let deleted = deleted.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The deletion of {layout_ref} was asked for but no answer came back \
                         ({reason}), so it may or may not have been made. Inspect the file \
                         again, or repeat the same delete_layout: it reports already_applied if \
                         the layout is gone."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        // Read back over a new connection: the layouts at the revision the
        // host reported, without the layout.
        let verified = match self.open() {
            Ok(mut fresh) => {
                let index = fresh.layout_index().map_err(Self::read_error)?;
                let member = fresh.layout_member(id).map_err(Self::read_error)?;
                let found = layouts_revision(index.as_ref());
                if found != deleted.revision || member.is_some() {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported {layout_ref} deleted, but a new connection reads \
                             other layouts."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&deleted.revision),
                        "found_revision": revision_text(&found),
                        "layout_gone": member.is_none(),
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&found) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the layout deleted, but reading the layouts back failed: \
                     {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if deleted.changed { "deleted" } else { "already_applied" },
            "layout": layout_ref,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&deleted.revision),
            "verified": verified,
        }))
    }

    /// Show a field object with another control
    /// (`om_data::layout_objects::plan_control`) through the host's
    /// authoring extension, then read the layout back over a new connection.
    fn set_field_control(&mut self, params: &FieldControlParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let file = self.file();
        let (layout_id, object) = refs::layout_object_id(&params.object, &file)?;
        let control = layout_objects::FIELD_CONTROLS
            .iter()
            .find(|(_, _, name)| name.to_lowercase().replace(['-', ' '], "_") == params.control)
            .map(|(display, ..)| *display)
            .ok_or_else(|| {
                AuthorError::new(
                    ErrorKind::InvalidRequest,
                    format!(
                        "{:?} is not a control: edit_box, drop_down_list, pop_up_menu, \
                         checkbox_set, radio_button_set or drop_down_calendar.",
                        params.control
                    ),
                )
            })?;
        // The value list as a field object keys it (layout-model.md 5.4).
        let value_list = match &params.value_list {
            None => None,
            Some(reference) => Some(
                om_hbam::serial::id_of_raw(refs::value_list_id(reference, &file)?).ok_or_else(
                    || {
                        AuthorError::new(
                            ErrorKind::InvalidRequest,
                            "No such value list.".to_owned(),
                        )
                    },
                )?,
            ),
        };
        let layout = self.load_layout(&LayoutParams {
            layout: Some(self.layout_ref(layout_id)),
            layout_name: None,
        })?;
        let layout_ref = self.layout_ref(layout.id);
        let object_ref = self.object_ref(layout.id, object);
        let mut opened = layout.opened;
        Self::require_grant(&mut opened, &catalog::SET_FIELD_CONTROL)?;
        let set = opened.field_control_on_host(wire::FieldControl {
            expected,
            layout: layout.id,
            object,
            control,
            value_list,
        });
        drop(opened);
        let label = layout_ref.clone();
        let set = set.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The control of {object_ref} was sent but no answer came back ({reason}), \
                         so it may or may not have been saved. Get the layout again, or repeat \
                         the same set_field_control: it reports already_applied if it is there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        // Read back over a new connection: the layout at the revision the
        // host reported, the object with its new control.
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh) => {
                if fresh.revision != set.revision
                    || !layout_objects::already_control(&fresh.member, object, control, value_list)
                {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported {object_ref}'s control set, but a new connection \
                             reads another layout."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&set.revision),
                        "found_revision": revision_text(&fresh.revision),
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the control set, but reading the layout back failed: {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if set.changed { "set" } else { "already_applied" },
            "layout": layout_ref,
            "object": object_ref,
            "control": params.control,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&set.revision),
            "verified": verified,
        }))
    }

    /// Set a layout's width (`om_data::layout_placement::plan_width`)
    /// through the host's authoring extension, then read the layout back
    /// over a new connection.
    fn set_layout_width(&mut self, params: &LayoutWidthParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let layout_ref = self.layout_ref(layout.id);
        let width = params.width;
        let mut opened = layout.opened;
        Self::require_grant(&mut opened, &catalog::SET_LAYOUT_WIDTH)?;
        let set = opened.layout_width_on_host(wire::LayoutWidth {
            expected,
            layout: layout.id,
            width,
        });
        drop(opened);
        let label = layout_ref.clone();
        let set = set.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The width of {layout_ref} was sent but no answer came back ({reason}), \
                         so it may or may not have been saved. Get the layout again, or repeat \
                         the same set_layout_width: it reports already_applied if it is there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        // Read back over a new connection: the layout at the revision the
        // host reported, at the width sent.
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh) => {
                if fresh.revision != set.revision
                    || !layout_placement::already_width(&fresh.member, width)
                {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported {layout_ref}'s width set, but a new connection \
                             reads another layout."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&set.revision),
                        "found_revision": revision_text(&fresh.revision),
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the width set, but reading the layout back failed: {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if set.changed { "set" } else { "already_applied" },
            "layout": layout_ref,
            "width": width,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&set.revision),
            "verified": verified,
        }))
    }

    /// Change a text object's words as typing into it in Layout mode does
    /// (`om_data::layout_placement::plan_words`) through the host's authoring
    /// extension, then read the layout back over a new connection.
    fn set_text(&mut self, params: &TextParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let file = self.file();
        let (layout_id, object) = refs::layout_object_id(&params.object, &file)?;
        let layout = self.load_layout(&LayoutParams {
            layout: Some(self.layout_ref(layout_id)),
            layout_name: None,
        })?;
        let layout_ref = self.layout_ref(layout.id);
        let object_ref = self.object_ref(layout.id, object);
        let text = params.text.clone();
        let mut opened = layout.opened;
        Self::require_grant(&mut opened, &catalog::SET_TEXT)?;
        let set = opened.text_on_host(wire::SetText {
            expected,
            layout: layout.id,
            object,
            text: text.clone(),
        });
        drop(opened);
        let label = layout_ref.clone();
        let set = set.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The words of {object_ref} were sent but no answer came back ({reason}), \
                         so they may or may not have been saved. Get the layout again, or repeat \
                         the same set_text: it reports already_applied if they are there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh)
                if fresh.revision != set.revision
                    || !layout_placement::already_words(&fresh.member, object, &text) =>
            {
                return Err(AuthorError::new(
                    ErrorKind::VerificationFailed,
                    format!(
                        "The host reported {object_ref}'s words set, but a new connection reads \
                         another layout."
                    ),
                ))
            }
            Ok(fresh) => {
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({ "fresh_connection": false, "reason": e.message }),
        };
        Ok(json!({
            "status": if set.changed { "set" } else { "already_applied" },
            "layout": layout_ref,
            "object": object_ref,
            "text": text,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&set.revision),
            "verified": verified,
        }))
    }

    /// Set a part's page options as Part Definition does
    /// (`om_data::layout_placement::plan_part_options`) through the host's
    /// authoring extension, then read the layout back over a new connection.
    /// Add, delete or resize a layout's part (operation 22,
    /// `layout_placement::plan_parts`): checked against the layout as
    /// stored, sent, and read back over a new connection.
    fn change_parts(&mut self, params: &ChangePartsParams) -> Result<Value, AuthorError> {
        use layout_placement::PartChange;
        let expected = parse_revision(&params.expected_revision)?;
        let layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let layout_ref = self.layout_ref(layout.id);
        let invalid = |message: String| AuthorError::new(ErrorKind::InvalidRequest, message);
        let parts = layout_placement::parts(&layout.member)
            .map_err(|e| AuthorError::new(ErrorKind::Unsupported, e.to_string()))?;
        // A part's kind as given, else as the layout stores the part: a
        // repeated delete names its kind, as keys change when a part goes.
        let named = |kind: &str| {
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 13]
                .into_iter()
                .find(|t| part_kind(PartKind::from_type(*t)) == kind)
                .ok_or_else(|| invalid(format!("“{kind}” is not a kind of part.")))
        };
        let stored = |what: &str| {
            let part = params.part.ok_or_else(|| {
                invalid(format!("{what} needs `part`: a part's id from get_layout."))
            })?;
            if let Some(kind) = params.kind.as_deref() {
                return Ok((part, named(kind)?));
            }
            parts
                .iter()
                .find(|p| p.id == part)
                .map(|p| (part, p.part_type))
                .ok_or_else(|| invalid(format!("{layout_ref} has no part {part}.")))
        };
        let height = |what: &str| {
            params
                .height
                .ok_or_else(|| invalid(format!("{what} needs `height`, in points.")))
        };
        let change = match params.action.as_str() {
            "add" => {
                let kind = params.kind.as_deref().ok_or_else(|| {
                    invalid("add needs `kind`: the part's kind, as get_layout names it.".into())
                })?;
                let part_type = layout_placement::ADDABLE_PARTS
                    .iter()
                    .copied()
                    .find(|t| part_kind(PartKind::from_type(*t)) == kind)
                    .ok_or_else(|| {
                        invalid(format!(
                            "“{kind}” is not a kind of part change_parts adds: title_header, \
                             header, leading_grand_summary, body, trailing_grand_summary, \
                             footer, title_footer, top_navigation, bottom_navigation."
                        ))
                    })?;
                PartChange::Add {
                    part_type,
                    size: height("add")?,
                }
            }
            "delete" => {
                let (part, part_type) = stored("delete")?;
                PartChange::Delete { part, part_type }
            }
            "resize" => {
                let (part, part_type) = stored("resize")?;
                PartChange::Resize {
                    part,
                    part_type,
                    size: height("resize")?,
                }
            }
            other => {
                return Err(invalid(format!(
                    "“{other}” is not an action: add, delete or resize."
                )))
            }
        };
        if expected != layout.revision {
            // A lost answer's change may be there; the host decides.
            if !layout_placement::already_parts(&layout.member, &change) {
                return Err(self.layout_conflict(&layout, &expected));
            }
        } else {
            layout_placement::plan_parts(&layout.member, &change).map_err(placement_refusal)?;
        }
        let (action, part, part_type, size) = match change {
            PartChange::Add { part_type, size } => (wire::PARTS_ADD, 0, part_type, size),
            PartChange::Delete { part, part_type } => (wire::PARTS_DELETE, part, part_type, 0.0),
            PartChange::Resize {
                part,
                part_type,
                size,
            } => (wire::PARTS_RESIZE, part, part_type, size),
        };
        let mut opened = layout.opened;
        Self::require_grant(&mut opened, &catalog::CHANGE_PARTS)?;
        let set = opened.parts_on_host(wire::ChangeParts {
            expected,
            layout: layout.id,
            action,
            part,
            part_type: part_type as u32,
            size,
        });
        drop(opened);
        let label = layout_ref.clone();
        let set = set.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The change of parts was sent but no answer came back ({reason}), so it \
                         may or may not have been saved. Get the layout again, or repeat the \
                         same change_parts: it reports already_applied if it is there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let fresh = self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        });
        let (verified, now) = match fresh {
            Ok(fresh)
                if fresh.revision != set.revision
                    || !layout_placement::already_parts(&fresh.member, &change) =>
            {
                return Err(AuthorError::new(
                    ErrorKind::VerificationFailed,
                    format!(
                        "The host reported {layout_ref}'s parts changed, but a new connection \
                         reads another layout."
                    ),
                ))
            }
            Ok(fresh) => {
                let now: Vec<Value> = layout_placement::parts(&fresh.member)
                    .unwrap_or_default()
                    .iter()
                    .map(|p| {
                        json!({
                            "id": p.id,
                            "kind": part_kind(p.kind()),
                            "top": p.top,
                            "height": p.height,
                        })
                    })
                    .collect();
                (
                    json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) }),
                    now,
                )
            }
            Err(e) => (
                json!({ "fresh_connection": false, "reason": e.message }),
                Vec::new(),
            ),
        };
        Ok(json!({
            "status": if set.changed { "changed" } else { "already_applied" },
            "layout": layout_ref,
            "parts": now,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&set.revision),
            "verified": verified,
        }))
    }

    fn set_part_options(&mut self, params: &PartOptionsParams) -> Result<Value, AuthorError> {
        use layout_placement::part_options as bits;
        let expected = parse_revision(&params.expected_revision)?;
        let layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let layout_ref = self.layout_ref(layout.id);
        let invalid = |message: String| AuthorError::new(ErrorKind::InvalidRequest, message);
        let parts = layout_placement::parts(&layout.member)
            .map_err(|e| AuthorError::new(ErrorKind::Unsupported, e.to_string()))?;
        let part = parts
            .iter()
            .find(|p| p.id == params.part)
            .ok_or_else(|| invalid(format!("{layout_ref} has no part {}.", params.part)))?;
        const NAMES: [(&str, u64); 4] = [
            ("page_break_before", bits::PAGE_BREAK_BEFORE),
            ("restart_page_numbers", bits::RESTART_PAGE_NUMBERS),
            ("break_across_pages", bits::BREAK_ACROSS_PAGES),
            ("discard_remainder", bits::DISCARD_REMAINDER),
        ];
        let mut flags = part.flags;
        for (name, on) in params.options.iter().flatten() {
            let (_, bit) = NAMES.iter().find(|(n, _)| n == name).ok_or_else(|| {
                let names: Vec<&str> = NAMES.iter().map(|(n, _)| *n).collect();
                invalid(format!(
                    "{name:?} is not a part option: {}.",
                    names.join(", ")
                ))
            })?;
            if *on {
                flags |= bit;
            } else {
                flags &= !bit;
            }
        }
        // As Part Definition: a page break after every N (0: none) turns a
        // break before each occurrence off, and the other way round.
        let mut page_break_after = part.page_break_after;
        match params.page_break_after {
            Some(0) => flags &= !bits::PAGE_BREAK_EVERY,
            Some(n) => {
                flags = (flags | bits::PAGE_BREAK_EVERY) & !bits::PAGE_BREAK_BEFORE;
                page_break_after = n;
            }
            None if flags & !part.flags & bits::PAGE_BREAK_BEFORE != 0 => {
                flags &= !bits::PAGE_BREAK_EVERY;
            }
            None => {}
        }
        let file = self.file();
        let break_field = match &params.sorted_by {
            None => None,
            Some(reference) => {
                let (table, field) = refs::field_id(reference, &file)?;
                if Some(table) != layout.table {
                    return Err(invalid(format!(
                        "{reference} is not a field of {layout_ref}'s own table."
                    )));
                }
                Some((layout.occurrence.unwrap_or_default(), table, field))
            }
        };
        let field = break_field.map(|(occurrence, _, field)| (occurrence, field));
        let options = (flags, page_break_after, field);
        let mut opened = layout.opened;
        Self::require_grant(&mut opened, &catalog::SET_PART_OPTIONS)?;
        let set = opened.part_options_on_host(wire::PartOptions {
            expected,
            layout: layout.id,
            part: params.part,
            flags,
            page_break_after,
            break_field,
        });
        drop(opened);
        let label = layout_ref.clone();
        let set = set.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The part's options were sent but no answer came back ({reason}), so \
                         they may or may not have been saved. Get the layout again, or repeat \
                         the same set_part_options: it reports already_applied if they are \
                         there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let fresh = self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        });
        let verified = match fresh {
            Ok(fresh)
                if fresh.revision != set.revision
                    || !layout_placement::already_part_options(
                        &fresh.member,
                        params.part,
                        options,
                    ) =>
            {
                return Err(AuthorError::new(
                    ErrorKind::VerificationFailed,
                    format!(
                        "The host reported part {} of {layout_ref} set, but a new connection \
                         reads another layout.",
                        params.part
                    ),
                ))
            }
            Ok(fresh) => {
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({ "fresh_connection": false, "reason": e.message }),
        };
        Ok(json!({
            "status": if set.changed { "set" } else { "already_applied" },
            "layout": layout_ref,
            "part": params.part,
            "options": NAMES
                .iter()
                .map(|(name, bit)| ((*name).to_owned(), json!(flags & bit != 0)))
                .collect::<serde_json::Map<_, _>>(),
            "page_break_after": if flags & bits::PAGE_BREAK_EVERY != 0 { page_break_after } else { 0 },
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&set.revision),
            "verified": verified,
        }))
    }

    /// Set a layout's name, views, General options and menu inclusion as
    /// Layout Setup does
    /// (`om_data::layout_catalog::plan_setup`) through the host's authoring
    /// extension, then read the layout back over a new connection.
    fn layout_setup(&mut self, params: &LayoutSetupParams) -> Result<Value, AuthorError> {
        use layout_catalog::{general, printing, views, LayoutWord, MENU_HIDDEN};
        let expected = parse_revision(&params.expected_revision)?;
        let layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let layout_ref = self.layout_ref(layout.id);
        // The flags as the host's Layout Setup reads them: the member's, else
        // the index entry's, so a request keeps what only the entry holds
        // (#977). Both come from one download. A concurrent change to a word
        // only the entry holds is not covered by the member's revision; see
        // docs/authoring.md, "Layout Setup".
        let stored = |word| {
            layout_catalog::layout_word(
                Some(&layout.member),
                layout.index.as_ref(),
                layout.id,
                word,
            )
            .unwrap_or(0)
        };
        let invalid = |message: String| AuthorError::new(ErrorKind::InvalidRequest, message);
        let view = |name: &str| match name {
            "form" => Ok(views::FORM_OFF),
            "list" => Ok(views::LIST_OFF),
            "table" => Ok(views::TABLE_OFF),
            other => Err(invalid(format!(
                "{other:?} is not a view: form, list or table."
            ))),
        };
        let mut flags = stored(LayoutWord::ViewFlags);
        if let Some(available) = &params.views {
            flags |= views::FORM_OFF | views::LIST_OFF | views::TABLE_OFF;
            for name in available {
                flags &= !view(name)?;
            }
        }
        if let Some(default) = &params.default_view {
            flags &= !(views::DEFAULT_LIST | views::DEFAULT_TABLE);
            flags |= match view(default)? {
                views::LIST_OFF => views::DEFAULT_LIST,
                views::TABLE_OFF => views::DEFAULT_TABLE,
                _ => 0,
            };
        }
        for (name, on) in params.options.iter().flatten() {
            let (.., bit, set_on) = general::OPTIONS
                .iter()
                .find(|(option, ..)| option == name)
                .ok_or_else(|| {
                    let names: Vec<&str> = general::OPTIONS.iter().map(|o| o.0).collect();
                    invalid(format!(
                        "{name:?} is not a Layout Setup option: {}.",
                        names.join(", ")
                    ))
                })?;
            if on == set_on {
                flags |= bit;
            } else {
                flags &= !bit;
            }
        }
        if let Some(fixed) = params.fixed_page_margins {
            if fixed {
                flags |= printing::FIXED_PAGE_MARGINS;
            } else {
                flags &= !printing::FIXED_PAGE_MARGINS;
            }
        }
        // A layout that stores no margins gets 36 points with "Use fixed
        // page margins", as the desktop's dialog shows them and every new
        // layout stores them: the host refuses the box without margins.
        let none_stored = layout_placement::definition(&layout.member)
            .is_ok_and(|def| def.custom_margins.is_none());
        let margins = params
            .page_margins
            .map(|m| (m.top, m.left, m.bottom, m.right))
            .or((params.fixed_page_margins == Some(true) && none_stored)
                .then_some((36.0, 36.0, 36.0, 36.0)))
            .map(|(top, left, bottom, right)| om_layout::Rect {
                top,
                left,
                bottom,
                right,
                ..Default::default()
            });
        let hidden = params.include_in_layout_menus.map_or(
            stored(LayoutWord::MemberFlags) & MENU_HIDDEN != 0,
            |include| !include,
        );
        let name = match &params.name {
            Some(name) => name.clone(),
            None => layout_catalog::member_name(&layout.member).ok_or_else(|| {
                AuthorError::new(
                    ErrorKind::Unsupported,
                    format!("{layout_ref} has no name OpenMaker reads."),
                )
            })?,
        };
        let mut opened = layout.opened;
        Self::require_grant(&mut opened, &catalog::LAYOUT_SETUP)?;
        // With margins, operation 23: the setup and the margins in one
        // commit.
        let set = opened.layout_setup_on_host(wire::LayoutSetup {
            expected,
            layout: layout.id,
            name: name.clone(),
            flags,
            hidden,
            margins: margins.as_ref().map(|m| [m.top, m.left, m.bottom, m.right]),
        });
        drop(opened);
        let label = layout_ref.clone();
        let set = set.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "Layout Setup of {layout_ref} was sent but no answer came back \
                         ({reason}), so it may or may not have been saved. Get the layout \
                         again, or repeat the same layout_setup: it reports already_applied if \
                         it is there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        // Read back over a new connection: the layout at the revision the
        // host reported, with the name and flags sent.
        let verified = match self.load_layout(&LayoutParams {
            layout: Some(layout_ref.clone()),
            layout_name: None,
        }) {
            Ok(fresh) => {
                // As the request read them: the member's, else the entry's.
                let read_word = |word| {
                    layout_catalog::layout_word(
                        Some(&fresh.member),
                        fresh.index.as_ref(),
                        fresh.id,
                        word,
                    )
                    .unwrap_or(0)
                };
                let read = (
                    layout_catalog::member_name(&fresh.member),
                    read_word(LayoutWord::ViewFlags),
                    read_word(LayoutWord::MemberFlags) & MENU_HIDDEN != 0,
                );
                let margins_read = margins
                    .as_ref()
                    .is_none_or(|m| layout_placement::already_margins(&fresh.member, m));
                if fresh.revision != set.revision
                    || read != (Some(name.clone()), flags, hidden)
                    || !margins_read
                {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported {layout_ref} set up, but a new connection reads \
                             another layout."
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&set.revision),
                        "found_revision": revision_text(&fresh.revision),
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&fresh.revision) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the layout set up, but reading it back failed: {}",
                    e.message
                ),
            }),
        };
        let available: Vec<&str> = [
            ("form", views::FORM_OFF),
            ("list", views::LIST_OFF),
            ("table", views::TABLE_OFF),
        ]
        .iter()
        .filter(|(_, bit)| flags & bit == 0)
        .map(|(name, _)| *name)
        .collect();
        Ok(json!({
            "status": if set.changed { "set_up" } else { "already_applied" },
            "layout": layout_ref,
            "name": name,
            "views": available,
            "default_view": if flags & views::DEFAULT_TABLE != 0 {
                "table"
            } else if flags & views::DEFAULT_LIST != 0 {
                "list"
            } else {
                "form"
            },
            "include_in_layout_menus": !hidden,
            "options": general::OPTIONS
                .iter()
                .map(|(option, _, bit, set_on)| ((*option).to_owned(), json!((flags & bit != 0) == *set_on)))
                .collect::<serde_json::Map<_, _>>(),
            "fixed_page_margins": flags & printing::FIXED_PAGE_MARGINS != 0,
            "page_margins": margins.map(|m| json!({
                "top": m.top, "bottom": m.bottom, "left": m.left, "right": m.right,
            })),
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&set.revision),
            "verified": verified,
        }))
    }

    /// Copy a layout as FileMaker Pro's Duplicate Layout does
    /// (`om_data::layout_catalog::plan_duplicate`) through the host's
    /// authoring extension, then read the copy back over a new connection.
    fn duplicate_layout(&mut self, params: &DuplicateLayoutParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let source = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let source_id = source.id;
        let source_ref = self.layout_ref(source_id);
        let name = layout_catalog::member_name(&source.member)
            .map(|name| layout_catalog::copy_name(&name))
            .ok_or_else(|| {
                AuthorError::new(
                    ErrorKind::Unsupported,
                    format!("{source_ref} has no name OpenMaker reads."),
                )
            })?;
        let mut opened = source.opened;
        Self::require_grant(&mut opened, &catalog::DUPLICATE_LAYOUT)?;
        let uuid = duplicate_request_uuid(&expected, source_id);
        let created = opened.duplicate_layout_on_host(wire::DuplicateLayout {
            expected,
            layout: source_id,
            uuid,
        });
        drop(opened);
        let label = format!("the layouts of {}", self.file());
        let created = created.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The copy of {source_ref} was asked for but no answer came back \
                         ({reason}), so it may or may not have been made. Inspect the file \
                         again, or repeat the same duplicate_layout: it reports already_applied \
                         if the copy is there."
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        // Read back over a new connection: the layouts at the revision the
        // host reported, the copy the one this request made (its UUID). A
        // repeat finds the copy as it is now: its name is the one it has,
        // which a rename of either layout since may have changed.
        let mut name = name;
        let verified = match self.open() {
            Ok(mut fresh) => {
                let index = fresh.layout_index().map_err(Self::read_error)?;
                let member = fresh
                    .layout_member(created.layout)
                    .map_err(Self::read_error)?;
                let found = layouts_revision(index.as_ref());
                let named = member.as_ref().and_then(layout_catalog::member_name);
                let made = member
                    .as_ref()
                    .and_then(|m| m.child(&[0xd8])?.value.as_deref())
                    == Some(&uuid[..]);
                if !created.changed {
                    if let Some(named) = &named {
                        name = named.clone();
                    }
                }
                if found != created.revision || !made || named.as_deref() != Some(name.as_str()) {
                    return Err(AuthorError::new(
                        ErrorKind::VerificationFailed,
                        format!(
                            "The host reported {} added, but a new connection reads other \
                             layouts.",
                            self.layout_ref(created.layout)
                        ),
                    )
                    .with_details(json!({
                        "expected_revision": revision_text(&created.revision),
                        "found_revision": revision_text(&found),
                        "name": named,
                    })));
                }
                json!({ "fresh_connection": true, "revision": revision_text(&found) })
            }
            Err(e) => json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the copy made, but reading it back failed: {}",
                    e.message
                ),
            }),
        };
        Ok(json!({
            "status": if created.changed { "created" } else { "already_applied" },
            "layout": {
                "ref": self.layout_ref(created.layout),
                "id": created.layout,
                "name": name,
            },
            "source": source_ref,
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&created.revision),
            "verified": verified,
        }))
    }

    /// Read the new layout back over a new connection: the file's layouts
    /// must be at the revision the host reported, with the layout named
    /// `name` on `occurrence`, a Form layout with one body part and no
    /// objects, listed among the layouts calculations name.
    fn verify_layout(
        &self,
        created: &wire::LayoutCreated,
        name: &str,
        occurrence: u64,
    ) -> Result<Value, AuthorError> {
        let unverified = |e: AuthorError| {
            json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the layout added, but reading it back failed: {}",
                    e.message
                ),
            })
        };
        let mut fresh = match self.open() {
            Ok(fresh) => fresh,
            Err(e) => return Ok(unverified(e)),
        };
        let index = match fresh.layout_index() {
            Ok(index) => index,
            Err(e) => return Ok(unverified(Self::read_error(e))),
        };
        let member = match fresh.layout_member(created.layout) {
            Ok(member) => member,
            Err(e) => return Ok(unverified(Self::read_error(e))),
        };
        let listed = match fresh.calc_schema() {
            Ok(names) => names.is_some_and(|names| {
                names
                    .layouts()
                    .iter()
                    .any(|l| u64::from(l.id) == created.layout && l.name == name)
            }),
            Err(e) => return Ok(unverified(Self::read_error(e))),
        };
        let found = layouts_revision(index.as_ref());
        let layout = member.as_ref().and_then(layout_placement::decode);
        let made = layout.as_ref().is_some_and(|layout| {
            layout
                .def
                .as_ref()
                .and_then(|def| om_layout::occurrence_id(def.table_alias_key))
                == Some(occurrence)
                && layout.parts.len() == 1
                && layout.parts[0].kind() == PartKind::Body
                && layout.objects.objects.is_empty()
        }) && member
            .as_ref()
            .and_then(layout_catalog::member_name)
            .as_deref()
            == Some(name);
        if found != created.revision || !made || !listed {
            return Err(AuthorError::new(
                ErrorKind::VerificationFailed,
                format!(
                    "The host reported {} added, but a new connection reads other layouts.",
                    self.layout_ref(created.layout)
                ),
            )
            .with_details(json!({
                "expected_revision": revision_text(&created.revision),
                "found_revision": revision_text(&found),
                "layout_as_planned": made,
                "listed_in_schema": listed,
            })));
        }
        Ok(json!({
            "fresh_connection": true,
            "revision": revision_text(&found),
            "listed_in_schema": true,
        }))
    }
}

impl Service {
    fn layout_ref(&self, layout: u64) -> String {
        reference("layout", layout, &self.file())
    }

    fn object_ref(&self, layout: u64, object: u64) -> String {
        reference("layout_object", format!("{layout}.{object}"), &self.file())
    }

    /// Open the file and read the layout `params` names, as stored.
    fn load_layout(&self, params: &LayoutParams) -> Result<LoadedLayout, AuthorError> {
        let file = self.file();
        let mut opened = self.open()?;
        let snapshot = opened.layout_catalog_snapshot().map_err(Self::read_error)?;
        let (members, index) = (snapshot.members, snapshot.index);
        let listed: Vec<_> = members
            .iter()
            .map(|(&id, member)| {
                (
                    id,
                    member
                        .child(&[0x10])
                        .and_then(om_scripts::node_text)
                        .unwrap_or_default(),
                )
            })
            .collect();
        let layouts = || listed.iter().map(|(id, name)| (*id, name.as_str()));
        let id = match (&params.layout, &params.layout_name) {
            (Some(reference), None) => refs::layout_id(reference, &file)?,
            (None, Some(name)) => match Lookup::by_name(layouts(), name) {
                Lookup::One(id) => id,
                Lookup::Missing => {
                    return Err(AuthorError::new(
                        ErrorKind::NotFound,
                        format!("{file} has no layout named “{name}”."),
                    ))
                }
                Lookup::Ambiguous => {
                    let candidates: Vec<String> = layouts()
                        .filter(|(_, n)| n.to_lowercase() == name.to_lowercase())
                        .map(|(id, _)| reference("layout", id, &file))
                        .collect();
                    return Err(AuthorError::new(
                        ErrorKind::AmbiguousReference,
                        format!(
                            "Several layouts of {file} are named “{name}”; name one by its \
                             reference."
                        ),
                    )
                    .with_details(json!({ "candidates": candidates })));
                }
            },
            _ => {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    "Name the layout with either `layout` (a reference from inspect) or \
                     `layout_name`.",
                ))
            }
        };
        let member = members.get(&id).cloned().ok_or_else(|| {
            AuthorError::new(ErrorKind::NotFound, format!("{file} has no layout {id}."))
        })?;
        let name = member
            .child(&[0x10])
            .and_then(om_scripts::node_text)
            .unwrap_or_default();
        let occurrence = layout_placement::definition(&member)
            .ok()
            .and_then(|def| om_layout::occurrence_id(def.table_alias_key));
        let schema = opened.database_schema().map_err(Self::read_error)?;
        let table = occurrence.and_then(|o| {
            schema
                .occurrences
                .iter()
                .find(|s| s.id == o)
                .and_then(|s| s.table_id)
        });
        Ok(LoadedLayout {
            revision: layout_revision(&member),
            opened,
            id,
            name,
            member,
            index,
            occurrence,
            table,
            schema,
        })
    }

    fn layout_json(&self, layout: &LoadedLayout) -> Value {
        json!({
            "ref": self.layout_ref(layout.id),
            "id": layout.id,
            "name": layout.name,
        })
    }

    /// The field of `schema`'s `table` with id `id`.
    fn schema_field(schema: &DatabaseSchema, table: u64, id: u64) -> Option<&SchemaField> {
        schema
            .tables
            .iter()
            .find(|t| t.id == table)?
            .fields
            .iter()
            .find(|f| f.id == id)
    }

    /// An object as `get_layout` and the placements list it.
    fn object_json(
        &self,
        layout: &LoadedLayout,
        decoded: &om_layout::Layout,
        object: &LayoutObject,
    ) -> Value {
        let mut value = json!({
            "ref": self.object_ref(layout.id, object.id),
            "key": object.id,
            "kind": object_kind(object),
            "frame": decoded.absolute_bounds(object.id).as_ref().map(frame_json),
        });
        if let Some(position) = decoded.objects.root.iter().position(|&r| r == object.id) {
            value["stacking"] = json!(position);
        }
        if let Some(parent) = object.parent {
            value["parent"] = json!(self.object_ref(layout.id, parent));
        }
        if let Some(name) = &object.name {
            value["name"] = json!(name);
        }
        if let Some(uuid) = &object.uuid {
            value["uuid"] = json!(uuid_text(uuid));
        }
        if let Some(spec) = object.field_spec() {
            let occurrence = om_layout::occurrence_id(spec.table_key);
            let field = spec.field_id();
            let table = occurrence.and_then(|o| {
                layout
                    .schema
                    .occurrences
                    .iter()
                    .find(|s| s.id == o)
                    .and_then(|s| s.table_id)
            });
            let named = table
                .zip(field)
                .and_then(|(t, f)| Self::schema_field(&layout.schema, t, f));
            value["field"] = json!({
                "ref": table.zip(field).map(|(t, f)| self.field_ref(t, f)),
                "occurrence": occurrence.map(|o| reference("occurrence", o, &self.file())),
                "name": named.map(|f| f.name.clone()),
                "repetition": spec.repetition,
            });
            if let Some(field_obj) = &object.field_obj {
                value["control"] = json!(match field_obj.display_type {
                    0 | 5 | 7 => "edit_box",
                    1 => "drop_down_list",
                    2 => "pop_up_menu",
                    3 => "checkbox_set",
                    4 => "radio_button_set",
                    6 => "drop_down_calendar",
                    _ => "other",
                });
            }
        }
        if let Some(text) = &object.text {
            value["text"] = json!(text);
        }
        value
    }

    fn get_layout(&mut self, params: &LayoutParams) -> Result<Value, AuthorError> {
        let mut layout = self.load_layout(params)?;
        let file = self.file();
        let decoded = layout_placement::decode(&layout.member).ok_or_else(|| {
            AuthorError::new(
                ErrorKind::Unsupported,
                format!("OpenMaker cannot read layout {}'s objects.", layout.id),
            )
        })?;
        let def = decoded.def.clone().unwrap_or_default();
        // The flags as Layout Setup reads them: the member's, else the
        // index entry's (#977), else the definition's.
        let view_flags = layout_catalog::layout_word(
            Some(&layout.member),
            layout.index.as_ref(),
            layout.id,
            layout_catalog::LayoutWord::ViewFlags,
        )
        .unwrap_or(def.flags);
        let parts: Vec<Value> = decoded
            .parts
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "kind": part_kind(p.kind()),
                    "top": p.top,
                    "height": p.height,
                })
            })
            .collect();
        let objects: Vec<Value> = decoded
            .objects
            .objects
            .iter()
            .map(|o| self.object_json(&layout, &decoded, o))
            .collect();
        let stack: Vec<String> = decoded
            .objects
            .root
            .iter()
            .map(|&o| self.object_ref(layout.id, o))
            .collect();
        let place = Self::available(&mut layout.opened, &catalog::PLACE_FIELD)?;
        Ok(json!({
            "layout": self.layout_json(&layout),
            "revision": revision_text(&layout.revision),
            "occurrence": layout.occurrence.map(|o| reference("occurrence", o, &file)),
            "table": layout.table.map(|t| reference("table", t, &file)),
            "width": def.base_width,
            "responsive": def.layout_client_type == 2,
            // Layout Setup > Printing: the layout prints within its page
            // margins only with fixed_page_margins.
            "fixed_page_margins": view_flags & layout_catalog::printing::FIXED_PAGE_MARGINS != 0,
            "page_margins": def.custom_margins.as_ref().map(|m| json!({
                "top": m.top, "bottom": m.bottom, "left": m.left, "right": m.right,
            })),
            "parts": parts,
            "objects": objects,
            "root_stack": stack,
            "stacking_order": "back to front: later objects draw over earlier ones",
            "place_field": place,
        }))
    }

    /// The field a placement names, of the layout's table.
    fn placement_field<'a>(
        &self,
        layout: &'a LoadedLayout,
        field: &Option<String>,
        field_name: &Option<String>,
    ) -> Result<(u64, &'a SchemaField), AuthorError> {
        let file = self.file();
        let layout_ref = self.layout_ref(layout.id);
        let table = layout.table.ok_or_else(|| {
            AuthorError::new(
                ErrorKind::Unsupported,
                format!(
                    "{layout_ref} shows no table of {file}; this version places fields of the \
                     layout's own table only."
                ),
            )
        })?;
        let fields = || {
            layout
                .schema
                .tables
                .iter()
                .find(|t| t.id == table)
                .map(|t| t.fields.as_slice())
                .unwrap_or_default()
        };
        let id = match (field, field_name) {
            (Some(named), None) => {
                let (named_table, id) = refs::field_id(named, &file)?;
                if named_table != table {
                    return Err(AuthorError::new(
                        ErrorKind::Unsupported,
                        format!(
                            "“{named}” is a field of {}, but {layout_ref} shows records of \
                             {}; this version places fields of the layout's own table only, \
                             not related fields.",
                            reference("table", named_table, &file),
                            reference("table", table, &file)
                        ),
                    ));
                }
                id
            }
            (None, Some(name)) => {
                let items = || fields().iter().map(|f| (f.id, f.name.as_str()));
                match Lookup::by_name(items(), name) {
                    Lookup::One(id) => id,
                    Lookup::Missing => {
                        return Err(AuthorError::new(
                            ErrorKind::NotFound,
                            format!(
                                "{} has no field named “{name}”.",
                                reference("table", table, &file)
                            ),
                        ))
                    }
                    Lookup::Ambiguous => {
                        return Err(AuthorError::new(
                            ErrorKind::AmbiguousReference,
                            format!(
                                "Several fields are named “{name}”; name one by its reference."
                            ),
                        )
                        .with_details(json!({
                            "candidates": items()
                                .filter(|(_, n)| n.to_lowercase() == name.to_lowercase())
                                .map(|(id, _)| self.field_ref(table, id))
                                .collect::<Vec<_>>(),
                        })))
                    }
                }
            }
            _ => {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    "Name the field with either `field` (a reference from inspect) or \
                     `field_name`.",
                ))
            }
        };
        let found = Self::schema_field(&layout.schema, table, id).ok_or_else(|| {
            AuthorError::new(
                ErrorKind::NotFound,
                format!("{} has no field {id}.", reference("table", table, &file)),
            )
        })?;
        if found.data_type == FieldKind::Container {
            return Err(AuthorError::new(
                ErrorKind::Unsupported,
                format!(
                    "“{}” is a container field; this version places text, number, date, time \
                     and timestamp fields in edit boxes only.",
                    found.name
                ),
            ));
        }
        Ok((table, found))
    }

    /// The placement `params` asks for on `layout`, of `field`.
    fn placement(layout: &LoadedLayout, field: u64, params: &PlacementParams) -> Placement {
        Placement {
            occurrence: layout.occurrence.unwrap_or_default(),
            field,
            frame: rect(&params.frame),
            label: params.label.as_ref().map(|label| Label {
                text: label.text.clone(),
                frame: rect(&label.frame),
            }),
        }
    }

    /// The objects a placement makes, as listed in its plan and result.
    fn placed_json(
        &self,
        layout: u64,
        placement: &Placement,
        field_object: u64,
        label_object: Option<u64>,
        table: u64,
        field: &SchemaField,
    ) -> Value {
        let mut objects = vec![json!({
            "ref": self.object_ref(layout, field_object),
            "key": field_object,
            "kind": "field",
            "control": "edit_box",
            "frame": frame_json(&placement.frame),
            "field": { "ref": self.field_ref(table, field.id), "name": field.name },
        })];
        if let (Some(key), Some(label)) = (label_object, &placement.label) {
            objects.push(json!({
                "ref": self.object_ref(layout, key),
                "key": key,
                "kind": "text",
                "frame": frame_json(&label.frame),
                "text": label.text,
            }));
        }
        json!(objects)
    }

    /// A conflict: the layout is at `current`, not `expected`.
    fn layout_conflict(&self, layout: &LoadedLayout, expected: &Revision) -> AuthorError {
        AuthorError::new(
            ErrorKind::Conflict,
            format!(
                "{} changed since the revision the placement was planned against; nothing was \
                 written.",
                self.layout_ref(layout.id)
            ),
        )
        .with_details(json!({
            "current_revision": revision_text(&layout.revision),
            "expected_revision": revision_text(expected),
        }))
    }

    fn plan_field_placement(&mut self, params: &PlacementParams) -> Result<Value, AuthorError> {
        let expected = params
            .expected_revision
            .as_deref()
            .map(parse_revision)
            .transpose()?;
        let mut layout = self.load_layout(&LayoutParams {
            layout: params.layout.clone(),
            layout_name: params.layout_name.clone(),
        })?;
        let place = Self::available(&mut layout.opened, &catalog::PLACE_FIELD)?;
        let (table, field) = self.placement_field(&layout, &params.field, &params.field_name)?;
        let placement = Self::placement(&layout, field.id, params);
        let field_json = json!({
            "ref": self.field_ref(table, field.id),
            "name": field.name,
            "type": type_name(field.data_type),
        });
        let stale = expected.is_some_and(|e| e != layout.revision);
        if let Some((field_object, label_object)) = stale
            .then(|| layout_placement::already_placed(&layout.member, &placement))
            .flatten()
        {
            return Ok(json!({
                "layout": self.layout_json(&layout),
                "base_revision": revision_text(&layout.revision),
                "already_applied": true,
                "field": field_json,
                "objects": self.placed_json(layout.id, &placement, field_object, label_object, table, field),
                "place": place,
            }));
        }
        if let Some(expected) = expected.filter(|_| stale) {
            return Err(self.layout_conflict(&layout, &expected));
        }
        let planned =
            layout_placement::plan(&layout.member, &placement).map_err(placement_refusal)?;
        let changes: Vec<Value> = planned
            .changes
            .iter()
            .map(|(key, holds)| json!({ "key": key, "holds": holds }))
            .collect();
        let part = layout_placement::parts(&layout.member)
            .ok()
            .and_then(|parts| parts.into_iter().find(|p| p.id == planned.part))
            .map(|p| json!({ "id": p.id, "kind": part_kind(p.kind()), "top": p.top, "height": p.height }));
        Ok(json!({
            "layout": self.layout_json(&layout),
            "base_revision": revision_text(&layout.revision),
            "already_applied": false,
            "field": field_json,
            "objects": self.placed_json(
                layout.id,
                &placement,
                planned.field_object,
                planned.label_object,
                table,
                field,
            ),
            "part": part,
            "stacking": planned.stacking,
            "overlaps": planned
                .overlaps
                .iter()
                .map(|&o| self.object_ref(layout.id, o))
                .collect::<Vec<_>>(),
            "key_allocation": "The host allocates the keys when it places the objects: the next \
                               after every key the layout uses, these unless the layout changes \
                               first. It gives each object fresh UUIDs.",
            "changes": changes,
            "place": place,
        }))
    }

    fn place_field(&mut self, params: &PlaceFieldParams) -> Result<Value, AuthorError> {
        let expected = parse_revision(&params.expected_revision)?;
        let plan = params.plan();
        let mut layout = self.load_layout(&LayoutParams {
            layout: plan.layout.clone(),
            layout_name: plan.layout_name.clone(),
        })?;
        let (table, field) = self.placement_field(&layout, &plan.field, &plan.field_name)?;
        let field = field.clone();
        let placement = Self::placement(&layout, field.id, &plan);
        let layout_id = layout.id;
        let label = self.layout_ref(layout_id);
        if expected != layout.revision {
            // A matching prior placement can be a lost answer, but only
            // the host can validate its current owning-table identity.
            // Let its guarded retry path decide, then verify fresh data.
            if layout_placement::already_placed(&layout.member, &placement).is_none() {
                return Err(self.layout_conflict(&layout, &expected));
            }
        } else {
            layout_placement::plan(&layout.member, &placement).map_err(placement_refusal)?;
        }
        Self::require_grant(&mut layout.opened, &catalog::PLACE_FIELD)?;
        let base = layout.member.clone();
        let placed = layout.opened.place_field(wire::PlaceField {
            layout: layout_id,
            expected,
            table,
            occurrence: placement.occurrence,
            field: field.id,
            frame: wire_frame(&placement.frame),
            label: placement
                .label
                .as_ref()
                .map(|l| (l.text.clone(), wire_frame(&l.frame))),
        });
        drop(layout);
        let placed = placed.map_err(|e| {
            authoring_error(e, &label, |reason| {
                AuthorError::new(
                    ErrorKind::OutcomeUnknown,
                    format!(
                        "The placement of “{}” on {label} was sent but no answer came back \
                         ({reason}), so it may or may not have been made. Read the layout \
                         again with get_layout, or repeat the same place_field: it reports \
                         already_applied if the objects are there.",
                        field.name
                    ),
                )
                .with_details(json!({ "previous_revision": revision_text(&expected) }))
            })
        })?;
        let label_object = Some(placed.label_object).filter(|&k| k != 0);
        let verified = self.verify_placement(layout_id, table, &placed, &placement, &base)?;
        Ok(json!({
            "status": if placed.changed { "placed" } else { "already_applied" },
            "layout": label,
            "objects": self.placed_json(layout_id, &placement, placed.field_object, label_object, table, &field),
            "previous_revision": revision_text(&expected),
            "revision": revision_text(&placed.revision),
            "verified": verified,
        }))
    }

    /// Read layout `layout` back over a new connection: it must be at the
    /// revision the host reported, decode as the desktop client decodes it
    /// with the new objects on top as `placement` makes them, list the
    /// field object among the layout's fields, and hold everything of
    /// `base` as it was.
    fn verify_placement(
        &self,
        layout: u64,
        table: u64,
        placed: &wire::Placed,
        placement: &Placement,
        base: &OwnedNode,
    ) -> Result<Value, AuthorError> {
        let unverified = |e: AuthorError| {
            json!({
                "fresh_connection": false,
                "reason": format!(
                    "The host reported the field placed, but reading it back failed: {}",
                    e.message
                ),
            })
        };
        let mut fresh = match self.open() {
            Ok(fresh) => fresh,
            Err(e) => return Ok(unverified(e)),
        };
        let member = match fresh.layout_member(layout) {
            Ok(Some(member)) => member,
            Ok(None) => {
                return Err(AuthorError::new(
                    ErrorKind::VerificationFailed,
                    format!(
                        "The host reported the field placed on {}, but a new connection does \
                         not find the layout.",
                        self.layout_ref(layout)
                    ),
                ))
            }
            Err(e) => return Ok(unverified(Self::read_error(e))),
        };
        let names = match fresh.calc_schema() {
            Ok(names) => names,
            Err(e) => return Ok(unverified(Self::read_error(e))),
        };
        let found = layout_revision(&member);
        let decoded = layout_placement::decode(&member);
        let label_object = Some(placed.label_object).filter(|&k| k != 0);
        let field_object = decoded
            .as_ref()
            .and_then(|d| d.objects.get(placed.field_object))
            .filter(|o| layout_placement::is_placed_field(o, placement));
        let label_ok = match (&placement.label, label_object) {
            (None, None) => true,
            (Some(label), Some(key)) => decoded
                .as_ref()
                .and_then(|d| d.objects.get(key))
                .is_some_and(|o| layout_placement::is_placed_label(o, label)),
            _ => false,
        };
        let expected_top: Vec<u64> = std::iter::once(placed.field_object)
            .chain(label_object)
            .collect();
        let on_top = decoded
            .as_ref()
            .is_some_and(|d| d.objects.root.ends_with(&expected_top));
        let uuid = field_object.and_then(|o| o.uuid).map(|u| uuid_text(&u));
        // Everything stored before: the object store's bytes, every other
        // key, each object's attributes, the field reference index.
        let stored = |m: &OwnedNode| layout_placement::stored_objects(m).ok().map(|s| s.0);
        let store_kept = match (stored(base), stored(&member)) {
            (Some(old), Some(new)) => new.starts_with(&old),
            _ => false,
        };
        let keys_kept = {
            let (old, new) = (kept_layout_keys(base), kept_layout_keys(&member));
            old == new
        };
        // Each object's attributes; the last key issued only rises, past
        // the new objects.
        let attributes_kept = base.child(&[5]).is_none_or(|old| {
            member.child(&[5]).is_some_and(|new| {
                old.children.iter().all(|record| {
                    new.child(&record.segment).is_some_and(|found| {
                        if record.segment == [0] {
                            found.children == record.children
                        } else {
                            found == record
                        }
                    })
                })
            })
        });
        let last_issued = layout_placement::last_issued(&member).ok().flatten();
        let issued_past = expected_top
            .iter()
            .all(|&key| last_issued.is_some_and(|last| last >= key))
            && layout_placement::last_issued(base)
                .ok()
                .flatten()
                .is_none_or(|old| last_issued.is_some_and(|last| last >= old));
        let index_kept = [1u8, 0x0a].iter().all(|&key| {
            base.child(&[key]).is_none_or(|old| {
                member
                    .child(&[key])
                    .is_some_and(|new| only_gained(old, new))
            })
        });
        let indexed = layout_placement::indexes(&member, placed.field_object);
        // The parts as stored, but for the new objects at the end of their
        // part's list.
        let parts_kept = layout_placement::part_lists_extended(base, &member, &expected_top);
        // The stored table view as the plan writes it from the one stored
        // before: the field's column added (#815), or none, as before. A
        // repeated request the host found already placed wrote nothing, so
        // the table view is the one read before it.
        let table_view_as_planned = if placed.changed {
            layout_placement::planned_table_view(base, placement)
                .is_ok_and(|planned| planned.as_ref() == member.child(&[0x0d]))
        } else {
            base.child(&[0x0d]) == member.child(&[0x0d])
        };
        // The object index and tab order as the plan writes them: the
        // stored entries as they were, the edit box's entry in each, and its
        // tab position in its attribute record. A repeated request wrote
        // nothing, as above.
        // ([#316](https://github.com/OpenMaker-Dev/openmaker/issues/316))
        let indexes_as_planned = if placed.changed {
            layout_placement::object_index_and_tab_order_as_planned(
                base,
                &member,
                placement,
                placed.field_object,
            )
        } else {
            [0x0a, 0x15]
                .iter()
                .all(|&key| base.child(&[key]) == member.child(&[key]))
        };
        let table_matches = names
            .as_ref()
            .and_then(|schema| schema.table_of(placement.occurrence))
            .is_some_and(|owner| owner.id == table);
        let listed = names
            .as_ref()
            .and_then(|n| n.layouts().iter().find(|l| u64::from(l.id) == layout))
            .is_some_and(|l| {
                l.fields.iter().any(|f| {
                    f.object == placed.field_object
                        && f.occurrence == placement.occurrence
                        && f.field == placement.field
                })
            });
        let kept = store_kept
            && keys_kept
            && attributes_kept
            && issued_past
            && index_kept
            && parts_kept
            && table_view_as_planned
            && indexes_as_planned;
        if found != placed.revision
            || !indexed
            || field_object.is_none()
            || !label_ok
            || !on_top
            || !kept
            || !listed
            || !table_matches
        {
            return Err(AuthorError::new(
                ErrorKind::VerificationFailed,
                format!(
                    "The host reported the field placed on {}, but a new connection reads \
                     another layout.",
                    self.layout_ref(layout)
                ),
            )
            .with_details(json!({
                "expected_revision": revision_text(&placed.revision),
                "found_revision": revision_text(&found),
                "field_object_as_planned": field_object.is_some(),
                "label_as_planned": label_ok,
                "on_top": on_top,
                "listed_among_layout_fields": listed,
                "owning_table_matches": table_matches,
                "stored_objects_kept": store_kept,
                "other_keys_kept": keys_kept,
                "object_attributes_kept": attributes_kept,
                "field_index_kept": index_kept,
                "object_indexed": indexed,
                "part_lists_extended_as_planned": parts_kept,
                "table_view_as_planned": table_view_as_planned,
                "object_index_and_tab_order_as_planned": indexes_as_planned,
                "last_issued_key_past_new_objects": issued_past,
            })));
        }
        Ok(json!({
            "fresh_connection": true,
            "revision": revision_text(&found),
            "decoded_as_desktop_client": true,
            "field_object_uuid": uuid,
            "on_top": true,
            "listed_among_layout_fields": true,
            "stored_objects_kept": true,
            "other_keys_kept": kept_layout_keys(base).len(),
        }))
    }
}

impl Service {
    /// Run the script on the host and wait for it to end. Nothing is sent
    /// until the script is found (and, with `expected_revision`, found at
    /// that revision); once the request is sent, a failure is
    /// `outcome_unknown`, never retried.
    fn run_script(&mut self, params: &RunScriptParams) -> Result<Value, AuthorError> {
        let expected = params
            .expected_revision
            .as_deref()
            .map(parse_revision)
            .transpose()?;
        let parameter = params.parameter.as_deref().unwrap_or("");
        if parameter.chars().count() > om_protocol::script::TEXT_LIMIT {
            return Err(AuthorError::new(
                ErrorKind::InvalidRequest,
                format!(
                    "The parameter is longer than {} characters, which Perform Script on \
                     Server does not send (FileMaker's error {}); nothing was run.",
                    om_protocol::script::TEXT_LIMIT,
                    om_protocol::script::PARAMETER_TOO_LONG
                ),
            ));
        }
        let mut opened = self.open()?;
        let definitions = opened.script_definitions().map_err(Self::read_error)?;
        let id = self.resolve(&definitions, &params.target())?;
        let label = reference("script", id, &self.file());
        let name = definitions
            .catalog
            .scripts
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let current = script_revision(&definitions.definitions[&id]);
        if let Some(expected) = expected.filter(|expected| *expected != current) {
            return Err(AuthorError::new(
                ErrorKind::Conflict,
                format!("{label} changed since the expected revision; nothing was run."),
            )
            .with_details(json!({
                "current_revision": revision_text(&current),
                "expected_revision": revision_text(&expected),
            })));
        }
        let call = om_data::runtime::ServerScriptCall {
            file: 0,
            script: id,
            parameter: params
                .parameter
                .as_ref()
                .map(|text| om_data::calc::Value::text(text.as_str())),
            wait: true,
        };
        let ran = opened.perform_script_on_server(&call).map_err(|e| {
            AuthorError::new(
                ErrorKind::OutcomeUnknown,
                format!(
                    "The run of {label} was requested but no answer came back ({e}): the \
                     script may have run, in part or whole. Check what it changes before \
                     running it again."
                ),
            )
            .with_details(json!({ "script": label, "revision": revision_text(&current) }))
        })?;
        drop(opened);
        Ok(json!({
            "script": label,
            "name": name,
            "revision": revision_text(&current),
            "status": "completed",
            "last_error": ran.error.0,
            "result": ran.result.as_ref().map(result_text).unwrap_or_default(),
        }))
    }
}

/// Completion uses a hydrated calculation schema: imported members have
/// client-only ids. References retain the owning file's native identity.
fn completion_field_reference(
    schema: &CalcSchema,
    occurrence: u64,
    field: u64,
    file: &str,
) -> Option<String> {
    let table = schema.table_of(occurrence)?;
    Some(match schema.table_context(table.id) {
        Some(owner) => reference(
            "field",
            format!("{}.{field}", owner.file_id(table.id)),
            &om_data::file_refs::hosted_name(&owner.file_name),
        ),
        None => reference("field", format!("{}.{field}", table.id), file),
    })
}

fn completion_occurrence_reference(schema: &CalcSchema, occurrence: u64, file: &str) -> String {
    match schema.file_context(occurrence) {
        Some(owner) => reference(
            "occurrence",
            owner.file_id(occurrence),
            &om_data::file_refs::hosted_name(&owner.file_name),
        ),
        None => reference("occurrence", occurrence, file),
    }
}

/// A script result as text: Perform Script on Server returns text.
fn result_text(value: &om_data::calc::Value) -> String {
    match value {
        om_data::calc::Value::Text(text) => text.as_str().to_owned(),
        other => {
            let locale = om_data::calc::Locale::en_us();
            let today = om_data::calc::Date::from_ymd(2001, 1, 1).expect("a valid date");
            other
                .to_text(&om_data::calc::Ctx::new(&locale, today))
                .into_string()
        }
    }
}

/// An authoring failure as an error; `outcome_unknown` makes the error for
/// a request that was sent and not answered, from the reason.
/// The new layout's UUID, which the host keeps as the request's identity:
/// the same for the same call made from the same revision, so a repeat after
/// a lost answer finds the layout it made, and different for any other call.
/// A name-based UUID (version 5 form) of a SHA-256 over the parameters.
fn layout_request_uuid(expected: &Revision, occurrence: u64, like: u64, name: &str) -> [u8; 16] {
    let request = OwnedNode::list(
        b"create_layout".to_vec(),
        vec![
            OwnedNode::leaf(vec![1], expected.to_vec()),
            OwnedNode::leaf(vec![2], occurrence.to_be_bytes().to_vec()),
            OwnedNode::leaf(vec![3], like.to_be_bytes().to_vec()),
            OwnedNode::leaf(vec![4], name.as_bytes().to_vec()),
        ],
    );
    let hash = design_paths::script_form_revision(&request);
    let mut uuid: [u8; 16] = hash[..16].try_into().expect("16 bytes");
    uuid[6] = (uuid[6] & 0x0f) | 0x50;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    uuid
}

/// The identity of a duplicate_layout request: the same for the same
/// layout at the same revision, so a repeat finds the copy it made.
fn duplicate_request_uuid(expected: &Revision, layout: u64) -> [u8; 16] {
    let request = OwnedNode::list(
        b"duplicate_layout".to_vec(),
        vec![
            OwnedNode::leaf(vec![1], expected.to_vec()),
            OwnedNode::leaf(vec![2], layout.to_be_bytes().to_vec()),
        ],
    );
    let hash = design_paths::script_form_revision(&request);
    let mut uuid: [u8; 16] = hash[..16].try_into().expect("16 bytes");
    uuid[6] = (uuid[6] & 0x0f) | 0x50;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    uuid
}

/// The layout catalog index's revision (`[4, 1]`): what create_layout's
/// `expected_revision` names.
fn layouts_revision(index: Option<&OwnedNode>) -> Revision {
    design_paths::catalog_revision(index.unwrap_or(&OwnedNode::list(vec![1], Vec::new())))
}

fn authoring_error(
    error: AuthoringError,
    label: &str,
    outcome_unknown: impl FnOnce(String) -> AuthorError,
) -> AuthorError {
    match error {
        AuthoringError::Refused {
            status: code,
            message,
            current: host_current,
        } => {
            let kind = match code {
                status::CONFLICT => ErrorKind::Conflict,
                status::PERMISSION_DENIED => ErrorKind::PermissionDenied,
                status::IN_USE | status::SCHEMA_IN_USE | status::LAYOUT_IN_USE => ErrorKind::Locked,
                status::SCRIPT_MISSING
                | status::TABLE_MISSING
                | status::LAYOUT_MISSING
                | status::FIELD_MISSING => ErrorKind::NotFound,
                status::NAME_EXISTS => ErrorKind::DuplicateName,
                status::NAME_UNUSABLE => ErrorKind::InvalidName,
                status::INVALID => ErrorKind::InvalidRequest,
                code if status::SAVE_FAILED.contains(&code) => ErrorKind::SaveFailed,
                _ => ErrorKind::Unsupported,
            };
            let mut details = json!({ "status": code });
            if let Some(revision) = host_current {
                details["current_revision"] = json!(revision_text(&revision));
            }
            AuthorError::new(kind, format!("{message} Nothing was written to {label}."))
                .with_details(details)
        }
        AuthoringError::Unavailable(reason) => AuthorError::new(ErrorKind::Unsupported, reason),
        AuthoringError::NotSent(reason) => AuthorError::new(
            ErrorKind::Connection,
            format!("The edit was not sent ({reason}); nothing was written to {label}."),
        ),
        AuthoringError::OutcomeUnknown(reason) => outcome_unknown(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_references_keep_related_files_native_ids() {
        use om_data::calc::{Locale, ValueType};
        use om_data::host::{ExternalOccurrence, FieldDef};

        let mut root = CalcSchema::new("A", Locale::en_us());
        root.add_table(32769, "Local");
        root.add_occurrence(2, "Local", 32769);
        root.add_field(32769, FieldDef::data(1, "First Name", ValueType::Text));
        root.add_external_occurrence(ExternalOccurrence {
            id: 3,
            name: "RelatedContacts".into(),
            source: "B".into(),
        });
        let mut related = CalcSchema::new("B", Locale::en_us());
        related.add_table(32769, "Contacts");
        related.add_occurrence(2, "Contacts", 32769);
        related.add_field(32769, FieldDef::data(1, "First Name", ValueType::Text));
        let imported = root.import_file_schema(&related);
        assert!(root.bind_external_occurrence(3, imported.tables[&32769]));
        let text = "RelatedContacts::Fi";
        let completion = om_data::formula_editor::complete(
            text,
            text.chars().count(),
            om_data::formula_editor::Scope {
                occurrence: Some(2),
                unqualified: false,
            },
            Some(&root),
        )
        .unwrap();
        let (occurrence, field) = completion.candidates[0].field.unwrap();
        assert_eq!(
            completion_field_reference(&root, occurrence, field, "A.fmp12").as_deref(),
            Some("field:32769.1@B.fmp12")
        );
        // A's occurrence of B's table still belongs to A.
        assert_eq!(
            completion_occurrence_reference(&root, occurrence, "A.fmp12"),
            "occurrence:3@A.fmp12"
        );
        // Imported occurrences (including the response context) belong to B.
        let occurrence = imported.occurrences[&2];
        assert_eq!(
            completion_occurrence_reference(&root, occurrence, "A.fmp12"),
            "occurrence:2@B.fmp12"
        );
        assert_eq!(
            completion_field_reference(&root, occurrence, 1, "A.fmp12").as_deref(),
            Some("field:32769.1@B.fmp12")
        );
        assert_eq!(
            completion_field_reference(&root, 2, 1, "A.fmp12").as_deref(),
            Some("field:32769.1@A.fmp12")
        );
        assert_eq!(
            completion_occurrence_reference(&root, 2, "A.fmp12"),
            "occurrence:2@A.fmp12"
        );
    }

    #[test]
    fn revisions_read_back_as_written() {
        let revision = [0xab; 32];
        let text = revision_text(&revision);
        assert!(text.starts_with("sha256:abab"));
        assert_eq!(parse_revision(&text).unwrap(), revision);
        for bad in ["abab", "sha256:zz", "sha256:", &text[..20]] {
            assert_eq!(
                parse_revision(bad).unwrap_err().kind,
                ErrorKind::InvalidRequest
            );
        }
    }
}
