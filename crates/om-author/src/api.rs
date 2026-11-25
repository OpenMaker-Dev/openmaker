//! The versioned JSON commands (`openmaker.authoring/v1`, docs/authoring.md):
//! a request names its version, its command and the command's parameters;
//! a response names the version and command and holds either the result or
//! a typed error. The CLI and the MCP tools build the same [`Command`]s,
//! each described once in [`crate::catalog`].

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{self, CommandSpec};

/// The version of the commands and their results.
pub const API_VERSION: &str = "openmaker.authoring/v1";

/// A command and its parameters. Each has one [`CommandSpec`] in
/// [`crate::catalog`], which names it and parses it.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Every command, with its parameters as JSON Schema. Needs no
    /// connection.
    Commands,
    /// What the host and account allow, and what this version does not do.
    Capabilities,
    /// The file's metadata: scripts, tables and fields, occurrences,
    /// layouts, value lists and data sources. Never records.
    Inspect(InspectParams),
    /// A script as editable text, with each step's fidelity.
    GetScript(ScriptParams),
    /// Compile edited text against the script and the file's names.
    Validate(EditParams),
    /// The change edited text makes: revisions and a diff. Writes nothing.
    Plan(EditParams),
    /// Replace the script's steps if it is still at the expected revision,
    /// then read it back over a new connection.
    Apply(ApplyParams),
    /// What adding a field to a table would change. Writes nothing.
    PlanField(FieldParams),
    /// Add a field to a table if the table's fields are still at the
    /// expected revision, then read it back over a new connection.
    CreateField(CreateFieldParams),
    /// A layout's parts and objects, with its revision.
    GetLayout(LayoutParams),
    /// What placing a field on a layout would change. Writes nothing.
    PlanFieldPlacement(PlacementParams),
    /// Place a field on a layout if the layout is still at the expected
    /// revision, then read it back over a new connection.
    PlaceField(PlaceFieldParams),
    SetObjectFrames(ObjectFramesParams),
    ArrangeObjects(ArrangeObjectsParams),
    DeleteObjects(DeleteObjectsParams),
    PlaceText(PlaceTextParams),
    PlacePortal(PlacePortalParams),
    DuplicateLayout(DuplicateLayoutParams),
    DeleteLayout(DeleteLayoutParams),
    LayoutSetup(LayoutSetupParams),
    SetLayoutWidth(LayoutWidthParams),
    SetFieldControl(FieldControlParams),
    SetPartOptions(PartOptionsParams),
    ChangeParts(ChangePartsParams),
    SetText(TextParams),
    CreateLayout(CreateLayoutParams),
    /// Run a script on the host (Perform Script on Server) and wait for its
    /// last error and result.
    RunScript(RunScriptParams),
    /// The items a field control's value list offers on a record
    /// (`om_data::choice`).
    ListChoices(ChoiceParams),
    /// Choose an item in a pop-up menu or a drop-down list, as a user does
    /// in a window. Needs a
    /// desktop window, which this service has not.
    ChooseValueListItem(ChooseParams),
    /// Enter a value through a pop-up menu's “Other…”, as a user does in a
    /// window (#869). Needs a desktop window, which this service has not.
    EnterOtherValue(OtherParams),
    /// A custom value list's name, values and revision (#869).
    GetValueListValues(ValueListParams),
    /// Replace a custom value list's values, as Edit Value List does.
    SetValueListValues(ValueListValuesParams),
    /// The file's accounts, privilege sets and extended privileges, as the
    /// host decodes them, with no password or other credential. Writes
    /// nothing.
    GetSecurity,
    /// The file's value lists, as Manage Value Lists shows them, and why
    /// they are read only (`om_data::value_lists`). Never records.
    GetValueLists(NoParams),
    /// The names completing the one typed at a cursor in formula text
    /// (`om_data::formula_editor::completion`). Reads metadata only.
    CompleteCalculation(CompletionParams),
    /// Make a new, blank file on this computer, as File > Create New...
    /// does (`om_data::new_file::create`). Needs no connection.
    CreateFile(CreateFileParams),
    /// Compare the designs of two files on this computer, with or without
    /// a base (`om_hbam::design_paths::compare_files`). Needs no
    /// connection.
    CompareDesigns(CompareDesignsParams),
    /// Every decoded reference to a design object (`om_data::design_refs`).
    FindUses(ObjectParams),
    /// Every decoded reference whose target was not found among the
    /// objects read.
    BrokenReferences(AnalysisParams),
    /// The objects no reference from another object was found for, in
    /// what was read.
    UnusedObjects(UnusedParams),
    /// A script's performs and the scripts that perform it, transitively.
    ScriptCalls(ScriptCallsParams),
}

