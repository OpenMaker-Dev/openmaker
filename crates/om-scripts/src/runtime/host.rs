//! The host boundary of the script runtime: everything a step needs beyond
//! control flow, variables, errors and script calls (script-steps.md 7.1,
//! script-runtime.md 12 "Required host hooks").
//!
//! [`ScriptHost`] is the calculation host ([`CalcHost`]) plus one trait per
//! capability group: [`Navigation`], [`Records`], [`Fields`], [`FoundSets`],
//! [`Windows`], [`Dialogs`], [`Files`], [`Network`], [`Accounts`] and
//! [`Platform`]. Every method has a default, so a host implements only what
//! it supports (`impl Windows for MyHost {}`): step methods default to
//! [`HostError::Unsupported`], which the runtime reports as FileMaker's
//! error 3 "Command is unavailable" (or stops on, see
//! [`super::Options::stop_on_unsupported`]); hooks default to what FileMaker
//! Server's engine does without a user interface.
//!
//! Arguments are the step's decoded parameters: calculations already
//! evaluated, references resolved to ids, options as Booleans and enums.
//! Specs FileMaker stores as opaque subtrees (find requests, sort orders,
//! import/export orders, print settings, file references) are passed as the
//! stored [`OwnedNode`], since their layouts belong to other workstreams.
//!
//! Variable targets never reach the host: the runtime writes `$`/`$$`
//! variables itself and passes only field targets.
//!
//! The trait is synchronous so the runtime can run on a worker thread (the
//! GUI's provider worker, a server session). A step that needs the user
//! (a dialog, a pause) may block in the host; a host that cannot block
//! returns [`HostError::Pending`] and later calls
//! [`super::Runtime::complete_step`] (script-runtime.md 1.7, multi-stage
//! steps).

use std::sync::Arc;

use om_calc::eval::{CalcHost, Context};
use om_calc::Value;
use om_hbam::OwnedNode;

use super::error::ErrorCode;
use super::trigger::{Trigger, TriggerAction};
use crate::{Script, ScriptStep};

/// A file of the session, numbered by the host. The runtime keeps `$$`
/// variables per file.
pub type FileId = u64;

/// A window of the session, numbered by the host.
pub type WindowId = u64;

/// How a host step ends other than with success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    /// The step failed with this FileMaker error; it becomes the step's
    /// error (`Get ( LastError )`, alerts, transaction errors).
    Error(ErrorCode),
    /// The host does not implement this capability. FileMaker reports a
    /// step it has no handler for as error 3 "Command is unavailable".
    Unsupported,
    /// The step goes on asynchronously (FileMaker's internal 3001): the
    /// runtime stops stepping until [`super::Runtime::complete_step`].
    Pending,
}

impl From<ErrorCode> for HostError {
    fn from(code: ErrorCode) -> Self {
        HostError::Error(code)
    }
}

pub type HostResult<T = ()> = Result<T, HostError>;

/// A field a step targets: a table occurrence, a field of its base table
/// and a 1-based repetition (calculated repetitions already evaluated).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldRef {
    pub occurrence: u64,
    pub field: u64,
    pub repetition: u32,
}

/// A window mode (`Get ( WindowMode )` numbering).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Browse,
    Find,
    Preview,
    Layout,
}

impl Mode {
    /// The trigger mode filter bit (script-runtime.md 7.2): 1 Browse, 2
    /// Find, 4 Preview; Layout mode runs no triggers.
    pub fn trigger_bit(self) -> u32 {
        match self {
            Mode::Browse => 1,
            Mode::Find => 2,
            Mode::Preview => 4,
            Mode::Layout => 0,
        }
    }
}

/// Go to Layout's (and Go to Related Record's) layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutTarget {
    /// The layout the script's window showed when the frame started.
    Original,
    /// A stored layout key.
    Key(u64),
    /// By name (calculated).
    Name(String),
    /// By number in the layout order (calculated).
    Number(i64),
}

/// Go to Record/Request/Page and Go to Portal Row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordMove {
    First,
    Last,
    Previous,
    Next,
    /// A 1-based position (calculated or stored).
    Number(i64),
}

/// Insert Text, Insert Calculated Result and friends: where the value goes
/// when the target is a field (or the active field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InsertTarget {
    /// `None`: the field that has the focus.
    pub field: Option<FieldRef>,
    /// "Select entire contents": replace the field's contents rather than
    /// inserting at the insertion point.
    pub select_all: bool,
}

/// The step inserting a file into a container field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertKind {
    /// Insert File (131): any file; `file` paths.
    File,
    /// Insert Picture (56): `image` paths.
    Picture,
    /// Insert PDF (158): `image` paths.
    Pdf,
    /// Insert Audio/Video (159): `movie` paths.
    AudioVideo,
}

/// Insert File, Insert Picture, Insert PDF and Insert Audio/Video
/// (script-steps.md 10, catalogs/script-step-params.csv).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertFile {
    pub kind: InsertKind,
    /// The path list, variables expanded; empty: the user chooses a file.
    pub paths: Vec<String>,
    /// "Store only a reference" (`kStoreReference`).
    pub reference: bool,
    pub with_dialog: bool,
    /// Where the file goes: a field (the active one when its field is
    /// `None`), or `None` for Insert File's variable target, whose value
    /// the host returns for the runtime to set.
    pub target: Option<InsertTarget>,
}

/// The clipboard steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clipboard {
    Cut,
    Copy,
    Paste { no_style: bool },
    Clear,
}

/// Show / Hide / Toggle choices (toolbars, menubar, text ruler, keyboard).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Show,
    Hide,
    Toggle,
}

impl Visibility {
    fn from_choice(v: u16) -> Option<Self> {
        Some(match v {
            1 => Visibility::Show,
            2 => Visibility::Hide,
            3 => Visibility::Toggle,
            _ => return None,
        })
    }
}

