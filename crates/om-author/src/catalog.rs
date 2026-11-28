//! The authoring commands, described once (AGENTS.md rule 3). Each
//! [`CommandSpec`] is one command of `openmaker.authoring/v1`: its stable
//! id (a request's `command`), its CLI name and MCP tool, its title and
//! description, its effect, what it needs from the host and the type of
//! its parameters. The request parser ([`crate::api`]), the MCP
//! tool list ([`crate::mcp`]), the CLI, `capabilities` and the service's
//! availability checks all read this table; the `commands` command returns
//! it, with each command's parameters as JSON Schema, without a
//! connection.
//!
//! Results are not typed here yet: each command's result is described in
//! `docs/authoring.md`, and every failure is an [`ErrorKind`].

use std::sync::Arc;

use rmcp::handler::server::common::schema_for_type;
use rmcp::model::JsonObject;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{AnalysisParams, AnchorParams, ObjectParams, ScriptCallsParams, UnusedParams};
use crate::api::{
    ApplyParams, ArrangeObjectsParams, AuthorError, ChangePartsParams, ChoiceParams, ChooseParams,
    Command, CompareDesignsParams, CompletionParams, CreateFieldParams, CreateFileParams,
    CreateLayoutParams, DeleteLayoutParams, DeleteObjectsParams, DuplicateLayoutParams, EditParams,
    ErrorKind, FieldControlParams, FieldParams, InspectParams, LayoutParams, LayoutSetupParams,
    LayoutWidthParams, MoveObjectsParams, NoParams, ObjectFramesParams, OtherParams,
    PartOptionsParams, PlaceFieldParams, PlacePortalParams, PlaceTextParams, PlaceWebViewerParams,
    PlacementParams, RunScriptParams, ScriptParams, TextParams, ValueListParams,
    ValueListValuesParams, API_VERSION,
};

/// What a command can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Nothing: it reads metadata, or nothing at all.
    Reads,
    /// The file's design, through the host's authoring grant. It checks a
    /// revision first. A repeated request can report `already_applied`
    /// when the earlier change can be verified.
    Authors,
    /// Whatever the script it runs does: records, other files the account
    /// may change, and systems outside the host. Running it again runs the
    /// script again.
    Runs,
    /// A record's field, as a user's edit in a window changes it: through
    /// the window's triggers, the record's draft, validation and commit.
    Edits,
    /// A new file on this computer. It never replaces a file.
    Creates,
}

/// What a command needs before it can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Needs {
    /// Nothing: it is answered without a connection.
    Nothing,
    /// The file, opened over a new connection with the configured account.
    File,
    /// The file, and the host's grant of an authoring operation.
    Grant(Grant),
    /// A desktop window: its field focus, the record's draft and its script
    /// triggers. This service has none, so such a command answers
    /// `unsupported` with `details.unavailable` = `no_window`.
    Window,
}

/// An authoring operation a host grants or refuses. Only OpenMaker Server
/// offers them (its authoring extension), and only to accounts allowed to
/// author; any other host, FileMaker Server included, answers
/// `unsupported` and nothing is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    /// Replacing a script's steps.
    ScriptSteps,
    /// Adding fields to tables.
    Fields,
    /// Placing fields on layouts.
    Layouts,
    /// Adding layouts.
    NewLayouts,
    /// Moving and resizing layout objects.
    LayoutObjects,
    /// Deleting layout objects.
    ObjectDeletion,
    /// Placing texts on layouts on their own.
    Texts,
    /// Changing value lists' custom values.
    ValueLists,
    /// Duplicating layouts.
    LayoutCopies,
    /// Deleting layouts.
    LayoutDeletion,
    /// Layout Setup: layouts' names, views and menu inclusion.
    LayoutSetup,
    /// Layouts' widths.
    LayoutWidths,
    /// Field objects' controls.
    FieldControls,
    /// Layout parts' page options.
    PartOptions,
    /// A layout text's words.
    TextEditing,
    AnchorEditing,
    /// Placing web viewers on layouts.
    WebViewers,
}

/// One command.
pub struct CommandSpec {
    /// The command's name in requests and responses. Never changes within
    /// a version.
    pub id: &'static str,
    /// The CLI's name for it.
    pub cli: &'static str,
    /// The MCP tool that runs it.
    pub tool: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// What it can change. A command that [`Effect::Reads`] changes
    /// nothing, whatever its parameters.
    pub effect: Effect,
    pub needs: Needs,
    /// The entry of `capabilities`' `operations` that says whether it is
    /// available on the host, if it has one.
    pub operation: Option<&'static str>,
    params: fn() -> Arc<JsonObject>,
    parse: fn(Value) -> Result<Command, AuthorError>,
}

impl CommandSpec {
    /// Whether it can change anything.
    pub fn writes(&self) -> bool {
        self.effect != Effect::Reads
    }

    /// The JSON Schema of the command's parameters.
    pub fn params_schema(&self) -> Arc<JsonObject> {
        (self.params)()
    }

    /// The command with `params` (an object, or null for none).
    pub fn parse(&self, params: Value) -> Result<Command, AuthorError> {
        (self.parse)(if params.is_null() { json!({}) } else { params })
    }

    /// The command as `commands` describes it.
    pub fn to_json(&self) -> Value {
        let (needs, grant) = match self.needs {
            Needs::Nothing => ("nothing", None),
            Needs::File => ("file", None),
            Needs::Grant(grant) => ("grant", Some(grant)),
            Needs::Window => ("window", None),
        };
        let mut out = json!({
            "id": self.id,
            "title": self.title,
            "description": self.description,
            "writes": self.writes(),
            "effect": match self.effect {
                Effect::Reads => "reads",
                Effect::Authors => "authors",
                Effect::Runs => "runs",
                Effect::Edits => "edits",
                Effect::Creates => "creates",
            },
            "needs": needs,
            "operation": self.operation,
            "cli": self.cli,
            "mcp_tool": self.tool,
            "params": Value::Object((*self.params_schema()).clone()),
        });
        if let Some(grant) = grant {
            out["grant"] = json!(match grant {
                Grant::ScriptSteps => "script_steps",
                Grant::Fields => "fields",
                Grant::Layouts => "layouts",
                Grant::NewLayouts => "new_layouts",
                Grant::LayoutObjects => "layout_objects",
                Grant::ObjectDeletion => "object_deletion",
                Grant::Texts => "texts",
                Grant::LayoutCopies => "layout_copies",
                Grant::LayoutDeletion => "layout_deletion",
                Grant::LayoutSetup => "layout_setup",
                Grant::LayoutWidths => "layout_widths",
                Grant::FieldControls => "field_controls",
                Grant::PartOptions => "part_options",
                Grant::TextEditing => "text_editing",
                Grant::AnchorEditing => "anchor_editing",
                Grant::WebViewers => "web_viewers",
                Grant::ValueLists => "value_lists",
            });
        }
        out
    }
}