impl Command {
    /// The command's description.
    pub fn spec(&self) -> &'static CommandSpec {
        match self {
            Command::Commands => &catalog::COMMANDS,
            Command::Capabilities => &catalog::CAPABILITIES,
            Command::Inspect(_) => &catalog::INSPECT,
            Command::GetScript(_) => &catalog::GET_SCRIPT,
            Command::Validate(_) => &catalog::VALIDATE,
            Command::Plan(_) => &catalog::PLAN,
            Command::Apply(_) => &catalog::APPLY,
            Command::PlanField(_) => &catalog::PLAN_FIELD,
            Command::CreateField(_) => &catalog::CREATE_FIELD,
            Command::GetLayout(_) => &catalog::GET_LAYOUT,
            Command::PlanFieldPlacement(_) => &catalog::PLAN_FIELD_PLACEMENT,
            Command::PlaceField(_) => &catalog::PLACE_FIELD,
            Command::SetObjectFrames(_) => &catalog::SET_OBJECT_FRAMES,
            Command::ArrangeObjects(_) => &catalog::ARRANGE_OBJECTS,
            Command::DeleteObjects(_) => &catalog::DELETE_OBJECTS,
            Command::PlaceText(_) => &catalog::PLACE_TEXT,
            Command::PlacePortal(_) => &catalog::PLACE_PORTAL,
            Command::DuplicateLayout(_) => &catalog::DUPLICATE_LAYOUT,
            Command::DeleteLayout(_) => &catalog::DELETE_LAYOUT,
            Command::LayoutSetup(_) => &catalog::LAYOUT_SETUP,
            Command::SetLayoutWidth(_) => &catalog::SET_LAYOUT_WIDTH,
            Command::SetFieldControl(_) => &catalog::SET_FIELD_CONTROL,
            Command::SetPartOptions(_) => &catalog::SET_PART_OPTIONS,
            Command::ChangeParts(_) => &catalog::CHANGE_PARTS,
            Command::SetText(_) => &catalog::SET_TEXT,
            Command::CreateLayout(_) => &catalog::CREATE_LAYOUT,
            Command::RunScript(_) => &catalog::RUN_SCRIPT,
            Command::ListChoices(_) => &catalog::LIST_CHOICES,
            Command::ChooseValueListItem(_) => &catalog::CHOOSE_VALUE_LIST_ITEM,
            Command::EnterOtherValue(_) => &catalog::ENTER_OTHER_VALUE,
            Command::GetValueListValues(_) => &catalog::GET_VALUE_LIST_VALUES,
            Command::SetValueListValues(_) => &catalog::SET_VALUE_LIST_VALUES,
            Command::GetSecurity => &catalog::GET_SECURITY,
            Command::GetValueLists(_) => &catalog::GET_VALUE_LISTS,
            Command::CompleteCalculation(_) => &catalog::COMPLETE_CALCULATION,
            Command::CreateFile(_) => &catalog::CREATE_FILE,
            Command::CompareDesigns(_) => &catalog::COMPARE_DESIGNS,
            Command::FindUses(_) => &catalog::FIND_USES,
            Command::BrokenReferences(_) => &catalog::BROKEN_REFERENCES,
            Command::UnusedObjects(_) => &catalog::UNUSED_OBJECTS,
            Command::ScriptCalls(_) => &catalog::SCRIPT_CALLS,
        }
    }

    /// The command's name, as requests and responses give it.
    pub fn name(&self) -> &'static str {
        self.spec().id
    }

    /// The command named `name` with `params` (an object, or null for
    /// none).
    pub fn from_parts(name: &str, params: Value) -> Result<Command, AuthorError> {
        let spec = catalog::get(name).ok_or_else(|| {
            let names: Vec<&str> = catalog::ALL.iter().map(|spec| spec.id).collect();
            AuthorError::new(
                ErrorKind::InvalidRequest,
                format!(
                    "Unknown command “{name}”; the commands are {}.",
                    names.join(", ")
                ),
            )
        })?;
        spec.parse(params)
    }
}