/// A window chosen by a window step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowTarget {
    Current,
    /// By name (calculated).
    Named(String),
}

/// A window's bounds in points; `None` keeps the current value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds {
    pub height: Option<f64>,
    pub width: Option<f64>,
    pub top: Option<f64>,
    pub left: Option<f64>,
}

/// New Window.
#[derive(Clone, Debug, PartialEq)]
pub struct NewWindow {
    pub name: Option<String>,
    pub bounds: Bounds,
    /// A stored layout key; `None` keeps the current layout.
    pub layout: Option<u64>,
    /// Window style (`mStepValue`) and option bits (`[00 0, 03]`), as stored.
    pub style: u16,
    pub options: Option<i64>,
}

impl NewWindow {
    /// The style and options the style word (`options`) asks for.
    pub fn window_options(&self) -> WindowOptions {
        WindowOptions::from_word(self.options)
    }
}

/// The style a New Window, or Go to Related Record's new window, opens
/// (script-steps.md 11.8, "New Window style word").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowStyle {
    Document,
    Dialog,
    Card,
    /// A style word whose style is not mapped (Floating Document's is not
    /// measured): the low byte of the word.
    Unknown(u8),
}

/// What a style word (`[00 0, 03]`) asks for. The low byte is the style:
/// 0x02 Document, 0x04 Dialog (FileMaker's XML), 0x10 Card (the only other
/// word in Claris's starter solutions: Tasks' Assignee Chooser card).
/// Options are bits above it; FileMaker clears those a style lacks
/// (Dialog has no Minimize; that card has Dim parent window only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowOptions {
    pub style: WindowStyle,
    /// Dim parent window (0x0010_0000): a card darkens its parent's
    /// layout area.
    pub dim_parent: bool,
    /// The close box. Close, Maximize and Resize are three bits that no
    /// sample tells apart (0x0200, 0x0001_0000, 0x0004_0000); a card has
    /// a close box when any is set (the Tasks card, without one, has none).
    pub close: bool,
    pub minimize: bool,
}

impl WindowOptions {
    /// FileMaker's own words for the styles: New Window's default Document
    /// (every option on), the Dialog of FileMaker's XML, and the Tasks
    /// starter solution's Card (Dim parent window on, no close box).
    pub const DOCUMENT_WORD: u32 = 0x0037_0602;
    pub const DIALOG_WORD: u32 = 0x8005_0204;
    pub const CARD_WORD: u32 = 0xC010_0010;
    const DIM_PARENT: u32 = 0x0010_0000;
    const MINIMIZE: u32 = 0x0002_0000;
    const CLOSE_MAXIMIZE_RESIZE: u32 = 0x0004_0000 | 0x0001_0000 | 0x0200;

    /// No word (a step OpenMaker made, or FileMaker's default) and a word
    /// without a style byte open a Document window.
    pub fn from_word(word: Option<i64>) -> WindowOptions {
        let Some(word) = word.map(|w| w as u32) else {
            return WindowOptions {
                style: WindowStyle::Document,
                dim_parent: false,
                close: true,
                minimize: true,
            };
        };
        let style = match word as u8 {
            0 | 0x02 => WindowStyle::Document,
            0x04 => WindowStyle::Dialog,
            0x10 => WindowStyle::Card,
            other => WindowStyle::Unknown(other),
        };
        WindowOptions {
            style,
            dim_parent: word & Self::DIM_PARENT != 0,
            close: word & Self::CLOSE_MAXIMIZE_RESIZE != 0,
            minimize: word & Self::MINIMIZE != 0,
        }
    }
}

/// Go to Related Record.
#[derive(Clone, Debug)]
pub struct RelatedRecords<'a> {
    /// The related table occurrence (`[00 0, 11]`).
    pub occurrence: Option<u64>,
    pub layout: Option<LayoutTarget>,
    /// Use the related table's owning file's layouts (`kOption`).
    pub external_layout: bool,
    /// The option word `[00 0, 03]` and flags, as stored (meaning partly
    /// mapped, script-steps.md 5.1).
    pub options: Option<i64>,
    pub show_only_related: bool,
    pub match_found_set: bool,
    pub new_window: bool,
    pub animation: Option<u32>,
    /// Name, height, width, top, left, retaining absent calculation slots.
    pub window: [Option<Value>; 5],
    pub step: &'a ScriptStep,
}

impl RelatedRecords<'_> {
    /// The new window's style and options (`options`, the style word).
    pub fn window_options(&self) -> WindowOptions {
        WindowOptions::from_word(self.options)
    }
}

/// Go to List of Records (228, script-steps.md 12.2).
#[derive(Clone, Debug, PartialEq)]
pub struct RecordList {
    /// The record ids as a return-separated list (calculation slot 7).
    pub ids: String,
    pub layout: Option<LayoutTarget>,
    pub animation: Option<u32>,
    /// "Show in new window" (`kTurnOn`): the window as Go to Related Record
    /// stores it, its name in slot 0, its bounds in slots 1-4 and its style
    /// word at `[00 0, 03]`. Its `layout` is `None` and `style` 0: the step
    /// goes to its own layout.
    pub new_window: Option<NewWindow>,
}

/// Commit Records/Requests and Open Transaction options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitOptions {
    pub with_dialog: bool,
    pub skip_validation: bool,
    pub override_ess_locking: bool,
}

/// Open Transaction's options (script-runtime.md 8.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransactionOptions {
    pub skip_validation: bool,
    pub override_ess_locking: bool,
    pub skip_auto_enter: bool,
}