fn read<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, AuthorError> {
    serde_json::from_value(params).map_err(|e| {
        AuthorError::new(
            ErrorKind::InvalidRequest,
            format!("Invalid parameters: {e}"),
        )
    })
}

pub static CREATE_FILE: CommandSpec = CommandSpec {
    id: om_data::new_file::CREATE_FILE,
    cli: "create-file",
    tool: "openmaker_create_file",
    title: "Create a new file",
    description: "Make a new, blank FileMaker file on this computer, as File > Create New... \
                  does: one table, its occurrence and one Form layout named as the file, \
                  OpenMaker's own theme, and the account Admin with full access and no \
                  password. `.fmp12` is added to a path without it; a file already there is \
                  never replaced. Needs no connection: the file is not hosted, and FileMaker \
                  Server will not host it until Admin has a password.",
    effect: Effect::Creates,
    needs: Needs::Nothing,
    operation: None,
    params: schema_for_type::<CreateFileParams>,
    parse: |params| read::<CreateFileParams>(params).map(Command::CreateFile),
};

pub static COMPARE_DESIGNS: CommandSpec = CommandSpec {
    id: om_hbam::design_paths::COMPARE_DESIGNS,
    cli: "compare-designs",
    tool: "openmaker_compare_designs",
    title: "Compare two files' designs",
    description: "Compare the designs of two FileMaker files on this computer, a source and a \
                  target, with an optional base they were both made from. With a base, objects \
                  are matched by kind and id when the base has them. Without one, by kind and \
                  id when their names agree, or when neither has a name and all their content \
                  is equal, which does not prove they share a history. An object agreeing in \
                  id alone, or without a base in name alone, is listed as a possible match, \
                  never matched. For each matched object it lists the parts that differ \
                  (design; step_data, any change in a step's stored parameters, a password's \
                  or another's alike; unidentified; data; derived; none when the same) and, \
                  with a base, which side changed it. It reports kinds, ids, names, these \
                  parts and design revisions, never content, passwords or edit tokens. Reads \
                  the files only. Needs no connection.",
    effect: Effect::Reads,
    needs: Needs::Nothing,
    operation: None,
    params: schema_for_type::<CompareDesignsParams>,
    parse: |params| read::<CompareDesignsParams>(params).map(Command::CompareDesigns),
};

pub static COMMANDS: CommandSpec = CommandSpec {
    id: "commands",
    cli: "commands",
    tool: "openmaker_commands",
    title: "List authoring commands",
    description: "Every authoring command: its id, title and description, whether it writes, \
                  what it needs from the host (nothing, the file, or the host's grant of an \
                  authoring operation, which capabilities reports), its CLI name and MCP tool, \
                  and its parameters as JSON Schema. Needs no connection.",
    effect: Effect::Reads,
    needs: Needs::Nothing,
    operation: None,
    params: schema_for_type::<NoParams>,
    parse: |params| read::<NoParams>(params).map(|_| Command::Commands),
};

pub static CAPABILITIES: CommandSpec = CommandSpec {
    id: "capabilities",
    cli: "capabilities",
    tool: "openmaker_capabilities",
    title: "Authoring capabilities",
    description: "What the host and account allow for the configured FileMaker file: which \
                  authoring operations are available, and the limits.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: None,
    params: schema_for_type::<NoParams>,
    parse: |params| read::<NoParams>(params).map(|_| Command::Capabilities),
};

pub static INSPECT: CommandSpec = CommandSpec {
    id: "inspect",
    cli: "inspect",
    tool: "openmaker_inspect",
    title: "Inspect solution metadata",
    description: "The file's scripts, tables and fields, table occurrences, layouts, value \
                  lists and data sources, each with a file-qualified reference. Reads metadata \
                  only, never records.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("inspect"),
    params: schema_for_type::<InspectParams>,
    parse: |params| read(params).map(Command::Inspect),
};

pub static GET_SCRIPT: CommandSpec = CommandSpec {
    id: "get_script",
    cli: "get-script",
    tool: "openmaker_get_script",
    title: "Get script source",
    description: "A script's steps as editable text (one step per line, tab-indented), its \
                  revision, and each step's fidelity: parts the visible text leaves out travel \
                  in `Stored …` options.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("get_script"),
    params: schema_for_type::<ScriptParams>,
    parse: |params| read(params).map(Command::GetScript),
};

pub static VALIDATE: CommandSpec = CommandSpec {
    id: "validate",
    cli: "validate",
    tool: "openmaker_validate_script",
    title: "Validate script source",
    description: "Compile edited script text against the file's names; reports each problem \
                  with its line. Writes nothing.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("validate"),
    params: schema_for_type::<EditParams>,
    parse: |params| read(params).map(Command::Validate),
};

pub static PLAN: CommandSpec = CommandSpec {
    id: "plan",
    cli: "plan",
    tool: "openmaker_plan_script_edit",
    title: "Plan a script edit",
    description: "The change edited script text would make: the base and after revisions, a \
                  unified diff and step counts. Writes nothing.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("plan"),
    params: schema_for_type::<EditParams>,
    parse: |params| read(params).map(Command::Plan),
};