/// A request as JSON: `{"version": API_VERSION, "command": ..., "params":
/// {...}}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: String,
    command: String,
    #[serde(default)]
    params: Value,
}

/// Read a JSON request.
pub fn parse_request(text: &str) -> Result<Command, AuthorError> {
    let envelope: Envelope = serde_json::from_str(text).map_err(|e| {
        AuthorError::new(
            ErrorKind::InvalidRequest,
            format!("The request is not a {API_VERSION} request: {e}"),
        )
    })?;
    if envelope.version != API_VERSION {
        return Err(AuthorError::new(
            ErrorKind::Unsupported,
            format!(
                "Request version “{}” is not supported; this tool speaks {API_VERSION}.",
                envelope.version
            ),
        ));
    }
    Command::from_parts(&envelope.command, envelope.params)
}

/// No parameters.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// The new file `create_file` makes.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateFileParams {
    /// Its path on this computer, relative to the working directory or
    /// absolute. `.fmp12` is added when the path does not end in it; its
    /// file name without the extension names the file, its table and its
    /// layout. A file already there is never replaced.
    pub path: String,
}

/// The files `compare_designs` compares, each a path on this computer,
/// relative to the working directory or absolute. Files are only read.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompareDesignsParams {
    /// The source: the file whose design is compared with the target's.
    pub source: String,
    /// The target.
    pub target: String,
    /// The base: the copy the source and the target were both made from.
    /// With it objects are matched by kind and id, and each side's
    /// changes since it are reported; without it by kind and id when their
    /// names agree, or when neither has a name and all their content is
    /// equal.
    #[serde(default)]
    pub base: Option<String>,
    /// Also list the matched objects that are the same; by default only
    /// those that differ are listed, and every part is counted.
    #[serde(default)]
    pub include_same: bool,
}

/// A design object for the analysis commands: `object`, a reference of
/// any kind `inspect`, `get_layout` or `get_value_lists` gives
/// (`script:5@Tasks.fmp12`, `field:32769.2@Tasks.fmp12`, ...) or of a
/// custom function, data source or relationship
/// (`custom_function:1@Tasks.fmp12`), or exactly one name: a script's,
/// table's, occurrence's, layout's, value list's, custom function's or
/// data source's, or `field_name` with the field's table (`table` or
/// `table_name`). A reference may name an object not found among the
/// objects read: the references naming it are returned. A name not found
/// is the error `not_found` (not among the objects read, in a file read
/// cleanly) or `not_known` (the file was not read cleanly), each with its
/// `coverage` in `details`.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObjectParams {
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub script_name: Option<String>,
    #[serde(default)]
    pub table_name: Option<String>,
    #[serde(default)]
    pub occurrence_name: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    #[serde(default)]
    pub value_list_name: Option<String>,
    #[serde(default)]
    pub custom_function_name: Option<String>,
    #[serde(default)]
    pub data_source_name: Option<String>,
    /// A field by name, of the table `table` (a reference) or
    /// `table_name` names.
    #[serde(default)]
    pub field_name: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
    /// The most references and coverage places listed (default 500, at
    /// most 100,000); totals are always given.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// How much an analysis lists.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalysisParams {
    /// The most references and coverage places listed (default 500, at
    /// most 100,000); totals are always given.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// A kind of design object `unused_objects` looks for references to.
#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Table,
    Field,
    Occurrence,
    Layout,
    Script,
    ValueList,
    CustomFunction,
    DataSource,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnusedParams {
    /// The kinds to look for objects of that no other object was found
    /// to refer to; every one when absent.
    #[serde(default)]
    pub kinds: Option<Vec<ObjectKind>>,
    /// The most objects and coverage places listed (default 500, at most
    /// 100,000); totals are always given.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Which way a call chain goes: through performs (Perform Script,
/// Perform Script on Server) only.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    /// The scripts it performs, transitively.
    Calls,
    /// The scripts that perform it, transitively.
    Callers,
    /// Both.
    Both,
}

/// A script, as for `get_script`, and the direction of its chain.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptCallsParams {
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub script_name: Option<String>,
    /// `calls`, `callers` or `both` (the default).
    #[serde(default)]
    pub direction: Option<CallDirection>,
    /// The most performs, other references between scripts and coverage
    /// places listed (default 500, at most 100,000); totals are always
    /// given.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Metadata `inspect` can list.