/// Show Custom Dialog (script-steps.md 5.1, corpus answers).
#[derive(Clone, Debug)]
pub struct CustomDialog {
    pub title: String,
    pub message: String,
    /// One to three buttons, default button first.
    pub buttons: Vec<DialogButton>,
    pub inputs: Vec<DialogInput>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogButton {
    pub label: String,
    /// Whether choosing it stores the input fields' values.
    pub commit: bool,
}

#[derive(Clone, Debug)]
pub struct DialogInput {
    pub label: String,
    pub password: bool,
    /// The current value of the input's field or variable.
    pub value: Value,
}

/// The user's answer to a custom dialog.
#[derive(Clone, Debug)]
pub struct DialogResponse {
    /// The 1-based button chosen (`Get ( LastMessageChoice )`).
    pub button: u8,
    /// The input values, in input order.
    pub inputs: Vec<Value>,
}

/// An error FileMaker Pro would alert with error capture off
/// (script-runtime.md 4.4).
#[derive(Clone, Debug)]
pub struct ErrorAlert {
    pub code: ErrorCode,
    pub script: String,
    pub step: u16,
    pub step_name: String,
    /// 1-based line in the script.
    pub line: usize,
    /// A continue/cancel alert (`SharedErrorsThatCauseConfirmAlert`).
    pub confirm: bool,
    /// Allow User Abort is On: the user may cancel the script.
    pub can_cancel: bool,
}

/// The user's answer to an error alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertChoice {
    Continue,
    /// Halt the script.
    Cancel,
}

/// A step error to log: FileMaker Server's event log (script-runtime.md
/// 4.9) or Set Error Logging's `ScriptErrors.log` (4.8).
#[derive(Clone, Debug)]
pub struct ErrorLogEntry {
    pub code: ErrorCode,
    pub file: FileId,
    pub script: String,
    pub step_name: String,
    pub line: usize,
    /// Set Error Logging's custom debug info, evaluated at log time.
    pub custom: Option<Value>,
    /// Set Error Logging's `ScriptErrors.log` rather than the server's
    /// event log.
    pub script_error_log: bool,
}

/// How a pause ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseEnd {
    /// The user continued or the duration elapsed.
    Resumed,
    /// The user canceled (Esc with Allow User Abort On).
    Canceled,
}

/// Whether the account may run a script (script-runtime.md 2.2, 9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptAccess {
    /// Script privilege at least "executable only".
    pub executable: bool,
    /// "Run script with full access privileges".
    pub full_access: bool,
}

impl Default for ScriptAccess {
    fn default() -> Self {
        ScriptAccess {
            executable: true,
            full_access: false,
        }
    }
}

/// Perform Script on Server (script-runtime.md 11.1).
#[derive(Clone, Debug)]
pub struct ServerScriptCall {
    pub file: FileId,
    pub script: u64,
    pub parameter: Option<Value>,
    /// Wait for completion.
    pub wait: bool,
}

/// What a server script returned (script-runtime.md 11.2 reply).
#[derive(Clone, Debug, Default)]
pub struct ServerScriptResult {
    /// The top script's Exit Script result; `None` when not waiting.
    pub result: Option<Value>,
    /// The server's `Get ( LastError )` at the end of the script.
    pub error: ErrorCode,
}

/// Perform Script on Server with Callback (script-runtime.md 11.4).
#[derive(Clone, Debug)]
pub struct ServerCallbackCall {
    pub call: ServerScriptCall,
    /// The local callback script and its parameter calculation, which the
    /// host runs as an OnPSOSCallback trigger when the result arrives.
    pub callback: Option<(FileId, u64)>,
    pub callback_parameter: Option<crate::Calculation>,
    /// The "current script" state option: 1 Halt, 2 Exit, 3 Resume, 4 Pause.
    pub state: u16,
}

/// Insert from URL.
#[derive(Clone, Debug)]
pub struct UrlRequest {
    pub url: String,
    pub curl_options: Option<String>,
    pub verify_ssl: bool,
    pub with_dialog: bool,
    pub encode_url: bool,
    /// The values of the `$` and `$$` variables the cURL options name
    /// (`-d @$data`, `--cookie $jar`), read before the request.
    pub variables: Vec<(String, Value)>,
    /// The field the response goes into (the active one when its field is
    /// `None`), or `None` for a variable: a container field takes
    /// container data, other fields text.
    pub target: Option<InsertTarget>,
}

/// Insert from URL's outcome.
#[derive(Clone, Debug)]
pub struct UrlResponse {
    /// What goes to the step's target.
    pub body: Value,
    /// Variables options write (`--dump-header $headers`,
    /// `--cookie-jar $jar`), set before the target.
    pub variables: Vec<(String, Value)>,
}

/// Replace Field Contents' "Replace with" (script-steps.md 10:
/// `[00 1, 03]` 1 current contents, 2 serial numbers, 3 calculated result).
pub enum Replacement<'a> {
    /// The current record's value of the field, into every record.
    CurrentContents,
    /// Serial numbers from `initial`, by `increment`; `update_serial`
    /// moves the field's next serial value past the last one.
    Serial {
        initial: String,
        increment: i64,
        update_serial: bool,
    },
    /// The calculation, evaluated on each record after the host made it
    /// current.
    Calculation(&'a mut dyn FnMut(&mut dyn ScriptHost) -> Value),
}

/// Data file encodings (Write to / Read from Data File).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf16,
    Utf8,
    Bytes,
}

impl Encoding {
    fn from_choice(v: u16) -> Self {
        match v {
            1 => Encoding::Utf16,
            3 => Encoding::Bytes,
            _ => Encoding::Utf8,
        }
    }
}

/// A step the host handles generically: its parameters read through the
/// step's schema, calculations evaluated. Used for steps whose host
/// capability has no dedicated method (spelling, media, devices, AI, PDF
/// building, Apple events, plug-in steps, import/export, ...).
#[derive(Clone, Debug)]
pub struct StepCall<'a> {
    pub id: u16,
    pub name: &'a str,
    pub step: &'a ScriptStep,
    pub args: Vec<StepArg<'a>>,
}