pub static APPLY: CommandSpec = CommandSpec {
    id: "apply",
    cli: "apply",
    tool: "openmaker_apply_script_edit",
    title: "Apply a script edit",
    description: "Replace the script's steps with the edited text if the script is still at \
                  expected_revision, then read it back over a new connection. Only the steps \
                  change. A retry of a native save can report outcome_unknown even if the \
                  requested steps are present, because the index upload also needs confirmation.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::ScriptSteps),
    operation: Some("apply_script_steps"),
    params: schema_for_type::<ApplyParams>,
    parse: |params| read(params).map(Command::Apply),
};

pub static PLAN_FIELD: CommandSpec = CommandSpec {
    id: "plan_field",
    cli: "plan-field",
    tool: "openmaker_plan_field",
    title: "Plan a new field",
    description: "What adding a field to a table would change: the field's definition (a \
                  normal field with stored values of type text, number, date, time or \
                  timestamp and FileMaker's defaults), the id the host would give it, the \
                  catalog keys it writes and the table's fields_revision. Reports unusable and \
                  duplicate names. Writes nothing.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("plan_field"),
    params: schema_for_type::<FieldParams>,
    parse: |params| read(params).map(Command::PlanField),
};

pub static CREATE_FIELD: CommandSpec = CommandSpec {
    id: "create_field",
    cli: "create-field",
    tool: "openmaker_create_field",
    title: "Create a field",
    description: "Add a field to a table if the table's fields are still at expected_revision \
                  (inspect's fields_revision), then read it back over a new connection. The \
                  host allocates the id; every other field and catalog key is kept. Repeating \
                  the same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::Fields),
    operation: Some("create_field"),
    params: schema_for_type::<CreateFieldParams>,
    parse: |params| read(params).map(Command::CreateField),
};

pub static GET_LAYOUT: CommandSpec = CommandSpec {
    id: "get_layout",
    cli: "get-layout",
    tool: "openmaker_get_layout",
    title: "Get a layout",
    description: "A layout's parts, width and objects (each with a file-qualified reference, \
                  its kind, frame, place in the stacking order, and for a field object the \
                  field it shows), and its revision. Reads metadata only, never records.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("get_layout"),
    params: schema_for_type::<LayoutParams>,
    parse: |params| read(params).map(Command::GetLayout),
};

pub static PLAN_FIELD_PLACEMENT: CommandSpec = CommandSpec {
    id: "plan_field_placement",
    cli: "plan-placement",
    tool: "openmaker_plan_field_placement",
    title: "Plan a field placement",
    description: "What placing a field of the layout's table on the layout would change: the \
                  edit box (and optional text label) with the keys the host would give them, \
                  the part, the objects it overlaps, the catalog keys it writes and the \
                  layout's revision. Reports frames outside the body or the layout's width, \
                  and containers in the way. Writes nothing.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("plan_field_placement"),
    params: schema_for_type::<PlacementParams>,
    parse: |params| read(params).map(Command::PlanFieldPlacement),
};

pub static PLACE_FIELD: CommandSpec = CommandSpec {
    id: "place_field",
    cli: "place-field",
    tool: "openmaker_place_field",
    title: "Place a field on a layout",
    description: "Place an edit box showing a field of the layout's table, with an optional \
                  text label, on top of the layout's objects in its body, if the layout is \
                  still at expected_revision (get_layout's revision), then read the layout back \
                  over a new connection. The host allocates the keys and UUIDs; every stored \
                  object and key is kept. Repeating the same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::Layouts),
    operation: Some("place_field"),
    params: schema_for_type::<PlaceFieldParams>,
    parse: |params| read(params).map(Command::PlaceField),
};

pub static SET_OBJECT_FRAMES: CommandSpec = CommandSpec {
    id: "set_object_frames",
    cli: "set-object-frames",
    tool: "openmaker_set_object_frames",
    title: "Move or resize layout objects",
    description: "Move or resize top-level objects of a layout (fields showing one repetition, \
                  text, shapes, lines, buttons, pictures) to new frames within the part that \
                  lists each and the layout's width, if the layout is still at expected_revision \
                  (get_layout's revision), then read the layout back over a new connection. Only \
                  each object's bounds and its object index entry change. Repeating the same \
                  call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::LayoutObjects),
    operation: Some("set_object_frames"),
    params: schema_for_type::<ObjectFramesParams>,
    parse: |params| read(params).map(Command::SetObjectFrames),
};

pub static ARRANGE_OBJECTS: CommandSpec = CommandSpec {
    id: "arrange_objects",
    cli: "arrange-objects",
    tool: "openmaker_arrange_objects",
    title: "Align, distribute or resize layout objects",
    description: "Align, distribute or resize top-level objects of a layout relative to one \
                  another, as Layout mode's Arrange menu does (Align, Distribute, Resize To), \
                  using expected_revision (get_layout's revision). Changes use set_object_frames \
                  for revision checking and read-back. Changed frames start on whole points. \
                  When no frames change, the command returns layout, arrangement, revision and \
                  status: unchanged at the expected revision or already_applied at another one.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::LayoutObjects),
    operation: Some("set_object_frames"),
    params: schema_for_type::<ArrangeObjectsParams>,
    parse: |params| read(params).map(Command::ArrangeObjects),
};

pub static MOVE_OBJECTS: CommandSpec = CommandSpec {
    id: om_data::layout_move::ID,
    cli: "move-objects",
    tool: "openmaker_move_objects",
    title: om_data::layout_move::LABEL,
    description: "Move top-level layout objects together by dx and dy in points, using the \
                  same rounding and top/left clamp as the Layout mode canvas. The offset \
                  applies only at expected_revision (get_layout's revision). Writes use \
                  set_object_frames for bounds checks and read-back.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::LayoutObjects),
    operation: Some("set_object_frames"),
    params: schema_for_type::<MoveObjectsParams>,
    parse: |params| read(params).map(Command::MoveObjects),
};