#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Scripts,
    Tables,
    Occurrences,
    Layouts,
    ValueLists,
    DataSources,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InspectParams {
    /// The metadata to list; everything when absent. Records are never
    /// read.
    #[serde(default)]
    pub include: Option<Vec<Part>>,
}

/// A script: by `script`, a file-qualified reference as `inspect` gives it
/// (`script:3@Tasks.fmp12`), or by `script_name`, which must name exactly
/// one script.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptParams {
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub script_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EditParams {
    /// The script, as for `get_script`.
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub script_name: Option<String>,
    /// The script's new steps in the text form `get_script` returns.
    pub source: String,
    /// The revision the text was edited from (`get_script`'s
    /// `revision`); a script changed since is reported as a conflict.
    #[serde(default)]
    pub expected_revision: Option<String>,
}

impl EditParams {
    pub fn target(&self) -> ScriptParams {
        ScriptParams {
            script: self.script.clone(),
            script_name: self.script_name.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplyParams {
    /// The script, as for `get_script`.
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub script_name: Option<String>,
    /// The script's new steps in the text form `get_script` returns.
    pub source: String,
    /// The revision the text was edited from (`get_script`'s `revision`):
    /// required, so no edit is written over a change it did not see.
    pub expected_revision: String,
    /// The `after_revision` a `plan` of the same text gave: the edit is
    /// refused if the text now compiles to anything else.
    #[serde(default)]
    pub expected_after_revision: Option<String>,
}

impl ApplyParams {
    pub fn target(&self) -> ScriptParams {
        ScriptParams {
            script: self.script.clone(),
            script_name: self.script_name.clone(),
        }
    }
}

/// The data type of a new field: the scalar types, each stored per record.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
}

/// A new field: a normal field with stored values and FileMaker's defaults
/// (no auto-enter or validation, indexed as needed, one repetition).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FieldParams {
    /// The table: `table`, a reference from inspect
    /// (`table:32769@Tasks.fmp12`), or `table_name`, which must name
    /// exactly one table of the file.
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub table_name: Option<String>,
    /// The field's name: at most 100 characters that a calculation can name
    /// without `${…}`, not one of the table's field names (compared as
    /// FileMaker does, ignoring case and accents).
    pub name: String,
    /// The field's data type.
    #[serde(rename = "type")]
    pub field_type: FieldType,
    /// The field's comment, if any.
    #[serde(default)]
    pub comment: Option<String>,
    /// The table's `fields_revision` from inspect: a table whose fields
    /// changed since is reported as a conflict.
    #[serde(default)]
    pub expected_revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateFieldParams {
    /// The table, as for `plan_field`.
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub table_name: Option<String>,
    /// The field's name, as for `plan_field`.
    pub name: String,
    /// The field's data type.
    #[serde(rename = "type")]
    pub field_type: FieldType,
    /// The field's comment, if any.
    #[serde(default)]
    pub comment: Option<String>,
    /// The table's `fields_revision` the field was planned against (from
    /// inspect or plan_field): required, so no field is added to a table
    /// that changed unseen.
    pub expected_revision: String,
}

impl CreateFieldParams {
    /// The same field, to plan.
    pub fn plan(&self) -> FieldParams {
        FieldParams {
            table: self.table.clone(),
            table_name: self.table_name.clone(),
            name: self.name.clone(),
            field_type: self.field_type,
            comment: self.comment.clone(),
            expected_revision: Some(self.expected_revision.clone()),
        }
    }
}

/// A layout: by `layout`, a file-qualified reference as `inspect` gives it
/// (`layout:1@Tasks.fmp12`), or by `layout_name`, which must name exactly
/// one layout.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LayoutParams {
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
}

/// A frame in layout coordinates, in points from the layout's top-left
/// corner, as FileMaker Pro's Position inspector shows it.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrameParams {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// Objects of a layout and the frames to move or resize them to.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObjectFramesParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// Each object, a `layout_object` reference from get_layout
    /// (`layout_object:1.4@Tasks.fmp12`), and its new frame: whole points
    /// left and top, inside the part that lists it and the layout's width.
    pub objects: Vec<ObjectFrameParams>,
    /// The layout's `revision` from get_layout: required, so nothing is
    /// moved on a layout that changed unseen.
    pub expected_revision: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObjectFrameParams {
    pub object: String,
    pub frame: FrameParams,
}

/// Objects of a layout to align, distribute or resize relative to one
/// another, as Layout mode's Arrange menu does.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ArrangeObjectsParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The objects, `layout_object` references from get_layout: two or
    /// more, three or more to distribute. Each is a top-level object that
    /// `set_object_frames` can move.
    pub objects: Vec<String>,
    /// One of `align_left_edges`, `align_vertical_centers`,
    /// `align_right_edges`, `align_top_edges`, `align_horizontal_centers`,
    /// `align_bottom_edges`, `distribute_horizontally`,
    /// `distribute_vertically`, `resize_to_smallest_width`,
    /// `resize_to_smallest_height`, `resize_to_smallest_width_and_height`,
    /// `resize_to_largest_width`, `resize_to_largest_height`,
    /// `resize_to_largest_width_and_height`.
    pub arrangement: String,
    /// The layout's `revision` from get_layout: required, so nothing is
    /// arranged on a layout that changed unseen.
    pub expected_revision: String,
}