impl StepCall<'_> {
    /// The first argument of a schema parameter.
    pub fn arg(&self, parameter: &str) -> Option<&Arg<'_>> {
        self.args
            .iter()
            .find(|a| a.parameter == parameter)
            .map(|a| &a.value)
    }
}

/// One stored part of a parameter.
#[derive(Clone, Debug)]
pub struct StepArg<'a> {
    /// The schema parameter's name.
    pub parameter: &'a str,
    pub location: crate::Location,
    pub value: Arg<'a>,
}

/// A decoded parameter part.
#[derive(Clone, Debug)]
pub enum Arg<'a> {
    Bool(bool),
    /// `mStepValue`.
    Choice(u16),
    /// A calculation's result.
    Value(Value),
    Text(String),
    Number(i64),
    Field(FieldRef),
    Variable(String),
    /// A layout, script, menu set or table key.
    Key(u64),
    /// The whole inline reference (data sources, privilege sets).
    Reserved([u8; 14]),
    /// A stored subtree (specs, file references, table occurrences).
    Node(&'a OwnedNode),
}

/// The runtime's whole host: calculations plus every capability group.
pub trait ScriptHost:
    CalcHost
    + Navigation
    + Records
    + Fields
    + FoundSets
    + Windows
    + Dialogs
    + Files
    + Network
    + Accounts
    + Platform
{
    /// The calculation context of the script's window: its layout's table
    /// occurrence, current record and portal row (script-steps.md 4.3).
    fn calc_context(&mut self) -> Context {
        Context::new()
    }

    /// The MBS plug-in subset's state (`runtime::plugins`). A host without
    /// it (the default) evaluates the plug-in's commands as without the
    /// plug-in.
    fn plugin_state(&mut self) -> Option<&mut super::plugins::PluginState> {
        None
    }

    /// The file whose window is current.
    fn current_file(&mut self) -> FileId {
        0
    }

    /// The current window.
    fn current_window(&mut self) -> Option<WindowId> {
        None
    }

    /// A script of a file's catalog; `None` when missing (error 104).
    fn script(&mut self, _file: FileId, _id: u64) -> Option<Arc<Script>> {
        None
    }

    /// A script id by name, compared without case (Perform Script by
    /// name); folders and separators do not count.
    fn script_named(&mut self, _file: FileId, _name: &str) -> Option<u64> {
        None
    }

    /// A file by data source name, compared with case (`source::script`).
    fn file_named(&mut self, _name: &str) -> Option<FileId> {
        None
    }

    /// Resolve a data source in the file containing the calling script,
    /// which may differ from the file of its selected window.
    fn file_named_from(&mut self, _from: FileId, name: &str) -> Option<FileId> {
        self.file_named(name)
    }

    /// The file a step's file reference names (`20`, a Perform Script into
    /// another file); FileMaker's default failure is 100.
    fn resolve_file(&mut self, _from: FileId, _reference: &OwnedNode) -> HostResult<FileId> {
        Err(ErrorCode::FILE_MISSING.into())
    }

    /// Find or open a window of `file` for a script to run in
    /// (`WindowFromFileRef`); a file without a usable window is error 5.
    fn window_for_file(&mut self, _file: FileId) -> HostResult<Option<WindowId>> {
        Ok(self.current_window())
    }

    /// Make `window` the script's window again (a frame resuming in its
    /// own window after a sub-script in another file).
    fn use_window(&mut self, _window: WindowId) -> HostResult {
        Ok(())
    }

    /// The account's access to a script.
    fn script_access(&mut self, _file: FileId, _script: u64) -> ScriptAccess {
        ScriptAccess::default()
    }

    /// Raise the file user to full access for one step, or restore it.
    fn set_full_access(&mut self, _file: FileId, _on: bool) {}

    /// The current window's mode.
    fn window_mode(&mut self) -> Mode {
        Mode::Browse
    }

    /// Leave Layout mode before a step runs (`CorrectUserMode`); a failure
    /// ends the frame.
    fn exit_layout_mode(&mut self) -> HostResult {
        Ok(())
    }

    /// Whether the session was aborted: an administrator disconnected a
    /// server script, or the user pressed Esc / Cmd-period.
    fn abort_requested(&mut self) -> bool {
        false
    }

    /// Called before each step runs (after the abort check): a host brings
    /// up to date what may have changed since the last step, such as a
    /// server session's copy of a schema another client saved.
    fn before_step(&mut self) {}

    /// Files held by active script frames or a transaction, for desktop close preflight.
    fn active_script_files(&mut self, _files: &[FileId]) {}

    /// Triggers that events in the last step caused, in event order. The
    /// runtime fires them after the step.
    fn take_triggers(&mut self) -> Vec<Trigger> {
        Vec::new()
    }

    /// Perform (`proceed`) or drop the default action of an event whose
    /// trigger script has finished (`TriggerAction::PerformAction`).
    fn trigger_action(&mut self, _action: &TriggerAction) -> HostResult {
        Ok(())
    }

    /// Show the error alert of a client with error capture off. The
    /// default, a host without a user interface, continues.
    fn error_alert(&mut self, _alert: &ErrorAlert) -> AlertChoice {
        AlertChoice::Continue
    }

    /// Log a step error (server event log, Set Error Logging).
    fn log_error(&mut self, _entry: &ErrorLogEntry) {}

    /// Wait out Pause/Resume Script: `None` until the user continues,
    /// otherwise a number of seconds. A host that cannot block returns
    /// [`HostError::Pending`] and calls [`super::Runtime::resume`] later.
    fn pause(&mut self, _seconds: Option<f64>) -> HostResult<PauseEnd> {
        Err(HostError::Unsupported)
    }

    /// Install OnTimer Script: run the script every `interval` seconds as
    /// an OnTimer trigger (event 203), or stop the timer (`None`).
    fn install_on_timer(&mut self, _timer: Option<OnTimer>) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// The MBS plug-in subset's `FM.RunScriptIdle` and `FM.StopScriptIdle`
    /// (`runtime::plugins`): schedule the idle script, replacing any, or
    /// cancel it (`None`).
    fn schedule_idle_script(&mut self, _idle: Option<super::plugins::IdleScript>) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// A step without a dedicated method. A returned value goes to the
    /// step's Target parameter, if it has one.
    fn other_step(&mut self, _call: &StepCall<'_>) -> HostResult<Option<Value>> {
        Err(HostError::Unsupported)
    }
}