pub static PLACE_TEXT: CommandSpec = CommandSpec {
    id: "place_text",
    cli: "place-text",
    tool: "openmaker_place_text",
    title: "Place a text on a layout",
    description: "Place a text object holding one line of text on a layout's body, on top of its \
                  objects, as FileMaker Pro's Text tool places one, if the layout is still at \
                  expected_revision (get_layout's revision), then read the layout back over a \
                  new connection. The host allocates the key and UUIDs; every stored object and \
                  key is kept. Repeating the same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::Texts),
    operation: Some("place_text"),
    params: schema_for_type::<PlaceTextParams>,
    parse: |params| read(params).map(Command::PlaceText),
};

pub static PLACE_WEB_VIEWER: CommandSpec = CommandSpec {
    id: "place_web_viewer",
    cli: "place-web-viewer",
    tool: "openmaker_place_web_viewer",
    title: "Place a web viewer on a layout",
    description: "Place a web viewer with a custom web address (calculation text, compiled \
                  against the file's names in the layout's table occurrence) on a layout's \
                  body, on top of its objects, as FileMaker Pro's Web Viewer tool and Web Viewer \
                  Setup make one, if the layout is still at expected_revision (get_layout's \
                  revision), then read the layout back over a new connection. Web Viewer \
                  Setup's options default to FileMaker Pro's for a new viewer; only option \
                  combinations FileMaker Pro was seen to store are written. The host allocates \
                  the key and UUID; every stored object and key is kept. Repeating the same call \
                  reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::WebViewers),
    operation: Some("place_web_viewer"),
    params: schema_for_type::<PlaceWebViewerParams>,
    parse: |params| read(params).map(Command::PlaceWebViewer),
};

pub static PLACE_PORTAL: CommandSpec = CommandSpec {
    id: "place_portal",
    cli: "place-portal",
    tool: "openmaker_place_portal",
    title: "Place a portal on a layout",
    description: "Place a portal showing rows of a related table occurrence on a layout's body, \
                  on top of its objects, with fields of that occurrence side by side in its \
                  first row, as FileMaker Pro's Portal tool, Portal Setup (its defaults but the \
                  number of rows) and Add Fields to Portal make one, if the layout is still at \
                  expected_revision (get_layout's revision), then read the layout back over a \
                  new connection. The host allocates the keys and UUIDs; every stored object \
                  and key is kept. Repeating the same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::Layouts),
    operation: Some("place_portal"),
    params: schema_for_type::<PlacePortalParams>,
    parse: |params| read(params).map(Command::PlacePortal),
};

pub static DELETE_OBJECTS: CommandSpec = CommandSpec {
    id: "delete_objects",
    cli: "delete-objects",
    tool: "openmaker_delete_objects",
    title: "Delete layout objects",
    description: "Delete top-level objects of a layout (fields showing one repetition, text, \
                  shapes, lines, buttons; not groups, portals, tab or slide controls, \
                  popovers, popover buttons or button bars, nor objects showing an image \
                  from the file's library or that another object names, such as a field's \
                  label), if the layout is still at \
                  expected_revision (get_layout's revision), then read the layout back over a \
                  new connection. Each object and its records go: its attributes, field \
                  references, name, tab order, part list and object index entries; the other \
                  objects keep every stored byte. Repeating the same call reports \
                  already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::ObjectDeletion),
    operation: Some("delete_objects"),
    params: schema_for_type::<DeleteObjectsParams>,
    parse: |params| read(params).map(Command::DeleteObjects),
};

pub static DUPLICATE_LAYOUT: CommandSpec = CommandSpec {
    id: "duplicate_layout",
    cli: "duplicate-layout",
    tool: "openmaker_duplicate_layout",
    title: "Duplicate a layout",
    description: "Copy a layout as FileMaker Pro's Duplicate Layout does: the copy, named \
                  \"<name> Copy\", is the layout with its parts, objects and settings, at the end \
                  of the layout list, and the file library counts the images its objects show \
                  again. Only if the file's layouts are still at expected_revision (inspect's \
                  layouts_revision); then it is read back over a new connection. A layout whose \
                  own styles show a library image is refused. Repeating the same call reports \
                  already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::LayoutCopies),
    operation: Some("duplicate_layout"),
    params: schema_for_type::<DuplicateLayoutParams>,
    parse: |params| read(params).map(Command::DuplicateLayout),
};

pub static DELETE_LAYOUT: CommandSpec = CommandSpec {
    id: "delete_layout",
    cli: "delete-layout",
    tool: "openmaker_delete_layout",
    title: "Delete a layout",
    description: "Delete a layout as FileMaker Pro's Delete Layout does: the layout and its \
                  objects go, the layout list closes up, and the file library counts the images \
                  its objects showed no more (an image no layout shows any more goes). Only if \
                  the file's layouts are still at expected_revision (inspect's \
                  layouts_revision); then the layout list is read back over a new connection. \
                  Folders, separators, the file's last layout and a layout whose styles show a \
                  library image are refused. Scripts and buttons that go to the layout keep \
                  naming it, as in FileMaker. Repeating the same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::LayoutDeletion),
    operation: Some("delete_layout"),
    params: schema_for_type::<DeleteLayoutParams>,
    parse: |params| read(params).map(Command::DeleteLayout),
};

pub static SET_FIELD_CONTROL: CommandSpec = CommandSpec {
    id: "set_field_control",
    cli: "set-field-control",
    tool: "openmaker_set_field_control",
    title: "Set a field's control style",
    description: "Show a field object as an edit box, drop-down list, pop-up menu, checkbox \
                  set, radio button set or drop-down calendar, with a value list for the list \
                  controls, as the Inspector's Control style does in FileMaker Pro. Only if the \
                  layout is still at expected_revision (get_layout's revision); then the layout \
                  is read back over a new connection. The object takes the theme's style for \
                  its new control. Containers, fields showing several repetitions and \
                  concealed edit boxes are refused, and so is a calendar on a field that is not \
                  a date. Repeating the same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::FieldControls),
    operation: Some("set_field_control"),
    params: schema_for_type::<FieldControlParams>,
    parse: |params| read(params).map(Command::SetFieldControl),
};