/// A text to place on a layout's body on its own, as the Text tool places
/// one.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlaceTextParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// One line of at most 1,000 characters.
    pub text: String,
    /// Whole points left and top, inside the body part and the layout's
    /// width, not over a group, portal, tab or slide control, popover or
    /// button bar.
    pub frame: FrameParams,
    /// The layout's `revision` from get_layout: required, so nothing is
    /// placed on a layout that changed unseen.
    pub expected_revision: String,
}

/// A portal to place on a layout's body, as FileMaker Pro's Portal tool,
/// Portal Setup and Add Fields to Portal make one.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlacePortalParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The table occurrence whose related records it shows (related to the
    /// layout's own in the relationship graph): a reference from inspect,
    /// or its name.
    #[serde(default)]
    pub occurrence: Option<String>,
    #[serde(default)]
    pub occurrence_name: Option<String>,
    /// The fields of that occurrence's table in its first row, left to
    /// right, by name: 1 to 20 text, number, date, time or timestamp
    /// fields.
    pub field_names: Vec<String>,
    /// How many rows it shows: 1 to 100, each tall enough for a field.
    pub rows: u32,
    /// Whole points left and top, inside the body part and the layout's
    /// width, not over a group, portal, tab or slide control, popover or
    /// button bar.
    pub frame: FrameParams,
    /// The layout's `revision` from get_layout: required, so nothing is
    /// placed on a layout that changed unseen.
    pub expected_revision: String,
}

/// Objects to delete from a layout.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeleteObjectsParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// Each object, a `layout_object` reference from get_layout
    /// (`layout_object:1.4@Tasks.fmp12`): a top-level object without
    /// contents that shows no image from the file's library.
    pub objects: Vec<String>,
    /// The layout's `revision` from get_layout: required, so nothing is
    /// deleted from a layout that changed unseen.
    pub expected_revision: String,
}

/// A text label placed with the field.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LabelParams {
    /// One line of at most 1,000 characters.
    pub text: String,
    pub frame: FrameParams,
}

/// A field to place on a layout: an edit box showing the field, on top of
/// the layout's objects, in its body part.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlacementParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The field: `field`, a reference from inspect
    /// (`field:32769.2@Tasks.fmp12`), or `field_name`, a field of the
    /// layout's table. It must be a field of the table the layout shows
    /// records of, and not a container.
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub field_name: Option<String>,
    /// The field object's frame, inside the layout's body part and width.
    pub frame: FrameParams,
    /// A text label placed with the field, if any.
    #[serde(default)]
    pub label: Option<LabelParams>,
    /// The layout's `revision` from get_layout: a layout changed since is
    /// reported as a conflict.
    #[serde(default)]
    pub expected_revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlaceFieldParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The field, as for `plan_field_placement`.
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub field_name: Option<String>,
    /// The field object's frame.
    pub frame: FrameParams,
    /// A text label placed with the field, if any.
    #[serde(default)]
    pub label: Option<LabelParams>,
    /// The layout's `revision` the placement was planned against (from
    /// get_layout or plan_field_placement): required, so nothing is placed
    /// on a layout that changed unseen.
    pub expected_revision: String,
}