/// Install OnTimer Script's timer.
#[derive(Clone, Debug)]
pub struct OnTimer {
    pub file: FileId,
    pub window: Option<WindowId>,
    pub script: u64,
    pub parameter: Option<crate::Calculation>,
    pub interval: f64,
}

/// Layouts, records, fields and objects the window shows.
pub trait Navigation {
    /// Go to Layout (6); `animation` is the stored transition (slot 6).
    fn go_to_layout(&mut self, _layout: &LayoutTarget, _animation: Option<u32>) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// Go to Record/Request/Page (16): `Ok(false)` when there is no
    /// further record in that direction (Exit after last, error 101).
    fn go_to_record(&mut self, _movement: RecordMove, _with_dialog: bool) -> HostResult<bool> {
        Err(HostError::Unsupported)
    }

    /// Go to Portal Row (99), as [`Self::go_to_record`].
    fn go_to_portal_row(
        &mut self,
        _movement: RecordMove,
        _select: bool,
        _with_dialog: bool,
    ) -> HostResult<bool> {
        Err(HostError::Unsupported)
    }

    /// Go to Field (17); `None` leaves every field.
    fn go_to_field(&mut self, _field: Option<FieldRef>, _select_or_perform: bool) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// Go to Next Field (4) and Go to Previous Field (5).
    fn go_to_next_field(&mut self, _forward: bool) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// Go to Object (145).
    fn go_to_object(&mut self, _name: &str, _repetition: u32) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// Go to Related Record (74).
    fn go_to_related_record(&mut self, _request: &RelatedRecords<'_>) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// Go to List of Records (228).
    fn go_to_list_of_records(&mut self, _request: &RecordList) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// Enter Browse (55), Find (22) or Preview (41) Mode; Enter Find Mode's
    /// stored requests when Restore is on.
    fn enter_mode(
        &mut self,
        _mode: Mode,
        _pause: bool,
        _requests: Option<&OwnedNode>,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// Close Popover (169).
    fn close_popover(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
}

/// Record commands and the record transaction.
pub trait Records {
    /// New Record/Request (7).
    fn new_record(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Duplicate Record/Request (8).
    fn duplicate_record(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Delete Record/Request (9).
    fn delete_record(&mut self, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Delete Portal Row (104).
    fn delete_portal_row(&mut self, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Delete All Records (10): the found set.
    fn delete_all_records(&mut self, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Revert Record/Request (51).
    fn revert_record(&mut self, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Commit Records/Requests (75).
    fn commit_records(&mut self, _options: CommitOptions) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Open Record/Request (133).
    fn open_record(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Copy Record/Request (101) and Copy All Records/Requests (98).
    fn copy_records(&mut self, _all: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Truncate Table (182): a base table key, or the current table.
    fn truncate_table(&mut self, _table: Option<u64>, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }

    /// Open Transaction (205): bind the window's record transaction.
    fn begin_transaction(&mut self, _options: TransactionOptions) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Commit Transaction (206): commit the active record and everything
    /// changed since [`Self::begin_transaction`]; on failure the host
    /// reverts and returns the error.
    fn commit_transaction(&mut self, _skip_validation: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Revert Transaction (207), an error with Set Revert Transaction on
    /// Error On, or Halt Script: undo every change since
    /// [`Self::begin_transaction`].
    fn revert_transaction(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
}

/// Field values, selections and the clipboard.
pub trait Fields {
    /// Set Field (76).
    fn set_field(&mut self, _field: FieldRef, _value: Value) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Set Field By Name (147): `Table::Field`, as calculated.
    fn set_field_by_name(&mut self, _name: &str, _value: Value) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// The field a calculated name names, as Set Field By Name (147)
    /// finds it: `Occurrence::Field`, or a field of the current layout's
    /// occurrence (Replace Field Contents by Name, 249).
    fn field_by_name(&mut self, _name: &str) -> HostResult<FieldRef> {
        Err(HostError::Unsupported)
    }
    /// The value of a field target (Show Custom Dialog's inputs).
    fn field_value_of(&mut self, _field: FieldRef) -> HostResult<Value> {
        Err(HostError::Unsupported)
    }
    /// Insert a value into a field target: Insert Text (61), Insert
    /// Calculated Result (77), Insert Current Date/Time/User Name (13, 14,
    /// 60), and the results of steps with a Target (Insert from URL, data
    /// files, Execute Data API, ...).
    fn insert(&mut self, _target: InsertTarget, _value: Value) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Insert from Index (11) and Insert from Last Visited (12).
    fn insert_from(&mut self, _target: InsertTarget, _last_visited: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Cut, Copy, Paste and Clear (46-49).
    fn clipboard(&mut self, _op: Clipboard, _target: InsertTarget) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Select All (50).
    fn select_all(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Set Selection (130): 1-based start and end positions.
    fn set_selection(&mut self, _field: Option<FieldRef>, _start: i64, _end: i64) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Undo/Redo (45): 1 Undo, 2 Redo, 3 Toggle.
    fn undo_redo(&mut self, _choice: u16) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Relookup Field Contents (40).
    fn relookup(&mut self, _field: Option<FieldRef>, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Set Next Serial Value (116).
    fn set_next_serial_value(&mut self, _field: FieldRef, _value: Value) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Replace Field Contents (91): every record of the found set; the
    /// active field when `field` is `None`.
    fn replace_field_contents(
        &mut self,
        _field: Option<FieldRef>,
        _with: &mut Replacement<'_>,
        _with_dialog: bool,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Insert File (131), Insert Picture (56), Insert PDF (158) and Insert
    /// Audio/Video (159): read the file and put it into the request's target
    /// field, returning the container value; for a variable target only
    /// return it.
    fn insert_file(&mut self, _request: &InsertFile) -> HostResult<Value> {
        Err(HostError::Unsupported)
    }
    /// Export Field Contents (132).
    fn export_field_contents(
        &mut self,
        _field: Option<FieldRef>,
        _paths: &[String],
        _create_folders: bool,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
}

/// Finds, sorts and the found set.
pub trait FoundSets {
    /// Show All Records (23).
    fn show_all_records(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Show Omitted Only (27).
    fn show_omitted_only(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Omit Record (25) and Omit Multiple Records (26).
    fn omit_records(&mut self, _count: i64, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Unsort Records (21).
    fn unsort_records(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Modify Last Find (24).
    fn modify_last_find(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Perform Find (28), Constrain (126) or Extend (127) Found Set with
    /// the stored requests, or the current find requests (`None`).
    fn perform_find(&mut self, _kind: FindKind, _requests: Option<&OwnedNode>) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// [`Self::perform_find`] run as a command from the window (a menu, the
    /// toolbar, a one-step button) rather than by a script: FileMaker's
    /// manual find, whose no match keeps the found set for the dialog's
    /// Modify Find and Cancel (client-ui.md, "Dialogs"). The default is a
    /// script's find.
    fn perform_find_command(&mut self, kind: FindKind, requests: Option<&OwnedNode>) -> HostResult {
        self.perform_find(kind, requests)
    }
    /// Perform Quick Find (150).
    fn perform_quick_find(&mut self, _text: &str) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Find Matching Records (155): 1 replace, 2 constrain, 3 extend.
    fn find_matching(&mut self, _field: Option<FieldRef>, _kind: FindKind) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Sort Records (39) with the stored order, or the dialog.
    fn sort_records(&mut self, _order: Option<&OwnedNode>, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Sort Records by Field (154): 1 ascending, 2 descending, 3 value list.
    fn sort_by_field(&mut self, _field: Option<FieldRef>, _direction: u16) -> HostResult {
        Err(HostError::Unsupported)
    }
}

/// How a find combines with the found set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindKind {
    Replace,
    Constrain,
    Extend,
}

/// Windows and what they show.
pub trait Windows {
    /// New Window (122).
    fn new_window(&mut self, _window: &NewWindow) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Close Window (121).
    fn close_window(&mut self, _target: &WindowTarget, _current_file_only: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Select Window (123).
    fn select_window(&mut self, _target: &WindowTarget, _current_file_only: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Set Window Title (124).
    fn set_window_title(
        &mut self,
        _target: &WindowTarget,
        _title: &str,
        _current_file_only: bool,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Move/Resize Window (119).
    fn move_resize_window(
        &mut self,
        _target: &WindowTarget,
        _bounds: Bounds,
        _current_file_only: bool,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Adjust Window (31): 1 Resize to Fit, 2 Maximize, 3 Minimize, 4
    /// Restore, 5 Hide.
    fn adjust_window(&mut self, _choice: u16) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Arrange All Windows (120).
    fn arrange_windows(&mut self, _choice: u16) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Freeze Window (79).
    fn freeze_window(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Refresh Window (80).
    fn refresh_window(&mut self, _flush_joins: bool, _flush_external: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Scroll Window (81): 1 Home, 2 End, 3 Page Up, 4 Page Down, 5 To
    /// Selection.
    fn scroll_window(&mut self, _choice: u16) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Set Zoom Level (97).
    fn set_zoom_level(&mut self, _choice: u16, _lock: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// View As (30): 1 Form, 2 List, 3 Table, 4 Cycle.
    fn view_as(&mut self, _choice: u16) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Show/Hide Toolbars (29), Show/Hide Menubar (166), Show/Hide Text
    /// Ruler (92), Enable Touch Keyboard (174).
    fn show_hide(&mut self, _what: Bar, _visibility: Visibility, _lock: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Refresh Object (167) and Refresh Portal (180). Repetition zero means
    /// all; a portal ignores repetitions. An empty portal name uses the active object.
    fn refresh_object(&mut self, _name: &str, _repetition: u32, _portal: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Set Web Viewer (146): the stored action and, for Go to URL, the URL.
    fn set_web_viewer(&mut self, _object: &str, _action: u16, _url: Option<&str>) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Perform JavaScript in Web Viewer (175).
    fn perform_javascript(
        &mut self,
        _object: &str,
        _function: &str,
        _args: &[Value],
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
}

/// The bars Show/Hide steps act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    /// Toolbars; `edit` includes the Edit Record toolbar.
    Toolbars {
        edit: bool,
    },
    Menubar,
    TextRuler,
    TouchKeyboard,
}

/// Dialogs and the menu commands that open them.
pub trait Dialogs {
    /// Show Custom Dialog (87). Blocks until the user answers (or returns
    /// [`HostError::Pending`]).
    fn show_custom_dialog(&mut self, _dialog: &CustomDialog) -> HostResult<DialogResponse> {
        Err(HostError::Unsupported)
    }
    /// The MBS plug-in subset's `Dialog.Run` (`runtime::plugins`): show
    /// the alert and wait for the answer.
    fn show_plugin_dialog(
        &mut self,
        _dialog: &super::plugins::PluginDialog,
    ) -> HostResult<super::plugins::PluginDialogAnswer> {
        Err(HostError::Unsupported)
    }
    /// The "Open Menu Item" steps (Open Manage Database, Open Script
    /// Workspace, Open Settings, ...): the step id says which.
    fn open_menu_item(&mut self, _step: u16) -> HostResult {
        Err(HostError::Unsupported)
    }
}

/// Database files and local files.
pub trait Files {
    /// Open File (33): a file reference, or a calculated path list.
    fn open_file(&mut self, _source: FileSource<'_>, _hidden: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    fn open_file_from(
        &mut self,
        _from: FileId,
        source: FileSource<'_>,
        hidden: bool,
    ) -> HostResult {
        self.open_file(source, hidden)
    }

    /// Close File (34).
    fn close_file(&mut self, _source: FileSource<'_>) -> HostResult {
        Err(HostError::Unsupported)
    }
    fn close_file_from(&mut self, _from: FileId, source: FileSource<'_>) -> HostResult {
        self.close_file(source)
    }

    /// Save a Copy as (37): 1 copy, 2 compacted, 3 clone, 4 self-contained.
    fn save_a_copy_as(
        &mut self,
        _paths: &[String],
        _kind: u16,
        _create_folders: bool,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Set Multi-User (84): 1 On, 2 On (Hidden), 3 Off.
    fn set_multi_user(&mut self, _choice: u16) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Set Use System Formats (94).
    fn set_use_system_formats(&mut self, _on: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Get File Exists (188).
    fn file_exists(&mut self, _path: &[String]) -> HostResult<bool> {
        Err(HostError::Unsupported)
    }
    /// Get File Size (189).
    fn file_size(&mut self, _path: &[String]) -> HostResult<i64> {
        Err(HostError::Unsupported)
    }
    /// Create Data File (190).
    fn create_data_file(&mut self, _path: &[String], _create_folders: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Open Data File (191): the new file id.
    fn open_data_file(&mut self, _path: &[String]) -> HostResult<i64> {
        Err(HostError::Unsupported)
    }
    /// Write to Data File (192).
    fn write_data_file(
        &mut self,
        _id: i64,
        _data: &Value,
        _encoding: Encoding,
        _line_feed: bool,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Read from Data File (193): `amount` bytes or characters, or the rest.
    fn read_data_file(
        &mut self,
        _id: i64,
        _amount: Option<i64>,
        _encoding: Encoding,
    ) -> HostResult<Value> {
        Err(HostError::Unsupported)
    }
    /// Get Data File Position (194).
    fn data_file_position(&mut self, _id: i64) -> HostResult<i64> {
        Err(HostError::Unsupported)
    }
    /// Set Data File Position (195).
    fn set_data_file_position(&mut self, _id: i64, _position: i64) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Close Data File (196).
    fn close_data_file(&mut self, _id: i64) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Delete File (197).
    fn delete_file(&mut self, _path: &[String]) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Rename File (199).
    fn rename_file(&mut self, _path: &[String], _new_name: &str) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// The user's desktop folder as a native path, without a closing
    /// separator, where the user has one (the plug-in subset's
    /// `Folders.UserDesktop`, `super::plugins`).
    fn desktop_folder(&mut self) -> Option<String> {
        None
    }
    /// Create the folder at native path `path`, and the folders above it
    /// that are missing; one already there is no error (the plug-in
    /// subset's `Files.CreateDirectory`).
    fn create_folder(&mut self, _path: &str) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// The bytes of the file at native path `path`, refused past `limit`
    /// (the plug-in subset's DynaPDF import files).
    fn read_file(&mut self, _path: &str, _limit: u64) -> HostResult<Vec<u8>> {
        Err(HostError::Unsupported)
    }
    /// Replace the file at native path `path` with `data`, in a folder that
    /// exists (the plug-in subset's DynaPDF output files).
    fn write_file(&mut self, _path: &str, _data: &[u8]) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// The native path of a FileMaker path list's first line for this
    /// platform, passing an existing native path through when asked (the
    /// plug-in subset's `Path.FileMakerPathToNativePath`).
    fn native_path(&mut self, _path: &str, _pass_through: bool) -> HostResult<String> {
        Err(HostError::Unsupported)
    }
    /// Delete the file at native path `path` (the plug-in subset's
    /// `Files.Delete`).
    fn delete_native_file(&mut self, _path: &str) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Lay `block` out as [`Files::render_pdf_text`] would, drawing nothing
    /// (the plug-in subset's `DynaPDF.WriteFText` and `WriteFTextEx`).
    /// Error 3: its text does not fit its rectangle's height; error 4: a
    /// line of it (a word longer than the width, or the spaces before a
    /// word) is wider than its rectangle, or, for a block whose ink must
    /// fit, its glyphs' outlines are; error 800: its face has no glyph for a
    /// character.
    fn check_pdf_text(&mut self, _block: &super::dynapdf_text::TextBlock) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// A PDF of one page per entry of `pages`, its blocks laid out and
    /// drawn, each page to be drawn over a page of a DynaPDF document.
    /// Errors as [`Files::check_pdf_text`]'s.
    fn render_pdf_text(
        &mut self,
        _pages: &[&[super::dynapdf_text::TextBlock]],
    ) -> HostResult<Vec<u8>> {
        Err(HostError::Unsupported)
    }
    /// Import Records (35): `call` holds the source paths and the stored
    /// specs (script-steps.md 11.3, 11.5; `om_transfer` decodes them).
    fn import_records(&mut self, _call: &TransferCall<'_>) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Export Records (36) and Save Records as Excel (143) (script-steps.md
    /// 11.4, 11.5, 11.7).
    fn export_records(&mut self, _call: &TransferCall<'_>) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Get Folder Path (181): the chosen folder.
    fn get_folder_path(
        &mut self,
        _title: &str,
        _default: &str,
        _allow_create: bool,
    ) -> HostResult<Value> {
        Err(HostError::Unsupported)
    }
}

/// Import Records (35), Export Records (36) or Save Records as Excel
/// (143): the step's options, its path list with variables expanded, and
/// its stored specs as their reassembled values.
#[derive(Clone, Debug)]
pub struct TransferCall<'a> {
    pub id: u16,
    pub step: &'a ScriptStep,
    /// Path list lines in order (`file:`, `filelinux:`, ...); empty when
    /// the step names no file.
    pub paths: Vec<String>,
    pub with_dialog: bool,
    /// "Specify import/export order" stored (`kRestore`).
    pub restore: bool,
    pub create_folders: bool,
    pub open_automatically: bool,
    pub create_email: bool,
    pub verify_ssl: bool,
    /// Import Records' data source kind (`mStepValue`): 1 file, 2 folder,
    /// 4 XML.
    pub source_kind: u16,
    /// `84` `FMImportOptions`.
    pub import_order: Option<Vec<u8>>,
    /// `85` `FMExportOptions`.
    pub export_order: Option<Vec<u8>>,
    /// `86` the translation profile.
    pub profile: Option<Vec<u8>>,
    /// `87` Import Records' profile extension.
    pub profile_ex: Option<Vec<u8>>,
    /// `f0` per-field import flags.
    pub field_flags: Option<Vec<u8>>,
    /// Save Records as Excel's calculation slots 0-5 evaluated (worksheet,
    /// title, subject, author and the rest; their order is not mapped).
    pub slots: Vec<Option<Value>>,
}

/// A database file a step names.
#[derive(Clone, Copy, Debug)]
pub enum FileSource<'a> {
    /// A stored file reference (`20`).
    Reference(&'a OwnedNode),
    /// A calculated path list or data source name.
    Path(&'a str),
    /// The current file.
    Current,
}

/// HTTP, mail, SQL, the Data API and the host's script engine.
pub trait Network {
    /// Open URL (111).
    fn open_url(&mut self, _url: &str, _with_dialog: bool, _external: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Insert from URL (160): the response, which the runtime stores in the
    /// step's target, and the variables the options write.
    fn fetch_url(&mut self, _request: &UrlRequest) -> HostResult<UrlResponse> {
        Err(HostError::Unsupported)
    }
    /// Execute FileMaker Data API (203): the JSON response.
    fn execute_data_api(&mut self, _request: &str) -> HostResult<Value> {
        Err(HostError::Unsupported)
    }
    /// Perform Script on Server (164): the server script's result (when
    /// waiting) and its final `Get ( LastError )`, which becomes the step's
    /// error. A client host passes the call to its connection to the host
    /// (`om_data::DataProvider::perform_script_on_server`, op30).
    fn perform_script_on_server(
        &mut self,
        _call: &ServerScriptCall,
    ) -> HostResult<ServerScriptResult> {
        Err(HostError::Unsupported)
    }
    /// Perform Script on Server with Callback (210): start the script
    /// without waiting and keep the callback script; when the host's result
    /// arrives (`om_data::DataProvider::server_script_results`, op105), fire
    /// it as an OnPSOSCallback trigger with that result and error preset.
    fn perform_script_on_server_with_callback(&mut self, _call: &ServerCallbackCall) -> HostResult {
        Err(HostError::Unsupported)
    }
}

/// Accounts and privileges (script-runtime.md 9; steps 83, 134-138).
pub trait Accounts {
    /// Add Account (134): the privilege set as stored in `mReserved`.
    fn add_account(
        &mut self,
        _name: &str,
        _password: &str,
        _privilege_set: &[u8; 14],
        _expire: bool,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Delete Account (135).
    fn delete_account(&mut self, _name: &str) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Reset Account Password (136).
    fn reset_account_password(
        &mut self,
        _name: &str,
        _password: &str,
        _expire: bool,
    ) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Enable Account (137).
    fn enable_account(&mut self, _name: &str, _active: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Change Password (83).
    fn change_password(&mut self, _old: &str, _new: &str, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Re-Login (138).
    fn re_login(&mut self, _account: &str, _password: &str, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
}

/// The application and the operating system.
pub trait Platform {
    /// Beep (93).
    fn beep(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Exit Application (44).
    fn exit_application(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Flush Cache to Disk (102).
    fn flush_cache(&mut self) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Dial Phone (65).
    fn dial_phone(&mut self, _number: &str, _with_dialog: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Install Menu Set (142): a menu set key (`None` = standard menus).
    fn install_menu_set(&mut self, _menu_set: Option<u64>, _file_default: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Allow Formatting Bar (115).
    fn allow_formatting_bar(&mut self, _on: bool) -> HostResult {
        Err(HostError::Unsupported)
    }
    /// Install Plug-In File (157): the container field holding it.
    fn install_plugin_file(&mut self, _field: Option<FieldRef>) -> HostResult {
        Err(HostError::Unsupported)
    }
}

pub(crate) fn visibility(v: u16) -> Option<Visibility> {
    Visibility::from_choice(v)
}

pub(crate) fn encoding(v: u16) -> Encoding {
    Encoding::from_choice(v)
}

#[cfg(test)]
mod window_style_tests {
    use super::{WindowOptions, WindowStyle};

    #[test]
    fn style_words_name_their_style_and_options() {
        let of = |word: u32| WindowOptions::from_word(Some(i64::from(word as i32)));
        let document = of(WindowOptions::DOCUMENT_WORD);
        assert_eq!(document.style, WindowStyle::Document);
        assert!(document.close && document.minimize);
        let dialog = of(WindowOptions::DIALOG_WORD);
        assert_eq!(dialog.style, WindowStyle::Dialog);
        assert!(dialog.close && !dialog.minimize);
        // The Tasks starter solution's card: Dim parent window, no close box.
        let card = of(WindowOptions::CARD_WORD);
        assert_eq!(card.style, WindowStyle::Card);
        assert!(card.dim_parent && !card.close && !card.minimize);
        assert_eq!(of(0x0037_0608).style, WindowStyle::Unknown(0x08));
        // No word is FileMaker's default, a document window.
        assert_eq!(WindowOptions::from_word(None).style, WindowStyle::Document);
    }
}