pub static CHANGE_PARTS: CommandSpec = CommandSpec {
    id: "change_parts",
    cli: "change-parts",
    tool: "openmaker_change_parts",
    title: "Add, delete or resize a layout's part",
    description: "Add a part of a kind (where FileMaker's order of parts puts it), delete an \
                  empty part, or set a part's height, as FileMaker Pro's Part Setup and a \
                  dragged part boundary do, if the layout is still at expected_revision \
                  (get_layout's revision); then read the layout back over a new connection. \
                  Parts are keyed again from the top, and the objects of the parts that move \
                  move with them. Refused: sub-summaries, a second part of a kind a layout has \
                  once, deleting a part that holds objects or the only part, and a height that \
                  leaves an object outside its part. Repeating the same call reports \
                  already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::PartOptions),
    operation: Some("change_parts"),
    params: schema_for_type::<ChangePartsParams>,
    parse: |params| read(params).map(Command::ChangeParts),
};

pub static SET_PART_OPTIONS: CommandSpec = CommandSpec {
    id: "set_part_options",
    cli: "set-part-options",
    tool: "openmaker_set_part_options",
    title: "Set a part's page options",
    description: "Set a part's page options as FileMaker Pro's Part Definition does: page \
                  break before each occurrence or after every N occurrences, restart page \
                  numbers, allow the part to break across pages, discard the remainder before \
                  a new page, and a leading sub-summary's sort field. Only the options Part \
                  Definition offers for the part's type; only if the layout is still at \
                  expected_revision (get_layout's revision); then the layout is read back over \
                  a new connection. Part types, heights and new or deleted parts are not \
                  changed. Repeating the same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::PartOptions),
    operation: Some("set_part_options"),
    params: schema_for_type::<PartOptionsParams>,
    parse: |params| read(params).map(Command::SetPartOptions),
};

pub static SET_OBJECT_ANCHORS: CommandSpec = CommandSpec {
    id: "set_object_anchors",
    cli: "set-object-anchors",
    tool: "openmaker_set_object_anchors",
    title: "Set an object's autosizing anchors",
    description: "Replace the stored edge anchors of one unlocked text or field on a fixed layout, including a child of a group or portal. Supply a nonempty list of edge names, the object reference and layout revision from get_layout. Saves through the host and verifies with a fresh connection. Repeating a completed edit reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::AnchorEditing),
    operation: Some("set_object_anchors"),
    params: schema_for_type::<AnchorParams>,
    parse: |params| read(params).map(Command::SetObjectAnchors),
};

pub static SET_TEXT: CommandSpec = CommandSpec {
    id: "set_text",
    cli: "set-text",
    tool: "openmaker_set_text",
    title: "Change a text's words",
    description: "Change the words of a text object or a field's label, as typing into it in \
                  FileMaker Pro's Layout mode does: one line of plain text. Only if the \
                  layout is still at expected_revision (get_layout's revision); then the \
                  layout is read back over a new connection. Styled text, merge fields and \
                  texts kept as calculations are refused. Repeating the same call reports \
                  already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::TextEditing),
    operation: Some("set_text"),
    params: schema_for_type::<TextParams>,
    parse: |params| read(params).map(Command::SetText),
};

pub static SET_LAYOUT_WIDTH: CommandSpec = CommandSpec {
    id: "set_layout_width",
    cli: "set-layout-width",
    tool: "openmaker_set_layout_width",
    title: "Set a layout's width",
    description: "Set a layout's width in points, as dragging its right edge in FileMaker Pro's \
                  Layout mode does. Only if the layout is still at expected_revision \
                  (get_layout's revision); then the layout is read back over a new connection. \
                  1 to 32000 points; objects beyond the new width stay, off the layout. \
                  Repeating the same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::LayoutWidths),
    operation: Some("set_layout_width"),
    params: schema_for_type::<LayoutWidthParams>,
    parse: |params| read(params).map(Command::SetLayoutWidth),
};

pub static LAYOUT_SETUP: CommandSpec = CommandSpec {
    id: "layout_setup",
    cli: "layout-setup",
    tool: "openmaker_layout_setup",
    title: "Set up a layout",
    description: "Set a layout's name, the views available (form, list, table), its default \
                  view, whether the layout menus list it, and its fixed page margins (in \
                  points) and whether it prints within them, as FileMaker Pro's Layout Setup \
                  does; what is left out stays as it is. Only if the layout is still at \
                  expected_revision (get_layout's revision); then the layout is read back over a \
                  new connection. At least one view stays available, the default among them. \
                  Folders and separators are refused. Repeating the same call reports \
                  already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::LayoutSetup),
    operation: Some("layout_setup"),
    params: schema_for_type::<LayoutSetupParams>,
    parse: |params| read(params).map(Command::LayoutSetup),
};

pub static CREATE_LAYOUT: CommandSpec = CommandSpec {
    id: "create_layout",
    cli: "create-layout",
    tool: "openmaker_create_layout",
    title: "Create a layout",
    description: "Add a Form layout for a computer, with one empty body part and no objects, \
                  showing records of a table occurrence, in the theme and width of a layout of \
                  the file, if the file's layouts are still at expected_revision (inspect's \
                  layouts_revision), then read it back over a new connection. The host \
                  allocates its id; every stored layout is kept. Repeating the same call reports \
                  already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::NewLayouts),
    operation: Some("create_layout"),
    params: schema_for_type::<CreateLayoutParams>,
    parse: |params| read(params).map(Command::CreateLayout),
};

pub static RUN_SCRIPT: CommandSpec = CommandSpec {
    id: "run_script",
    cli: "run-script",
    tool: "openmaker_run_script",
    title: "Run a script",
    description: "Run a script on the host with Perform Script on Server, as the configured \
                  account and with its privileges, wait for it to end, and return its final \
                  Get ( LastError ) and its Exit Script result. The script may change records \
                  and anything else the account may change. With expected_revision, a script \
                  that changed since is reported as a conflict and not run. A call whose answer \
                  is lost reports outcome_unknown: the script may have run.",
    effect: Effect::Runs,
    needs: Needs::File,
    operation: Some("run_script"),
    params: schema_for_type::<RunScriptParams>,
    parse: |params| read(params).map(Command::RunScript),
};