impl PlaceFieldParams {
    /// The same placement, to plan.
    pub fn plan(&self) -> PlacementParams {
        PlacementParams {
            layout: self.layout.clone(),
            layout_name: self.layout_name.clone(),
            field: self.field.clone(),
            field_name: self.field_name.clone(),
            frame: self.frame,
            label: self.label.clone(),
            expected_revision: Some(self.expected_revision.clone()),
        }
    }
}

/// A new layout: a Form layout for a computer with one empty body part.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateLayoutParams {
    /// The layout's name: at most 100 characters, no line breaks. Layouts
    /// may share a name.
    pub name: String,
    /// The table occurrence it shows records of: `occurrence`, a reference
    /// from inspect (`occurrence:13631490@Tasks.fmp12`), or
    /// `occurrence_name`, which must name exactly one occurrence.
    #[serde(default)]
    pub occurrence: Option<String>,
    #[serde(default)]
    pub occurrence_name: Option<String>,
    /// The layout whose theme and width the new one takes, as for
    /// get_layout (`layout` or `layout_name`); none: the first layout in the
    /// file's order that has a theme.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The file's `layouts_revision` from inspect: required, so no layout
    /// is added to a layout list that changed unseen.
    pub expected_revision: String,
}

/// A layout to copy, as FileMaker Pro's Duplicate Layout copies one.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DuplicateLayoutParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The file's `layouts_revision` from inspect: required, so no layout
    /// is added to a layout list that changed unseen.
    pub expected_revision: String,
}

/// A field object's new control.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FieldControlParams {
    /// The field object, as `get_layout` gives it.
    pub object: String,
    /// The layout's `revision` from get_layout: required.
    pub expected_revision: String,
    /// "edit_box", "drop_down_list", "pop_up_menu", "checkbox_set",
    /// "radio_button_set" or "drop_down_calendar".
    pub control: String,
    /// The value list, as `get_value_lists` gives it: required for a
    /// drop-down list, pop-up menu, checkbox set or radio button set; left
    /// out, the object keeps its own.
    #[serde(default)]
    pub value_list: Option<String>,
}

/// A text object's new words.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TextParams {
    /// The text object, as `get_layout` gives it: a text, or a field's
    /// label.
    pub object: String,
    /// The layout's `revision` from get_layout: required.
    pub expected_revision: String,
    /// The new words: one line of plain text, no merge fields.
    pub text: String,
}

/// A change to a layout's parts, as Part Setup's Create and Delete and a
/// dragged part boundary make it.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChangePartsParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The layout's `revision` from get_layout: required.
    pub expected_revision: String,
    /// `add`, `delete` or `resize`.
    pub action: String,
    /// For `delete` and `resize`: the part's `id`, as get_layout lists it.
    #[serde(default)]
    pub part: Option<u64>,
    /// The part's kind, as get_layout names them: title_header, header,
    /// leading_grand_summary, body, trailing_grand_summary, footer,
    /// title_footer, top_navigation, bottom_navigation (not sub-summaries).
    /// Required for `add`. For `delete` and `resize` it is taken from the
    /// part when left out; give it to repeat a deletion whose answer was
    /// lost, since the parts below are keyed again.
    #[serde(default)]
    pub kind: Option<String>,
    /// For `add` and `resize`: its height, in whole points.
    #[serde(default)]
    pub height: Option<f64>,
}

/// Part Setup's options for one part of a layout.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PartOptionsParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The layout's `revision` from get_layout: required.
    pub expected_revision: String,
    /// The part's `id`, as get_layout lists it.
    pub part: u64,
    /// Page options to turn on (true) or off (false), by name:
    /// "page_break_before", "restart_page_numbers", "break_across_pages",
    /// "discard_remainder". Options not named keep their state.
    #[serde(default)]
    pub options: Option<std::collections::BTreeMap<String, bool>>,
    /// A page break after every this many occurrences, 1 to 32767; 0
    /// turns it off. It turns a break before each occurrence off, and the
    /// other way round, as Part Definition does. Left out, as stored.
    #[serde(default)]
    pub page_break_after: Option<u32>,
    /// For a leading sub-summary, the field it is sorted by: a field of the
    /// layout's own table, as `field:TABLE.FIELD@File`. Left out, as stored.
    #[serde(default)]
    pub sorted_by: Option<String>,
}

/// A layout's new width.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LayoutWidthParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The layout's `revision` from get_layout: required.
    pub expected_revision: String,
    /// The width in points, 1 to 32000.
    pub width: f64,
}