pub static LIST_CHOICES: CommandSpec = CommandSpec {
    id: "list_choices",
    cli: "list-choices",
    tool: "openmaker_list_choices",
    title: "List a value list's choices",
    description: "The items a field control's value list offers on a record of the layout's \
                  table, as the desktop's pop-up menus, drop-down lists and option sets offer \
                  them: each item's stored value and the label the control shows, with \
                  dividers marked. Reads the records the list's values come from, with the \
                  account's privileges. The list is complete or an error: another file's or a \
                  calculated list, more than 100,000 items, or a table the account cannot read \
                  is reported, never a shorter list. Controls in portals, of related fields or \
                  showing several repetitions are refused.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("list_choices"),
    params: schema_for_type::<ChoiceParams>,
    parse: |params| read(params).map(Command::ListChoices),
};

pub static CHOOSE_VALUE_LIST_ITEM: CommandSpec = CommandSpec {
    id: "choose_value_list_item",
    cli: "choose-item",
    tool: "openmaker_choose_value_list_item",
    title: "Choose a pop-up menu or drop-down list item",
    description: "Choose an item in a pop-up menu or a drop-down list on a record, as a user \
                  does in a window: the field's value becomes the item's stored value, after \
                  the window's triggers, in the record's draft, validated and committed with \
                  the record. It needs a desktop window, which this service has not: it \
                  answers unsupported with details.unavailable = no_window and changes \
                  nothing.",
    effect: Effect::Edits,
    needs: Needs::Window,
    operation: Some("choose_value_list_item"),
    params: schema_for_type::<ChooseParams>,
    parse: |params| read(params).map(Command::ChooseValueListItem),
};

pub static GET_VALUE_LIST_VALUES: CommandSpec = CommandSpec {
    id: "get_value_list_values",
    cli: "get-value-list-values",
    tool: "openmaker_get_value_list_values",
    title: "Read a custom value list's values",
    description: "Read a value list of custom values as Edit Value List shows it (#869): its \
                  name, its values in order, one per item, \"-\" for a divider, and its \
                  revision for set_value_list_values. A list whose values come from a field \
                  is refused. Writes nothing.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("get_value_list_values"),
    params: schema_for_type::<ValueListParams>,
    parse: |params| read(params).map(Command::GetValueListValues),
};

pub static SET_VALUE_LIST_VALUES: CommandSpec = CommandSpec {
    id: "set_value_list_values",
    cli: "set-value-list-values",
    tool: "openmaker_set_value_list_values",
    title: "Change a custom value list's values",
    description: "Replace a custom value list's values as FileMaker Pro's Edit Value List does \
                  (#869): one item per entry, \"-\" for a divider, no item holding a return. \
                  Only if the list is still at expected_revision (get_value_list_values' \
                  revision); then the list is read back over a new connection. Repeating the \
                  same call reports already_applied.",
    effect: Effect::Authors,
    needs: Needs::Grant(Grant::ValueLists),
    operation: Some("set_value_list_values"),
    params: schema_for_type::<ValueListValuesParams>,
    parse: |params| read(params).map(Command::SetValueListValues),
};

pub static ENTER_OTHER_VALUE: CommandSpec = CommandSpec {
    id: "enter_other_value",
    cli: "enter-other",
    tool: "openmaker_enter_other_value",
    title: "Enter a value through a pop-up menu's Other…",
    description: "Enter a value through a pop-up menu's “Other…” item on a record, as a user \
                  does in a window (#869): the text becomes the field's value, in the record's \
                  draft, validated and committed with the record, and is not added to the \
                  list. It needs a desktop window, which this service has not: it answers \
                  unsupported with details.unavailable = no_window and changes nothing.",
    effect: Effect::Edits,
    needs: Needs::Window,
    operation: Some("enter_other_value"),
    params: schema_for_type::<OtherParams>,
    parse: |params| read(params).map(Command::EnterOtherValue),
};

pub static GET_SECURITY: CommandSpec = CommandSpec {
    id: "get_security",
    cli: "get-security",
    tool: "openmaker_get_security",
    title: "Get the file's security",
    description: "The file's accounts (name, type, whether active, whether the password must \
                  be changed, privilege set), privilege sets (name, record access, password \
                  lifetime, accounts, extended privileges) and extended privileges, as the host \
                  decodes them. No password, password hash or other credential is ever part of \
                  it. Only OpenMaker Server offers it, and only to an account of the file in \
                  the file's built-in [Full Access] set. Accounts and privilege sets cannot be \
                  changed: `editable` is false, with the reason.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("get_security"),
    params: schema_for_type::<NoParams>,
    parse: |params| read::<NoParams>(params).map(|_| Command::GetSecurity),
};

pub static GET_VALUE_LISTS: CommandSpec = CommandSpec {
    id: "get_value_lists",
    cli: "get-value-lists",
    tool: "openmaker_get_value_lists",
    title: "Get value lists",
    description: "The file's value lists in their display order, as Manage Value Lists shows \
                  them: each list's reference, name and source (its custom values; or the \
                  field it takes values from, with the second field, the related-values \
                  occurrence and the sort options; or another file's or a calculated list), \
                  and lists OpenMaker cannot decode, marked unreadable with the reason. Value \
                  lists are read only in this version, and the result says why. Reads \
                  metadata only, never records.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("get_value_lists"),
    params: schema_for_type::<NoParams>,
    parse: |params| read::<NoParams>(params).map(Command::GetValueLists),
};

pub static COMPLETE_CALCULATION: CommandSpec = CommandSpec {
    id: "complete_calculation",
    cli: "complete-calculation",
    tool: "openmaker_complete_calculation",
    title: "Complete a name in a calculation",
    description: "The names completing the one typed at a cursor in formula text, as Specify \
                  Calculation lists them while typing: FileMaker's functions and Get functions, \
                  Get selectors after `Get (`, the file's custom functions, and fields of the \
                  context occurrence or of `Occurrence::`. Each candidate has the text that \
                  replaces the typed prefix (`replace`, in characters; text after the cursor is \
                  kept) and the selection afterwards. Nothing is completed inside text literals, \
                  comments or a word, and `refused` says why. Fields and custom functions come \
                  only from the file's catalogs; `sources` says when they are unavailable. The \
                  matching and order are OpenMaker's, not measured against FileMaker. Reads \
                  metadata only, never records.",
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("complete_calculation"),
    params: schema_for_type::<CompletionParams>,
    parse: |params| read(params).map(Command::CompleteCalculation),
};