/// Layout Setup's name, views and "Include in layout menus" for a layout;
/// what is left out stays as it is.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LayoutSetupParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The layout's `revision` from get_layout: required.
    pub expected_revision: String,
    /// The layout's new name.
    #[serde(default)]
    pub name: Option<String>,
    /// The views available in Browse and Find mode: some of "form", "list"
    /// and "table".
    #[serde(default)]
    pub views: Option<Vec<String>>,
    /// The view the layout opens in: "form", "list" or "table", one of the
    /// views available.
    #[serde(default)]
    pub default_view: Option<String>,
    /// Whether the layout menus list the layout.
    #[serde(default)]
    pub include_in_layout_menus: Option<bool>,
    /// Layout Setup's General options to turn on (true) or off (false), by
    /// name: "save_record_changes", "show_field_frames",
    /// "delineate_fields", "show_record_indicator", "enable_quick_find".
    /// Options not named keep their state.
    #[serde(default)]
    pub options: Option<std::collections::BTreeMap<String, bool>>,
}

/// A layout to delete, as FileMaker Pro's Delete Layout deletes one.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeleteLayoutParams {
    /// The layout, as for `get_layout`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub layout_name: Option<String>,
    /// The file's `layouts_revision` from inspect: required, so no layout
    /// is deleted from a layout list that changed unseen.
    pub expected_revision: String,
}

/// A field control and a record of the layout's table.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChoiceParams {
    /// The field control: a layout object reference from get_layout
    /// (`layout_object:1.4@Tasks.fmp12`).
    pub object: String,
    /// The record's id, in the table the layout shows records of.
    pub record: u64,
}

/// A choice in a pop-up menu or a drop-down list.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChooseParams {
    /// The pop-up menu or drop-down list, as for `list_choices`.
    pub object: String,
    /// The record, as for `list_choices`.
    pub record: u64,
    /// The stored `value` of the item to choose, from `list_choices`.
    pub value: String,
}

/// A value list, as `get_value_lists` gives it (`value_list:1@File.fmp12`).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ValueListParams {
    pub value_list: String,
}

/// A custom value list's new values.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ValueListValuesParams {
    /// The value list, as `get_value_lists` gives it.
    pub value_list: String,
    /// The values in order, one item each, "-" for a divider.
    pub values: Vec<String>,
    /// The list's `revision` from get_value_list_values: required.
    pub expected_revision: String,
}

/// A value entered through a pop-up menu's “Other…”.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OtherParams {
    /// The pop-up menu, as for `list_choices`.
    pub object: String,
    /// The record, as for `list_choices`.
    pub record: u64,
    /// The text to enter as the field's value.
    pub text: String,
}

/// Formula text and a cursor in it, to complete the name typed there.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompletionParams {
    /// The formula text, as Specify Calculation's editor holds it: at most
    /// 1,000,000 bytes.
    pub text: String,
    /// The cursor, in characters (Unicode scalar values) from the start of
    /// `text`. Default: the end of the text.
    #[serde(default)]
    pub cursor: Option<usize>,
    /// The occurrence unqualified field names are read from, as Specify
    /// Calculation's context popup chooses it: an occurrence reference
    /// from inspect (`occurrence:13631490@Tasks.fmp12`). Default: the
    /// file's first occurrence, as the dialog opens a new calculation.
    #[serde(default)]
    pub occurrence: Option<String>,
    /// The occurrence by name instead.
    #[serde(default)]
    pub occurrence_name: Option<String>,
    /// At most this many candidates, best first (default 50, at most
    /// 1000); `total` counts them all.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// A script to run on the host, as Perform Script on Server runs it.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunScriptParams {
    /// The script, as for `get_script`.
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub script_name: Option<String>,
    /// The script parameter, which the script reads with
    /// Get ( ScriptParameter ): text of at most 1,000,000 characters. None
    /// when absent.
    #[serde(default)]
    pub parameter: Option<String>,
    /// The script's `revision` (from get_script or apply): a script that
    /// changed since is reported as a conflict and not run. Checked just
    /// before the run is requested, not atomically with it.
    #[serde(default)]
    pub expected_revision: Option<String>,
}

impl RunScriptParams {
    pub fn target(&self) -> ScriptParams {
        ScriptParams {
            script: self.script.clone(),
            script_name: self.script_name.clone(),
        }
    }
}