/// Every command, in the order the MCP tool list, `commands` and the CLI's
/// usage give them.
/// An analysis command's description, ending with what every analysis
/// result says of its coverage.
macro_rules! analysis {
    ($text:literal) => {
        concat!(
            $text,
            " Results report what was read, not facts about the whole file: check them \
             before acting on them. Every result carries how far to trust it: \
             `read_cleanly` with its `anomalies`, `consistent` (whether the design was seen \
             to change while it was read), the places not decoded and the references made \
             by a name looked up when the file runs (Evaluate, ExecuteSQL, a script \
             performed by name), catalogs the host did not give, `omitted` and `stopped`. \
             An object asked for and not found is the error `not_found` (not among the \
             objects read) only for a file read cleanly, else `not_known`; either carries \
             the same `coverage` in its details. Reads metadata only, never records."
        )
    };
}

pub static FIND_USES: CommandSpec = CommandSpec {
    id: "find_uses",
    cli: "find-uses",
    tool: "openmaker_find_uses",
    title: "Find where an object is used",
    description: analysis!(
        "Every reference OpenMaker decodes to a design object (a script, table, \
                  field, table occurrence, layout, value list, custom function or data \
                  source) from another object: each with the object that holds it and the \
                  place in it (a script's step and line, a layout object, a field's \
                  formula), whether it resolves, and for a script what the holder does \
                  with it (`script_role`: `performs`, `performs_on_server`, `installs` or \
                  `names`). Coverage lists the places that could refer to the object but \
                  were not read."
    ),
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("find_uses"),
    params: schema_for_type::<ObjectParams>,
    parse: |params| read(params).map(Command::FindUses),
};

pub static BROKEN_REFERENCES: CommandSpec = CommandSpec {
    id: "broken_references",
    cli: "broken-references",
    tool: "openmaker_broken_references",
    title: "Find references whose target was not found",
    description: analysis!(
        "Every reference OpenMaker decodes whose target was not found among the \
                  objects read (`target_not_found`): a field, script, layout, occurrence, \
                  value list, custom function or data source the reference names, with the \
                  object that holds it, the place in it and what was not found. Only in a \
                  file read cleanly; otherwise such a reference is `unresolved`."
    ),
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("broken_references"),
    params: schema_for_type::<AnalysisParams>,
    parse: |params| read(params).map(Command::BrokenReferences),
};

pub static UNUSED_OBJECTS: CommandSpec = CommandSpec {
    id: "unused_objects",
    cli: "unused-objects",
    tool: "openmaker_unused_objects",
    title: "Find objects no reference was found for",
    description: analysis!(
        "The tables, fields, occurrences, layouts, scripts, value lists, custom \
                  functions and data sources for which no reference from another object was \
                  found in what was read (`no_reference_found`; a reference an object holds \
                  to itself, such as a script performing itself, does not count): \
                  candidates to check, never objects known to be unused. \
                  `nothing_known_unread` says, per kind, whether any place not read or name \
                  looked up at run time could refer to one."
    ),
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("unused_objects"),
    params: schema_for_type::<UnusedParams>,
    parse: |params| read(params).map(Command::UnusedObjects),
};

pub static SCRIPT_CALLS: CommandSpec = CommandSpec {
    id: "script_calls",
    cli: "script-calls",
    tool: "openmaker_script_calls",
    title: "Get a script's call chain",
    description: analysis!(
        "The scripts a script performs and the scripts that perform it (Perform \
                  Script, Perform Script on Server), transitively, each with its depth, \
                  every perform between them with its step and `script_role`, and the \
                  cycles found. Each script is walked once. References between the scripts \
                  walked and other scripts that are not performs (a script trigger, timer \
                  or callback installed, `installs`; a script only named, as Validate \
                  Scripts names its scripts, `names`) are listed apart, in \
                  `other_references`, and not followed."
    ),
    effect: Effect::Reads,
    needs: Needs::File,
    operation: Some("script_calls"),
    params: schema_for_type::<ScriptCallsParams>,
    parse: |params| read(params).map(Command::ScriptCalls),
};

pub static ALL: [&CommandSpec; 44] = [
    &CAPABILITIES,
    &INSPECT,
    &GET_SCRIPT,
    &VALIDATE,
    &PLAN,
    &APPLY,
    &PLAN_FIELD,
    &CREATE_FIELD,
    &GET_LAYOUT,
    &PLAN_FIELD_PLACEMENT,
    &PLACE_FIELD,
    &SET_OBJECT_FRAMES,
    &ARRANGE_OBJECTS,
    &MOVE_OBJECTS,
    &DELETE_OBJECTS,
    &PLACE_TEXT,
    &PLACE_PORTAL,
    &PLACE_WEB_VIEWER,
    &CREATE_LAYOUT,
    &DUPLICATE_LAYOUT,
    &DELETE_LAYOUT,
    &LAYOUT_SETUP,
    &SET_LAYOUT_WIDTH,
    &SET_FIELD_CONTROL,
    &SET_PART_OPTIONS,
    &CHANGE_PARTS,
    &SET_TEXT,
    &SET_OBJECT_ANCHORS,
    &RUN_SCRIPT,
    &LIST_CHOICES,
    &CHOOSE_VALUE_LIST_ITEM,
    &ENTER_OTHER_VALUE,
    &GET_VALUE_LIST_VALUES,
    &SET_VALUE_LIST_VALUES,
    &GET_SECURITY,
    &GET_VALUE_LISTS,
    &COMPLETE_CALCULATION,
    &CREATE_FILE,
    &COMPARE_DESIGNS,
    &FIND_USES,
    &BROKEN_REFERENCES,
    &UNUSED_OBJECTS,
    &SCRIPT_CALLS,
    &COMMANDS,
];