/// What kind of failure an error is.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The request is malformed or names nothing to do.
    InvalidRequest,
    /// The host could not be reached, or refused the login.
    Connection,
    /// The object was not found. For the design analysis commands: not
    /// among the objects read, in a file read cleanly, which is a finding,
    /// not proof that the file lacks it (`details.coverage` says how far
    /// to trust it).
    NotFound,
    /// Whether the object is there is not known: the file was not read
    /// cleanly (`details.reason` says what made it not clean,
    /// `details.coverage` gives the rest). Never "does not exist".
    NotKnown,
    /// The reference names another file than the one connected to.
    WrongFile,
    /// A name that several objects share.
    AmbiguousReference,
    /// Not available for this host, file or version.
    Unsupported,
    /// The account may not do it.
    PermissionDenied,
    /// The script is not at the expected revision; nothing was written.
    Conflict,
    /// Another user holds the script or catalog; nothing was written.
    Locked,
    /// The text does not compile; nothing was written.
    InvalidSource,
    /// The name cannot be used for the new object; nothing was written.
    InvalidName,
    /// Another object of the table has the name; nothing was written.
    DuplicateName,
    /// The edit or run was sent but no answer came: it may or may not have
    /// been applied, or the script may have run in part or whole. Read the
    /// object again, or check what the script changes, before doing
    /// anything else.
    OutcomeUnknown,
    /// The host could not save the file; nothing was written.
    SaveFailed,
    /// The host reported the edit applied but a new connection read
    /// something else.
    VerificationFailed,
}

/// An error, with details for the program reading it.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorError {
    pub kind: ErrorKind,
    pub message: String,
    /// An object, or null.
    pub details: Value,
}

impl AuthorError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Value::Null,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    pub fn to_json(&self) -> Value {
        let mut error = json!({ "kind": self.kind, "message": self.message });
        if !self.details.is_null() {
            error["details"] = self.details.clone();
        }
        error
    }
}

impl std::fmt::Display for AuthorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AuthorError {}

/// The response to command `command`.
pub fn response(command: &str, outcome: Result<Value, AuthorError>) -> Value {
    match outcome {
        Ok(result) => json!({
            "version": API_VERSION,
            "command": command,
            "ok": true,
            "result": result,
        }),
        Err(error) => json!({
            "version": API_VERSION,
            "command": command,
            "ok": false,
            "error": error.to_json(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_name_their_version_command_and_params() {
        let request = r#"{"version": "openmaker.authoring/v1", "command": "get_script",
            "params": {"script": "script:3@Tasks.fmp12"}}"#;
        assert_eq!(
            parse_request(request).unwrap(),
            Command::GetScript(ScriptParams {
                script: Some("script:3@Tasks.fmp12".into()),
                script_name: None,
            })
        );
        let old = r#"{"version": "openmaker.authoring/v0", "command": "capabilities"}"#;
        assert_eq!(parse_request(old).unwrap_err().kind, ErrorKind::Unsupported);
        let unknown = r#"{"version": "openmaker.authoring/v1", "command": "capabilities",
            "params": {"extra": 1}}"#;
        assert_eq!(
            parse_request(unknown).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        // Apply needs the revision the text was edited from, and so does a
        // new field; a field's type is one of the scalar types.
        let blind = json!({"script": "script:3@Tasks.fmp12", "source": ""});
        assert!(Command::from_parts("apply", blind).is_err());
        let field = json!({"table_name": "Tasks", "name": "Due", "type": "date"});
        assert!(Command::from_parts("plan_field", field.clone()).is_ok());
        assert!(Command::from_parts("create_field", field).is_err());
        let container = json!({"table_name": "Tasks", "name": "Photo", "type": "container"});
        assert!(Command::from_parts("plan_field", container).is_err());
        // A placement needs a whole frame, and placing it the layout's
        // revision.
        let place = json!({"layout_name": "Tasks", "field_name": "Due",
            "frame": {"left": 10, "top": 60, "width": 120, "height": 20}});
        assert!(Command::from_parts("plan_field_placement", place.clone()).is_ok());
        assert!(Command::from_parts("place_field", place).is_err());
        let partial = json!({"layout_name": "Tasks", "field_name": "Due",
            "frame": {"left": 10, "top": 60}});
        assert!(Command::from_parts("plan_field_placement", partial).is_err());
    }
}