/// The command with id `id`.
pub fn get(id: &str) -> Option<&'static CommandSpec> {
    ALL.iter().copied().find(|spec| spec.id == id)
}

/// The command the CLI calls `name`.
pub fn by_cli(name: &str) -> Option<&'static CommandSpec> {
    ALL.iter().copied().find(|spec| spec.cli == name)
}

/// The command MCP tool `name` runs.
pub fn by_tool(name: &str) -> Option<&'static CommandSpec> {
    ALL.iter().copied().find(|spec| spec.tool == name)
}

/// The `commands` command's result.
pub fn describe() -> Value {
    json!({
        "api_version": API_VERSION,
        "commands": ALL.iter().map(|spec| spec.to_json()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The ids, CLI names and tools are each unique; each spec parses to
    /// its own command, which names that spec; design changes need the
    /// host's grant, a run needs the file, and an edit of a record needs a
    /// window.
    #[test]
    fn every_command_has_one_spec_and_writes_only_with_a_grant() {
        for names in [
            ALL.map(|spec| spec.id),
            ALL.map(|spec| spec.cli),
            ALL.map(|spec| spec.tool),
        ] {
            assert_eq!(names.iter().collect::<HashSet<_>>().len(), ALL.len());
        }
        let frame = json!({"left": 10, "top": 60, "width": 120, "height": 20});
        let mut samples = json!({
            "get_script": {"script_name": "S"},
            "validate": {"script_name": "S", "source": ""},
            "plan": {"script_name": "S", "source": ""},
            "apply": {"script_name": "S", "source": "", "expected_revision": "r"},
            "plan_field": {"table_name": "T", "name": "F", "type": "text"},
            "create_field": {"table_name": "T", "name": "F", "type": "text",
                             "expected_revision": "r"},
            "get_layout": {"layout_name": "L"},
            "plan_field_placement": {"layout_name": "L", "field_name": "F", "frame": frame},
            "place_field": {"layout_name": "L", "field_name": "F", "frame": frame,
                            "expected_revision": "r"},
            "create_layout": {"name": "N", "occurrence_name": "O", "expected_revision": "r"},
            "set_object_frames": {"layout_name": "L", "expected_revision": "r",
                                  "objects": [{"object": "layout_object:1.4@F.fmp12",
                                               "frame": frame}]},
            "arrange_objects": {"layout_name": "L", "expected_revision": "r",
                                "objects": ["layout_object:1.4@F.fmp12",
                                            "layout_object:1.5@F.fmp12"],
                                "arrangement": "align_left_edges"},
            "move_objects": {"layout_name": "L", "expected_revision": "r",
                             "objects": ["layout_object:1.4@F.fmp12"], "dx": -1, "dy": 2},
            "delete_objects": {"layout_name": "L", "expected_revision": "r",
                               "objects": ["layout_object:1.4@F.fmp12"]},
            "place_text": {"layout_name": "L", "text": "T", "frame": frame,
                           "expected_revision": "r"},
            "place_portal": {"layout_name": "L", "occurrence_name": "O", "field_names": ["F"],
                             "rows": 3, "frame": frame, "expected_revision": "r"},
            "place_web_viewer": {"layout_name": "L", "address": "\"https://example.com\"",
                                 "frame": frame, "expected_revision": "r"},
            "duplicate_layout": {"layout_name": "L", "expected_revision": "r"},
            "delete_layout": {"layout_name": "L", "expected_revision": "r"},
            "layout_setup": {"layout_name": "L", "expected_revision": "r", "views": ["form"]},
            "set_layout_width": {"layout_name": "L", "expected_revision": "r", "width": 900},
            "set_field_control": {"object": "o", "expected_revision": "r", "control": "pop_up_menu"},
            "set_part_options": {"layout_name": "L", "expected_revision": "r", "part": 2},
            "change_parts": {"layout_name": "L", "expected_revision": "r", "action": "resize",
                             "part": 2, "height": 100},
            "set_text": {"object": "o", "expected_revision": "r", "text": "Name"},
            "run_script": {"script_name": "S"},
            "list_choices": {"object": "layout_object:1.4@F.fmp12", "record": 1},
            "choose_value_list_item": {"object": "layout_object:1.4@F.fmp12", "record": 1,
                                       "value": "v"},
            "enter_other_value": {"object": "layout_object:1.4@F.fmp12", "record": 1,
                                  "text": "Dr"},
            "get_value_list_values": {"value_list": "value_list:1@F.fmp12"},
            "set_value_list_values": {"value_list": "value_list:1@F.fmp12",
                                      "values": ["Home", "-"], "expected_revision": "r"},
            "get_value_lists": {},
            "complete_calculation": {"text": "Le"},
            "create_file": {"path": "New.fmp12"},
            "compare_designs": {"source": "A.fmp12", "target": "B.fmp12"},
            "find_uses": {"object": "field:32769.1@F.fmp12"},
            "broken_references": {},
            "unused_objects": {"kinds": ["script"]},
            "script_calls": {"script_name": "S", "direction": "callers"},
        });
        samples["set_object_anchors"] =
            json!({"object": "o", "expected_revision": "r", "anchors": ["left", "top"]});
        for spec in ALL {
            assert!(std::ptr::eq(get(spec.id).unwrap(), spec));
            let command = spec.parse(samples[spec.id].clone()).unwrap();
            assert!(std::ptr::eq(command.spec(), spec), "{}", spec.id);
            let granted = matches!(spec.needs, Needs::Grant(_));
            match spec.effect {
                Effect::Reads => assert!(!granted, "{}", spec.id),
                Effect::Authors => assert!(granted, "{}", spec.id),
                Effect::Runs => assert_eq!(spec.needs, Needs::File, "{}", spec.id),
                Effect::Edits => assert_eq!(spec.needs, Needs::Window, "{}", spec.id),
                Effect::Creates => assert_eq!(spec.needs, Needs::Nothing, "{}", spec.id),
            }
        }
    }
}
