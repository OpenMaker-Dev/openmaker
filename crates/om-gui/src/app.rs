//! The top-level OpenMaker window, laid out like a FileMaker document
//! window: menu bar, status toolbar, layout bar, then the file's content. With
//! no file open it shows the Hosts dialog instead.
//!
//! Drawing code never calls the provider. It records [`Action`]s, which the app
//! applies after the frame is laid out; actions that need data become worker
//! requests, and replies are folded back in at the start of the next frame.

use egui::{
    vec2, Color32, CornerRadius, Id, Key, KeyboardShortcut, Modifiers, RichText, Ui,
    ViewportCommand,
};
use om_data::found::FindRequest;
use om_data::scripts::save_records::{Format as SaveFormat, SaveRecords};
use om_data::{DataError, DataProvider, LocalOpen};

use crate::connect::{self, ConnectForm, ConnectStatus, HostsView};
use crate::content;
use crate::content::PartsSent;
use crate::icons::{self, Icon};
use crate::layout_bar::{self, LayoutBarContext};
use crate::layout_setup::{LayoutSetup, LayoutSetupAnswer};
use crate::manage_layouts::{ManageLayouts, ManageLayoutsResult};
use crate::menubar::{self, MenuContext};
use crate::new_layout::{NewLayout, NewLayoutAnswer};
use crate::part_setup::{PartRow, PartSetup, PartSetupAnswer};
use crate::session::{PageKey, Session, Target, DEFAULT_PAGE_SIZE};
use crate::theme;
use crate::toolbar::{self, ToolbarContext};
use crate::widgets;
use crate::worker::{self, PollError, Reply, Request, Worker};

mod portal_clicks;
mod recovery;
mod scripts;
mod web_viewers;
mod window_styles;
pub use window_styles::CardFrame;
mod windows;
use crate::scripting::Start;

/// The window title when no file is open.
pub const APP_TITLE: &str = "OpenMaker";

/// FileMaker's four modes. The View menu checks the current one; the
/// toolbar and layout bar change with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Work with records.
    #[default]
    Browse,
    /// Enter find requests.
    Find,
    /// Design the layout.
    Layout,
    /// See the printed result.
    Preview,
}

impl Mode {
    pub const ALL: [Self; 4] = [Self::Browse, Self::Find, Self::Layout, Self::Preview];

    /// The View menu label.
    pub fn menu_label(self) -> &'static str {
        match self {
            Self::Browse => "Browse Mode",
            Self::Find => "Find Mode",
            Self::Layout => "Layout Mode",
            Self::Preview => "Preview Mode",
        }
    }

    /// FileMaker's mode shortcut (Cmd/Ctrl + B, F, L, U).
    pub fn shortcut(self) -> KeyboardShortcut {
        let key = match self {
            Self::Browse => Key::B,
            Self::Find => Key::F,
            Self::Layout => Key::L,
            Self::Preview => Key::U,
        };
        KeyboardShortcut::new(Modifiers::COMMAND, key)
    }

    /// The script runtime's name for the mode.
    pub fn runtime(self) -> om_data::runtime::Mode {
        match self {
            Self::Browse => om_data::runtime::Mode::Browse,
            Self::Find => om_data::runtime::Mode::Find,
            Self::Layout => om_data::runtime::Mode::Layout,
            Self::Preview => om_data::runtime::Mode::Preview,
        }
    }
}

/// How records are presented, mirroring FileMaker's View As choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// One record at a time.
    #[default]
    Form,
    /// The records one after another.
    List,
    /// The records as a spreadsheet grid.
    Table,
}

impl From<crate::scripting::View> for ViewMode {
    fn from(view: crate::scripting::View) -> Self {
        match view {
            crate::scripting::View::Form => ViewMode::Form,
            crate::scripting::View::List => ViewMode::List,
            crate::scripting::View::Table => ViewMode::Table,
        }
    }
}

impl From<ViewMode> for crate::scripting::View {
    fn from(view: ViewMode) -> Self {
        match view {
            ViewMode::Form => crate::scripting::View::Form,
            ViewMode::List => crate::scripting::View::List,
            ViewMode::Table => crate::scripting::View::Table,
        }
    }
}

impl ViewMode {
    pub const ALL: [Self; 3] = [Self::Form, Self::List, Self::Table];

    pub fn label(self) -> &'static str {
        match self {
            Self::Form => "Form",
            Self::List => "List",
            Self::Table => "Table",
        }
    }
}

/// An Edit menu command on the field the user is typing in
/// (`OpenMakerApp::edit_field`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditCommand {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Clear,
    SelectAll,
}

/// Where keys a keyboard commit held for the next frame were typed
/// (`OpenMakerApp::shortcuts`): the window's file, mode, layout and record,
/// and the active field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HeldKeysOwner {
    file: u64,
    mode: Mode,
    record: Option<(u32, i64)>,
    field: Option<String>,
}

/// Whether a field editor applies `event` to its text or selection: typed
/// text, a paste or cut, an input method's text, a key pressed.
fn edits_text(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Text(_)
            | egui::Event::Paste(_)
            | egui::Event::Cut
            | egui::Event::Ime(_)
            | egui::Event::Key { pressed: true, .. }
    )
}

/// Step ids of the commands that run as a single step.
pub(crate) mod steps {
    pub const UNSORT: u16 = 21;
    pub const SHOW_ALL: u16 = 23;
    pub const MODIFY_LAST_FIND: u16 = 24;
    pub const OMIT_RECORD: u16 = 25;
    pub const OMIT_MULTIPLE: u16 = 26;
    pub const SHOW_OMITTED: u16 = 27;
    pub const PERFORM_FIND: u16 = 28;
    pub const EXPORT_RECORDS: u16 = 36;
    pub const IMPORT_RECORDS: u16 = 35;
    pub const SORT_RECORDS: u16 = 39;
    pub const DELETE_ALL: u16 = 10;
    pub const NEW_RECORD: u16 = 7;
    pub const DUPLICATE_RECORD: u16 = 8;
    pub const DELETE_RECORD: u16 = 9;
    pub const CONSTRAIN: u16 = 126;
    pub const EXTEND: u16 = 127;
    pub const NEW_WINDOW: u16 = 122;
    pub const INSERT_PICTURE: u16 = 56;
    pub const INSERT_FROM_INDEX: u16 = 11;
    pub const INSERT_FROM_LAST_VISITED: u16 = 12;
    pub const INSERT_CURRENT_DATE: u16 = 13;
    pub const INSERT_CURRENT_TIME: u16 = 14;
    pub const INSERT_CURRENT_USER_NAME: u16 = 60;
    pub const COPY_RECORD: u16 = 101;
    pub const INSERT_FILE: u16 = 131;
    pub const EXPORT_FIELD_CONTENTS: u16 = 132;
    pub const PERFORM_QUICK_FIND: u16 = 150;
}

/// Keyboard shortcuts for implemented commands. `COMMAND` is Cmd on macOS
/// and Ctrl elsewhere.
pub(crate) mod shortcuts {
    use egui::{Key, KeyboardShortcut, Modifiers};

    const CMD: Modifiers = Modifiers::COMMAND;

    pub const QUIT: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::Q);
    pub const SETTINGS: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::Comma);
    pub const OPEN: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::O);
    pub const PRINT: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::P);
    pub const CLOSE: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::W);
    pub const MINIMIZE: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::M);
    pub const ZOOM_IN: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::Plus);
    pub const ZOOM_IN_ALTERNATE: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::Equals);
    pub const ZOOM_OUT: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::Minus);
    pub const REFRESH: KeyboardShortcut = KeyboardShortcut::new(CMD.plus(Modifiers::SHIFT), Key::R);
    pub const STATUS_TOOLBAR: KeyboardShortcut =
        KeyboardShortcut::new(CMD.plus(Modifiers::ALT), Key::S);
    /// FileMaker uses the Control key for these on every platform.
    pub const PREVIOUS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::ArrowUp);
    pub const NEXT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::ArrowDown);
    pub const NEW_REQUEST: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::N);
    pub const DELETE_REQUEST: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::E);
}

/// The Layout-mode tool palette.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum LayoutTool {
    #[default]
    Selection,
    Text,
    Shape,
    Field,
    Button,
    Panel,
    Portal,
    Chart,
    WebViewer,
    FormatPainter,
    Part,
}

impl LayoutTool {
    pub const ALL: [Self; 11] = [
        Self::Selection,
        Self::Text,
        Self::Shape,
        Self::Field,
        Self::Button,
        Self::Panel,
        Self::Portal,
        Self::Chart,
        Self::WebViewer,
        Self::FormatPainter,
        Self::Part,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Selection => "Selection",
            Self::Text => "Text Tools",
            Self::Shape => "Shape Tools",
            Self::Field => "Field Tools",
            Self::Button => "Button Tools",
            Self::Panel => "Panel Tools",
            Self::Portal => "Portal",
            Self::Chart => "Chart",
            Self::WebViewer => "Web Viewer",
            Self::FormatPainter => "Format Painter",
            Self::Part => "Part Tool",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::Selection => Icon::Selection,
            Self::Text => Icon::TextTool,
            Self::Shape => Icon::ShapeTool,
            Self::Field => Icon::FieldTool,
            Self::Button => Icon::ButtonTool,
            Self::Panel => Icon::PanelTool,
            Self::Portal => Icon::Portal,
            Self::Chart => Icon::Chart,
            Self::WebViewer => Icon::WebViewer,
            Self::FormatPainter => Icon::FormatPainter,
            Self::Part => Icon::PartTool,
        }
    }

    /// Whether FileMaker shows an options chevron beside the tool.
    pub fn has_options(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Shape | Self::Field | Self::Button | Self::Panel
        )
    }
}

/// The fields Layout mode's Field tool can place: those of the layout's own
/// table occurrence's table that `om_data::layout_placement` places (not
/// containers), in id order, each with that table.
fn placeable_fields(
    calcs: &crate::calc::Calculations,
    session: &Session,
) -> Vec<crate::layout_canvas::FieldChoice> {
    let Some(schema) = calcs.schema() else {
        return Vec::new();
    };
    let Some(table) = crate::calc::layout_occurrence(session).and_then(|o| schema.table_of(o))
    else {
        return Vec::new();
    };
    table
        .fields
        .values()
        .filter(|field| field.value_type != om_data::calc::ValueType::Container)
        .map(|field| crate::layout_canvas::FieldChoice {
            table: table.id,
            field: field.id,
            name: field.name.clone(),
            date: field.value_type == om_data::calc::ValueType::Date,
        })
        .collect()
}

/// The find requests being edited in Find mode, by field name as Perform
/// Find takes them: the layout's own fields bare, related ones
/// `Occurrence::Field`. There is always at least one.
#[derive(Clone, Debug)]
pub(crate) struct FindState {
    requests: Vec<FindRequest>,
    current: usize,
    /// The criteria field that last had focus, for Insert > Operators.
    focused_field: Option<String>,
    /// The request was just made: its first field takes the focus.
    focus_first: bool,
    /// The field object to focus next, when the one chosen was not drawn
    /// (on another tab panel, in a closed popover).
    focus_object: Option<u64>,
}

impl Default for FindState {
    fn default() -> Self {
        Self {
            requests: vec![FindRequest::default()],
            current: 0,
            focused_field: None,
            focus_first: true,
            focus_object: None,
        }
    }
}

impl FindState {
    pub fn len(&self) -> usize {
        self.requests.len()
    }

    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn current(&self) -> &FindRequest {
        &self.requests[self.current]
    }

    pub fn current_mut(&mut self) -> &mut FindRequest {
        &mut self.requests[self.current]
    }

    pub fn set_focused_field(&mut self, field: &str) {
        if self.focused_field.as_deref() != Some(field) {
            self.focused_field = Some(field.to_owned());
        }
    }

    /// Whether the current request's first field should take the focus,
    /// once.
    pub fn take_focus_first(&mut self) -> bool {
        std::mem::take(&mut self.focus_first)
    }

    /// The field object to focus instead of the first one, once.
    pub fn take_focus_object(&mut self) -> Option<u64> {
        self.focus_object.take()
    }

    pub fn focus_object(&mut self, object: u64) {
        self.focus_object = Some(object);
    }

    fn add(&mut self, request: FindRequest) {
        self.requests.push(request);
        self.current = self.requests.len() - 1;
        self.focus_first = true;
    }

    fn delete(&mut self) {
        if self.requests.len() > 1 {
            self.requests.remove(self.current);
            self.current = self.current.min(self.requests.len() - 1);
        }
    }

    fn set_omit(&mut self, omit: bool) {
        self.requests[self.current].omit = omit;
    }

    fn go_to(&mut self, index: usize) {
        self.current = index.min(self.requests.len() - 1);
    }

    fn insert_operator(&mut self, operator: &str) {
        let Some(field) = self.focused_field.clone() else {
            return;
        };
        let request = &mut self.requests[self.current];
        let text = format!("{}{operator}", request.get(&field).unwrap_or_default());
        request.set(&field, text);
    }
}

/// How long Quit waits for the provider to finish closing the files.
const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Records > Delete Record's confirmation, as FileMaker Pro words it
/// (client-ui.md, "Delete confirmations"); the Delete Record step's with
/// its dialog too.
pub(crate) const DELETE_RECORD_MESSAGE: &str = "Permanently delete this ENTIRE record?";

/// Something the user asked for during a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    ListFiles,
    ManageLicense,
    /// The app menu's Settings...: the Settings dialog.
    ShowSettings,
    /// The Window menu's list: bring window `id` to the front.
    SelectWindow(u32),
    PickFile(String),
    Connect,
    CancelConnect,
    /// File > Open: choose a file on this computer (the file open
    /// closes first).
    ShowOpenFile,
    /// File > Create New...: name a new, blank file, which then opens
    /// (the file open closes first).
    ShowCreateNew,
    /// Open this file on this computer.
    OpenLocal(std::path::PathBuf),
    /// Close the open file (if any) and show the Hosts dialog: every
    /// window closes, one at a time ([`Action::CloseWindow`]).
    CloseFile,
    /// Open the window's file again after its connection was lost
    /// (recovery.rs).
    Reconnect,
    /// After a reconnect: let the edits the host's copy no longer matches
    /// be saved, read records that could not be read back again, give the
    /// held edits up for the host's records, or put the reconnected
    /// window's notes away.
    KeepRecoveredEdits,
    CheckRecoveredEdits,
    DiscardRecoveredEdits,
    DismissRecovery,
    /// The window's partly applied save (#1006): read back the files whose
    /// outcome is unknown, send only the work not applied, or discard what
    /// was not applied.
    CheckUnsettledSave,
    ResumeUnsettledSave,
    DiscardUnsettledSave,
    /// The window's new record a reconnect could not make again (#152):
    /// try again, or discard it.
    RecreateRecord,
    DiscardNewRecord,
    /// Close the window the app works on, its record committed first,
    /// after OnWindowClose (and OnLastWindowClose for the file's last
    /// window, which closes the file).
    CloseWindow,
    SelectLayout(u32),
    GoToRecord(u64),
    GoToPage(u64),
    NewRecord,
    DuplicateRecord,
    DeleteRecord,
    /// The dialog of Delete Record with a portal row selected.
    CancelDeletePortalRow,
    ConfirmDeletePortalRow,
    ConfirmDeleteRecord,
    CancelDeleteRecord,
    CommitRecord,
    RevertRecord,
    RunButton {
        object_id: u64,
        record_index: u64,
        /// The portal row the button was clicked in, if any.
        portal: Option<PortalClick>,
    },
    RunScript(u64),
    /// File > Export Records, after the current record's commit/veto flow.
    ExportRecords,
    /// File > Save/Send Records As > Excel or PDF, and the toolbars' Save
    /// as buttons: the command (`om_scripts::save_records`) as the menus run
    /// it, after the same commit/veto flow.
    SaveRecordsAs(SaveFormat),
    /// File > Import Records > File, through the shared import worker.
    ImportRecords,
    /// A command that is one script step (found-set commands, Perform
    /// Find): the step id and its flags.
    Step(u16, u32),
    /// Edit > Undo, Redo, Cut, Copy, Paste, Clear or Select All in the
    /// field the user is typing in.
    EditField(EditCommand),
    /// The toolbar's Quick Find: Perform Quick Find with the search box's
    /// text.
    QuickFind(String),
    ScriptWorkspace,
    DismissScriptWorkspace,
    /// The Scripts menu shows the file's scripts: read its catalog once.
    ReadScripts,
    Refresh,
    LoadRecordRange(u64),
    SetView(ViewMode),
    ZoomIn,
    ZoomOut,
    ActualSize,
    SetMode(Mode),
    NewRequest,
    DuplicateRequest,
    DeleteRequest,
    GoToRequest(usize),
    SetOmit(bool),
    InsertOperator(&'static str),
    PerformFind,
    SelectTool(LayoutTool),
    /// Double-click Field Tools to keep it selected after each draft.
    LockFieldTool,
    ToggleInspector,
    ToggleObjects,
    ManageLayouts,
    ManageDatabase,
    /// File > Change Password… (#830).
    ChangePassword,
    DismissDatabase,
    ManageSecurity,
    DismissSecurity,
    ManageValueLists,
    DismissValueLists,
    DismissManageLayouts,
    /// Layouts > New Layout / Report..., Manage Layouts' New: the dialog.
    NewLayout,
    /// Its Finish: add the layout, then open it in Layout mode.
    CreateLayout(om_data::layout_catalog::LayoutRequest),
    /// Layouts > Duplicate Layout, Manage Layouts' Duplicate: copy the
    /// layout, then open the copy in Layout mode.
    DuplicateLayout(u32),
    /// Layouts > Delete Layout..., Manage Layouts' Delete: ask, as
    /// FileMaker Pro asks, then delete the layout.
    DeleteLayout(u32),
    /// Layouts > Layout Setup...: its dialog, over the layout shown.
    LayoutSetup,
    SetUpLayout(om_data::layout_catalog::LayoutSetupChange),
    /// Layouts > Part Setup...: its dialog, and Part Definition's change.
    PartSetup,
    SetPartOptions(om_data::layout_placement::PartOptionsChange),
    ConfirmDeleteLayout,
    CancelDeleteLayout,
    /// Go to the layout the window added and, once it is shown, Layout
    /// mode: one navigation, so a record saved first does not split it.
    OpenNewLayout(u32),
    /// Layouts > Save Layout: place the Field tool's new field.
    SaveLayout,
    /// Layouts > Revert Layout: drop it.
    RevertLayout,
    /// Delete or Edit > Clear in Layout mode: delete the selected object,
    /// as a change Save Layout stores.
    DeleteLayoutObject,
    ToggleToolbar,
    ToggleFormattingBar,
    /// The user typed into a field the account's privilege set lets them
    /// enter but not change: FileMaker Pro's alert, the value unchanged.
    RefuseTyping(om_data::EditRefusal),
    /// A value-list control's command it cannot do here: its alert.
    RefuseChoice(om_data::choice::ChoiceError),
    /// A pop-up menu's Edit…: read the list and open Edit Value List
    /// (#869).
    EditValueList(u64),
    /// Edit Value List's OK: save the list's values.
    SetValueListValues(om_data::value_lists::ValuesChange),
    DismissAlert,
    /// Discard input refused by before-mutation preparation, keeping accepted drafts.
    DiscardEntryCandidates,
    Minimize,
    Quit,
}

/// The account dialog of a related file that refused the window's file's
/// account (#214): the window that needed it and the file as the provider
/// names it.
struct RelatedLogin {
    window: u32,
    file: String,
    dialog: crate::open_file::LoginDialog,
}

/// A click on a button in a portal row (layout-model.md 5.3): the portal,
/// the record whose portal it is, and the row's related record and number
/// as the window showed them. The record identifies the row; its number is
/// only what the user saw then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PortalClick {
    pub layout: u64,
    /// The portal object.
    pub portal: u64,
    /// The portal's table occurrence.
    pub occurrence: u64,
    /// The window's record (the portal's parent).
    pub parent: u64,
    /// The row's related record.
    pub record: u64,
    /// The row, from 1, in the portal's own order.
    pub row: u64,
    /// The row's number was read from the rows the window showed in the
    /// frame that runs the button. A click held for exit triggers or a
    /// save is not: its row is found again by its record first
    /// (portal_clicks.rs).
    pub current: bool,
    /// A row without a record (`record` is `NO_RECORD`): which one.
    pub unsaved: Option<UnsavedRow>,
}

/// A portal row without a record: the creation row, or the row of a record
/// the user began in it (its `RelatedRow::New` id).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum UnsavedRow {
    Creation,
    Begun(u64),
}

impl Action {
    /// The action as it goes on after waiting (for field exit triggers or
    /// a save): a portal click must find its row again.
    pub(crate) fn held(self) -> Action {
        match self {
            Action::RunButton {
                object_id,
                record_index,
                portal: Some(click),
            } => Action::RunButton {
                object_id,
                record_index,
                portal: Some(PortalClick {
                    current: false,
                    ..click
                }),
            },
            action => action,
        }
    }

    /// Whether the window can still do this once the connection to the
    /// host is lost: close (the file closes window by window), quit,
    /// dismiss, bring another window to the front, and change only how it
    /// looks.
    fn works_offline(&self) -> bool {
        matches!(
            self,
            Action::CloseFile
                | Action::CloseWindow
                | Action::Reconnect
                | Action::SelectWindow(_)
                | Action::SetView(_)
                | Action::CancelConnect
                | Action::Quit
                | Action::Minimize
                | Action::DismissAlert
                | Action::DiscardEntryCandidates
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::ActualSize
                | Action::ToggleToolbar
                | Action::ToggleFormattingBar
                | Action::ToggleInspector
                | Action::ToggleObjects
                | Action::SelectTool(_)
                | Action::DismissScriptWorkspace
                | Action::DismissDatabase
                | Action::DismissSecurity
                | Action::DismissValueLists
                | Action::DismissManageLayouts
                | Action::CancelDeleteRecord
                | Action::CancelDeletePortalRow
        )
    }

    /// Whether the window can do this while edits a reconnect could not
    /// verify are held (recovery.rs): nothing that saves, moves to another
    /// record, closes or runs a script, only what looks, and the edits'
    /// own choices.
    fn works_while_held(&self) -> bool {
        matches!(
            self,
            Action::SelectWindow(_)
                | Action::SetView(_)
                | Action::Minimize
                | Action::DismissAlert
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::ActualSize
                | Action::ToggleToolbar
                | Action::ToggleFormattingBar
                | Action::ToggleInspector
                | Action::ToggleObjects
                | Action::SelectTool(_)
                | Action::DismissScriptWorkspace
                | Action::DismissDatabase
                | Action::DismissSecurity
                | Action::DismissValueLists
                | Action::DismissManageLayouts
                | Action::CancelDeleteRecord
                | Action::CancelDeletePortalRow
                | Action::LoadRecordRange(_)
                | Action::RevertRecord
                | Action::KeepRecoveredEdits
                | Action::CheckRecoveredEdits
                | Action::DiscardRecoveredEdits
        )
    }
}

/// The OpenMaker application.
pub struct OpenMakerApp {
    worker: Worker,
    /// Bumped whenever a session is abandoned; replies from older epochs are
    /// dropped.
    epoch: u64,
    form: ConnectForm,
    hosts: HostsView,
    status: ConnectStatus,
    /// The file being opened while `status` is `Connecting`.
    opening: Option<Target>,
    /// The Hosts dialog's file list, for the host it was asked of.
    listing: connect::Listing,
    /// The token of the last listing asked for.
    listings: u64,
    /// The front window, or the one being drawn or updated; the file's
    /// other windows wait in `windows` (windows.rs), the most recently in
    /// front last.
    w: windows::Doc,
    windows: Vec<windows::Doc>,
    presentation: Option<windows::Presentation>,
    closed_windows: std::collections::HashSet<u32>,
    window_files: std::collections::HashMap<u32, u64>,
    /// The windows' ids, titles and names (`Get ( WindowName )`), front
    /// first, as this frame began.
    window_list: Vec<(u32, String, String)>,
    manage_layouts: Option<ManageLayouts>,
    new_layout: Option<NewLayout>,
    layout_setup: Option<LayoutSetup>,
    /// Edit Value List, and the window whose menu's Edit… opened it (#869).
    value_list_edit: Option<(u32, crate::document::value_dialogs::EditDialog)>,
    part_setup: Option<PartSetup>,
    error: Option<String>,
    worker_stopped: bool,
    /// Closing the file (and then quitting, when `true`): each window
    /// that closes closes the next, until a trigger or a save stops it.
    closing: Option<bool>,
    /// Every window closed for Quit: the root viewport may close now.
    quit_ready: bool,
    /// Quit waits from then for the provider to handle the files' Closes,
    /// and the saves queued before them (`Request::KeepFoundSet`), so a
    /// save is never cut off by the app ending.
    quit_waiting: Option<std::time::Instant>,
    title: String,
    styled: bool,
    sample_data: bool,
    database: Option<crate::database::ManageDatabase>,
    security: Option<crate::security::ManageSecurity>,
    security_requests: u64,
    value_lists: Option<crate::value_lists::ManageValueLists>,
    value_list_requests: u64,
    /// Scripts the worker runs, as the window sees them.
    script: scripts::ScriptUi,
    workspace: Option<crate::script_workspace::ScriptWorkspace>,
    /// The last request made for a Script Workspace: each reply names its
    /// own, so only the workspace that asked takes it.
    workspace_requests: u64,
    /// What the Script Workspace asked for once a save it waited for came
    /// back (Run): the front window's next frame takes it, as it takes the
    /// workspace's own buttons (not while a script runs).
    workspace_followups: Vec<Action>,
    client_license: bool,
    license_dialog: Option<crate::licensing::LicenseDialog>,
    /// The Settings dialog, and the file its OK saves in.
    settings: Option<crate::settings::SettingsDialog>,
    settings_path: Option<std::path::PathBuf>,
    /// The windows' web viewers, the native window eframe made for the
    /// root viewport (the only one whose handle eframe gives out), and those
    /// found for the other windows' viewports.
    web: crate::web_viewer::WebViewers,
    native_parent: Result<crate::web_viewer::NativeParent, crate::web_viewer::Unavailable>,
    viewport_windows: crate::web_viewer::ViewportWindows,
    /// Open the form's file when the app first draws (`open_on_launch`).
    open_on_launch: bool,
    /// The reconnect of a file whose connection was lost (recovery.rs).
    reconnect: Option<recovery::ReconnectDialog>,
    /// File > Open's chooser, and the account a local file asks for.
    open_file: Option<crate::open_file::OpenFileDialog>,
    create_new: Option<crate::create_new::CreateNewDialog>,
    local_login: Option<crate::open_file::LoginDialog>,
    /// A new password for the account the local file refused with 211
    /// (#157), shown in the sign-in dialog's place.
    change_password: Option<crate::open_file::ChangePasswordDialog>,
    /// File > Change Password…'s dialog for the open local file (#830), with
    /// the window and file it was asked in.
    own_password: Option<(u32, u64, crate::open_file::ChangePasswordDialog)>,
    /// The related files of each open file, by file id, that refused its
    /// account and that no dialog has asked about yet (#214).
    related_sign_ins: std::collections::BTreeMap<u64, Vec<om_data::RelatedSignIn>>,
    /// Those asked about, by file id and name: not again in the session.
    related_asked: std::collections::BTreeSet<(u64, String)>,
    /// The account dialog of a related file, in the window that needed it.
    related_login: Option<RelatedLogin>,
    /// File > Open was chosen with a file open: the chooser shows once it
    /// has closed (a veto or a refused save keeps it, and the chooser
    /// does not show).
    open_after_close: bool,
    /// Create New waits for the open file to close.
    create_after_close: bool,
    /// The file Create New just made: when it opens, Manage Database opens
    /// on it, as in FileMaker Pro. Kept by path, so an open that was
    /// cancelled leaves no other file's open to take it.
    database_after_open: Option<std::path::PathBuf>,
    /// Why the open local file cannot be written, when it cannot.
    local_read_only: Option<String>,
}

impl OpenMakerApp {
    /// Create the app around a data provider. The provider moves to a
    /// background thread; the UI never calls it directly.
    pub fn new(provider: Box<dyn DataProvider>) -> Self {
        Self::with_license_store(provider, Box::<crate::licensing::OsLicenseStore>::default())
    }

    /// Supply a secure store, for embedding the client or isolated UI tests.
    pub fn with_license_store(
        provider: Box<dyn DataProvider>,
        store: Box<dyn crate::licensing::LicenseStore>,
    ) -> Self {
        Self::with_print_dispatcher(provider, store, crate::printing::default_dispatcher())
    }

    pub(crate) fn with_print_dispatcher(
        provider: Box<dyn DataProvider>,
        store: Box<dyn crate::licensing::LicenseStore>,
        dispatcher: std::sync::Arc<dyn crate::printing::Dispatcher>,
    ) -> Self {
        let sample_data = provider.is_sample();
        let client_license = provider.supports_client_license();
        Self {
            worker: Worker::with_print_dispatcher(provider, store, dispatcher),
            epoch: 0,
            form: ConnectForm::default(),
            hosts: HostsView::default(),
            status: ConnectStatus::Idle,
            opening: None,
            listing: connect::Listing::None,
            listings: 0,
            w: windows::Doc::new(windows::FIRST),
            windows: Vec::new(),
            presentation: None,
            closed_windows: Default::default(),
            window_files: std::collections::HashMap::from([(windows::FIRST, 0)]),
            window_list: Vec::new(),
            manage_layouts: None,
            new_layout: None,
            layout_setup: None,
            value_list_edit: None,
            part_setup: None,
            error: None,
            worker_stopped: false,
            closing: None,
            quit_ready: false,
            quit_waiting: None,
            title: APP_TITLE.to_owned(),
            styled: false,
            sample_data,
            database: None,
            security: None,
            security_requests: 0,
            value_lists: None,
            value_list_requests: 0,
            script: scripts::ScriptUi::default(),
            workspace: None,
            workspace_requests: 0,
            workspace_followups: Vec::new(),
            client_license,
            license_dialog: None,
            settings: None,
            settings_path: crate::settings::default_path(),
            web: Default::default(),
            native_parent: Err(crate::web_viewer::Unavailable::Window),
            viewport_windows: Default::default(),
            open_on_launch: false,
            reconnect: None,
            open_file: None,
            create_new: None,
            local_login: None,
            change_password: None,
            own_password: None,
            related_sign_ins: Default::default(),
            related_asked: Default::default(),
            related_login: None,
            open_after_close: false,
            create_after_close: false,
            database_after_open: None,
            local_read_only: None,
        }
    }

    /// Prefill the native Hosts form from `OPENMAKER_*` variables.
    pub(crate) fn with_connection_defaults(mut self) -> Self {
        if !self.sample_data {
            self.form = ConnectForm::from_env();
        }
        self
    }

    /// Save Settings in `path` rather than the OS account's settings file
    /// (embedding hosts, isolated tests).
    #[must_use]
    pub fn with_settings_file(mut self, path: std::path::PathBuf) -> Self {
        self.settings_path = Some(path);
        self
    }

    /// Open `connection`'s file when the app first draws, as the Hosts
    /// dialog's Open does (embedding hosts, scripted launches, tests).
    #[must_use]
    pub fn open_on_launch(mut self, connection: om_data::Connection) -> Self {
        self.form = ConnectForm {
            host: connection.host,
            port: connection.port.to_string(),
            account: connection.account,
            password: connection.password,
            file: connection.file,
        };
        self.open_on_launch = true;
        self
    }

    /// Start in the given view.
    #[must_use]
    pub fn with_view(mut self, view: ViewMode) -> Self {
        self.w.view = view;
        self
    }

    /// The current view.
    pub fn view(&self) -> ViewMode {
        self.w.view
    }

    /// The current mode.
    pub fn mode(&self) -> Mode {
        self.w.mode
    }

    /// Whether a file is open.
    pub fn is_open(&self) -> bool {
        self.w.session.is_some()
    }

    /// Why the open file's connection to the host was lost, if it was.
    pub fn connection_lost(&self) -> Option<&str> {
        self.w.connection_lost.as_deref()
    }

    /// The error shown in the alert, if any (a script's error alert
    /// included).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref().or_else(|| self.script.alert_text())
    }

    /// Whether a script is running (paused included).
    pub fn script_running(&self) -> bool {
        self.script.running
    }

    /// The window's record edit, as the removed "Unsaved record changes"
    /// bar showed it (#871): `Some(false)` while the record has changes
    /// not committed, `Some(true)` while a save of it is in flight, `None`
    /// otherwise, and while the lost-connection banner or the recovery bar
    /// (Keep / Discard My Changes) shows instead. For tests and agents: the
    /// window shows no bar for it, as FileMaker Pro shows none.
    pub fn record_edit_state(&self) -> Option<bool> {
        let recovering = self
            .w
            .recovery
            .as_ref()
            .is_some_and(recovery::Recovery::shows);
        (self.w.session.is_some()
            && self.w.connection_lost.is_none()
            && !recovering
            && (self.record_modified() || self.w.document.pending))
            .then_some(self.w.document.pending)
    }

    /// Whether the window has settled: a file is open, its layout and
    /// records have loaded, the data its calculations, portals and their
    /// rows' records read is in (`Calculations::awaiting_data`), no portal
    /// row click waits for its row (`DocumentState::awaiting_portal`), and
    /// no script runs or is about to (the layout's OnLayoutEnter).
    pub fn is_settled(&self) -> bool {
        self.w.session.as_ref().is_some_and(|s| {
            // A layout the account may not see loads nothing: its gray
            // page is complete at once (#696).
            s.layout.is_some_and(|id| !s.shows_layout(id)) && s.pending.is_none()
                || !matches!(s.render, crate::session::RenderState::Loading)
                    && s.page.is_some()
                    && s.pending.is_none()
        }) && !self.script.running
            && self.w.pending_enter.is_none()
            && !self.w.reconnected
            // A window whose connection is gone reads nothing: it waits
            // for no data.
            && (self.w.connection_lost.is_some() || !self.w.calcs.awaiting_data())
            && !self.w.document.awaiting_portal()
            && !self.w.document.preparing_edit()
    }

    /// Sends a change of the layout's parts from this window, holding its
    /// canvas until the host answers and the layout is read again
    /// (`LayoutWorkspace::hold_parts`).
    fn send_parts(&mut self, ctx: &egui::Context, sent: PartsSent) {
        self.w.layout_workspace.hold_parts(sent.clone());
        let request = match sent {
            PartsSent::Parts(change) => Request::SetLayoutParts(change),
            PartsSent::Options(change) => Request::SetPartOptions(change),
        };
        self.request(ctx, request);
    }

    /// After the host answers a change Part Setup or a part boundary sent:
    /// without a connection, this window's Part Setup closes, since no
    /// answer comes; refused because the layout changed meanwhile (306), the
    /// dialog that sent it closes, as its parts and revision are stale (it
    /// opens again on the layout as read now). Every window of the file
    /// reads the layout again when it may have changed.
    fn part_change_answered(
        &mut self,
        ctx: &egui::Context,
        answered: bool,
        result: Result<om_data::layout_objects::FramedObjects, DataError>,
    ) {
        let window = self.w.id;
        let stale = result.as_ref().is_err_and(|err| {
            err.lost || (answered && err.code == Some(om_data::layout_placement::CONFLICT))
        });
        if stale {
            self.part_setup = self.part_setup.take().filter(|d| d.window != window);
        }
        if let Err(err) = &result {
            self.fail(err);
        }
        if layout_change_done(&result) {
            self.reread_file_layouts(ctx);
        }
    }

    /// Whether Layout mode holds changes not saved yet, or sent with Save
    /// Layout and not answered.
    pub fn layout_unsaved(&self) -> bool {
        self.w.layout_workspace.has_changes() || self.w.layout_workspace.saving
    }

    /// The document window's title (`Get ( WindowName )`).
    pub fn window_name(&self) -> String {
        self.window_title()
    }

    /// Whether a script is paused.
    pub fn script_paused(&self) -> bool {
        self.script.paused.is_some()
    }

    /// What started each script the window sent to run, oldest first: a
    /// Scripts menu script (`Script 12`), a button (`Button Perform
    /// Script`), a trigger (`LayoutEnter 7`: its event and script id); for
    /// tests and measures that run a file's scripts and triggers.
    pub fn scripts_started(&self) -> &[String] {
        &self.script.started
    }

    /// What a script shows and waits on (`alert`, `custom dialog`,
    /// `confirmation`, ...), if anything; for tests and measures that run a
    /// file's scripts.
    pub fn script_prompt(&self) -> Option<&'static str> {
        self.script.prompt_kind()
    }

    /// Draw one frame. [`eframe::App::ui`] calls this; it is public so the app
    /// can be hosted elsewhere (tests, other shells).
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.styled {
            theme::install(&ctx);
            // Layout zoom leaves menus/toolbars at the application's scale.
            ctx.options_mut(|options| options.zoom_with_keyboard = false);
            // Keys go to the keystroke triggers before egui sees them.
            ctx.add_plugin(scripts::KeyGate::default());
            self.styled = true;
        }
        if std::mem::take(&mut self.open_on_launch) {
            self.apply(&ctx, Action::Connect);
        }
        self.poll_worker(&ctx);
        self.web_viewer_outcomes(&ctx);
        self.tick_timer(&ctx);
        self.abort_keys(&ctx);
        if self.open_after_close && self.closing.is_none() {
            self.open_after_close = false;
            if self.w.session.is_none() {
                self.open_file = Some(crate::open_file::OpenFileDialog::new());
            }
        }
        if self.create_after_close && self.closing.is_none() {
            self.create_after_close = false;
            if self.w.session.is_none() {
                self.create_new = Some(crate::create_new::CreateNewDialog::new());
            }
        }
        // The root viewport shows the file's first window; each other
        // window has a viewport of its own (windows.rs). Embedded
        // viewports (a backend without native windows) share the root's
        // input, which goes to the front window only.
        let embedded = ctx.embed_viewports();
        let front = self.w.id;
        self.window_list = self
            .window_ids()
            .into_iter()
            .filter_map(|id| {
                self.with_window(id, |app| {
                    (id, app.window_title(), app.window_name_for_get())
                })
            })
            .collect();
        self.quit_after_closes_done(&ctx);
        // The root window's close box quits: every window of the file
        // closes first, as Quit does, and a veto or a refused save keeps
        // the app open.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_ready {
            if self.quit_waiting.is_some() {
                // Quit already waits for the files to close.
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            } else if self.w.session.is_some() {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                if !self.script.busy() && self.closing.is_none() {
                    self.apply(&ctx, Action::Quit);
                }
            }
        }
        let root = self.root_window();
        // A window's cards are drawn on it, in its viewport
        // (window_styles.rs). Web viewers are children of the viewport's
        // native window: eframe gives out the root's, and the others' are
        // found as they are shown (`ViewportWindows`).
        let parent = self.native_parent;
        self.viewport_windows
            .begin(parent, !embedded && self.web.hosts_pages());
        self.draw_viewport(ui, root, embedded, front, parent);
        self.note_idle_activity(ui.ctx());
        self.with_window(root, |app| app.sync_title(&ctx));
        for id in self.window_ids() {
            if id == root || self.is_card(id) {
                continue;
            }
            let (title, dialog) = self
                .with_window(id, |app| (app.window_title(), app.w.style.is_dialog()))
                .unwrap_or_default();
            let mut builder = egui::ViewportBuilder::default()
                .with_title(title.clone())
                .with_inner_size([960.0, 680.0]);
            if dialog {
                // A dialog stays in front of the other windows, and cannot
                // be minimized.
                builder = builder
                    .with_window_level(egui::WindowLevel::AlwaysOnTop)
                    .with_minimize_button(false);
            }
            let mut close = false;
            self.viewport_windows.before_show();
            ctx.show_viewport_immediate(windows::viewport(id), builder, |ui, class| {
                let embedded = class == egui::ViewportClass::EmbeddedWindow;
                close = ui.input(|i| i.viewport().close_requested());
                let parent = if embedded {
                    Err(crate::web_viewer::Unavailable::Window)
                } else {
                    self.viewport_windows.parent(id, &title)
                };
                self.draw_viewport(ui, id, embedded, front, parent);
                self.note_idle_activity(ui.ctx());
            });
            if close {
                // The window's own close box closes it as the user's
                // action: OnWindowClose, then its record's commit (with
                // OnRecordCommit); the window stays until both let it go
                // (a veto or a refused save keeps it open). A window an
                // open dialog blocks stays; one with a card closes the card
                // first (`apply`).
                ctx.send_viewport_cmd_to(windows::viewport(id), egui::ViewportCommand::CancelClose);
                if let Some(dialog) = self.blocking_dialog(id) {
                    ctx.send_viewport_cmd_to(self.viewport_of(dialog), ViewportCommand::Focus);
                    continue;
                }
                self.with_window(id, |app| {
                    if !app.script.busy() {
                        app.apply(&ctx, Action::CloseWindow);
                    }
                });
            }
        }
        self.present_window(&ctx);
        // After every window has noted its input: a key pressed in another
        // window this frame is activity, not a script's start.
        self.tick_idle(&ctx);
        self.retain_web_viewers();
        let open = self.window_ids();
        self.viewport_windows.retain(|id| open.contains(&id));
        self.web_viewer_outcomes(&ctx);
    }

    /// The window's menu bar at the top of `ui`. The Window menu lists no
    /// cards (Claris: "FileMaker Pro does not include cards in the Window
    /// menu").
    /// Edit > `command` in the field the user is typing in. Cut, Copy and
    /// Paste go to its editor through the platform's clipboard, as its keys
    /// do; the others change its text or selection here.
    fn edit_field(&mut self, ctx: &egui::Context, command: EditCommand) {
        use crate::document::FieldEdit;
        let edit = match command {
            EditCommand::Cut => return ctx.send_viewport_cmd(ViewportCommand::RequestCut),
            EditCommand::Copy => return ctx.send_viewport_cmd(ViewportCommand::RequestCopy),
            EditCommand::Paste => return ctx.send_viewport_cmd(ViewportCommand::RequestPaste),
            EditCommand::Undo => FieldEdit::Undo,
            EditCommand::Redo => FieldEdit::Redo,
            EditCommand::Clear => FieldEdit::Clear,
            EditCommand::SelectAll => FieldEdit::SelectAll,
        };
        let Some((layout, record)) = self.current_layout_record() else {
            return;
        };
        if let Some(refusal) = self.w.document.edit_field(ctx, layout, record, edit) {
            self.apply(ctx, Action::RefuseTyping(refusal));
        }
    }

    /// The window's layout and current record.
    fn current_layout_record(&self) -> Option<(u64, i64)> {
        let session = self.w.session.as_ref()?;
        Some((u64::from(session.layout?), session.current_record()?.1.id))
    }

    /// The menu commands chosen this frame (`chosen`) while a field is
    /// entered: one other than the field's own (Edit's, and Insert's into
    /// the field) ends the field's editing first, its validation, save and
    /// exit triggers running, and runs once they let it go
    /// (`WindowState::menu_exit`), as a keyboard commit does. A click on the
    /// menus leaves the field entered (`draw_window_with`), so the field's
    /// own commands act on it.
    fn menu_leaves_field(&mut self, ui: &Ui, chosen: &[Action]) {
        let own = |action: &Action| {
            matches!(
                action,
                Action::EditField(_)
                    | Action::Step(
                        steps::INSERT_CURRENT_DATE
                            | steps::INSERT_CURRENT_TIME
                            | steps::INSERT_CURRENT_USER_NAME
                            | steps::INSERT_FROM_INDEX
                            | steps::INSERT_FROM_LAST_VISITED,
                        _
                    )
            )
        };
        let Some(action) = chosen.iter().find(|action| !own(action)) else {
            return;
        };
        if self.w.document.active.is_none() {
            return;
        }
        ui.memory_mut(|memory| {
            if let Some(editor) = memory.focused() {
                memory.surrender_focus(editor);
            }
        });
        self.w.menu_exit = Some(action.clone());
    }

    /// A text field of the current record is entered for typing in Browse,
    /// where Insert's Current Date, Current Time, Current User Name, From
    /// Index and From Last Visited Record put their text.
    fn text_field_entered(&self) -> bool {
        self.w.mode == Mode::Browse
            && self.w.connection_lost.is_none()
            && !self.w.document.pending
            && self.w.document.active.is_some()
            && self
                .w
                .session
                .as_ref()
                .is_none_or(|session| self.w.document.selected_container(session).is_none())
    }

    pub(super) fn menu_bar(&self, ui: &mut Ui, input: bool, actions: &mut Vec<Action>) -> bool {
        let window_title = self.window_title();
        let container = input
            && !self.script.busy()
            && self.script.prompt.is_none()
            && self.w.mode == Mode::Browse
            && self.w.connection_lost.is_none()
            && !self.w.document.pending
            && self
                .w
                .session
                .as_ref()
                .is_some_and(|session| self.w.document.selected_container(session).is_some());
        let windows: Vec<_> = self
            .window_list
            .iter()
            .filter(|(id, _, _)| !self.is_card(*id))
            .cloned()
            .collect();
        menubar::show(
            ui,
            &MenuContext {
                mode: self.w.mode,
                view: self.w.view,
                zoom: self.w.document.zoom.percent(),
                session: self.w.session.as_ref(),
                find: &self.w.find,
                toolbar: self.w.show_toolbar,
                formatting: self.w.show_formatting,
                inspector: self.w.inspector,
                objects: self.w.objects,
                window_title: &window_title,
                windows: &windows,
                window: self.w.id,
                scripts: &self.w.scripts,
                scripts_read: self.w.scripts_read == windows::ScriptsRead::Read,
                set_access: self
                    .w
                    .calcs
                    .schema()
                    .and_then(|schema| schema.authenticated_account.set_access.as_ref()),
                record_create: self.w.record_capabilities.create && !self.w.document.pending,
                record_delete: self.delete_enabled(),
                // Held edits wait for Keep or Discard My Changes, which
                // refuses every other command until then.
                revert_record: input
                    && !self.script.busy()
                    && self.script.prompt.is_none()
                    && !self.recovery_holds()
                    && om_data::scripts::revert_record::available(
                        self.w.mode.runtime(),
                        if self.w.mode == Mode::Find {
                            self.w
                                .find
                                .current()
                                .criteria
                                .iter()
                                .any(|(_, text)| !text.is_empty())
                        } else {
                            self.record_modified()
                        },
                        self.w.document.pending,
                    ),
                design: self.design_allowed(),
                export_records: input
                    && !self.script.busy()
                    && self.script.prompt.is_none()
                    && self.can_export_records()
                    && self.exports(),
                print: self.prints(),
                menus: self.menu_commands(),
                save_records: &self.save_records_enabled(input),
                import_records: input
                    && !self.script.busy()
                    && self.script.prompt.is_none()
                    && self.can_import_records(),
                container,
                // A related container's row was checked as it was selected.
                container_insert: container
                    && (self.w.record_capabilities.update
                        || self.w.session.as_ref().is_some_and(|session| {
                            self.w
                                .document
                                .selected_container(session)
                                .is_some_and(|selection| selection.row.is_some())
                        })),
                text_field: input
                    && !self.script.busy()
                    && self.script.prompt.is_none()
                    && self.text_field_entered(),
                own_field: self.w.document.active_related.is_none(),
                field_undo: self
                    .current_layout_record()
                    .filter(|_| self.text_field_entered())
                    .map_or((false, false), |(layout, record)| {
                        self.w.document.field_undo(ui.ctx(), layout, record)
                    }),
                record_copy: input
                    && !self.script.busy()
                    && self.script.prompt.is_none()
                    && self.w.mode == Mode::Browse
                    && self.w.connection_lost.is_none()
                    && self.w.document.active.is_none()
                    && !container
                    && self.current_layout_record().is_some(),
                client_license: self.client_license,
                layout_edit: self.w.mode == Mode::Layout
                    && self.w.layout_workspace.has_changes()
                    && !self.w.layout_workspace.saving,
                layout_delete: self.w.mode == Mode::Layout
                    && self.design_allowed()
                    && self.w.session.as_ref().is_some_and(|session| {
                        !self.w.layout_workspace.saving
                            && self.w.layout_workspace.selection_count(session) > 0
                    }),
            },
            actions,
        )
    }

    /// Draw the window the app works on in `ui`. `input`: the window takes
    /// the keyboard; `front`: it shows the app's dialogs; `parent`: the
    /// native window of its viewport, when known (web viewers); `sharing`:
    /// for a window that shares its viewport or waits behind another
    /// (window_styles.rs), whether its menu bar shows, the menu bar's
    /// actions, and whether it is covered.
    /// Adjust Window [Resize to Fit] that ran before this window's content
    /// area (or a bar the step showed) was measured, now that it is
    /// (#1115): the size the step would have given it (`Placement::fit`),
    /// with the layout, zoom and bars as the step saw them, in this
    /// window's viewport. A bar the step showed that still was not drawn
    /// waits while the script runs (its window update can be held back, by
    /// Freeze Window); after that the window fits with the bars as drawn.
    fn fit_window(&mut self, ctx: &egui::Context) {
        let Some(fit) = self.w.fit else {
            return;
        };
        if self.w.style.card().is_some() {
            self.w.fit = None;
            return;
        }
        let Some(mut placement) = self.w.placement.clone() else {
            return;
        };
        let size = placement
            .fit(fit.toolbar, fit.menubar, fit.zoom, fit.layout)
            .or_else(|| {
                (!self.script.busy())
                    .then(|| {
                        placement.fit(placement.toolbar, placement.menubar, fit.zoom, fit.layout)
                    })
                    .flatten()
            });
        let Some((width, height)) = size else {
            return;
        };
        self.w.fit = None;
        let (chrome_w, chrome_h) = placement.chrome();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            (width - chrome_w).max(1.0) as f32,
            (height - chrome_h).max(1.0) as f32,
        )));
        placement.move_resize(None, None, Some(width), Some(height));
        self.w.placement = Some(placement);
    }

    fn draw_window_with(
        &mut self,
        ui: &mut Ui,
        input: bool,
        front: bool,
        parent: Result<crate::web_viewer::NativeParent, crate::web_viewer::Unavailable>,
        sharing: window_styles::Sharing,
    ) {
        let window_styles::Sharing {
            menu,
            covered,
            mut actions,
        } = sharing;
        if self.w.hidden {
            self.w.keys = Default::default();
            self.w.document.drop_portal_click();
            self.w.document.drop_option_change();
            ui.painter()
                .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, theme::PASTEBOARD);
            self.w.document.web_viewers.clear();
            self.reconcile_web_viewers(ui.ctx(), parent);
            return;
        }
        self.web_viewer_note(parent);

        let ctx = ui.ctx().clone();
        let drawn = self.w.id;
        // A window whose save may be partly applied edits nothing (#1006),
        // from the frame it learns so, not once the provider's capabilities
        // say so: no typing is taken that settling the save would drop.
        self.w.document.capabilities = if self.w.unsettled.is_some() || self.w.unrecreated.is_some()
        {
            om_data::RecordCapabilities::default()
        } else {
            self.w.record_capabilities.clone()
        };
        // A portal row's focus a step left on another record or layout
        // has nothing to land on any more.
        let here =
            self.w.session.as_ref().and_then(|session| {
                Some((u64::from(session.layout?), session.current_record()?.1.id))
            });
        if self
            .w
            .document
            .portal_focus
            .zip(here)
            .is_some_and(|(focus, here)| (focus.layout, focus.parent) != here)
        {
            self.w.document.portal_focus = None;
        }
        // A portal row selected on another record or layout is not
        // selected any more, also when the window comes back to it.
        if self.selected_portal_row().is_none() {
            self.w.document.selected_row = None;
        }
        // Related records the record deletes with its next commit leave
        // their portals now; what it keeps uncommitted of the others shows.
        let open = self.uncommitted().cloned().unwrap_or_default();
        self.w.document.deleted_related = open
            .related
            .iter()
            .filter(|d| d.deleted)
            .filter_map(|d| Some((d.table, d.record?)))
            .collect();
        self.w.document.related_drafts = open
            .related
            .iter()
            .filter(|d| !d.deleted)
            .filter_map(|d| Some((d.table, d.record?, d.cells.clone())))
            .collect();
        let mut begun: std::collections::BTreeMap<u64, Vec<_>> = Default::default();
        // A begun row shows what was typed over what its rules entered,
        // once an ordinary commit's OnObjectSave previewed them (#1047).
        for d in open.related.iter().filter(|d| d.record.is_none()) {
            if let Some(id) = d.begun {
                begun
                    .entry(d.occurrence)
                    .or_default()
                    .push((id, d.table, d.previewed_cells()));
            }
        }
        self.w.document.begun_rows = begun;
        self.take_keystrokes(&ctx);
        let scroll_context = self.scroll_context();
        let zoom = self.w.document.zoom.percent();
        self.w.document.scroll.context(scroll_context, zoom);
        self.w
            .document
            .keep_container(self.w.session.as_ref(), self.w.mode == Mode::Browse);
        self.w.document.begin_frame();
        if covered {
            // A card over it or a dialog in front: its web viewers' native
            // views, which take input of their own, hide.
            self.w.document.overlays.push(egui::Rect::EVERYTHING);
        }
        // A portal click or option set change that waited goes on only
        // when no script runs and the record's page is in
        // (portal_clicks.rs, document.rs).
        self.w.document.clicks_held = self.script.busy()
            || self.script.prompt.is_some()
            || self.w.session.as_ref().is_some_and(|s| s.pending.is_some());
        // Tab moves the document's focus only while its keyboard is its own:
        // no dialog, alert, card, script or prompt over it.
        self.w.document.keys_held =
            !input || covered || self.script.busy() || self.dialog_over_document();

        if input && !self.script.busy() && self.script.prompt.is_none() {
            self.shortcuts(ui, &mut actions);
        }

        self.w.menu_drawn = menu && self.w.menubar;
        let mut on_menus = false;
        if self.w.menu_drawn {
            let top = ui.available_rect_before_wrap().top();
            let before = actions.len();
            on_menus = self.menu_bar(ui, input, &mut actions);
            self.menu_leaves_field(ui, &actions[before..]);
            self.w.menu_height = ui.available_rect_before_wrap().top() - top;
        }
        if front {
            self.pause_bar(ui);
        }

        if self.sample_data {
            egui::Panel::top("sample_data_notice")
                .exact_size(22.0)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Sample data - no server connection").color(theme::CAPTION),
                    );
                });
        }
        if let Some(reason) = self.local_read_only.as_ref().filter(|_| {
            self.w
                .session
                .as_ref()
                .is_some_and(|s| s.target.local.is_some())
        }) {
            egui::Panel::top("read_only_notice")
                .exact_size(22.0)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "This file is read-only: {reason}. Changes cannot be saved."
                        ))
                        .color(theme::CAPTION),
                    );
                });
        }
        if self.w.session.is_some() && self.w.connection_lost.is_some() {
            self.lost_banner(ui, &mut actions);
        } else if self.w.session.is_some() && self.w.unsettled.is_some() {
            self.unsettled_bar(ui, &mut actions);
        } else if self.w.session.is_some() && self.w.unrecreated.is_some() {
            self.unrecreated_bar(ui, &mut actions);
        } else if self.w.session.is_some()
            && self
                .w
                .recovery
                .as_ref()
                .is_some_and(recovery::Recovery::shows)
        {
            self.recovery_bar(ui, &mut actions);
        }
        if self.w.document.failed_edit() {
            egui::Panel::top("refused_entry_notice").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Field entry was refused. Discard that input before trying again.");
                    if ui.button("Discard Input").clicked() {
                        actions.push(Action::DiscardEntryCandidates);
                    }
                });
            });
        }
        // A record being edited takes no bar: FileMaker Pro shows none, and
        // the layout keeps its place (#871). Clicking outside the fields,
        // Command-Enter, another record, mode or layout, and closing commit
        // it; Records > Revert Record... reverts it.
        // The Share menu's Save as and Print are File menu commands, which
        // Editing only and Minimum menus leave out.
        let save_records = if self.full_menus() {
            self.save_records_enabled(input)
        } else {
            Vec::new()
        };
        let (mut content_rect, mut bars) = (None, None);
        match &self.w.session {
            Some(session) => {
                let bars_top = ui.available_rect_before_wrap().top();
                if self.w.show_toolbar {
                    toolbar::show(
                        ui,
                        &ToolbarContext {
                            mode: self.w.mode,
                            session,
                            find: &self.w.find,
                            tool: self.w.tool,
                            inspector: self.w.inspector,
                            objects: self.w.objects,
                            record_create: self.w.record_capabilities.create
                                && !self.w.document.pending,
                            record_delete: self.delete_enabled(),
                            save_records: &save_records,
                            print: self.prints() && self.full_menus(),
                            usable: self.bars_usable(),
                            commands: self.full_menus(),
                            found_set: self.w.found_set,
                        },
                        &mut self.w.toolbar,
                        &mut actions,
                    );
                    layout_bar::show(
                        ui,
                        &LayoutBarContext {
                            mode: self.w.mode,
                            view: self.w.view,
                            session,
                            schema: self.w.calcs.schema().map(|schema| schema.as_ref()),
                            find: &self.w.find,
                            formatting: self.w.show_formatting,
                            design: self.design_allowed(),
                            layouts: self.bars_usable(),
                            modes: self.full_menus(),
                        },
                        &mut actions,
                    );
                }
                if self.w.show_toolbar {
                    bars = Some(ui.available_rect_before_wrap().top() - bars_top);
                }
                if self.w.show_formatting {
                    layout_bar::formatting_bar(ui);
                }
                let frame = egui::Frame::new().fill(theme::CONTENT_BG);
                let globals = self.w.globals.clone();
                let mut info = crate::calc::session_info(
                    session,
                    self.w.calcs.schema().map(|s| s.as_ref()),
                    &self.w.calcs.account,
                    self.window_mode(),
                );
                info.host_application_version = self.w.calcs.host_version.clone();
                info.privileges = self.w.calcs.privileges.clone();
                info.privilege_set_name = info
                    .privileges
                    .as_ref()
                    .map(|p| p.account_set_name.clone())
                    .unwrap_or_default();
                info.window = Some({
                    let mut window = self.window_placement(&ctx).window_info(
                        self.w.show_toolbar,
                        self.w.menubar,
                        self.w.document.zoom.percent(),
                        &self.w.uuid,
                    );
                    window.style = self.w.style.number();
                    window
                });
                // The window's name, and the file's windows front first
                // (`WindowNames`).
                info.window_name = self.window_name_for_get();
                // The window's view, as a script in this window reads it.
                info.layout_view_state = Some(match self.w.view {
                    ViewMode::Form => 0,
                    ViewMode::List => 1,
                    ViewMode::Table => 2,
                });
                // The focus, the record's open state and changed fields, the
                // find requests and the sort.
                self.window_get_state(
                    session,
                    self.w.calcs.schema().map(|s| s.as_ref()),
                    &mut info,
                );
                if self.window_list.len() > 1 {
                    info.windows = self
                        .window_list
                        .iter()
                        .map(|(_, _, name)| (info.file_name.clone(), name.clone()))
                        .collect();
                }
                let evaluator = crate::calc::Evaluator::new(
                    &self.w.calcs,
                    session,
                    &self.w.document,
                    info,
                    &globals,
                );
                // A loaded schema without an evaluator: the layout has no
                // table occurrence to evaluate in.
                self.w.document.calculations_unavailable = evaluator.is_none()
                    && (self.w.calcs.unavailable() || self.w.calcs.schema().is_some());
                let placeable = if self.w.mode == Mode::Layout {
                    placeable_fields(&self.w.calcs, session)
                } else {
                    Vec::new()
                };
                // A click on the menu bar or its menus leaves the field
                // entered, with its selection, as FileMaker Pro leaves it:
                // the Insert commands act on it. egui gives a text editor's
                // focus up on a click anywhere else, as the editor draws.
                let surrender = ctx.options(|o| o.input_options.surrender_focus_on);
                if on_menus {
                    ctx.options_mut(|o| {
                        o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never;
                    });
                }
                let shown =
                    egui::CentralPanel::default()
                        .frame(frame)
                        .show(ui, |ui| match self.w.mode {
                            Mode::Browse => content::browse(
                                ui,
                                session,
                                self.w.view,
                                &mut self.w.document,
                                evaluator.as_ref(),
                                &mut actions,
                            ),
                            Mode::Find => content::find(
                                ui,
                                session,
                                self.w.view,
                                &mut self.w.find,
                                &mut self.w.document,
                                (
                                    self.w.calcs.schema().map(|schema| schema.as_ref()),
                                    evaluator.as_ref(),
                                ),
                                &mut actions,
                            ),
                            Mode::Layout => content::layout(
                                ui,
                                session,
                                self.w.tool,
                                content::LayoutPanels {
                                    inspector: self.w.inspector,
                                    objects: self.w.objects,
                                },
                                (
                                    &placeable,
                                    self.w.calcs.schema().map(|schema| schema.as_ref()),
                                ),
                                &mut self.w.layout_workspace,
                                &mut self.w.document.scroll,
                            ),
                            Mode::Preview => crate::preview::show(
                                ui,
                                session,
                                evaluator.as_ref(),
                                &self.w.document.panels,
                                self.w.document.zoom,
                                &mut self.w.document.scroll,
                                &mut actions,
                            ),
                        });
                if self.w.mode == Mode::Layout
                    && self.w.layout_workspace.take_field_finished()
                    && !self.w.layout_workspace.field_tool_locked
                {
                    // Completing a single-use placement keeps its new draft
                    // selected for binding; an explicit tool click clears it.
                    self.w.tool = LayoutTool::Selection;
                }
                ctx.options_mut(|o| o.input_options.surrender_focus_on = surrender);
                content_rect = Some(shown.response.rect);
            }
            None if self.open_file.is_some()
                || self.create_new.is_some()
                || self.local_login.is_some() =>
            {
                // File > Open's dialogs stand in for the Hosts dialog.
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PASTEBOARD))
                    .show(ui, |_| {});
            }
            None => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ui, |ui| {
                        connect::hosts_dialog(
                            ui,
                            &mut self.form,
                            &mut self.hosts,
                            self.status,
                            self.client_license,
                            &self.listing,
                            &mut actions,
                        );
                    });
                self.forget_other_hosts();
            }
        }
        if content_rect.is_some() {
            self.w.content_rect = content_rect;
        }
        if let Some(bars) = bars {
            self.w.bars = bars;
        }
        // Where this window is on the desktop, as its viewport reports it.
        if self.w.session.is_some() {
            self.w.placement = Some(self.window_placement(&ctx));
        }
        if self.w.fit.is_some() && content_rect.is_some() {
            self.fit_window(&ctx);
        }

        if front {
            self.dialogs(&ctx, &mut actions);
        }
        if self.w.delete_portal_row.is_some() {
            // FileMaker Pro's text is not known (as for Delete Portal Row's
            // dialog, script-runtime.md).
            egui::Modal::new(Id::unique(("delete_portal_row", self.w.id))).show(&ctx, |ui| {
                ui.heading("Delete this related record?");
                ui.label("The record is deleted from its table when this record is saved.");
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        actions.push(Action::CancelDeletePortalRow);
                    }
                    if ui.button("Delete").clicked() {
                        actions.push(Action::ConfirmDeletePortalRow);
                    }
                });
            });
        }
        if self.w.delete_layout.is_some() {
            // FileMaker Pro's alert, word for word.
            match crate::script_workspace::alert(
                &ctx,
                "Permanently delete this layout?",
                &["Cancel", "Delete"],
            ) {
                Some(0) => actions.push(Action::CancelDeleteLayout),
                Some(_) => actions.push(Action::ConfirmDeleteLayout),
                None => {}
            }
        }
        if self.w.delete_record {
            // FileMaker Pro's alert, local or hosted (client-ui.md,
            // "Delete confirmations"): its message and no more.
            egui::Modal::new(Id::unique(("delete_record", self.w.id))).show(&ctx, |ui| {
                ui.heading(DELETE_RECORD_MESSAGE);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        actions.push(Action::CancelDeleteRecord);
                    }
                    if ui.button("Delete").clicked() {
                        actions.push(Action::ConfirmDeleteRecord);
                    }
                });
            });
        }
        // Everything over the document is drawn: the window's web views
        // follow what Browse painted, hidden where something covers them.
        self.reconcile_web_viewers(&ctx, parent);

        // Field focus and edits, for object triggers.
        if self.w.session.is_some() {
            let existing = self.scroll_context().mode == om_data::runtime::Mode::Browse
                && !self
                    .w
                    .uncommitted
                    .as_ref()
                    .is_some_and(|(_, _, open)| open.new);
            let requests = self.w.document.prepare_candidates(existing);
            for (token, request) in requests {
                self.request(&ctx, Request::PrepareRecordEdit(token, request));
            }
        }
        let (mut left, mut entered, mut modified) = self.w.document.end_frame();
        self.w.document.hold_candidate_actions(
            &mut left,
            &mut entered,
            &mut modified,
            &mut actions,
        );
        // Related records the user edited belong to the record before any
        // action or trigger of this frame looks at it.
        self.sync_related_edits();
        // A first edit and focus change can share a frame. Retain before
        // validation, save, exit or enter triggers can send the draft.
        self.retain_drafts(&ctx);
        // A menu command waits for the field it left only in this frame.
        let menu_exit = self.w.menu_exit.take();
        if left.is_some() || entered.is_some() {
            let was_modified = left.as_ref().is_some_and(|(f, _)| {
                modified.iter().any(|(m, _)| m == f)
                    || match self.w.document.left_related {
                        // Edited, and not saved since (#1040).
                        Some(related) => {
                            self.w.document.related_field_edited(&related)
                                && !self.related_saved(&related)
                        }
                        None => self.w.document.edited(f),
                    }
            });
            // The click that left the field: a menu command's, a button's,
            // else the layout background's, which commits the record
            // (#1066).
            let button = menu_exit
                .or_else(|| {
                    actions
                        .iter()
                        .find(|action| matches!(action, Action::RunButton { .. }))
                        .cloned()
                })
                .or_else(|| {
                    actions
                        .iter()
                        .find(|action| matches!(action, Action::CommitRecord))
                        .cloned()
                });
            let held = button.as_ref().map(std::mem::discriminant);
            if self.focus_changed(&ctx, left, entered, was_modified, button) {
                // This click began the exit chain; its held continuation
                // runs only after all exit triggers approve. Later input
                // while a script is already running remains discarded.
                actions.retain(|action| Some(std::mem::discriminant(action)) != held);
            }
        }
        for (field, object) in modified {
            self.field_modified(&ctx, &field, object);
        }
        // A portal click waiting for its row goes once the user does
        // anything else in the window (portal_clicks.rs).
        if actions
            .iter()
            .any(|action| !matches!(action, Action::LoadRecordRange(_)))
        {
            self.w.document.drop_portal_click();
        }

        if self.script.busy() || self.script.prompt.is_some() {
            // A running script owns the window; only quitting and the
            // window's own controls go on.
            actions.retain(|a| matches!(a, Action::DismissAlert | Action::Quit | Action::Minimize));
        }
        if self.w.style.card().is_some() {
            // A card has no status toolbar or formatting bar to show.
            actions.retain(|a| !matches!(a, Action::ToggleToolbar | Action::ToggleFormattingBar));
        }
        // What this frame painted asked for the context it painted (#987).
        let painted = (self.w.id, self.w.calcs.reads_for(self.w.read_context()));
        for action in actions {
            self.apply(&ctx, action);
        }
        // Edits begun this frame answer to the record as the window showed
        // it; those given up release it (recovery.rs).
        self.retain_drafts(&ctx);
        // The reads of a layout, view or found set the window has left stop
        // (#987). Needs the frame painted before an action left its context
        // go with that context's token, now cancelled: they are answered
        // superseded and the next frame asks for what the window shows.
        let current = self.w.calcs.reads_for(self.w.read_context());
        let reads = if painted.0 == self.w.id {
            painted.1
        } else {
            current
        };
        // A field's list is built on the worker with the same reads
        // (#745), stopped with them. Nothing goes to a host whose
        // connection is gone (`request`): a list is asked for again once it
        // is back, not waited for meanwhile.
        let builds = self.w.calcs.take_list_builds();
        if self.w.connection_lost.is_some() {
            self.w.calcs.unsent_builds(&builds);
        }
        for build in builds {
            let cancel = reads.clone();
            let build = Box::new(build);
            self.request(&ctx, Request::BuildValueList { build, cancel });
        }
        let needs = self.w.calcs.take_wanted();
        if !needs.is_empty() {
            if self.w.connection_lost.is_some() {
                // Nothing goes to a host whose connection is gone
                // (`request`): what was found missing is asked for once
                // it is back, not counted as on its way.
                self.w.calcs.unsent(needs);
            } else {
                self.request(
                    &ctx,
                    Request::CalcData {
                        needs,
                        cancel: reads,
                    },
                );
            }
        }
        // A frame that sent nothing and waits for nothing drew only fresh
        // results: needs that failed or wait for an unreachable host are
        // not sent, and keep no result from before the last change.
        self.w.calcs.settle_last_results();
        // What waits for a related file's unreachable host is asked again
        // then (#740): draw again at that time.
        if let Some(at) = self.w.calcs.retry_at() {
            ctx.request_repaint_after(at.saturating_duration_since(std::time::Instant::now()));
        }
        // A list a control opened asks the host now, never from paint.
        let (epoch, wants) = self.w.calcs.take_list_wants();
        for (list, table, field) in wants {
            let request = Request::HostFieldValues {
                list,
                table,
                field,
                epoch,
            };
            self.request(&ctx, request);
        }
        self.w.calcs.end_list_frame();
        // An action may have brought another window to the front: that
        // one settles its keys when it is drawn.
        if self.w.id == drawn {
            self.settle_keystrokes(&ctx, input);
        }
    }

    /// The app's dialogs, over the front window.
    fn dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let ctx = ctx.clone();
        if let (Some(dialog), Some(session)) = (&mut self.manage_layouts, &self.w.session) {
            // Associated Table: each layout's table occurrence, by name.
            let tables: std::collections::BTreeMap<u32, String> =
                self.w
                    .calcs
                    .schema()
                    .map(|schema| {
                        schema
                            .layouts()
                            .iter()
                            .filter_map(|layout| {
                                // Another file's occurrence keeps its name when
                                // that file is not open.
                                let id = layout.occurrence?;
                                let name = schema.occurrence(id).map(|o| o.name.clone()).or_else(
                                    || schema.external_occurrence(id).map(|o| o.name.clone()),
                                )?;
                                Some((layout.id, name))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            if let Some(result) = dialog.show(&ctx, session, &tables) {
                actions.push(Action::DismissManageLayouts);
                match result {
                    ManageLayoutsResult::Open(id) => actions.push(Action::SelectLayout(id)),
                    ManageLayoutsResult::New => actions.push(Action::NewLayout),
                    ManageLayoutsResult::Duplicate(id) => actions.push(Action::DuplicateLayout(id)),
                    ManageLayoutsResult::Delete(id) => actions.push(Action::DeleteLayout(id)),
                    ManageLayoutsResult::Cancel => {}
                }
            }
        }
        // Layout Setup's dialog is its window's and its layout's: it goes
        // when another window comes to the front, the window leaves Layout
        // mode or shows another layout.
        if self.layout_setup.as_ref().is_some_and(|dialog| {
            dialog.window != self.w.id
                || self.w.mode != Mode::Layout
                || self.w.session.as_ref().and_then(|s| s.layout) != Some(dialog.layout)
        }) {
            self.layout_setup = None;
        }
        // A parts change held until the layout is read again.
        self.w
            .layout_workspace
            .release_parts(self.w.session.as_ref());
        // Part Setup closes as Layout Setup does.
        if self.part_setup.as_ref().is_some_and(|dialog| {
            dialog.window != self.w.id
                || self.w.mode != Mode::Layout
                || self.w.session.as_ref().and_then(|s| s.layout) != Some(dialog.layout)
        }) {
            self.part_setup = None;
        }
        // A part added or deleted goes to the host once the dialog is drawn.
        let mut parts_change = None;
        if let Some(dialog) = &mut self.part_setup {
            match dialog.show(&ctx) {
                PartSetupAnswer::Open => {}
                PartSetupAnswer::Done => self.part_setup = None,
                // A window whose connection is gone sends nothing, so the
                // dialog would wait for an answer that never comes.
                PartSetupAnswer::Change(_) | PartSetupAnswer::Parts(_)
                    if self.w.connection_lost.is_some() =>
                {
                    self.part_setup = None;
                }
                PartSetupAnswer::Change(change) => actions.push(Action::SetPartOptions(change)),
                PartSetupAnswer::Parts(change) => parts_change = Some(change),
            }
        }
        if let Some(change) = parts_change {
            self.send_parts(&ctx, PartsSent::Parts(change));
        }
        if let Some(dialog) = &mut self.layout_setup {
            match dialog.show(&ctx) {
                LayoutSetupAnswer::Open => {}
                LayoutSetupAnswer::Cancel => self.layout_setup = None,
                LayoutSetupAnswer::Ok(change) => {
                    self.layout_setup = None;
                    actions.push(Action::SetUpLayout(change));
                }
            }
        }
        // Edit Value List is its window's: it closes when another window
        // comes to the front or the file closes.
        if self
            .value_list_edit
            .as_ref()
            .is_some_and(|(window, _)| *window != self.w.id || self.w.session.is_none())
        {
            self.value_list_edit = None;
        }
        if let Some((_, dialog)) = &mut self.value_list_edit {
            use crate::document::value_dialogs::{self, EditAnswer};
            match value_dialogs::edit(&ctx, dialog) {
                None => {}
                Some(EditAnswer::Cancel) => self.value_list_edit = None,
                // A window whose connection is gone sends nothing.
                Some(EditAnswer::Ok(_)) if self.w.connection_lost.is_some() => {
                    self.value_list_edit = None;
                }
                Some(EditAnswer::Ok(values)) => {
                    dialog.pending = true;
                    actions.push(Action::SetValueListValues(
                        om_data::value_lists::ValuesChange {
                            list: dialog.list,
                            expected: dialog.revision,
                            values,
                        },
                    ));
                }
            }
        }
        // A window of another file came to the front: the dialog was its
        // file's.
        if self
            .new_layout
            .as_ref()
            .is_some_and(|dialog| dialog.file != self.w.file_id || self.w.session.is_none())
        {
            self.new_layout = None;
        }
        if let Some(dialog) = &mut self.new_layout {
            let occurrences: Vec<(u64, String)> = self
                .w
                .calcs
                .schema()
                .map(|schema| {
                    schema
                        .occurrences_in_order()
                        .into_iter()
                        .map(|o| (o.id, o.name.clone()))
                        .collect()
                })
                .unwrap_or_default();
            match dialog.show(&ctx, &occurrences) {
                NewLayoutAnswer::Open => {}
                NewLayoutAnswer::Cancel => self.new_layout = None,
                NewLayoutAnswer::Finish(request) => {
                    self.new_layout = None;
                    actions.push(Action::CreateLayout(request));
                }
            }
        }
        if self.w.mode == Mode::Layout {
            self.w
                .layout_workspace
                .show_button_setup(&ctx, self.w.calcs.schema().cloned());
            let own = self
                .w
                .session
                .as_ref()
                .and_then(crate::calc::layout_occurrence);
            self.w
                .layout_workspace
                .show_portal_setup(&ctx, self.w.calcs.schema().cloned(), own);
            // A part boundary dragged with the Selection tool goes to the
            // host at once, as Part Setup's changes do.
            if let Some(resize) = self.w.layout_workspace.take_part_resize() {
                let layout = self.w.session.as_ref().and_then(|s| s.layout);
                if let (Some(layout), Some(expected), None, false) = (
                    layout,
                    resize.revision,
                    &self.w.connection_lost,
                    self.w.layout_workspace.saving,
                ) {
                    let change = om_data::layout_placement::PartsChange {
                        layout,
                        expected,
                        change: om_data::layout_placement::PartChange::Resize {
                            part: resize.part,
                            part_type: resize.part_type,
                            size: resize.size,
                        },
                    };
                    self.send_parts(&ctx, PartsSent::Parts(change));
                }
            }
        }
        actions.append(&mut self.workspace_followups);
        // Only the windows of the workspace's file show it.
        let file = self.w.file_id;
        if let Some(workspace) = self.workspace.as_mut().filter(|w| w.owns(file)) {
            workspace.set_names(self.w.calcs.schema().cloned());
            let workspace_actions = workspace.show(&ctx);
            self.workspace_actions(&ctx, workspace_actions, actions);
        }
        if let Some(database) = &mut self.database {
            database.set_names(self.w.calcs.schema().cloned());
            if let Some(action) = database.show(&ctx) {
                match action {
                    crate::database::DatabaseAction::Cancel => {
                        self.request(&ctx, Request::CancelSchema);
                        actions.push(Action::DismissDatabase);
                    }
                    crate::database::DatabaseAction::Save => {
                        self.request(&ctx, Request::CommitSchema)
                    }
                    crate::database::DatabaseAction::Apply(change) => {
                        self.request(&ctx, Request::ChangeSchema(change))
                    }
                }
            }
        }
        if let Some(security) = self.security.as_mut().filter(|s| s.owns(self.w.file_id)) {
            match security.show(&ctx) {
                Some(crate::security::SecurityAction::Close) => {
                    actions.push(Action::DismissSecurity)
                }
                Some(crate::security::SecurityAction::Reload) => {
                    self.request_security_catalog(&ctx)
                }
                None => {}
            }
        }
        if let Some(dialog) = self.value_lists.as_mut().filter(|d| d.owns(self.w.file_id)) {
            if dialog.show(&ctx) {
                actions.push(Action::DismissValueLists);
            }
        }
        if let Some(dialog) = &mut self.license_dialog {
            use crate::licensing::{LicenseAction, LicenseKey};
            if let Some(action) = dialog.show(&ctx) {
                let request = match action {
                    LicenseAction::Save => {
                        dialog.pending = true;
                        Some(Request::SaveLicense(LicenseKey::new(
                            dialog.input.trim().to_owned(),
                        )))
                    }
                    LicenseAction::Remove => {
                        dialog.pending = true;
                        Some(Request::RemoveLicense)
                    }
                    LicenseAction::Close => {
                        self.license_dialog = None;
                        None
                    }
                };
                if let Some(request) = request {
                    self.request(&ctx, request);
                }
            }
        }
        if let Some(dialog) = &mut self.settings {
            use crate::settings::SettingsAction;
            match dialog.show(&ctx) {
                Some(SettingsAction::Save(setting)) => {
                    match crate::settings::set_user_name(self.settings_path.as_deref(), setting) {
                        Ok(()) => self.settings = None,
                        Err(err) => dialog.error = Some(err.to_string()),
                    }
                }
                Some(SettingsAction::Cancel) => self.settings = None,
                None => {}
            }
        }
        self.reconnect_dialog(&ctx);
        self.local_dialogs(&ctx, actions);
        self.change_password_dialog(&ctx);
        self.own_password_dialog(&ctx);
        self.related_sign_in(&ctx);
        self.alert(&ctx, actions);
        self.prompt(&ctx);
    }

    /// File > Open's chooser and a local file's account dialog.
    fn local_dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use crate::open_file::{ChooserAction, LoginAction};
        if let Some(dialog) = &mut self.open_file {
            match dialog.show(ctx) {
                Some(ChooserAction::Open(path)) => actions.push(Action::OpenLocal(path)),
                Some(ChooserAction::Cancel) => self.open_file = None,
                None => {}
            }
        }
        if let Some(dialog) = &mut self.create_new {
            use crate::create_new::CreateAction;
            match dialog.show(ctx) {
                // The create_file operation, then the new file opens as
                // its Admin with no password, as FileMaker Pro opens it.
                Some(CreateAction::Create(path)) => match om_data::new_file::create(&path) {
                    Ok(created) => {
                        self.create_new = None;
                        self.database_after_open = Some(created.path.clone());
                        let file = om_data::LocalFile {
                            path: created.path,
                            credentials: Some((
                                created.account.to_owned(),
                                zeroize::Zeroizing::new(String::new()),
                            )),
                        };
                        self.open_local(ctx, file);
                    }
                    Err(e) => dialog.error = Some(e.to_string()),
                },
                Some(CreateAction::Cancel) => self.create_new = None,
                None => {}
            }
        }
        let Some(dialog) = &mut self.local_login else {
            return;
        };
        match dialog.show(ctx) {
            Some(LoginAction::Open) if !dialog.pending => {
                dialog.pending = true;
                dialog.error = None;
                let file = om_data::LocalFile {
                    path: dialog.path.clone(),
                    credentials: Some((
                        dialog.account.trim().to_owned(),
                        zeroize::Zeroizing::new(dialog.password.to_string()),
                    )),
                };
                use zeroize::Zeroize;
                dialog.password.zeroize();
                self.open_local(ctx, file);
            }
            Some(LoginAction::Cancel) => {
                // The file waits in the provider for an account: it closes.
                self.local_login = None;
                if self.status == ConnectStatus::Connecting {
                    self.close_file(ctx);
                } else {
                    self.epoch += 1;
                    let window = self.w.id;
                    if let Err(err) = self.worker.send(ctx, self.epoch, window, Request::Close) {
                        self.fail(&err);
                    }
                }
            }
            _ => {}
        }
    }

    /// Ask for an account for a related file that refused this window's
    /// file's account (#214) once the window's layout needs it: a field or
    /// portal of an occurrence of the file's tables. FileMaker Pro asks
    /// then, not at open (reported by a FileMaker Pro 26 run on a local
    /// pair); one dialog at a time, each file once a session.
    fn raise_related_sign_in(&mut self) {
        if let Some(login) = &self.related_login {
            let window = login.window;
            if self.w.id == window || self.windows.iter().any(|d| d.id == window) {
                return;
            }
            // Its window closed: the file stays unavailable.
            self.related_login = None;
        }
        let file = self.w.file_id;
        let Some(pending) = self.related_sign_ins.get_mut(&file) else {
            return;
        };
        let Some(crate::session::RenderState::Loaded(render)) =
            self.w.session.as_ref().map(|s| &s.render)
        else {
            return;
        };
        let used: std::collections::BTreeSet<u64> = render
            .layout
            .objects
            .objects
            .iter()
            .filter_map(|object| {
                object
                    .field_spec()
                    .map(|spec| spec.table_key)
                    .or_else(|| object.portal.as_ref().map(|p| p.table_alias_key))
            })
            .filter_map(om_layout::occurrence_id)
            .collect();
        let Some(index) = pending
            .iter()
            .position(|s| !s.occurrences.is_disjoint(&used))
        else {
            return;
        };
        let sign_in = pending.remove(index);
        self.related_asked.insert((file, sign_in.file.clone()));
        // FileMaker Pro fills the user's name first (#214's FileMaker Pro run).
        let account = om_data::host::machine::user_name();
        self.related_login = Some(RelatedLogin {
            window: self.w.id,
            dialog: crate::open_file::LoginDialog::related(&sign_in.file, account),
            file: sign_in.file,
        });
    }

    /// A related file's account dialog (`raise_related_sign_in`): Sign in
    /// sends one Login with the account typed; Cancel leaves the file
    /// unavailable for the session.
    fn related_sign_in(&mut self, ctx: &egui::Context) {
        use crate::open_file::LoginAction;
        self.raise_related_sign_in();
        let window = self.w.id;
        let Some(RelatedLogin { file, dialog, .. }) =
            self.related_login.as_mut().filter(|l| l.window == window)
        else {
            return;
        };
        match dialog.show(ctx) {
            Some(LoginAction::Open) if !dialog.pending => {
                dialog.pending = true;
                dialog.error = None;
                let job = worker::SignInJob {
                    file: file.clone(),
                    account: dialog.account.trim().to_owned(),
                    password: zeroize::Zeroizing::new(dialog.password.to_string()),
                };
                use zeroize::Zeroize;
                dialog.password.zeroize();
                self.request(ctx, Request::SignInRelated(Box::new(job)));
            }
            Some(LoginAction::Cancel) => self.related_login = None,
            _ => {}
        }
    }

    /// The change-password dialog (#157): OK sends the new password once it
    /// and its confirmation match; Cancel closes the waiting file, as the
    /// sign-in dialog's Cancel does.
    fn change_password_dialog(&mut self, ctx: &egui::Context) {
        use crate::open_file::ChangeAction;
        let Some(dialog) = &mut self.change_password else {
            return;
        };
        match dialog.show(ctx) {
            Some(ChangeAction::Change) if !dialog.pending => {
                if *dialog.new != *dialog.confirm {
                    dialog.error =
                        Some("The new password and its confirmation do not match.".into());
                    return;
                }
                dialog.pending = true;
                dialog.error = None;
                let new = std::mem::take(&mut dialog.new);
                use zeroize::Zeroize;
                dialog.confirm.zeroize();
                let path = dialog.path.clone();
                self.error = None;
                self.epoch += 1;
                self.status = ConnectStatus::Connecting;
                self.opening = Some(Target {
                    host: String::new(),
                    file: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    local: Some(path),
                });
                self.request(
                    ctx,
                    Request::ChangeDuePassword(Box::new(worker::NewPassword(new))),
                );
            }
            Some(ChangeAction::Cancel) => {
                // The file waits in the provider for an account: it closes,
                // as after the sign-in dialog's Cancel.
                self.change_password = None;
                if self.status == ConnectStatus::Connecting {
                    self.close_file(ctx);
                } else {
                    self.epoch += 1;
                    let window = self.w.id;
                    if let Err(err) = self.worker.send(ctx, self.epoch, window, Request::Close) {
                        self.fail(&err);
                    }
                }
            }
            _ => {}
        }
    }

    /// File > Change Password…'s dialog (#830): OK sends the old and new
    /// passwords once the new one and its confirmation match; a refusal
    /// keeps the dialog up with its reason.
    fn own_password_dialog(&mut self, ctx: &egui::Context) {
        use crate::open_file::ChangeAction;
        let Some((window, file, _)) = &self.own_password else {
            return;
        };
        let (window, file) = (*window, *file);
        // Its window closed, or shows another file now: the change is not
        // sent to another file.
        let alive = |d: &windows::Doc| d.id == window && d.file_id == file && d.session.is_some();
        if !(alive(&self.w) || self.windows.iter().any(alive)) {
            self.own_password = None;
            return;
        }
        if self.w.id != window {
            return;
        }
        let Some((_, _, dialog)) = &mut self.own_password else {
            return;
        };
        match dialog.show(ctx) {
            Some(ChangeAction::Change) if !dialog.pending => {
                if *dialog.new != *dialog.confirm {
                    dialog.error =
                        Some("The new password and its confirmation do not match.".into());
                    return;
                }
                dialog.pending = true;
                dialog.error = None;
                let pair = worker::PasswordPair {
                    old: dialog.old.take().unwrap_or_default(),
                    new: std::mem::take(&mut dialog.new),
                };
                dialog.old = Some(Default::default());
                use zeroize::Zeroize;
                dialog.confirm.zeroize();
                self.request(ctx, Request::ChangePassword(Box::new(pair)));
            }
            Some(ChangeAction::Cancel) => self.own_password = None,
            _ => {}
        }
    }

    /// Open `file`, a file on this computer, in this window.
    fn open_local(&mut self, ctx: &egui::Context, file: om_data::LocalFile) {
        self.open_local_with(ctx, file, false);
    }

    /// [`Self::open_local`]; with `ask` and no credentials, asking for an
    /// account whatever File Options say.
    fn open_local_with(&mut self, ctx: &egui::Context, file: om_data::LocalFile, ask: bool) {
        self.error = None;
        self.epoch += 1;
        self.status = ConnectStatus::Connecting;
        let name = file
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.opening = Some(Target {
            host: String::new(),
            file: name,
            local: Some(file.path.clone()),
        });
        if ask && file.credentials.is_none() {
            self.request(ctx, Request::OpenLocalAsking(file.path));
        } else {
            self.request(ctx, Request::OpenLocal(file));
        }
    }

    /// The window's mode as `Get ( WindowMode )` reports it.
    fn window_mode(&self) -> om_data::host::WindowMode {
        match self.w.mode {
            Mode::Browse => om_data::host::WindowMode::Browse,
            Mode::Find => om_data::host::WindowMode::Find,
            Mode::Preview => om_data::host::WindowMode::Preview,
            Mode::Layout => om_data::host::WindowMode::Layout,
        }
    }

    /// A dialog or alert over the document has the keyboard.
    fn dialog_over_document(&self) -> bool {
        self.manage_layouts.is_some()
            || self.layout_setup.is_some()
            || self.part_setup.is_some()
            || self.w.layout_workspace.setting_up_button()
            || self.new_layout.is_some()
            || self.database.is_some()
            || self.security.is_some()
            || self.value_lists.is_some()
            || self.workspace.is_some()
            || self.license_dialog.is_some()
            || self.settings.is_some()
            || self.reconnect.is_some()
            || self.open_file.is_some()
            || self.create_new.is_some()
            || self.local_login.is_some()
            || self.own_password.is_some()
            || self.w.delete_record
            || self.w.delete_layout.is_some()
            || self.w.delete_portal_row.is_some()
            || self.error.is_some()
            || self.script.prompt.is_some()
            || self.w.document.other_dialog_open()
            || self.value_list_edit.is_some()
    }

    /// Whether the keypad's Enter goes to the next object rather than
    /// committing the record: the active field object's "Go to next object
    /// using" includes the Enter key (`FieldObject::next_object_on_enter`).
    /// That FileMaker Pro then goes on rather than commits is inferred from
    /// the option (layout-model.md 5.4.1), not measured.
    fn enter_goes_to_next_object(&self) -> bool {
        let (Some(_), Some(object), Some(session)) = (
            &self.w.document.active,
            self.w.document.active_object,
            &self.w.session,
        ) else {
            return false;
        };
        let crate::session::RenderState::Loaded(layout) = &session.render else {
            return false;
        };
        layout
            .layout
            .objects
            .get(object)
            .and_then(|object| object.field_obj.as_ref())
            .is_some_and(om_layout::FieldObject::next_object_on_enter)
    }

    fn shortcuts(&self, ui: &Ui, actions: &mut Vec<Action>) {
        let open = self.w.session.is_some();
        // Document shortcuts must not act through dialogs or while searching
        // their contents (e.g. Cmd+L must not change mode behind a modal).
        if self.dialog_over_document() {
            return;
        }
        let browse = self.w.mode == Mode::Browse;
        // Delete and Backspace go to a text field that has the keyboard.
        let typing = ui.ctx().egui_wants_keyboard_input();
        let shown_layout = self.w.session.as_ref().and_then(|session| session.layout);
        let object_selected = self.w.mode == Mode::Layout
            && self
                .w
                .session
                .as_ref()
                .is_some_and(|session| self.w.layout_workspace.selection_count(session) > 0);
        let mut commit = false;
        // Insert's shortcuts, into the field entered (the menu's own
        // availability: Editing only keeps them too).
        let inserts = self.text_field_entered()
            && self.menu_commands() != om_data::host::privilege_set::MenuCommands::Minimum;
        // Copy with no field entered or other widget typing: the record's
        // (Edit > Copy's availability).
        let record_copy = browse
            && !typing
            && self.w.document.active.is_none()
            && self.w.connection_lost.is_none()
            && self.menu_commands() != om_data::host::privilege_set::MenuCommands::Minimum
            && self.current_layout_record().is_some()
            && !self
                .w
                .session
                .as_ref()
                .is_some_and(|session| self.w.document.selected_container(session).is_some());
        // The keypad's Enter, unlike Return, commits the record or goes to
        // the next object (`keypad`) from a field of the record or with no
        // editor focused. In an open menu or list, or in a box of the
        // toolbar (Quick Find, the record number), it acts as Return.
        let keypad_enter = browse
            && !egui::Popup::is_any_open(ui.ctx())
            && (self.w.document.active.is_some() || !typing);
        let enter_goes_next = keypad_enter && self.enter_goes_to_next_object();
        // A commit key that came after typing in the same frame waited for
        // this one: the editor has applied the typing (below). It goes back
        // only in the next frame, and only to the file, mode, layout, record
        // and field it was typed in. Otherwise (a script ran or moved, a
        // dialog came up) it is dropped rather than commit what the window
        // shows by then.
        let held_id = egui::Id::unique("held_commit_keys").with(ui.ctx().viewport_id());
        let frame = ui.ctx().cumulative_frame_nr();
        let typed_in = HeldKeysOwner {
            file: self.w.file_id,
            mode: self.w.mode,
            record: self
                .w
                .session
                .as_ref()
                .and_then(|session| Some((session.layout?, session.current_record()?.1.id))),
            field: self.w.document.active.clone(),
        };
        let held: Option<(u64, HeldKeysOwner, Vec<egui::Event>)> =
            ui.ctx().data_mut(|data| data.remove_temp(held_id));
        let mut hold = None;
        ui.input_mut(|input| {
            if let Some((_, _, held)) =
                held.filter(|(at, owner, _)| at + 1 == frame && *owner == typed_in)
            {
                input.events.splice(0..0, held);
            }
            if input.consume_shortcut(&shortcuts::QUIT) {
                actions.push(Action::Quit);
            }
            if input.consume_shortcut(&shortcuts::SETTINGS) {
                actions.push(Action::ShowSettings);
            }
            if input.consume_shortcut(&shortcuts::OPEN) {
                actions.push(Action::ShowOpenFile);
            }
            if input.consume_shortcut(&shortcuts::MINIMIZE) {
                actions.push(Action::Minimize);
            }
            if !open {
                return;
            }
            if input.consume_shortcut(&shortcuts::CLOSE) {
                actions.push(Action::CloseFile);
            }
            if input.consume_shortcut(&shortcuts::PRINT)
                && matches!(self.w.mode, Mode::Browse | Mode::Preview)
                && self.prints()
            {
                actions.push(Action::Step(43, 0));
            }
            for mode in Mode::ALL {
                if input.consume_shortcut(&mode.shortcut()) && mode != self.w.mode {
                    actions.push(Action::SetMode(mode));
                }
            }
            if input.consume_shortcut(&shortcuts::REFRESH) {
                actions.push(Action::Refresh);
            }
            if input.consume_shortcut(&KeyboardShortcut::new(
                Modifiers::COMMAND.plus(Modifiers::SHIFT),
                Key::D,
            )) {
                actions.push(Action::ManageDatabase);
            }
            if input.consume_shortcut(&KeyboardShortcut::new(
                Modifiers::COMMAND.plus(Modifiers::SHIFT),
                Key::S,
            )) {
                actions.push(Action::ScriptWorkspace);
            }
            if self.w.mode == Mode::Layout
                && self.w.layout_workspace.has_changes()
                && input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S))
            {
                actions.push(Action::SaveLayout);
            }
            if self.w.mode == Mode::Layout
                && input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::N))
            {
                actions.push(Action::NewLayout);
            }
            if let Some(layout) = shown_layout.filter(|_| self.w.mode == Mode::Layout) {
                if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::E)) {
                    actions.push(Action::DeleteLayout(layout));
                }
            }
            if object_selected
                && !typing
                && (input.consume_key(Modifiers::NONE, Key::Delete)
                    || input.consume_key(Modifiers::NONE, Key::Backspace))
            {
                actions.push(Action::DeleteLayoutObject);
            }
            if inserts {
                // Before New Record: egui's match of Command-N ignores Shift.
                for (modifiers, key, step) in [
                    (Modifiers::COMMAND, Key::Minus, steps::INSERT_CURRENT_DATE),
                    (
                        Modifiers::COMMAND,
                        Key::Semicolon,
                        steps::INSERT_CURRENT_TIME,
                    ),
                    (
                        Modifiers::COMMAND.plus(Modifiers::SHIFT),
                        Key::N,
                        steps::INSERT_CURRENT_USER_NAME,
                    ),
                    (Modifiers::COMMAND, Key::I, steps::INSERT_FROM_INDEX),
                    (
                        Modifiers::COMMAND,
                        Key::Quote,
                        steps::INSERT_FROM_LAST_VISITED,
                    ),
                ] {
                    if input.consume_shortcut(&KeyboardShortcut::new(modifiers, key)) {
                        actions.push(Action::Step(step, 0));
                    }
                }
            }
            // With no field entered, Copy copies the record, as Edit > Copy
            // does then (`menubar::edit_menu`).
            if record_copy {
                let copies = input.events.len();
                input
                    .events
                    .retain(|event| !matches!(event, egui::Event::Copy));
                if input.events.len() != copies {
                    actions.push(Action::Step(steps::COPY_RECORD, 0));
                }
            }
            if self.w.mode == Mode::Browse {
                // A field editor applies its keys as it draws, after this,
                // and only while it has the focus, which a keyboard commit
                // takes from it before it draws (below). So a commit key
                // after typing in the same frame waits for the next frame,
                // with the keys after it, and the editor applies the typing
                // first.
                let commit_key = |event: &egui::Event| {
                    matches!(
                        event,
                        egui::Event::Key {
                            key: Key::Enter,
                            pressed: true,
                            modifiers,
                            ..
                        } if modifiers.command || modifiers.mac_cmd
                    ) || (keypad_enter && !enter_goes_next && crate::keypad::is_enter_press(event))
                };
                if let Some(at) = input.events.iter().position(commit_key) {
                    if typing && input.events[..at].iter().any(edits_text) {
                        hold = Some(input.events.split_off(at));
                    }
                }
                if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::N)) {
                    actions.push(Action::NewRecord);
                }
                if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::D)) {
                    actions.push(Action::DuplicateRecord);
                }
                if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::E)) {
                    actions.push(Action::DeleteRecord);
                }
                if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter)) {
                    actions.push(Action::CommitRecord);
                    commit = true;
                }
                let at = input.events.iter().position(crate::keypad::is_enter_press);
                if let Some(at) = at.filter(|_| keypad_enter) {
                    if enter_goes_next {
                        // As a Tab: the document moves the focus.
                        input.events[at] = egui::Event::Key {
                            key: Key::Tab,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Modifiers::NONE,
                        };
                    } else {
                        input
                            .events
                            .retain(|event| !crate::keypad::is_enter_press(event));
                        if !commit {
                            actions.push(Action::CommitRecord);
                            commit = true;
                        }
                    }
                }
            }
            if input.consume_shortcut(&shortcuts::STATUS_TOOLBAR) {
                actions.push(Action::ToggleToolbar);
            }
            if (browse && self.w.view == ViewMode::Form) || self.w.mode == Mode::Preview {
                if input.consume_shortcut(&shortcuts::ZOOM_IN)
                    || input.consume_shortcut(&shortcuts::ZOOM_IN_ALTERNATE)
                {
                    actions.push(Action::ZoomIn);
                }
                if input.consume_shortcut(&shortcuts::ZOOM_OUT) {
                    actions.push(Action::ZoomOut);
                }
            }
            if browse
                && input.consume_shortcut(&KeyboardShortcut::new(
                    Modifiers::COMMAND.plus(Modifiers::SHIFT),
                    Key::T,
                ))
            {
                actions.push(Action::Step(steps::OMIT_MULTIPLE, 0));
            }
            if browse && !input.modifiers.alt && !input.modifiers.shift {
                for (key, step) in [
                    (Key::J, steps::SHOW_ALL),
                    (Key::T, steps::OMIT_RECORD),
                    (Key::R, steps::MODIFY_LAST_FIND),
                    (Key::S, steps::SORT_RECORDS),
                ] {
                    if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, key)) {
                        actions.push(Action::Step(step, 0));
                    }
                }
            }
            // Going to the previous or next record is Records > Go to
            // Record, which Editing only and Minimum keep (#137's and
            // #651's readings).
            let previous = input.consume_shortcut(&shortcuts::PREVIOUS);
            let next = input.consume_shortcut(&shortcuts::NEXT);
            if self.w.mode == Mode::Find {
                if input.consume_shortcut(&shortcuts::NEW_REQUEST) {
                    actions.push(Action::NewRequest);
                }
                if input.consume_shortcut(&shortcuts::DELETE_REQUEST) {
                    actions.push(Action::DeleteRequest);
                }
                let current = self.w.find.current_index();
                if previous && current > 0 {
                    actions.push(Action::GoToRequest(current - 1));
                }
                if next {
                    actions.push(Action::GoToRequest(current + 1));
                }
            } else if self.w.mode == Mode::Preview {
                if let Some((current, count)) =
                    self.w.session.as_ref().and_then(Session::preview_position)
                {
                    if previous && current > 0 {
                        actions.push(Action::GoToPage(current - 1));
                    }
                    if next && current + 1 < count {
                        actions.push(Action::GoToPage(current + 1));
                    }
                }
            } else if let Some(session) = &self.w.session {
                if previous && session.current > 0 {
                    actions.push(Action::GoToRecord(session.current - 1));
                }
                if next {
                    actions.push(Action::GoToRecord(session.current + 1));
                }
            }
        });
        if let Some(hold) = hold {
            ui.ctx()
                .data_mut(|data| data.insert_temp(held_id, (frame, typed_in, hold)));
            ui.ctx().request_repaint();
        }
        // Keyboard Commit leaves the field (script-runtime.md 7.1):
        // surrender its editor before drawing so the existing focus-change
        // path holds Commit for Validate/Save/Exit, as for a background
        // click. A veto restores that field; the held record/window guards
        // prevent committing a different record after a trigger navigates.
        if commit && self.w.document.active.is_some() {
            ui.memory_mut(|memory| {
                if let Some(editor) = memory.focused() {
                    memory.surrender_focus(editor);
                }
            });
        }
    }

    /// The Script Workspace's actions once a save it waited for came back,
    /// in whichever window asked: they wait for the front window's next
    /// frame (`workspace_followups`).
    fn workspace_after_save(
        &mut self,
        ctx: &egui::Context,
        workspace_actions: Vec<crate::script_workspace::ScriptWorkspaceAction>,
    ) {
        let mut actions = Vec::new();
        self.workspace_actions(ctx, workspace_actions, &mut actions);
        self.workspace_followups.append(&mut actions);
    }

    /// Carry out what the Script Workspace asked for.
    fn workspace_actions(
        &mut self,
        ctx: &egui::Context,
        workspace_actions: Vec<crate::script_workspace::ScriptWorkspaceAction>,
        actions: &mut Vec<Action>,
    ) {
        use crate::script_workspace::ScriptWorkspaceAction;
        for action in workspace_actions {
            match action {
                ScriptWorkspaceAction::Close => actions.push(Action::DismissScriptWorkspace),
                ScriptWorkspaceAction::Save(script) => self
                    .request_workspace(ctx, |workspace| Request::SaveScript { workspace, script }),
                ScriptWorkspaceAction::SaveCatalog(catalog) => {
                    self.request_workspace(ctx, |workspace| Request::SaveScriptCatalog {
                        workspace,
                        catalog,
                    })
                }
                ScriptWorkspaceAction::Run(id) => actions.push(Action::RunScript(id)),
            }
        }
    }

    /// FileMaker titles a document window `file (host)`; Set Window Title
    /// replaces it.
    fn window_title(&self) -> String {
        if let (Some(title), Some(_)) = (&self.w.title, &self.w.session) {
            return title.clone();
        }
        match &self.w.session {
            Some(session) => {
                let file = &session.target.file;
                let stem = om_data::file_refs::file_stem(file);
                if self.sample_data {
                    format!("{stem} - Sample Data")
                } else if session.target.local.is_some() {
                    // A file on this computer has no host to name.
                    stem.to_owned()
                } else {
                    format!("{stem} ({})", session.target.host)
                }
            }
            None => APP_TITLE.to_owned(),
        }
    }

    /// A FileMaker-style alert for errors: the app glyph, a bold message
    /// and a blue OK button.
    fn alert(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(message) = self.error.clone() else {
            return;
        };
        let frame = egui::Frame::new()
            .fill(theme::DIALOG_BG)
            .stroke(egui::Stroke::new(1.0, Color32::from_gray(0xC4)))
            .corner_radius(CornerRadius::same(12))
            .shadow(ctx.global_style().visuals.window_shadow)
            .inner_margin(egui::Margin::same(16));
        egui::Modal::new(Id::unique("alert"))
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(30))
            .show(ctx, |ui| {
                ui.set_width(260.0);
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(52.0, 52.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(12), theme::DEFAULT_BUTTON);
                    icons::paint(
                        ui.painter(),
                        rect.shrink(12.0),
                        Icon::Data,
                        Color32::WHITE,
                        theme::DEFAULT_BUTTON,
                    );
                    ui.add_space(10.0);
                    // A message taller than the window scrolls, so the
                    // alert and its OK stay on screen (#981): the icon,
                    // the spacing, OK and the margins take 130 points,
                    // and 45 more stay free above and below.
                    let height = (ctx.content_rect().height() - 220.0).max(60.0);
                    // Its scroll state is the message's: another alert
                    // starts at its top.
                    egui::ScrollArea::vertical()
                        .id_salt(("alert message", &message))
                        .max_height(height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&message)
                                            .font(egui::FontId::new(theme::BODY, theme::semibold()))
                                            .color(theme::TEXT),
                                    )
                                    .wrap(),
                                );
                            });
                        });
                    ui.add_space(14.0);
                    let ok = widgets::dialog_button(ui, "OK", true, true);
                    // Alerts are modal: only OK, Return or Escape dismiss
                    // them, never a click outside.
                    let key = ui.input(|input| {
                        input.key_pressed(Key::Enter) || input.key_pressed(Key::Escape)
                    });
                    if ok.clicked() || key {
                        actions.push(Action::DismissAlert);
                    }
                });
            });
    }

    fn can_export_records(&self) -> bool {
        self.w.session.as_ref().is_some_and(|s| s.layout.is_some())
            && matches!(self.w.mode, Mode::Browse | Mode::Preview)
            && self.w.connection_lost.is_none()
            && !self.w.document.pending
    }

    /// Whether File > Import Records can run here: as Export Records, in
    /// Browse mode, for an account that may change some record. FileMaker
    /// Pro disables Import's source items under [Read-Only Access] and keeps
    /// them under [Data Entry Only] (#156's reading).
    fn can_import_records(&self) -> bool {
        self.can_export_records()
            && self.w.mode == Mode::Browse
            && self
                .w
                .calcs
                .schema()
                .and_then(|schema| schema.authenticated_account.set_access.as_ref())
                .is_none_or(om_data::host::security::SetAccess::edits_any_record)
    }

    /// The account's privilege set as the host enforces it.
    fn set_access(&self) -> Option<&om_data::host::security::SetAccess> {
        self.w
            .calcs
            .schema()
            .and_then(|schema| schema.authenticated_account.set_access.as_ref())
    }

    /// Whether the account may print (session-identity.md 2.3).
    fn prints(&self) -> bool {
        self.set_access()
            .is_none_or(om_data::host::security::SetAccess::prints)
    }

    /// The menu commands the account's set makes available
    /// (session-identity.md 2.3).
    fn menu_commands(&self) -> om_data::host::privilege_set::MenuCommands {
        self.set_access().map_or(
            om_data::host::privilege_set::MenuCommands::All,
            om_data::host::security::SetAccess::menus,
        )
    }

    fn full_menus(&self) -> bool {
        self.menu_commands() == om_data::host::privilege_set::MenuCommands::All
    }

    /// The status toolbar and the layout bar: usable under Editing only
    /// (FileMaker Pro 26.0.3, #137's reading), disabled under Minimum.
    /// Pro's menus keep layout and record navigation under Minimum, but of
    /// its toolbar only New, Delete Record, Find and Sort were read there
    /// (#651), so OpenMaker leaves both bars out [I].
    fn bars_usable(&self) -> bool {
        self.menu_commands() != om_data::host::privilege_set::MenuCommands::Minimum
    }

    /// The actions that are menu commands only (a menu item, its shortcut
    /// or a toolbar or layout bar button), which Editing only and Minimum
    /// menus leave out (FileMaker Pro Help, "Editing other privileges").
    /// Opening, closing and switching windows and files, running scripts,
    /// and what a layout object, a field or a dialog does stay; so do
    /// going to a record and committing it, which clicks on the layout do
    /// too, and performing a find, which Return in a request's field does
    /// (their toolbar, menu and shortcut are disabled instead).
    fn menu_command(menus: om_data::host::privilege_set::MenuCommands, action: &Action) -> bool {
        // Both keep layout navigation, the status toolbar and formatting
        // bar, zoom and Minimize Window (FileMaker Pro 26.0.3, #651's
        // reading).
        if matches!(
            action,
            Action::SelectLayout(_)
                | Action::ToggleToolbar
                | Action::ToggleFormattingBar
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::ActualSize
                | Action::Minimize
        ) {
            return false;
        }
        // Editing only also keeps Show All, Show Omitted Only and Refresh
        // Window (#137's reading lists what Pro disables) and Export Field
        // Contents (#651's), and going back to Browse from a mode a script
        // entered (Cancel Find, Exit Preview), which grants nothing. Its
        // Edit and Insert menus keep Copy (the record's, with no field
        // entered) and the Insert commands into the field entered, as its
        // menus offer them [I: inferred, as Edit's own commands are].
        let editing = matches!(
            action,
            Action::Refresh
                | Action::Step(
                    steps::SHOW_ALL
                        | steps::SHOW_OMITTED
                        | steps::EXPORT_FIELD_CONTENTS
                        | steps::COPY_RECORD
                        | steps::INSERT_CURRENT_DATE
                        | steps::INSERT_CURRENT_TIME
                        | steps::INSERT_CURRENT_USER_NAME
                        | steps::INSERT_FROM_INDEX
                        | steps::INSERT_FROM_LAST_VISITED,
                    _
                )
                | Action::SetMode(Mode::Browse)
        );
        if editing && menus == om_data::host::privilege_set::MenuCommands::EditingOnly {
            return false;
        }
        matches!(
            action,
            Action::NewRecord
                | Action::DuplicateRecord
                | Action::DeleteRecord
                | Action::ExportRecords
                | Action::SaveRecordsAs(_)
                | Action::ImportRecords
                | Action::Step(..)
                | Action::QuickFind(_)
                | Action::SetView(_)
                | Action::SetMode(_)
                | Action::NewRequest
                | Action::DuplicateRequest
                | Action::DeleteRequest
                | Action::Refresh
                | Action::ChangePassword
        ) || Self::changes_design(action)
    }

    /// Whether the account may export records (session-identity.md 2.3).
    fn exports(&self) -> bool {
        self.set_access()
            .is_none_or(om_data::host::security::SetAccess::exports)
    }

    /// Whether Save Records as `format` can run here: the command's own
    /// check, the account's privileges (PDF is printing, Excel exporting:
    /// FileMaker Pro Help, "Editing other privileges") and the window
    /// connected with no save in flight.
    fn can_save_records(&self, format: SaveFormat) -> bool {
        let allowed = if format == SaveFormat::Pdf {
            self.prints()
        } else {
            self.exports()
        };
        if !allowed {
            return false;
        }
        let layout = self.w.session.as_ref().is_some_and(|s| s.layout.is_some());
        SaveRecords::interactive(format)
            .check(self.w.mode.runtime(), layout)
            .is_ok()
            && self.w.connection_lost.is_none()
            && !self.w.document.pending
    }

    /// The Save Records as commands the menus and toolbars offer: those
    /// that can run, while the window takes `input` and no script or dialog
    /// is in the way.
    fn save_records_enabled(&self, input: bool) -> Vec<SaveFormat> {
        if !input || self.script.busy() || self.script.prompt.is_some() {
            return Vec::new();
        }
        SaveFormat::ALL
            .into_iter()
            .filter(|format| self.can_save_records(*format))
            .collect()
    }

    /// Whether `action` saves the window's record first (a navigation,
    /// closing, a replaced found set), and whether the record has changes
    /// to save.
    fn saves_first(&self, action: &Action) -> (bool, bool) {
        let navigate = matches!(
            *action,
            Action::SelectLayout(_)
                | Action::OpenNewLayout(_)
                | Action::GoToRecord(_)
                | Action::NewRecord
                | Action::DuplicateRecord
                | Action::RunButton { .. }
                | Action::ExportRecords
                | Action::SaveRecordsAs(_)
                | Action::ImportRecords
                | Action::Refresh
                | Action::CloseWindow
                // Quick Find replaces the found set: the record is saved
                // first, its OnRecordCommit able to keep it open.
                | Action::QuickFind(_)
                // The record's edits are kept by layout: deleting the
                // layout they were made on would strand them, so they are
                // saved before Delete Layout asks (OpenMaker's choice;
                // FileMaker Pro's is not measured).
                | Action::DeleteLayout(_)
                // FileMaker Pro 26.0.3 commits the record when Edit… is
                // chosen, before Edit Value List opens (#869): it opens
                // only once the save is done, never on a refused one.
                | Action::EditValueList(_)
        ) || matches!(*action, Action::SetMode(mode) if mode != self.w.mode && self.w.mode == Mode::Browse);
        // A button's script starts from what scripts left on the record
        // (it is no commit); the user's own typing is saved first.
        let modified = match action {
            Action::RunButton { .. } => self
                .w
                .session
                .as_ref()
                .is_some_and(|session| !self.w.document.changes(session).is_empty()),
            _ => self.record_modified(),
        };
        (navigate, modified)
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        // A window's card closes before it (Claris: closing a card's
        // parent first closes the card); closing the file or quitting
        // goes on to the parent once the card has closed.
        if action == Action::CloseWindow && self.w.session.is_some() {
            if let Some(card) = self.card_of(self.w.id) {
                self.with_window(card, |app| app.apply(ctx, Action::CloseWindow));
                return;
            }
        }
        if self.w.connection_lost.is_some() && !action.works_offline() {
            return;
        }
        if action == Action::ExportRecords && !(self.can_export_records() && self.exports()) {
            return;
        }
        // Printing the account's set denies does nothing either.
        if matches!(action, Action::Step(42 | 43, _)) && !self.prints() {
            return;
        }
        if let Action::SaveRecordsAs(format) = action {
            if !self.can_save_records(format) {
                return;
            }
        }
        if action == Action::ImportRecords && !self.can_import_records() {
            return;
        }
        // A disabled design command reached by its shortcut does nothing.
        if Self::changes_design(&action) && !self.design_allowed() {
            return;
        }
        // Nor does a menu command the account's Editing only or Minimum
        // menus leave out, reached by its shortcut or a toolbar.
        if !self.full_menus() && Self::menu_command(self.menu_commands(), &action) {
            return;
        }
        // Nor does a layout the account may not see (the menu leaves it out).
        if let Action::SelectLayout(id) = action {
            if self.w.session.as_ref().is_some_and(|s| !s.shows_layout(id)) {
                return;
            }
        }
        // Another layout, or another mode, would drop the layout's unsaved
        // changes: the user saves or reverts them first (OpenMaker's
        // choice; FileMaker Pro asks whether to save).
        let leaves_layout = match &action {
            Action::SelectLayout(id) => self.w.session.as_ref().and_then(|s| s.layout) != Some(*id),
            Action::SetMode(mode) => *mode != Mode::Layout,
            _ => false,
        };
        if leaves_layout && self.w.mode == Mode::Layout && self.w.layout_workspace.has_changes() {
            self.error = Some(
                "This layout's changes are not saved. Choose Layouts > Save Layout or Revert \
                 Layout first."
                    .into(),
            );
            return;
        }
        // Before any trigger: nothing may save or script the held edits.
        if !action.works_while_held() && self.recovery_holds() {
            self.error = Some(recovery::HELD.into());
            self.closing = None;
            return;
        }
        // A window whose save may be partly applied saves nothing until it
        // is settled, its bar says how (#1006): a save asked for does
        // nothing, and an action that saves the record first is refused
        // with the bar's reason, before any trigger.
        if (self.w.unsettled.is_some() || self.w.unrecreated.is_some())
            && self.w.connection_lost.is_none()
        {
            if action == Action::CommitRecord {
                return;
            }
            if let (true, true) = self.saves_first(&action) {
                self.error = Some(
                    if self.w.unrecreated.is_some() {
                        recovery::UNRECREATED
                    } else {
                        recovery::UNSETTLED
                    }
                    .into(),
                );
                self.closing = None;
                return;
            }
        }
        // The action's pre-event triggers already let it go on.
        let approved = self.script.bypass;
        if self.hold_for_trigger(ctx, &action) {
            return;
        }
        let (navigate, modified) = self.saves_first(&action);
        // On a lost connection the edits cannot be saved; the banner says
        // closing discards them.
        if navigate && modified && self.w.connection_lost.is_none() {
            // A navigation's OnRecordCommit ran with its other pre-event
            // triggers: the save it leads to does not run it again.
            self.script.bypass = approved && Self::commits_first(&action);
            // A portal click finds its row again after the save.
            self.w.deferred_action = Some(action.held());
            self.apply(ctx, Action::CommitRecord);
            self.script.bypass = false;
            return;
        }
        match action {
            Action::ShowSettings => {
                if self.settings.is_none() {
                    self.settings = Some(crate::settings::SettingsDialog::new());
                }
            }
            Action::ManageLicense => {
                if self.client_license && self.status == ConnectStatus::Idle {
                    self.license_dialog = Some(crate::licensing::LicenseDialog::default());
                    self.request(ctx, Request::LicenseStatus);
                }
            }
            Action::NewRecord | Action::DuplicateRecord => {
                if self.w.document.pending || !self.w.record_capabilities.create {
                    return;
                }
                if self.w.session.as_ref().and_then(|s| s.layout).is_none() {
                    return;
                }
                // The New Record and Duplicate Record steps' operation: the
                // record is held in the window's record session until it is
                // committed, and Revert releases it (#892).
                let step = match action {
                    Action::DuplicateRecord => steps::DUPLICATE_RECORD,
                    _ => steps::NEW_RECORD,
                };
                self.run_new_record_step(ctx, step);
            }
            Action::CommitRecord => {
                if self.w.document.pending {
                    return;
                }
                self.sync_related_edits();
                if self.uncommitted().is_some() {
                    // Scripts left changes on the record, or the user
                    // edited its related records: they are committed with
                    // the user's own changes, together.
                    self.save_on_worker(ctx, Start::Commit);
                    return;
                }
                if !self.w.record_capabilities.update {
                    return;
                }
                let Some(session) = &self.w.session else {
                    return;
                };
                let values = self.w.document.changes(session);
                if values.is_empty() {
                    return;
                }
                let Some((_, record)) = session.current_record() else {
                    return;
                };
                let (layout_id, record_id) = (session.layout.unwrap_or_default(), record.id);
                self.w.globals_only = self.only_globals(&values);
                self.w.document.pending = true;
                self.w.in_flight = Some(recovery::Write::Update {
                    layout: layout_id,
                    record: record_id,
                });
                self.request(
                    ctx,
                    Request::UpdateRecord {
                        layout_id,
                        record_id,
                        values,
                    },
                );
            }
            Action::RevertRecord => {
                if self.w.document.pending {
                    return;
                }
                if let Some(session) = &self.w.session {
                    self.w.document.revert(session);
                }
                self.release_current_record(ctx);
                self.w.deferred_action = None;
                self.w.document.cancel_pending_focus();
                if self.uncommitted().is_some() {
                    // What scripts left on the record goes too.
                    self.save_on_worker(ctx, Start::Revert);
                    return;
                }
                let key = self.w.session.as_mut().and_then(Session::refresh);
                self.request_page(ctx, key);
            }
            Action::DeleteRecord if self.selected_portal_row().is_some() => {
                // With a portal row selected, Delete Record deletes its
                // record, when the portal allows it (layout-model.md 5.3.2).
                if self.delete_enabled() {
                    self.w.delete_portal_row = self.selected_portal_row();
                }
            }
            Action::CancelDeletePortalRow => self.w.delete_portal_row = None,
            Action::ConfirmDeletePortalRow => {
                if let Some(row) = self.w.delete_portal_row.take() {
                    self.delete_portal_row(row);
                }
            }
            Action::DeleteRecord => {
                if !self.w.document.pending && self.w.record_capabilities.delete {
                    self.w.delete_record = self
                        .w
                        .session
                        .as_ref()
                        .and_then(Session::current_record)
                        .is_some();
                }
            }
            Action::CancelDeleteRecord => self.w.delete_record = false,
            Action::ConfirmDeleteRecord => {
                self.w.delete_record = false;
                if self.w.document.pending || !self.w.record_capabilities.delete {
                    return;
                }
                let Some(session) = &self.w.session else {
                    return;
                };
                let Some((_, record)) = session.current_record() else {
                    return;
                };
                let (layout_id, record_id) = (session.layout.unwrap_or_default(), record.id);
                // A new record the window's record session holds is not on
                // the host: the window's own Delete Record releases it (#892).
                if self.uncommitted().is_some_and(|open| open.session) {
                    self.run_step(
                        ctx,
                        steps::DELETE_RECORD,
                        om_data::scripts::flag::NO_INTERACT,
                    );
                    return;
                }
                self.w.document.pending = true;
                self.w.in_flight = Some(recovery::Write::Delete {
                    layout: layout_id,
                    record: record_id,
                });
                self.request(
                    ctx,
                    Request::DeleteRecord {
                        layout_id,
                        record_id,
                    },
                );
            }
            Action::RunButton {
                object_id,
                record_index,
                portal: None,
            } => {
                self.run_button(ctx, object_id, record_index, None);
            }
            Action::RunButton {
                object_id,
                record_index,
                portal: Some(click),
            } => self.portal_button(ctx, object_id, record_index, click),
            Action::RunScript(id) => self.start_script(ctx, Start::Script { id }),
            Action::ExportRecords => self.run_step(ctx, steps::EXPORT_RECORDS, 0),
            Action::SaveRecordsAs(format) => {
                self.run_command(ctx, SaveRecords::interactive(format).step())
            }
            Action::ImportRecords => self.run_step(ctx, steps::IMPORT_RECORDS, 0),
            Action::Step(id, flags) => self.run_step(ctx, id, flags),
            Action::EditField(command) => self.edit_field(ctx, command),
            Action::QuickFind(text) => self.quick_find(ctx, text),
            Action::ScriptWorkspace => {
                let file = self.w.file_id;
                if self.w.session.is_some()
                    && !self.workspace.as_ref().is_some_and(|w| w.owns(file))
                {
                    // Another file's workspace goes: one shows at a time.
                    self.workspace_followups.clear();
                    self.workspace = Some(
                        crate::script_workspace::ScriptWorkspace::new(self.window_title())
                            .owned_by(file),
                    );
                    self.request_workspace(ctx, |workspace| Request::Scripts {
                        workspace: Some(workspace),
                    });
                }
            }
            Action::DismissScriptWorkspace => self.workspace = None,
            Action::ReadScripts => {
                if self.w.session.is_some() && self.w.scripts_read == windows::ScriptsRead::Unread {
                    self.w.scripts_read = windows::ScriptsRead::Asked;
                    self.request(ctx, Request::Scripts { workspace: None });
                }
            }
            Action::ListFiles => match self.form.server() {
                Ok(_) if self.status != ConnectStatus::Idle => {}
                Ok((host, port)) => {
                    self.error = None;
                    self.listing.forget();
                    self.listings += 1;
                    let canceled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    self.listing = connect::Listing::Pending {
                        token: self.listings,
                        server: (host.clone(), port),
                        canceled: canceled.clone(),
                    };
                    self.status = ConnectStatus::ListingFiles;
                    // Apart from the open file, if any, and its requests.
                    let sent = self.worker.list_files(
                        ctx,
                        self.epoch,
                        self.w.id,
                        self.listings,
                        host,
                        port,
                        canceled,
                    );
                    if let Err(err) = sent {
                        self.listing.forget();
                        self.status = ConnectStatus::Idle;
                        self.error = Some(err.message);
                    }
                }
                Err(message) => self.error = Some(message),
            },
            Action::PickFile(name) => self.form.file = name,
            Action::Connect => {
                if self.status != ConnectStatus::Idle || self.w.session.is_some() {
                    return;
                }
                match self.form.connection() {
                    Ok(connection) => {
                        self.error = None;
                        self.epoch += 1;
                        self.status = ConnectStatus::Connecting;
                        self.opening = Some(Target {
                            host: connection.host.clone(),
                            file: connection.file.clone(),
                            local: None,
                        });
                        self.request(
                            ctx,
                            Request::Open {
                                connection,
                                use_saved_license: self.hosts.use_saved_license,
                            },
                        );
                    }
                    Err(message) => self.error = Some(message),
                }
            }
            Action::CloseFile if self.w.session.is_some() => {
                self.closing = Some(false);
                self.apply(ctx, Action::CloseWindow);
            }
            // Stops the listing; nothing goes to the provider.
            Action::CancelConnect if self.status == ConnectStatus::ListingFiles => {
                self.listing.cancel();
                self.status = ConnectStatus::Idle;
            }
            Action::CancelConnect | Action::CloseFile => self.close_file(ctx),
            Action::Reconnect => self.open_reconnect(),
            Action::KeepRecoveredEdits => self.keep_recovered_edits(),
            Action::CheckRecoveredEdits => self.check_recovered_edits(ctx),
            Action::DiscardRecoveredEdits => self.discard_recovered_edits(ctx),
            Action::DismissRecovery => self.dismiss_recovery(),
            Action::CheckUnsettledSave => self.settle_save(ctx, worker::Settle::Check),
            Action::ResumeUnsettledSave => self.settle_save(ctx, worker::Settle::Resume),
            Action::DiscardUnsettledSave => self.settle_save(ctx, worker::Settle::Discard),
            Action::RecreateRecord => self.recreate(ctx, true),
            Action::DiscardNewRecord => self.recreate(ctx, false),
            Action::ShowCreateNew => {
                if self.w.session.is_some() {
                    self.create_after_close = true;
                    self.apply(ctx, Action::CloseFile);
                } else if self.status == ConnectStatus::Idle && self.local_login.is_none() {
                    self.open_file = None;
                    self.create_new = Some(crate::create_new::CreateNewDialog::new());
                }
            }
            Action::ShowOpenFile => {
                if self.w.session.is_some() {
                    self.open_after_close = true;
                    self.apply(ctx, Action::CloseFile);
                } else if self.status == ConnectStatus::Idle && self.local_login.is_none() {
                    self.open_file = Some(crate::open_file::OpenFileDialog::new());
                }
            }
            Action::OpenLocal(path) => {
                self.open_file = None;
                if self.status == ConnectStatus::Idle && self.w.session.is_none() {
                    // Shift (or Option/Alt) held: ask for an account even
                    // when File Options sign in without asking, as
                    // FileMaker Pro does (local-files.md 3.1).
                    let ask = ctx.input(|i| i.modifiers.shift || i.modifiers.alt);
                    self.open_local_with(
                        ctx,
                        om_data::LocalFile {
                            path,
                            credentials: None,
                        },
                        ask,
                    );
                }
            }
            Action::CloseWindow => {
                if self.w.session.is_none() {
                    self.closing = None;
                } else if !self.windows.iter().any(|d| d.file_id == self.w.file_id) {
                    // The file's last window: the file closes.
                    let quit = self.closing == Some(true);
                    self.close_file(ctx);
                    if quit {
                        if self.w.session.is_some() {
                            self.closing = Some(true);
                            self.apply(ctx, Action::CloseWindow);
                        } else {
                            self.quit_after_closes(ctx);
                        }
                    }
                } else {
                    let returns = self.w.style.parent().is_some();
                    if let Some((window, request)) = self.w.found_set_to_keep() {
                        if let Err(err) = self.worker.send(ctx, self.epoch, window, request) {
                            self.fail(&err);
                        }
                    }
                    self.remove_window(self.w.id);
                    if returns {
                        // A card or dialog returns to its parent or opener.
                        let viewport = self.viewport_of(self.w.id);
                        ctx.send_viewport_cmd_to(viewport, ViewportCommand::Focus);
                    }
                    if self.closing.is_some() {
                        self.apply(ctx, Action::CloseWindow);
                    }
                }
            }
            Action::SelectLayout(layout_id) => {
                self.w.document.cancel_pending_focus();
                // The layout gone to shows the view it last had in this
                // window, at once, or the one its definition stores once it
                // loads (#805). The layout left already has its entry, if
                // it was ever shown: until then the window's view is the
                // previous layout's and is not recorded for it.
                let schema = self.w.calcs.schema().map(AsRef::as_ref);
                let key = self
                    .w
                    .session
                    .as_mut()
                    .map(|session| session.select_layout_with_schema(layout_id, schema));
                if let Some(view) = self.w.views.get(&layout_id) {
                    self.w.view = *view;
                }
                self.request_layout(ctx, key);
                self.after_action(ctx, &action);
            }
            Action::GoToPage(index) => {
                if self.w.mode == Mode::Preview {
                    if let Some(session) = &mut self.w.session {
                        session.go_to_preview(index);
                    }
                    ctx.request_repaint();
                }
            }
            Action::GoToRecord(index) => {
                let moved = self.w.session.as_ref().is_some_and(|s| s.current != index);
                let key = self
                    .w
                    .session
                    .as_mut()
                    .and_then(|session| session.go_to(index));
                self.request_page(ctx, key);
                if moved {
                    self.after_action(ctx, &action);
                }
            }
            Action::LoadRecordRange(index) => {
                if self.error.is_some() {
                    return;
                }
                let key = self
                    .w
                    .session
                    .as_mut()
                    .and_then(|session| session.request_range(index));
                self.request_page(ctx, key);
            }
            Action::Refresh => {
                let key = self.w.session.as_mut().and_then(Session::refresh);
                self.request_page(ctx, key);
            }
            Action::SetView(view) => {
                if self
                    .w
                    .session
                    .as_ref()
                    .is_some_and(|session| session.view_enabled(view))
                    && view != self.w.view
                {
                    self.w.view = view;
                    self.w.remember_view();
                    self.after_action(ctx, &action);
                }
            }
            Action::ZoomIn | Action::ZoomOut | Action::ActualSize => {
                if self.w.session.as_ref().is_some_and(|session| {
                    matches!(session.render, crate::session::RenderState::Loaded(_))
                }) && ((self.w.mode == Mode::Browse && self.w.view == ViewMode::Form)
                    || self.w.mode == Mode::Preview)
                {
                    match action {
                        Action::ZoomIn => self.w.document.zoom.zoom_in(),
                        Action::ZoomOut => self.w.document.zoom.zoom_out(),
                        Action::ActualSize => self.w.document.zoom.actual_size(),
                        _ => unreachable!(),
                    }
                    ctx.request_repaint();
                }
            }
            Action::SetMode(mode) => {
                if self.w.session.is_some() && mode != self.w.mode {
                    self.w.document.cancel_pending_focus();
                    if mode == Mode::Preview {
                        if let Some(session) = &mut self.w.session {
                            session.preview_page = 0;
                        }
                    }
                    if self.w.mode == Mode::Find {
                        // Get ( RequestCount ) keeps the requests.
                        self.w.requests = self.w.find.requests.len() as u64;
                    }
                    if mode == Mode::Find {
                        // Find mode starts from one blank request.
                        self.w.find = FindState::default();
                    }
                    self.w.mode = mode;
                    // Get ( WindowMode ) changed.
                    self.w.calcs.forget_results();
                    self.after_action(ctx, &action);
                }
            }
            Action::NewRequest => self.w.find.add(FindRequest::default()),
            Action::DuplicateRequest => self.w.find.add(self.w.find.current().clone()),
            Action::DeleteRequest => self.w.find.delete(),
            Action::GoToRequest(index) => self.w.find.go_to(index),
            Action::SetOmit(omit) => self.w.find.set_omit(omit),
            Action::InsertOperator(operator) => self.w.find.insert_operator(operator),
            Action::PerformFind => self.run_step(ctx, steps::PERFORM_FIND, 0),
            Action::SelectTool(tool) => {
                self.w.layout_workspace.clear_selection();
                self.w.tool = tool;
                self.w.layout_workspace.field_tool_locked = false;
            }
            Action::LockFieldTool => {
                self.w.layout_workspace.clear_selection();
                self.w.tool = LayoutTool::Field;
                self.w.layout_workspace.field_tool_locked = true;
            }
            Action::ToggleInspector => self.w.inspector = !self.w.inspector,
            Action::ToggleObjects => self.w.objects = !self.w.objects,
            Action::ManageLayouts => {
                if self.w.session.is_some() {
                    self.manage_layouts = Some(ManageLayouts::default());
                }
            }
            Action::DismissManageLayouts => self.manage_layouts = None,

            // A change with the host: Part Setup would read the layout as
            // it was.
            Action::PartSetup if self.w.layout_workspace.saving => {
                self.error = Some(
                    "This layout's last change is still being saved; try again when it is.".into(),
                );
            }
            // A field drawn with the Field tool and not saved would be
            // dropped when the new layout opens: the user saves or reverts
            // it first (OpenMaker's choice; FileMaker Pro asks whether to
            // save the layout).
            // As for New Layout/Report: the copy opens, which would drop the
            // layout's unsaved changes.
            Action::NewLayout
            | Action::DuplicateLayout(_)
            | Action::DeleteLayout(_)
            | Action::LayoutSetup
            | Action::PartSetup
                if self.w.layout_workspace.has_changes() =>
            {
                self.error = Some(
                    "This layout's changes are not saved. Choose Layouts > Save Layout or \
                     Revert Layout first."
                        .into(),
                );
            }
            Action::NewLayout => {
                if let Some(session) = &self.w.session {
                    self.new_layout = Some(NewLayout::new(
                        self.w.file_id,
                        crate::calc::layout_occurrence(session),
                        session.layout,
                    ));
                }
            }
            Action::OpenNewLayout(id) => {
                self.w.layout_mode_for = Some(id);
                self.apply(ctx, Action::SelectLayout(id));
            }
            Action::CreateLayout(request) => self.request(ctx, Request::CreateLayout(request)),
            Action::DuplicateLayout(layout) => self.request(ctx, Request::DuplicateLayout(layout)),
            Action::DeleteLayout(layout) => match self.delete_layout_refusal(layout) {
                Some(refusal) => self.error = Some(refusal),
                None => self.w.delete_layout = Some(layout),
            },
            Action::CancelDeleteLayout => self.w.delete_layout = None,
            Action::LayoutSetup => self.open_layout_setup(),
            Action::PartSetup => self.open_part_setup(),
            Action::SetPartOptions(change) => self.send_parts(ctx, PartsSent::Options(change)),
            Action::SetUpLayout(change) => self.request(ctx, Request::LayoutSetup(change)),
            Action::ConfirmDeleteLayout => {
                if let Some(layout) = self.w.delete_layout.take() {
                    // Another window may have been edited while the alert
                    // was open.
                    if let Some(refusal) = self.delete_layout_refusal(layout) {
                        self.error = Some(refusal);
                        return;
                    }
                    // The window then shows the layout after it in the
                    // list (the one before, for the last), as FileMaker Pro
                    // shows Contact Details after deleting Contact List.
                    self.w.after_delete = self.w.session.as_ref().and_then(|session| {
                        let ids: Vec<u32> = session
                            .layouts
                            .iter()
                            .map(|l| l.id)
                            .filter(|id| session.shows_layout(*id))
                            .collect();
                        let at = ids.iter().position(|id| *id == layout)?;
                        ids.get(at + 1)
                            .or_else(|| at.checked_sub(1).and_then(|before| ids.get(before)))
                            .copied()
                    });
                    self.request(ctx, Request::DeleteLayout(layout));
                }
            }
            Action::SaveLayout => self.save_layout(ctx),
            // The field Save Layout sent stays until the host answers.
            Action::RevertLayout if !self.w.layout_workspace.saving => {
                self.w.layout_workspace.drop_width();
                self.w.layout_workspace.drop_new_object();
                self.w.layout_workspace.drop_moves();
                self.w.layout_workspace.drop_deletions();
                self.w.layout_workspace.drop_controls();
                self.w.layout_workspace.drop_words();
            }
            Action::RevertLayout => {}
            Action::DeleteLayoutObject if self.w.mode == Mode::Layout => {
                let Some(session) = &self.w.session else {
                    return;
                };
                let workspace = &mut self.w.layout_workspace;
                if !workspace.saving
                    && !workspace.delete_selected(session)
                    && workspace.selection_count(session) > 0
                {
                    self.error = Some(
                        "OpenMaker cannot delete every selected object. Nothing was deleted. It deletes fields showing \
                         one repetition, text, shapes, lines and buttons (not popover buttons) that hold no other \
                         objects, are not inside one, show no image from the file's library \
                         and are not a field's label."
                            .into(),
                    );
                }
            }
            Action::DeleteLayoutObject => {}
            Action::ChangePassword => {
                let local = self
                    .w
                    .session
                    .as_ref()
                    .and_then(|session| session.target.local.clone());
                if let Some(path) = local {
                    self.own_password = Some((
                        self.w.id,
                        self.w.file_id,
                        crate::open_file::ChangePasswordDialog::own(
                            path,
                            self.w.calcs.account.clone(),
                        ),
                    ));
                }
            }
            Action::ManageDatabase => {
                if let Some(session) = &self.w.session {
                    self.database = Some(crate::database::ManageDatabase::new(
                        session.target.file.clone(),
                    ));
                    self.request(ctx, Request::BeginSchemaEdit);
                }
            }
            Action::DismissDatabase => self.database = None,
            Action::ManageSecurity => {
                if let Some(session) = &self.w.session {
                    self.security = Some(crate::security::ManageSecurity::new(
                        self.w.file_id,
                        session.target.file.clone(),
                    ));
                    self.request_security_catalog(ctx);
                }
            }
            Action::DismissSecurity => self.security = None,
            Action::ManageValueLists => {
                if let Some(session) = &self.w.session {
                    self.value_lists = Some(crate::value_lists::ManageValueLists::new(
                        self.w.file_id,
                        session.target.file.clone(),
                    ));
                    self.request_value_lists(ctx);
                }
            }
            Action::DismissValueLists => self.value_lists = None,
            Action::ToggleToolbar => self.w.show_toolbar = !self.w.show_toolbar,
            Action::ToggleFormattingBar => self.w.show_formatting = !self.w.show_formatting,
            Action::RefuseTyping(refusal) => {
                self.error = Some(refusal.message().into());
                let record =
                    self.w.session.as_ref().and_then(|session| {
                        Some((session.layout?, session.current_record()?.1.id))
                    });
                self.w.document.hold_focus(record);
            }
            Action::RefuseChoice(error) => self.error = Some(error.message()),
            Action::EditValueList(list) => self.request(ctx, Request::CustomValueList(list)),
            Action::SetValueListValues(change) => {
                self.request(ctx, Request::SetValueListValues(change));
            }
            Action::DiscardEntryCandidates => {
                self.w.document.discard_failed_edits();
                self.error = None;
                self.w.document.release_focus();
            }
            Action::DismissAlert => {
                self.error = None;
                self.w.document.release_focus();
            }
            Action::SelectWindow(id) => {
                // A window behind an open dialog cannot come forward; a
                // window with a card comes forward with its card in front.
                if let Some(dialog) = self.blocking_dialog(id) {
                    ctx.send_viewport_cmd_to(self.viewport_of(dialog), ViewportCommand::Focus);
                    return;
                }
                if self.script.busy() || self.presentation.is_some() {
                    self.presentation = Some(windows::Presentation::User);
                }
                let id = *self.card_chain(id).last().expect("one window");
                let viewport = self.viewport_of(id);
                self.bring_to_front(id);
                ctx.send_viewport_cmd_to(viewport, ViewportCommand::Focus);
            }
            Action::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
            Action::Quit if self.w.session.is_some() => {
                self.closing = Some(true);
                self.apply(ctx, Action::CloseWindow);
            }
            Action::Quit => self.quit_after_closes(ctx),
        }
    }

    /// Quit once the provider has handled the files' Closes: at most
    /// [`QUIT_WAIT`] after asking, so a stalled provider does not keep the
    /// app open.
    fn quit_after_closes(&mut self, ctx: &egui::Context) {
        self.quit_waiting
            .get_or_insert_with(std::time::Instant::now);
        self.quit_after_closes_done(ctx);
    }

    fn quit_after_closes_done(&mut self, ctx: &egui::Context) {
        let Some(since) = self.quit_waiting else {
            return;
        };
        if self.worker.closes_pending() && since.elapsed() < QUIT_WAIT {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        self.quit_waiting = None;
        self.quit_ready = true;
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, ViewportCommand::Close);
    }

    /// The file closes (or the connection attempt stops): its windows
    /// go, and the next file starts from a fresh window (its title, bars,
    /// zoom, view and `Get ( WindowUUID )` included).
    fn close_file(&mut self, ctx: &egui::Context) {
        let was_busy = self.w.session.is_some() || self.status == ConnectStatus::Connecting;
        let file = self.w.file_id;
        // Each of the file's windows keeps its found set in a local file,
        // the front one last (file-metadata.md 11), before it closes.
        let others: Vec<&windows::Doc> =
            self.windows.iter().filter(|d| d.file_id == file).collect();
        let keeps: Vec<_> = others
            .into_iter()
            .chain(std::iter::once(&self.w))
            .filter_map(windows::Doc::found_set_to_keep)
            .collect();
        for (window, request) in keeps {
            if let Err(err) = self.worker.send(ctx, self.epoch, window, request) {
                self.fail(&err);
            }
        }
        // Their calculation reads stop with them (#987).
        self.w.calcs.cancel_reads();
        for doc in self.windows.iter().filter(|d| d.file_id == file) {
            doc.calcs.cancel_reads();
        }
        let preserve_script =
            self.script.running && self.script.owner_file.is_some_and(|owner| owner != file);
        let window = self.w.id;
        self.forget_reconnect(file);
        self.closed_windows.insert(window);
        self.closed_windows.extend(
            self.windows
                .iter()
                .filter(|d| d.file_id == file)
                .map(|d| d.id),
        );
        self.windows.retain(|d| d.file_id != file);
        let remaining = self.windows.iter().rposition(|d| !d.hidden);
        self.w = remaining
            .map(|i| self.windows.remove(i))
            .unwrap_or_else(|| windows::Doc::new(windows::FIRST));
        self.opening = None;
        self.status = ConnectStatus::Idle;
        if self.w.session.is_none() {
            self.local_read_only = None;
        }
        self.manage_layouts = None;
        self.new_layout = None;
        self.layout_setup = None;
        self.value_list_edit = None;
        self.part_setup = None;
        self.database = None;
        self.security = None;
        self.value_lists = None;
        self.workspace = None;
        self.workspace_followups.clear();
        if !preserve_script {
            self.script = scripts::ScriptUi::default();
        }
        self.closing = None;
        if was_busy {
            if self.w.session.is_none() && self.windows.is_empty() {
                self.epoch += 1;
            }
            if let Err(err) = self.worker.send(ctx, self.epoch, window, Request::Close) {
                self.fail(&err);
            }
        }
    }

    /// Ask for the value-list catalog for the open dialog, as a new
    /// request: a reply to any earlier one no longer reaches it.
    fn request_value_lists(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.value_lists.as_mut().filter(|d| d.owns(self.w.file_id)) else {
            return;
        };
        self.value_list_requests += 1;
        let token = self.value_list_requests;
        dialog.requested(token);
        self.request(ctx, Request::ValueLists { token });
    }

    fn request_security_catalog(&mut self, ctx: &egui::Context) {
        let Some(security) = self.security.as_mut().filter(|s| s.owns(self.w.file_id)) else {
            return;
        };
        self.security_requests += 1;
        let token = self.security_requests;
        security.requested(token);
        self.request(ctx, Request::SecurityCatalog { token });
    }

    /// Send the front window's Script Workspace's request, under a token
    /// of its own that the reply names (`ScriptWorkspace::takes`). Nothing
    /// is sent for a workspace of another file.
    fn request_workspace(&mut self, ctx: &egui::Context, request: impl FnOnce(u64) -> Request) {
        let file = self.w.file_id;
        let Some(workspace) = self.workspace.as_mut().filter(|w| w.owns(file)) else {
            return;
        };
        self.workspace_requests += 1;
        let token = self.workspace_requests;
        workspace.requested(token);
        self.request(ctx, request(token));
    }

    /// The Script Workspace whose request `token` this reply, in the front
    /// window's file, answers (`ScriptWorkspace::takes`).
    fn workspace_reply(
        &mut self,
        token: u64,
    ) -> Option<&mut crate::script_workspace::ScriptWorkspace> {
        let file = self.w.file_id;
        let workspace = self.workspace.as_mut()?;
        workspace.takes(file, token).then_some(workspace)
    }

    fn request(&mut self, ctx: &egui::Context, request: Request) {
        // Nothing more goes to a host whose connection is gone.
        if self.w.connection_lost.is_some() && !matches!(request, Request::Close) {
            return;
        }
        if let Err(err) = self.worker.send(ctx, self.epoch, self.w.id, request) {
            self.fail(&err);
        }
    }

    /// Load a newly selected layout: its geometry, then the page `key`.
    /// Requests run in order, so the geometry is in by the time the records
    /// are and Form view does not flash the generic field list first.
    fn request_layout(&mut self, ctx: &egui::Context, key: Option<PageKey>) {
        // A layout the account may not see draws `<No Access>` (or the
        // blank opening page) and offers no record commands; it reads its
        // found set's ids and count only, which the toolbar shows, as
        // FileMaker Pro 26.0.3 counts the records Go to Related Record
        // found there (#696).
        let hidden = key.is_some_and(|key| {
            self.w
                .session
                .as_ref()
                .is_some_and(|s| !s.shows_layout(key.layout_id))
        });
        if hidden {
            self.w.record_capabilities = Default::default();
            self.request_page(ctx, key);
            ctx.request_repaint();
            return;
        }
        if let Some(key) = key {
            // First, so the window is on the record a local file was closed
            // on before the layout's triggers run.
            self.request(
                ctx,
                Request::SavedPosition {
                    layout_id: key.layout_id,
                },
            );
            let shown = std::mem::take(&mut self.w.record_capabilities);
            self.w.shown_capabilities.get_or_insert(shown);
            self.request(
                ctx,
                Request::RecordCapabilities {
                    layout_id: key.layout_id,
                },
            );
            self.request(
                ctx,
                Request::RenderLayout {
                    layout_id: key.layout_id,
                },
            );
            self.request(
                ctx,
                Request::LayoutGeometry {
                    layout_id: key.layout_id,
                },
            );
            self.request_page(ctx, Some(key));
        }
    }

    fn request_page(&mut self, ctx: &egui::Context, key: Option<PageKey>) {
        // Through a layout the account may not see, the provider gives the
        // found set's ids and count, no value (#696).
        if let Some(key) = key {
            self.request(ctx, Request::Records(key));
        }
    }

    /// The provider opened `target`'s file as `account`: the window
    /// starts on it, asking first for its calculation schema, then its
    /// layouts. Its script catalog is read when the Scripts menu first
    /// shows it (`windows::ScriptsRead`, #1053).
    fn file_opened(
        &mut self,
        ctx: &egui::Context,
        target: Target,
        opened: crate::worker::Opened,
        account: String,
    ) {
        self.closed_windows.clear();
        self.window_files = std::collections::HashMap::from([(windows::FIRST, 0)]);
        self.presentation = None;
        // A new session: its related files are asked about anew (#214).
        self.related_sign_ins.clear();
        self.related_asked.clear();
        self.related_login = None;
        self.w.session = Some(Session::new(target, DEFAULT_PAGE_SIZE));
        self.w.calcs.reset();
        self.w.calcs.account = account;
        self.w.calcs.host_version = opened.host_version.unwrap_or_default();
        self.w.calcs.privileges = opened.privileges;
        self.script.window_opened = true;
        // The schema first: its document preferences name the layout the
        // window opens on and the file triggers due when it has.
        self.w.scripts = Default::default();
        self.w.scripts_read = windows::ScriptsRead::Unread;
        self.request(ctx, Request::CalcSchema);
        self.request(ctx, Request::Layouts);
    }

    /// The file's calculation schema, first built or built again (its
    /// windows' calculation data starts over), or `grown` as related files
    /// opened on demand (what they read stays: no id changed), with what it
    /// says about the file's related files.
    pub(crate) fn accept_calc_schema(
        &mut self,
        result: Result<Option<std::sync::Arc<om_data::host::CalcSchema>>, DataError>,
        warnings: Vec<String>,
        sign_ins: Vec<om_data::RelatedSignIn>,
        grown: bool,
    ) {
        match result {
            Ok(schema) => {
                // The related files that refused the account wait for
                // a layout that needs them (`raise_related_sign_in`),
                // with no alert: FileMaker Pro asks for an account then
                // and shows `<File Missing>` after Cancel, no warning.
                let file = self.w.file_id;
                let warnings: Vec<String> = warnings
                    .into_iter()
                    .filter(|warning| {
                        !sign_ins
                            .iter()
                            .any(|s| warning.starts_with(&format!("Related file “{}” ", s.file)))
                    })
                    .collect();
                let pending = sign_ins
                    .into_iter()
                    .filter(|s| !self.related_asked.contains(&(file, s.file.clone())))
                    .collect();
                self.related_sign_ins.insert(file, pending);
                // No schema at all: this file's windows have no
                // calculations, rather than waiting for them.
                for doc in std::iter::once(&mut self.w)
                    .chain(self.windows.iter_mut())
                    .filter(|d| d.file_id == file)
                {
                    match &schema {
                        Some(_) if grown => doc.calcs.grow_schema(schema.clone()),
                        Some(_) => doc.calcs.set_schema(schema.clone()),
                        None => doc.calcs.schema_unavailable(),
                    }
                }
                if let Some(report) = self.w.calcs.report_related_files(warnings) {
                    self.error = Some(report);
                }
            }
            Err(error) => {
                let file = self.w.file_id;
                for doc in std::iter::once(&mut self.w)
                    .chain(self.windows.iter_mut())
                    .filter(|d| d.file_id == file)
                {
                    doc.calcs.schema_unavailable();
                }
                if !error.lost {
                    self.error = Some(format!(
                        "Calculations are unavailable for this file: {error}"
                    ));
                }
            }
        }
    }

    /// Layouts > Save Layout: store the layout's changes with the shared
    /// operations, which plan them as the host does and have the host save
    /// them: the width (`DataProvider::set_layout_width`), moved objects
    /// (`set_layout_object_frames`), deleted ones (`delete_layout_objects`),
    /// field objects' controls, one at a time (`set_field_control`), then
    /// the Field tool's new field (`place_layout_field`, then its control)
    /// or the Button tool's new button (`place_layout_button`), each on the
    /// layout the one before made. Each change stays on the canvas until the
    /// host answers.
    fn save_layout(&mut self, ctx: &egui::Context) {
        if self.w.layout_workspace.saving {
            return;
        }
        // A text being typed is saved with the rest, as Return would keep it.
        self.w.layout_workspace.finish_retyping();
        // The layout's width goes first; moved and resized objects next, in
        // one request; deletions and a new field follow on the layout they
        // made (`objects_framed`).
        if let Some(width) = self.w.layout_workspace.new_width() {
            let (Some(expected), Some(layout)) = (
                width.revision,
                self.w.session.as_ref().and_then(|s| s.layout),
            ) else {
                self.error = Some(
                    "This layout cannot be saved here: the window does not know which version \
                     of it is shown."
                        .into(),
                );
                return;
            };
            self.w.layout_workspace.saving = true;
            self.request(
                ctx,
                Request::SetLayoutWidth(om_data::layout_placement::LayoutWidthChange {
                    layout,
                    expected,
                    width: width.width,
                }),
            );
            return;
        }
        let moved = self.w.layout_workspace.moved().clone();
        if !moved.frames.is_empty() {
            let Some(expected) = moved.revision else {
                self.error = Some(
                    "This layout cannot be saved here: the window does not know which version \
                     of it is shown."
                        .into(),
                );
                return;
            };
            let Some(layout) = self.w.session.as_ref().and_then(|s| s.layout) else {
                self.error =
                    Some("The layout has not loaded yet; save it again when it has.".into());
                return;
            };
            self.w.layout_workspace.saving = true;
            self.request(
                ctx,
                Request::SetObjectFrames(om_data::layout_objects::LayoutObjectFrames {
                    layout,
                    frames: moved
                        .frames
                        .iter()
                        .map(|(&object, f)| om_data::layout_objects::ObjectFrame {
                            object,
                            frame: om_layout::Rect {
                                top: f.top,
                                left: f.left,
                                bottom: f.bottom,
                                right: f.right,
                                ..om_layout::Rect::default()
                            },
                        })
                        .collect(),
                    expected,
                }),
            );
            return;
        }
        let deleted = self.w.layout_workspace.deleted().clone();
        if !deleted.keys.is_empty() {
            let Some(expected) = deleted.revision else {
                self.error = Some(
                    "This layout cannot be saved here: the window does not know which version \
                     of it is shown."
                        .into(),
                );
                return;
            };
            let Some(layout) = self.w.session.as_ref().and_then(|s| s.layout) else {
                self.error =
                    Some("The layout has not loaded yet; save it again when it has.".into());
                return;
            };
            self.w.layout_workspace.saving = true;
            self.request(
                ctx,
                Request::DeleteObjects(om_data::layout_objects::LayoutObjectDeletion {
                    layout,
                    objects: deleted.keys.iter().copied().collect(),
                    expected,
                }),
            );
            return;
        }
        let controls = self.w.layout_workspace.controls().clone();
        if let Some((&object, &(control, value_list))) = controls.objects.iter().next() {
            self.save_layout_control(ctx, (object, control, value_list), controls.revision);
            return;
        }
        let words = self.w.layout_workspace.words().clone();
        if let Some((&object, text)) = words.objects.iter().next() {
            let (Some(expected), Some(layout)) = (
                words.revision,
                self.w.session.as_ref().and_then(|s| s.layout),
            ) else {
                self.error = Some(
                    "This layout cannot be saved here: the window does not know which version \
                     of it is shown."
                        .into(),
                );
                return;
            };
            self.w.layout_workspace.saving = true;
            self.request(
                ctx,
                Request::SetLayoutText(om_data::layout_placement::TextChange {
                    layout,
                    expected,
                    object,
                    text: text.clone(),
                }),
            );
            return;
        }
        if let Some(new_button) = self.w.layout_workspace.new_button().cloned() {
            self.save_layout_button(ctx, new_button);
            return;
        }
        if let Some(new_portal) = self.w.layout_workspace.new_portal().cloned() {
            self.save_layout_portal(ctx, new_portal);
            return;
        }
        if let Some(new_text) = self.w.layout_workspace.new_text().cloned() {
            self.save_layout_text(ctx, new_text);
            return;
        }
        let Some(new_field) = self.w.layout_workspace.new_field().cloned() else {
            return;
        };
        let Some(choice) = new_field.field.clone() else {
            self.error =
                Some("Choose the field to place in the Inspector, then save the layout.".into());
            return;
        };
        if (1..=4).contains(&new_field.control) && new_field.value_list.is_none() {
            self.error = Some(format!(
                "Choose the value list {} shows in the Inspector's Values from, then save the \
                 layout.",
                choice.name
            ));
            return;
        }
        // Only a layout whose revision the window read can be edited: the
        // host refuses the placement if the layout changed since.
        let Some(expected) = new_field.revision else {
            self.error = Some(
                "This layout cannot be saved here: the window does not know which version of \
                 it is shown."
                    .into(),
            );
            return;
        };
        let Some((layout, occurrence)) =
            self.w.session.as_ref().and_then(|session| {
                Some((session.layout?, crate::calc::layout_occurrence(session)?))
            })
        else {
            self.error = Some("The layout has not loaded yet; save it again when it has.".into());
            return;
        };
        self.w.layout_workspace.saving = true;
        self.request(
            ctx,
            Request::PlaceLayoutField(om_data::layout_placement::LayoutFieldPlacement {
                layout,
                table: choice.table,
                placement: om_data::layout_placement::Placement {
                    occurrence,
                    field: choice.field,
                    frame: new_field.frame(),
                    // Create label: the field's name, left of the field.
                    label: new_field
                        .label
                        .then(|| new_field.label_frame())
                        .flatten()
                        .map(|frame| om_data::layout_placement::Label {
                            text: choice.name.clone(),
                            frame,
                        }),
                },
                expected,
            }),
        );
    }

    /// Save Layout with field object `object`'s new control and value list
    /// (`None`: as stored), at `revision`, the layout's the change was made
    /// on (`DataProvider::set_field_control`). A list control needs a list:
    /// without one nothing is sent.
    fn save_layout_control(
        &mut self,
        ctx: &egui::Context,
        (object, control, value_list): (u64, u32, Option<u64>),
        revision: Option<om_data::layout_placement::Revision>,
    ) {
        let Some(expected) = revision else {
            self.error = Some(
                "This layout cannot be saved here: the window does not know which version of \
                 it is shown."
                    .into(),
            );
            return;
        };
        let Some(session) = self.w.session.as_ref() else {
            return;
        };
        let Some(layout) = session.layout else {
            self.error = Some("The layout has not loaded yet; save it again when it has.".into());
            return;
        };
        let stored = match &session.render {
            crate::session::RenderState::Loaded(document) => document
                .layout
                .objects
                .objects
                .iter()
                .find(|o| o.id == object)
                .and_then(|o| o.field_obj.as_ref())
                .map(|field| field.value_list_key),
            _ => None,
        };
        if (1..=4).contains(&control) && value_list.or(stored).unwrap_or(0) == 0 {
            self.error = Some(
                "Choose the value list the field shows in the Inspector's Values from, then \
                 save the layout."
                    .into(),
            );
            return;
        }
        self.w.layout_workspace.saving = true;
        self.request(
            ctx,
            Request::SetFieldControl(om_data::layout_objects::FieldControlChange {
                layout,
                expected,
                object,
                control,
                value_list,
            }),
        );
    }

    /// Save Layout with a new text, at the revision of the layout it was
    /// drawn on (`DataProvider::place_layout_text`).
    fn save_layout_text(&mut self, ctx: &egui::Context, text: crate::layout_canvas::NewText) {
        if text.text.trim().is_empty() {
            self.error = Some("Type the text on the layout, then save the layout.".into());
            return;
        }
        let Some(expected) = text.revision else {
            self.error = Some(
                "This layout cannot be saved here: the window does not know which version of \
                 it is shown."
                    .into(),
            );
            return;
        };
        let Some(layout) = self.w.session.as_ref().and_then(|session| session.layout) else {
            self.error = Some("The layout has not loaded yet; save it again when it has.".into());
            return;
        };
        self.w.layout_workspace.saving = true;
        self.request(
            ctx,
            Request::PlaceLayoutText(om_data::layout_placement::LayoutTextPlacement {
                layout,
                text: om_data::layout_placement::Label {
                    text: text.text.clone(),
                    frame: text.frame(),
                },
                expected,
            }),
        );
    }

    /// Save Layout with a new button: its label and its action (Perform
    /// Script or a single step), at the revision of the layout it was drawn
    /// on.
    fn save_layout_button(&mut self, ctx: &egui::Context, button: crate::layout_canvas::NewButton) {
        let Some(step) = button.step.as_ref() else {
            self.error =
                Some("Give the button its action in Button Setup, then save the layout.".into());
            return;
        };
        let action = match crate::script_workspace::button_setup::button_action(step) {
            Ok(action) => action,
            Err(message) => {
                self.error = Some(message);
                return;
            }
        };
        let Some(expected) = button.revision else {
            self.error = Some(
                "This layout cannot be saved here: the window does not know which version of \
                 it is shown."
                    .into(),
            );
            return;
        };
        let Some(layout) = self.w.session.as_ref().and_then(|session| session.layout) else {
            self.error = Some("The layout has not loaded yet; save it again when it has.".into());
            return;
        };
        self.w.layout_workspace.saving = true;
        self.request(
            ctx,
            Request::PlaceLayoutButton(om_data::layout_placement::LayoutButtonPlacement {
                layout,
                placement: om_data::layout_placement::ButtonPlacement {
                    frame: button.frame(),
                    label: button.label.clone(),
                    action,
                },
                expected,
            }),
        );
    }

    /// Save Layout with a new portal: the records, rows and fields Portal
    /// Setup and Add Fields to Portal set, at the revision of the layout it
    /// was drawn on.
    fn save_layout_portal(&mut self, ctx: &egui::Context, portal: crate::layout_canvas::NewPortal) {
        let Some(setup) = portal.setup.as_ref() else {
            self.error = Some(
                "Choose the portal's related records and fields in Portal Setup, then save the \
                 layout."
                    .into(),
            );
            return;
        };
        let Some(expected) = portal.revision else {
            self.error = Some(
                "This layout cannot be saved here: the window does not know which version of \
                 it is shown."
                    .into(),
            );
            return;
        };
        let Some(layout) = self.w.session.as_ref().and_then(|session| session.layout) else {
            self.error = Some("The layout has not loaded yet; save it again when it has.".into());
            return;
        };
        self.w.layout_workspace.saving = true;
        self.request(
            ctx,
            Request::PlaceLayoutPortal(om_data::layout_placement::LayoutPortalPlacement {
                layout,
                table: setup.table,
                portal: om_data::layout_placement::PortalPlacement {
                    occurrence: setup.occurrence,
                    frame: portal.frame(),
                    rows: setup.rows,
                    fields: setup.fields.iter().map(|(id, _)| *id).collect(),
                },
                expected,
            }),
        );
    }

    /// The host's answer to Save Layout. Placed: the new field is dropped
    /// from the canvas and every window of the file reads the layout again,
    /// as the host now stores it. The layout changed since it was shown
    /// (306): nothing was written; the field is dropped and the layout read
    /// again, for the user to place it on what is there now. Refused for
    /// another reason: it stays for the user to change. Not sent, or sent
    /// without an answer: it is dropped, and the layout read again shows
    /// whether the host placed it.
    /// A new button's answer is read the same way.
    fn layout_object_placed(&mut self, ctx: &egui::Context, result: Result<(), DataError>) {
        self.w.layout_workspace.saving = false;
        let reread = match &result {
            Ok(_) => true,
            Err(err) => {
                !err.lost
                    && err
                        .code
                        .is_none_or(|code| code == om_data::layout_placement::CONFLICT)
            }
        };
        if reread {
            self.w.layout_workspace.drop_new_object();
        }
        if let Err(err) = &result {
            self.fail(err);
        }
        if reread {
            let file = self.w.file_id;
            for id in self.window_ids() {
                self.with_window(id, |app| {
                    if app.w.file_id != file {
                        return;
                    }
                    app.w.document.clear_layout_cache();
                    app.w.calcs.data_changed();
                    let key = app.w.session.as_mut().and_then(Session::refresh);
                    app.request_layout(ctx, key);
                });
            }
        }
    }

    /// The host's answer to Save Layout's moved objects. Saved: they are
    /// dropped from the canvas, the deletions and a new field or button made
    /// meanwhile are sent next on the layout as now stored, and every window
    /// of the file reads the layout again. The layout changed since it was
    /// shown (306): nothing was written; the moves and the deletions made on
    /// the same layout are dropped and the layout read again. Another
    /// refusal keeps them, to be changed.
    fn objects_framed(
        &mut self,
        ctx: &egui::Context,
        result: Result<om_data::layout_objects::FramedObjects, DataError>,
    ) {
        self.w.layout_workspace.saving = false;
        // The revision the moves were sent at, before they are dropped.
        let sent = self.w.layout_workspace.moved().revision;
        let done = layout_change_done(&result);
        if done {
            self.w.layout_workspace.drop_moves();
            if result.is_err() {
                self.w.layout_workspace.drop_deletions();
                self.w.layout_workspace.drop_controls();
                self.w.layout_workspace.drop_words();
            }
        }
        if let Err(err) = &result {
            self.fail(err);
        }
        if done {
            self.reread_file_layouts(ctx);
        }
        if let Ok(framed) = result {
            self.save_next(ctx, framed.changed.then_some((sent, framed.revision)));
        }
    }

    /// The host's answer to Save Layout's width, as [`Self::objects_framed`]:
    /// saved, the moves and the rest follow; refused because the layout
    /// changed, the width is dropped; either way every window of the file
    /// reads the layout again.
    fn width_set(
        &mut self,
        ctx: &egui::Context,
        result: Result<om_data::layout_objects::FramedObjects, DataError>,
    ) {
        self.w.layout_workspace.saving = false;
        let sent = self.w.layout_workspace.new_width().and_then(|w| w.revision);
        let done = layout_change_done(&result);
        if done {
            self.w.layout_workspace.drop_width();
        }
        if let Err(err) = &result {
            self.fail(err);
        }
        if done {
            self.reread_file_layouts(ctx);
        }
        if let Ok(set) = result {
            self.save_next(ctx, set.changed.then_some((sent, set.revision)));
        }
    }

    /// The host's answer to Save Layout's control for the first field object
    /// in [`crate::layout_canvas::ControlChanges`], as
    /// [`Self::objects_framed`]: saved, the next object's control (or the
    /// new object) is sent on the layout as now stored; refused because the
    /// layout changed (306), every control change is dropped; either way
    /// every window of the file reads the layout again. Another refusal
    /// keeps them, to be changed.
    fn control_set(
        &mut self,
        ctx: &egui::Context,
        result: Result<om_data::layout_objects::FramedObjects, DataError>,
    ) {
        self.w.layout_workspace.saving = false;
        let controls = self.w.layout_workspace.controls();
        let sent = controls.revision;
        let object = controls.objects.keys().next().copied();
        let done = layout_change_done(&result);
        match (&result, object) {
            (Ok(_), Some(object)) => self.w.layout_workspace.control_sent(object),
            (Err(_), _) if done => {
                self.w.layout_workspace.drop_controls();
                self.w.layout_workspace.drop_words();
            }
            _ => {}
        }
        if let Err(err) = &result {
            self.fail(err);
        }
        if done {
            self.reread_file_layouts(ctx);
        }
        if let Ok(set) = result {
            self.save_next(ctx, set.changed.then_some((sent, set.revision)));
        }
    }

    /// The host's answer to Save Layout's words for the first text object
    /// in [`crate::layout_canvas::WordChanges`], as [`Self::control_set`]:
    /// saved, the next text's words (or the new object) are sent on the
    /// layout as now stored; refused because the layout changed (306),
    /// every retyped text is dropped; another refusal keeps them.
    fn words_set(
        &mut self,
        ctx: &egui::Context,
        result: Result<om_data::layout_objects::FramedObjects, DataError>,
    ) {
        self.w.layout_workspace.saving = false;
        let words = self.w.layout_workspace.words();
        let sent = words.revision;
        let object = words.objects.keys().next().copied();
        let done = layout_change_done(&result);
        match (&result, object) {
            (Ok(_), Some(object)) => self.w.layout_workspace.words_sent(object),
            (Err(_), _) if done => self.w.layout_workspace.drop_words(),
            _ => {}
        }
        if let Err(err) = &result {
            self.fail(err);
        }
        if done {
            self.reread_file_layouts(ctx);
        }
        if let Ok(set) = result {
            self.save_next(ctx, set.changed.then_some((sent, set.revision)));
        }
    }

    /// The host's answer to Save Layout's deleted objects, as
    /// [`Self::objects_framed`]: saved, a new field is sent next; refused
    /// because the layout changed (306), the deletions are dropped; either
    /// way every window of the file reads the layout again.
    fn objects_deleted(
        &mut self,
        ctx: &egui::Context,
        result: Result<om_data::layout_objects::DeletedObjects, DataError>,
    ) {
        self.w.layout_workspace.saving = false;
        let sent = self.w.layout_workspace.deleted().revision;
        let done = layout_change_done(&result);
        if done {
            self.w.layout_workspace.drop_deletions();
            if result.is_err() {
                self.w.layout_workspace.drop_controls();
                self.w.layout_workspace.drop_words();
            }
        }
        if let Err(err) = &result {
            self.fail(err);
        }
        if done {
            self.reread_file_layouts(ctx);
        }
        if let Ok(deleted) = result {
            self.save_next(ctx, deleted.changed.then_some((sent, deleted.revision)));
        }
    }

    /// Save Layout's next change, after the host answered one. `applied`:
    /// the host applied it, taking the layout from the revision it was sent
    /// at to the one it reported; the changes still to send that were made
    /// on that first revision now go at the second
    /// (`LayoutWorkspace::rebase`). A change made on another revision (the
    /// layout read again since, after another client's change) keeps its
    /// own, so the host refuses it if the layout changed (306), and so do
    /// all of them when the answer was a repeat (`changed` false).
    fn save_next(
        &mut self,
        ctx: &egui::Context,
        applied: Option<(
            Option<om_data::layout_placement::Revision>,
            om_data::layout_placement::Revision,
        )>,
    ) {
        let workspace = &mut self.w.layout_workspace;
        if let Some((Some(from), to)) = applied {
            workspace.rebase(from, to);
        }
        if workspace.has_changes() {
            self.save_layout(ctx);
        }
    }

    /// Every window of this window's file reads its layout again, as the
    /// host now stores it.
    fn reread_file_layouts(&mut self, ctx: &egui::Context) {
        let file = self.w.file_id;
        for id in self.window_ids() {
            self.with_window(id, |app| {
                if app.w.file_id != file {
                    return;
                }
                app.w.document.clear_layout_cache();
                app.w.calcs.data_changed();
                let key = app.w.session.as_mut().and_then(Session::refresh);
                app.request_layout(ctx, key);
            });
        }
    }

    /// Layout Setup's dialog over the layout the window shows in Layout
    /// mode, with its name, menu inclusion, view flags and revision.
    fn open_layout_setup(&mut self) {
        let Some(session) = &self.w.session else {
            return;
        };
        let geometry = match &session.geometry {
            crate::session::Geometry::Loaded(geometry) => geometry.revision,
            _ => None,
        };
        let (Some(layout), Some(revision), Some(def)) =
            (session.layout, geometry, session.layout_def())
        else {
            self.error = Some("The layout has not loaded yet; try again when it has.".into());
            return;
        };
        let name = session
            .layouts
            .iter()
            .find(|l| l.id == layout)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        let in_menus = session
            .layout_menu
            .iter()
            .find(|entry| entry.id == layout)
            .is_none_or(|entry| entry.flags & om_data::layout_catalog::MENU_HIDDEN == 0);
        let occurrence = crate::calc::layout_occurrence(session)
            .and_then(|id| {
                let schema = self.w.calcs.schema()?;
                Some(
                    schema
                        .occurrences_in_order()
                        .into_iter()
                        .find(|o| o.id == id)?
                        .name
                        .clone(),
                )
            })
            .unwrap_or_default();
        self.layout_setup = Some(LayoutSetup::new(
            self.w.id,
            (layout, revision),
            (name, occurrence),
            (def.flags, def.menu_set_key),
            in_menus,
        ));
    }

    /// Part Setup over the layout the window shows in Layout mode: its
    /// parts as the window read them, and the fields of the layout's table a
    /// sub-summary may be sorted by.
    fn open_part_setup(&mut self) {
        let Some(session) = &self.w.session else {
            return;
        };
        let (Some(layout), crate::session::Geometry::Loaded(geometry)) =
            (session.layout, &session.geometry)
        else {
            self.error = Some("The layout has not loaded yet; try again when it has.".into());
            return;
        };
        let (Some(revision), crate::session::RenderState::Loaded(document)) =
            (geometry.revision, &session.render)
        else {
            self.error = Some("The layout has not loaded yet; try again when it has.".into());
            return;
        };
        let schema = self.w.calcs.schema();
        let occurrence = crate::calc::layout_occurrence(session).unwrap_or_default();
        let table = schema.and_then(|s| s.table_of(occurrence));
        let mut fields: Vec<(u64, String)> = table
            .map(|t| t.fields.values().map(|f| (f.id, f.name.clone())).collect())
            .unwrap_or_default();
        fields.sort_by_key(|(_, name)| name.to_lowercase());
        let field_name = |field: u64| {
            fields
                .iter()
                .find(|(id, _)| *id == field)
                .map_or_else(|| format!("field {field}"), |(_, name)| name.clone())
        };
        let parts = document
            .layout
            .parts
            .iter()
            .map(|part| PartRow {
                id: part.id,
                part_type: part.part_type,
                flags: part.flags,
                page_break_after: part.page_break_after,
                sorted_by: part
                    .break_field()
                    .ok()
                    .flatten()
                    .map(|f| (f.field, field_name(f.field))),
                objects: geometry
                    .objects
                    .iter()
                    .filter(|o| {
                        o.key != 0 && o.rect.top >= part.top && o.rect.top < part.top + part.height
                    })
                    .count(),
            })
            .collect();
        self.part_setup = Some(PartSetup::new(
            self.w.id,
            (layout, revision),
            (occurrence, table.map_or(0, |t| t.id)),
            fields,
            parts,
        ));
    }

    /// The host's answer to New Layout/Report: every window of the file
    /// reads its layout list and the calculation schema again, and this
    /// window opens the new layout in Layout mode once its list has it.
    fn layout_created(
        &mut self,
        ctx: &egui::Context,
        result: Result<om_data::layout_catalog::CreatedLayout, DataError>,
    ) {
        match &result {
            Ok(created) => self.w.new_layout = Some(created.layout),
            Err(err) => self.fail(err),
        }
        if result.as_ref().is_err_and(|err| err.lost) {
            return;
        }
        self.request(ctx, Request::CalcSchema);
        let file = self.w.file_id;
        for id in self.window_ids() {
            self.with_window(id, |app| {
                if app.w.file_id == file {
                    app.request(ctx, Request::Layouts);
                }
            });
        }
    }

    /// Why layout `layout` cannot be deleted now: a window of the file
    /// holds unsaved changes made on it (`Doc::unsaved_on_layout`), which
    /// the deletion would strand. This window's record was saved before
    /// the deletion was asked (`Action::DeleteLayout` navigates).
    fn delete_layout_refusal(&self, layout: u32) -> Option<String> {
        let file = self.w.file_id;
        std::iter::once(&self.w)
            .chain(&self.windows)
            .any(|w| w.file_id == file && w.unsaved_on_layout(layout))
            .then(|| {
                "A window of this file has unsaved changes on this layout. Save or revert \
                 them before deleting the layout."
                    .into()
            })
    }

    /// The host's answer to Delete Layout: every window of the file reads
    /// its layout list again, a window that showed the layout going to
    /// another; this window shows the layout after it, in Layout mode.
    fn layout_deleted(
        &mut self,
        ctx: &egui::Context,
        result: Result<om_data::layout_catalog::RemovedLayout, DataError>,
    ) {
        let next = self.w.after_delete.take();
        match &result {
            Ok(_) => self.w.new_layout = next,
            Err(err) => self.fail(err),
        }
        if result.as_ref().is_err_and(|err| err.lost) {
            return;
        }
        self.request(ctx, Request::CalcSchema);
        let file = self.w.file_id;
        for id in self.window_ids() {
            self.with_window(id, |app| {
                if app.w.file_id == file {
                    app.request(ctx, Request::Layouts);
                }
            });
        }
    }

    /// Show an error and stop waiting on whatever the Hosts dialog was doing.
    /// A lost connection shows as the banner instead.
    fn fail(&mut self, err: &DataError) {
        if err.lost && self.w.session.is_some() {
            self.lose_connection(err);
            return;
        }
        self.error = Some(err.message.clone());
        self.status = ConnectStatus::Idle;
        self.opening = None;
    }

    /// A listing of a host's files came back. One no longer waited for is
    /// dropped: nothing about the dialog, an opening file or a window
    /// changes for it. A failure shows in the file list, for its host.
    fn on_listed(&mut self, listed: worker::Listed) {
        if !self.listing.accept(listed) {
            return;
        }
        if self.status == ConnectStatus::ListingFiles {
            self.status = ConnectStatus::Idle;
        }
        let server = self.listing.server().cloned();
        if let Some(listing) = self.listing.files_for(server.as_ref()) {
            if self.form.file.trim().is_empty() {
                if let [only] = listing.files.as_slice() {
                    self.form.file = only.name.clone();
                }
            }
        }
    }

    /// Forget a listing of a host or port the form no longer names,
    /// stopping one under way.
    fn forget_other_hosts(&mut self) {
        let server = self.form.server().ok();
        if self.listing.server().is_some() && self.listing.server() != server.as_ref() {
            if self.listing.pending() && self.status == ConnectStatus::ListingFiles {
                self.status = ConnectStatus::Idle;
            }
            self.listing.forget();
        }
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        while let Some(listed) = self.worker.poll_listed() {
            self.on_listed(listed);
        }
        loop {
            match self.worker.poll() {
                Ok(response) if response.epoch == self.epoch => self.on_response(ctx, response),
                Ok(_) => {}
                Err(PollError::Empty) => break,
                Err(PollError::Stopped) => {
                    if !self.worker_stopped {
                        self.worker_stopped = true;
                        self.fail(&worker::stopped_error());
                    }
                    break;
                }
            }
        }
    }

    /// The connection to the host is gone. Each window of the file puts
    /// back what it showed if a layout was still loading, stops waiting for
    /// replies, and keeps its unsaved edits on screen; closing the file
    /// stops waiting for the saves it needed.
    fn lose_connection(&mut self, err: &DataError) {
        if self.w.connection_lost.is_some() || self.w.session.is_none() {
            return;
        }
        self.w.connection_lost = Some(err.message.clone());
        let file = self.w.file_id;
        for id in self.window_ids() {
            self.with_window(id, |app| {
                if app.w.file_id != file {
                    return;
                }
                app.w.connection_lost = Some(err.message.clone());
                let doc = &mut app.w;
                let restored = doc
                    .session
                    .as_mut()
                    .is_some_and(crate::session::Session::connection_lost);
                if restored {
                    // The layout put back may share its id with the one replaced.
                    doc.document.clear_layout_cache();
                    // It shows in its own view again, not the one the
                    // layout given up was switched to.
                    let layout = doc.session.as_ref().and_then(|s| s.layout);
                    if let Some(view) = layout.and_then(|l| doc.views.get(&l)) {
                        doc.view = *view;
                    }
                }
                if let Some(shown) = doc.shown_capabilities.take().filter(|_| restored) {
                    // Its drafts stay editable, so they stay visible.
                    doc.record_capabilities = shown;
                }
                if doc.document.pending {
                    doc.unanswered = doc.in_flight.take().or(doc.unanswered);
                }
                // A later reconnect checks every edit again.
                doc.recovery = None;
                doc.reconnected = false;
                doc.document.pending = false;
                doc.deferred_action = None;
                doc.delete_record = false;
                doc.delete_portal_row = None;
            });
        }
        self.closing = None;
        self.stop_scripts();
    }

    /// The window's notice of a lost connection, in place of the unsaved
    /// changes bar.
    fn lost_banner(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let unsaved = self.w.document.unsaved();
        // What scripts left on the record goes with the file too.
        let scripts_left = self.uncommitted().is_some();
        egui::Panel::top("connection_lost").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("The connection to the host was lost.").strong());
                let detail = match unsaved {
                    0 if !scripts_left => "This window shows the data last received; nothing \
                                           more can be read or saved until you reconnect."
                        .to_owned(),
                    0 => "This window shows the data last received; nothing more can be \
                          read or saved. The record's unsaved changes are kept in this window \
                          and will be lost when you close the file; copy what you need first."
                        .to_owned(),
                    n => format!(
                        "This window shows the data last received; nothing more can be \
                         read or saved. {n} unsaved field {} kept in this window: \
                         reconnecting checks them against the host before they can be saved, \
                         and closing the file loses them.",
                        if n == 1 { "change is" } else { "changes are" }
                    ),
                };
                ui.label(detail);
                let can_reconnect = !self.script.running && self.reconnect.is_none();
                if ui
                    .add_enabled(can_reconnect, egui::Button::new("Reconnect…"))
                    .clicked()
                {
                    actions.push(Action::Reconnect);
                }
                if ui.button("Close File").clicked() {
                    actions.push(Action::CloseFile);
                }
            });
        });
    }

    fn on_reply(&mut self, ctx: &egui::Context, reply: Reply) {
        // A dismissed/reopened dialog or another file cannot consume this reply.
        if matches!(
            &reply,
            Reply::Security { token, .. }
                if !self.security.as_ref().is_some_and(|s| s.accepts(self.w.file_id, *token))
        ) {
            return;
        }
        if matches!(
            &reply,
            Reply::ValueLists { token, .. }
                if !self.value_lists.as_ref().is_some_and(|d| d.accepts(self.w.file_id, *token))
        ) {
            return;
        }
        if let Some(err) = reply.error().filter(|err| err.lost) {
            let err = err.clone();
            self.lose_connection(&err);
        }
        // Once the connection is lost the window keeps what it shows: the
        // replies still queued for layouts and pages (failures, mostly)
        // would replace the layout the loss put back.
        let presentation = matches!(
            reply,
            Reply::RenderLayout { .. }
                | Reply::LayoutGeometry { .. }
                | Reply::RecordCapabilities { .. }
                | Reply::Records { .. }
        );
        if presentation && self.w.connection_lost.is_some() {
            return;
        }
        match reply {
            // Taken above; a reconnect's answer is the app's (windows.rs).
            Reply::ConnectionLost(_) | Reply::Reconnected(_) | Reply::Released => {}
            // Unwrapped for the file's windows in `on_response`; here, this
            // window's file takes the schema before the answer.
            Reply::Grown { grown, then } => {
                self.accept_calc_schema(grown.result, grown.warnings, grown.sign_ins, true);
                self.on_reply(ctx, *then);
            }
            Reply::RecordEditPrepared(token, request, result) => {
                if !self
                    .w
                    .document
                    .accept_edit_preparation(token, &request, result.is_ok())
                {
                    return;
                }
                match result {
                    Ok(om_data::EditPreparation::Prepared) => {
                        let layout = request.baseline.layout;
                        let record = request.baseline.record;
                        if !self
                            .w
                            .uncommitted
                            .as_ref()
                            .is_some_and(|(l, r, _)| *l == layout && *r == record)
                        {
                            self.w.uncommitted = Some((layout, record, Default::default()));
                        }
                        if let Some((_, _, open)) = &mut self.w.uncommitted {
                            open.session = true;
                        }
                    }
                    Ok(om_data::EditPreparation::Unavailable) => {}
                    Err(error) => self.fail(&error),
                }
                ctx.request_repaint();
            }
            Reply::Rechecked { records, related } => self.rechecked(ctx, records, related),
            Reply::SaveSettled {
                settle,
                result,
                report,
            } => self.save_settled(ctx, settle, result, report),
            Reply::Recreated(result) => self.recreated(ctx, result),
            Reply::License(result) => {
                if let Some(dialog) = &mut self.license_dialog {
                    dialog.accept(result);
                }
            }
            Reply::Files(listed) => self.on_listed(listed),
            Reply::Opened(result) => {
                let target = self.opening.take();
                self.status = ConnectStatus::Idle;
                match (result, target) {
                    (Ok(opened), Some(target)) => {
                        self.form.password.clear();
                        let account = self.form.account.clone();
                        self.file_opened(ctx, target, opened, account);
                    }
                    (Ok(_), None) => {}
                    (Err(err), _) => self.fail(&err),
                }
            }
            Reply::Layouts(result) if self.w.reconnected => {
                // A reconnect's fresh layout list: every window of the
                // file takes it, or reloads what it had when it failed.
                self.reload_reconnected(ctx, result.as_ref().ok());
                if let Err(error) = result {
                    self.fail(&error);
                }
            }
            Reply::OpenedLocal(result) => {
                let target = self.opening.take();
                // Only the open of the file Create New made opens Manage
                // Database.
                let created = self.database_after_open.take();
                self.status = ConnectStatus::Idle;
                match (result, target) {
                    (Ok((LocalOpen::Opened { read_only }, opened)), Some(target)) => {
                        self.local_login = None;
                        self.change_password = None;
                        self.local_read_only = read_only;
                        let account = opened.account.clone().unwrap_or_default();
                        let made = created.is_some() && created == target.local;
                        self.file_opened(ctx, target, opened, account);
                        if made {
                            self.apply(ctx, Action::ManageDatabase);
                        }
                    }
                    (Ok((LocalOpen::NeedsLogin { account }, _)), Some(target)) => {
                        match &mut self.local_login {
                            Some(dialog) => dialog.pending = false,
                            None => {
                                let path = target.local.unwrap_or_default();
                                self.local_login =
                                    Some(crate::open_file::LoginDialog::new(path, account));
                            }
                        }
                    }
                    (Ok(_), None) => {}
                    // The old password no longer admitted (212), or the
                    // account made inactive (210): the account waits no
                    // more, and the waiting file closes.
                    (Err(err), _)
                        if self.change_password.is_some()
                            && matches!(err.code, Some(212 | 210)) =>
                    {
                        self.change_password = None;
                        self.epoch += 1;
                        let window = self.w.id;
                        if let Err(close) =
                            self.worker.send(ctx, self.epoch, window, Request::Close)
                        {
                            self.fail(&close);
                        }
                        self.fail(&err);
                    }
                    // A new password refused (206, 208, 209, 11) or not
                    // saved: the change dialog asks again, the account
                    // still waiting in the provider.
                    (Err(err), _) if self.change_password.is_some() => {
                        if let Some(dialog) = &mut self.change_password {
                            dialog.pending = false;
                            dialog.error = Some(err.message);
                        }
                    }
                    // The password must be changed first: the change dialog
                    // takes the sign-in dialog's place, the file waiting.
                    (Err(err), _) if err.code == Some(211) && self.local_login.is_some() => {
                        if let Some(login) = self.local_login.take() {
                            self.change_password =
                                Some(crate::open_file::ChangePasswordDialog::new(
                                    login.path,
                                    login.account,
                                ));
                        }
                    }
                    (Err(err), _) => match &mut self.local_login {
                        // The account was refused (a wrong password, an
                        // inactive account or one whose password must be
                        // changed, or one OpenMaker does not admit yet):
                        // the dialog asks again, the file waiting in the
                        // provider.
                        Some(dialog) if matches!(err.code, Some(212 | 210 | 211 | 3 | 9)) => {
                            dialog.pending = false;
                            dialog.error = Some(err.message);
                        }
                        _ => {
                            self.local_login = None;
                            self.fail(&err);
                        }
                    },
                }
            }
            Reply::Layouts(result) => {
                // The shown layout, and whether the window holds unsaved
                // changes made on it: another client may have deleted it.
                let shown = self.w.session.as_ref().and_then(|session| {
                    let id = session.layout?;
                    let layout = session.layouts.iter().find(|l| l.id == id)?;
                    Some((id, layout.name.clone()))
                });
                let unsaved = shown
                    .as_ref()
                    .is_some_and(|(id, _)| self.w.unsaved_on_layout(*id));
                let Some(session) = self.w.session.as_mut() else {
                    return;
                };
                match result {
                    Ok((layouts, menu)) => {
                        session.layout_menu = menu;
                        let schema = self.w.calcs.schema().map(AsRef::as_ref);
                        // No layout yet: the file's first window.
                        let starting = session.layout.is_none();
                        let mut key = session.set_layouts_for(layouts, schema);
                        // Its changes cannot be saved without the layout; no
                        // lock kept another client from deleting it (#680):
                        // the user is told rather than left to find them gone.
                        if let Some((_, name)) = shown.filter(|(id, _)| {
                            unsaved && !session.layouts.iter().any(|l| l.id == *id)
                        }) {
                            self.error = Some(format!(
                                "The layout \"{name}\" was deleted by another user. This \
                                 window's unsaved changes on it could not be saved."
                            ));
                        }
                        // File Options "Switch to layout".
                        let default = schema
                            .and_then(|s| s.file_options.as_ref()?.default_layout())
                            .and_then(|id| u32::try_from(id).ok());
                        if starting {
                            // The first window opens on it, else on the
                            // first layout, also when the account may not
                            // see that one (#696).
                            if let Some(start) = session.open_on(default) {
                                key = Some(start);
                            }
                        } else if let (Some(_), Some(default)) = (
                            key,
                            default.filter(|id| {
                                session.shows_layout(*id)
                                    && session.layouts.iter().any(|l| l.id == *id)
                            }),
                        ) {
                            // The shown layout is gone: File Options'
                            // layout, when the account may see it.
                            key = Some(session.select_layout(default));
                        }
                        // The first window's layout: OnLayoutEnter once it
                        // has loaded. A hidden one is never entered: it
                        // reads nothing and runs no trigger [I].
                        self.w.pending_enter = key
                            .map(|k| k.layout_id)
                            .filter(|id| session.shows_layout(*id));
                        self.request_layout(ctx, key);
                        // A layout this window added opens in Layout mode.
                        if let Some(id) = self.w.new_layout.take().filter(|id| {
                            self.w
                                .session
                                .as_ref()
                                .is_some_and(|s| s.layouts.iter().any(|l| l.id == *id))
                        }) {
                            self.apply(ctx, Action::OpenNewLayout(id));
                        }
                    }
                    Err(err) => {
                        session.layouts_loading = false;
                        self.fail(&err);
                    }
                }
            }
            Reply::RenderLayout { layout_id, result } => {
                self.w.document.clear_layout_cache();
                if let Some(session) = self.w.session.as_mut() {
                    // The view the window last showed the layout in, also
                    // when the layout is drawn again, while the layout
                    // still offers it; else the one its definition stores.
                    if session.layout == Some(layout_id) {
                        let flags = match result.as_ref() {
                            Ok(Some(layout)) => {
                                Some(layout.layout.def.as_ref().map_or(0, |def| def.flags))
                            }
                            _ => None,
                        };
                        let kept = self.w.views.get(&layout_id).copied().filter(|view| {
                            flags.is_none_or(|f| crate::scripting::View::from(*view).enabled(f))
                        });
                        if let Some(view) = kept {
                            self.w.view = view;
                        } else if let Some(flags) = flags {
                            self.w.view = crate::scripting::View::stored(flags).into();
                            self.w.views.insert(layout_id, self.w.view);
                        }
                    }
                    if let Err(err) = session.accept_render_layout(layout_id, *result) {
                        self.fail(&err);
                    }
                }
                self.layout_entered(ctx, layout_id);
                // The layout this window added is shown: Layout mode.
                if self.w.layout_mode_for == Some(layout_id)
                    && self
                        .w
                        .session
                        .as_ref()
                        .is_some_and(|s| s.layout == Some(layout_id))
                {
                    self.w.layout_mode_for = None;
                    self.apply(ctx, Action::SetMode(Mode::Layout));
                }
            }
            Reply::LayoutGeometry { layout_id, result } => {
                let Some(session) = self.w.session.as_mut() else {
                    return;
                };
                if let Err(err) = session.accept_geometry(layout_id, result) {
                    self.fail(&err);
                }
            }
            Reply::RecordCapabilities { layout_id, result } => {
                if self
                    .w
                    .session
                    .as_ref()
                    .is_some_and(|session| session.layout == Some(layout_id))
                {
                    self.w.shown_capabilities = None;
                    match result {
                        Ok(capabilities) => self.w.record_capabilities = capabilities,
                        Err(error) => self.fail(&error),
                    }
                }
            }
            Reply::SavedPosition {
                layout_id,
                position,
            } => {
                let Some(session) = self.w.session.as_mut() else {
                    return;
                };
                // The record the file was closed on, unless the window has
                // moved since this layout opened.
                if let Some(index) =
                    position.filter(|_| session.layout == Some(layout_id) && session.current == 0)
                {
                    let next = session.go_to(index);
                    self.request_page(ctx, next);
                }
            }
            Reply::Records {
                key,
                result,
                found,
                no_access,
                generation,
            } => {
                let Some(session) = self.w.session.as_mut() else {
                    return;
                };
                if let Ok(page) = &result {
                    // Fresh values: calculations read the page's records
                    // again, and keep what else they read while the
                    // provider saw no change (#1042).
                    let table = (session.layout == Some(key.layout_id))
                        .then(|| crate::calc::layout_occurrence(session))
                        .flatten()
                        .and_then(|occurrence| self.w.calcs.schema()?.table_of(occurrence))
                        .map(|table| table.id);
                    let records: Vec<u64> = page
                        .records
                        .iter()
                        .filter_map(|record| u64::try_from(record.id).ok())
                        .collect();
                    self.w.calcs.page_read(generation, table, &records);
                }
                // Only the page the window waits for tells its found set.
                let primary = session.pending == Some(key);
                match session.accept_page_with(key, result, no_access) {
                    Ok(next) => {
                        if let Some(found) = found.filter(|_| primary) {
                            self.w.found_set = Some(found);
                        }
                        self.request_page(ctx, next);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            Reply::Updated {
                layout_id,
                record_id,
                result,
            } => {
                self.w.document.pending = false;
                self.w.in_flight = None;
                self.w.calcs.data_changed();
                match result {
                    Ok(record) => {
                        // Typed globals are the session's values: the record
                        // was not written.
                        let globals_only = std::mem::take(&mut self.w.globals_only);
                        let transaction = (!globals_only)
                            .then(|| self.window_transaction(layout_id, "Modified", &record))
                            .flatten();
                        // Until the page is read again it holds the record as
                        // it was before the save: the saved values of the
                        // edited fields go into it now, so an edited field
                        // does not show its earlier value meanwhile (#783).
                        if !globals_only {
                            let edited = self.w.document.edited_fields(layout_id, record_id);
                            if let Some(session) = self.w.session.as_mut() {
                                session.show_saved(layout_id, &record, &edited);
                            }
                        }
                        self.w.document.clear_record(layout_id, record_id);
                        self.recovered_record_saved(ctx, layout_id, record_id);
                        self.refresh_other_windows(ctx);
                        if let Some(action) = self.w.deferred_action.take() {
                            // The successful-save refresh below also fulfills
                            // an explicitly deferred Refresh Window command.
                            if action != Action::Refresh {
                                self.resume_saved_action(ctx, action);
                            }
                        }
                        let key = self.w.session.as_mut().and_then(Session::refresh);
                        self.request_page(ctx, key);
                        self.start_transaction(ctx, transaction);
                    }
                    Err(err) => {
                        // The action the save held goes; closing the file
                        // stops with this window open.
                        self.w.deferred_action = None;
                        self.w.document.cancel_pending_focus();
                        self.closing = None;
                        if !self.recheck_refused_save(ctx, layout_id, record_id, &err) {
                            self.fail(&err);
                        }
                    }
                }
            }
            Reply::Deleted {
                layout_id,
                record_id,
                result,
            } => {
                self.w.document.pending = false;
                self.w.in_flight = None;
                self.w.calcs.data_changed();
                match result {
                    Ok(()) => {
                        // The page still holds the record's last values.
                        let shown = self.w.session.as_ref().and_then(|s| {
                            let page = s.page.as_ref()?;
                            let record = page.data.records.iter().find(|r| r.id == record_id)?;
                            Some(record.clone())
                        });
                        let transaction = shown.and_then(|record| {
                            self.window_transaction(layout_id, "Deleted", &record)
                        });
                        self.w.document.clear_record(layout_id, record_id);
                        // What the record held uncommitted (a new record's
                        // being new) goes with it: nothing commits or
                        // reverts a deleted record.
                        if self
                            .w
                            .uncommitted
                            .as_ref()
                            .is_some_and(|(l, r, _)| (*l, *r) == (layout_id, record_id))
                        {
                            self.w.uncommitted = None;
                        }
                        self.refresh_other_windows(ctx);
                        let key = self.w.session.as_mut().and_then(Session::refresh);
                        self.request_page(ctx, key);
                        self.after_action(ctx, &Action::GoToRecord(0));
                        self.start_transaction(ctx, transaction);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            Reply::Script(event) => self.on_script_event(ctx, event),
            // The file's windows take the scripts as read or saved; the
            // Script Workspace takes only the replies to its own requests
            // (`ScriptWorkspace::takes`): never those of a workspace closed
            // or replaced since, of another file, or of the file's own
            // reads, which would put a stale catalog under its drafts.
            Reply::Scripts { workspace, result } => {
                let file = self.w.file_id;
                for doc in std::iter::once(&mut self.w).chain(&mut self.windows) {
                    if doc.file_id != file {
                        continue;
                    }
                    match &result {
                        Ok(catalog) => {
                            doc.scripts = catalog.clone();
                            doc.scripts_read = windows::ScriptsRead::Read;
                        }
                        // The Scripts menu asks again when next shown.
                        Err(_) if doc.scripts_read == windows::ScriptsRead::Asked => {
                            doc.scripts_read = windows::ScriptsRead::Unread;
                        }
                        Err(_) => {}
                    }
                }
                match workspace {
                    Some(token) => {
                        if let Some(workspace) = self.workspace_reply(token) {
                            workspace.accept_catalog(result);
                        }
                    }
                    None => {
                        if let Err(error) = result {
                            self.fail(&error);
                        }
                    }
                }
            }
            Reply::ScriptSaved {
                workspace: token,
                result,
            } => {
                if let Ok(script) = &result {
                    if let Some(existing) = self
                        .w
                        .scripts
                        .scripts
                        .iter_mut()
                        .find(|s| s.id == script.id)
                    {
                        *existing = script.clone();
                    } else {
                        self.w.scripts.scripts.push(script.clone());
                    }
                }
                if let Some(workspace) = self.workspace_reply(token) {
                    let workspace_actions = workspace.accept_saved(result);
                    self.workspace_after_save(ctx, workspace_actions);
                }
            }
            Reply::ScriptCatalogSaved {
                workspace: token,
                result,
            } => {
                if let Ok(catalog) = &result {
                    let file = self.w.file_id;
                    for doc in std::iter::once(&mut self.w).chain(&mut self.windows) {
                        if doc.file_id == file {
                            doc.scripts = catalog.clone();
                        }
                    }
                }
                if let Some(workspace) = self.workspace_reply(token) {
                    workspace.accept_catalog_saved(result);
                }
            }
            Reply::Schema { result, changed } => {
                // OK's answer: saved, the dialog closes; refused, it stays
                // with the reason and shows the edit as the provider has
                // it now.
                if changed {
                    match &result {
                        Ok(_) => self.database = None,
                        Err(error) => {
                            if let Some(database) = &mut self.database {
                                database.save_failed(error.clone());
                            }
                            self.request(ctx, Request::BeginSchemaEdit);
                        }
                    }
                }
                if changed && result.is_ok() {
                    // Every window of the file shows the changed schema.
                    let file = self.w.file_id;
                    for id in self.window_ids() {
                        self.with_window(id, |app| {
                            if app.w.file_id != file {
                                return;
                            }
                            app.w.document.clear_layout_cache();
                            app.w.calcs.data_changed();
                            let key = app.w.session.as_mut().and_then(Session::refresh);
                            app.request_layout(ctx, key);
                        });
                    }
                    // And its new tables and fields: the provider read the
                    // catalogs again on commit, but the windows keep the
                    // calculation schema they asked for until they ask
                    // again, and Layout mode's Field tool lists the fields
                    // of that schema (#726). Its reply sets it on every
                    // window of the file.
                    self.request(ctx, Request::CalcSchema);
                }
                // An edit that could not begin (another user has the
                // schema) is an alert, as in FileMaker Pro.
                match (&mut self.database, result) {
                    (_, _) if changed => {}
                    (Some(database), Err(error)) if !database.has_schema() => {
                        self.database = None;
                        self.error = Some(error.message);
                    }
                    (Some(database), result) => database.accept_schema(result),
                    (None, _) => {}
                }
            }
            Reply::Security { result, .. } => {
                if let Some(security) = &mut self.security {
                    security.accept(result);
                }
            }
            Reply::ValueLists { result, .. } => {
                if let Some(dialog) = &mut self.value_lists {
                    dialog.accept(result);
                }
            }
            Reply::PasswordChanged(result) => match result {
                Ok(()) => self.own_password = None,
                Err(error) => {
                    if let Some((_, _, dialog)) = &mut self.own_password {
                        dialog.pending = false;
                        dialog.error = Some(error.message);
                    }
                }
            },
            Reply::RelatedSignIn { file, result } => {
                let asked = self
                    .related_login
                    .as_mut()
                    .filter(|login| login.file == file)
                    .map(|login| &mut login.dialog);
                match (result, asked) {
                    // The schema is built again with the file's tables. A
                    // Reconnect keeps no password, so after one the file
                    // refuses again and is asked about again.
                    (Ok(()), _) => {
                        self.related_login = None;
                        self.related_asked.remove(&(self.w.file_id, file));
                        self.request(ctx, Request::CalcSchema);
                    }
                    // A refusal (one Login, noted for the session): the
                    // dialog stays, for another account or Cancel.
                    (Err(error), Some(dialog)) => {
                        dialog.pending = false;
                        dialog.error = Some(error.message);
                    }
                    (Err(_), None) => {}
                }
            }
            Reply::CalcSchema {
                result,
                warnings,
                sign_ins,
            } => self.accept_calc_schema(result, warnings, sign_ins, false),
            Reply::HostFieldValues {
                list,
                epoch,
                result,
            } => {
                self.w.calcs.accept_host_list(list, epoch, result);
                ctx.request_repaint();
            }
            Reply::ValueListBuilt {
                list,
                ticket,
                result,
                retry_after,
            } => {
                self.w
                    .calcs
                    .accept_built_list(list, ticket, result, retry_after);
                ctx.request_repaint();
            }
            Reply::CalcData { needs, result } => {
                // A lost connection has its own banner; its calculation
                // errors add nothing to it.
                let lost = matches!(result.as_ref(), Err(error) if error.lost);
                let generation = self.w.calcs.cache.borrow().generation;
                if let Some(report) = self.w.calcs.accept_data(&needs, *result) {
                    if !lost {
                        self.error = Some(report);
                    }
                }
                if self.w.calcs.cache.borrow().generation != generation {
                    if let Some(session) = &self.w.session {
                        session.report_data_changed();
                    }
                }
            }
            Reply::Closed => {}
            Reply::LayoutFieldPlaced(result) => {
                self.w.layout_workspace.saving = false;
                match result {
                    Ok(placed) => {
                        let field = self.w.layout_workspace.new_field().cloned();
                        self.w.layout_workspace.field_placed();
                        if let Some(field) = field {
                            if let Some(from) = field.revision.filter(|_| placed.changed) {
                                self.w.layout_workspace.rebase(from, placed.revision);
                            }
                            self.w.layout_workspace.set_placed_control(
                                placed.field_object,
                                (field.control, field.value_list),
                                // A repeated acknowledgement can include another
                                // client's changes. It cannot authorize new edits.
                                if placed.changed {
                                    placed.revision
                                } else {
                                    field.revision.unwrap_or(placed.revision)
                                },
                            );
                        }
                        self.reread_file_layouts(ctx);
                        self.save_next(ctx, None);
                    }
                    Err(error) => {
                        // No completion or refusal consumes an unsaved field.
                        // In particular, a changed table/revision cannot rebind it.
                        let reread = !error.lost;
                        self.fail(&error);
                        if reread {
                            self.reread_file_layouts(ctx);
                        }
                    }
                }
            }
            Reply::LayoutButtonPlaced(result) => self.layout_object_placed(ctx, result.map(|_| ())),
            Reply::LayoutPortalPlaced(result) => self.layout_object_placed(ctx, result.map(|_| ())),
            Reply::LayoutTextPlaced(result) => self.layout_object_placed(ctx, result.map(|_| ())),
            Reply::ObjectsFramed(result) => self.objects_framed(ctx, result),
            Reply::LayoutWidthSet(result) => self.width_set(ctx, result),
            Reply::FieldControlSet(result) => self.control_set(ctx, result),
            Reply::LayoutTextSet(result) => self.words_set(ctx, result),
            Reply::ObjectsDeleted(result) => self.objects_deleted(ctx, result),
            Reply::LayoutCreated(result) => self.layout_created(ctx, result),
            Reply::LayoutDeleted(result) => self.layout_deleted(ctx, result),
            Reply::PartOptionsSet(sent, result) => {
                // Part Setup shows the part as saved, at the layout's new
                // revision; every window of the file reads the layout again.
                // Only the dialog that sent the change takes its answer.
                let revision = result.as_ref().ok().map(|set| set.revision);
                self.w.layout_workspace.parts_answered(
                    &PartsSent::Options(sent),
                    layout_change_done(&result),
                    revision,
                );
                let window = self.w.id;
                let answered = self
                    .part_setup
                    .as_mut()
                    .filter(|d| d.window == window)
                    .is_some_and(|dialog| dialog.answered(&sent, revision));
                self.part_change_answered(ctx, answered, result);
            }
            Reply::LayoutPartsSet(sent, result) => {
                // Part Setup shows the parts keyed again, at the layout's new
                // revision; every window of the file reads the layout again.
                let revision = result.as_ref().ok().map(|set| set.revision);
                self.w.layout_workspace.parts_answered(
                    &PartsSent::Parts(sent.clone()),
                    layout_change_done(&result),
                    revision,
                );
                let window = self.w.id;
                let answered = self
                    .part_setup
                    .as_mut()
                    .filter(|d| d.window == window)
                    .is_some_and(|dialog| dialog.parts_answered(&sent, revision));
                self.part_change_answered(ctx, answered, result);
            }
            Reply::CustomValueList(list, result) => match result {
                Ok(custom) => {
                    let dialog = crate::document::value_dialogs::EditDialog::new(list, custom);
                    self.value_list_edit = Some((self.w.id, dialog));
                }
                Err(err) => self.fail(&err),
            },
            Reply::ValueListValuesSet(result) => {
                match &result {
                    Ok(_) => self.value_list_edit = None,
                    // The dialog stays with what was typed: a refusal (a
                    // stale revision, a lock, too long a text) alerts, and
                    // the user changes the text or cancels.
                    Err(err) => {
                        if let Some((_, dialog)) = &mut self.value_list_edit {
                            dialog.pending = false;
                        }
                        self.fail(err);
                    }
                }
                // The menus read the lists from the calculation schema.
                if !result.as_ref().is_err_and(|err| err.lost) {
                    self.request(ctx, Request::CalcSchema);
                }
            }
            Reply::LayoutSetUp(result) => {
                // Every window of the file reads its layouts again: the
                // name and menus in its list, the views in its layout.
                if let Err(err) = &result {
                    self.fail(err);
                }
                if !result.as_ref().is_err_and(|err| err.lost) {
                    let file = self.w.file_id;
                    for id in self.window_ids() {
                        self.with_window(id, |app| {
                            if app.w.file_id == file {
                                app.request(ctx, Request::Layouts);
                            }
                        });
                    }
                    self.reread_file_layouts(ctx);
                }
            }
        }
    }

    fn sync_title(&mut self, ctx: &egui::Context) {
        let title = self.window_title();
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for OpenMakerApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.native_parent = crate::web_viewer::NativeParent::of(frame);
        self.show(ui);
    }
}

/// Whether Save Layout's answer `result` ends a change: saved, not written
/// because the layout changed since it was shown (306), or sent without an
/// answer (no code): the change is dropped and the layout read again,
/// showing what the host stores. Another refusal keeps the change, to be
/// changed.
fn layout_change_done<T>(result: &Result<T, DataError>) -> bool {
    match result {
        Ok(_) => true,
        Err(err) => {
            !err.lost
                && err
                    .code
                    .is_none_or(|code| code == om_data::layout_placement::CONFLICT)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoLicense;
    impl crate::licensing::LicenseStore for NoLicense {
        fn load(&mut self) -> Result<Option<crate::licensing::LicenseKey>, DataError> {
            Ok(None)
        }
        fn save(&mut self, _: &crate::licensing::LicenseKey) -> Result<(), DataError> {
            Ok(())
        }
        fn remove(&mut self) -> Result<(), DataError> {
            Ok(())
        }
    }

    #[test]
    fn view_zoom_commands_and_shortcuts_only_scale_the_form_document() {
        let mut app = OpenMakerApp::with_license_store(
            Box::new(om_data::mock::MockProvider::new()),
            Box::new(NoLicense),
        );
        let context = egui::Context::default();
        context
            .run_ui(egui::RawInput::default(), |ui| app.show(ui))
            .drop_without_applying_deltas();
        assert!(!context.options(|options| options.zoom_with_keyboard));
        app.w.session = Some(Session::new(
            Target {
                host: "test".into(),
                file: "Sample".into(),
                local: None,
            },
            DEFAULT_PAGE_SIZE,
        ));
        app.w.view = ViewMode::Form;
        app.apply(&context, Action::ZoomIn);
        assert_eq!(
            app.w.document.zoom.percent(),
            100,
            "loading layouts do not enable zoom"
        );
        app.w.session.as_mut().unwrap().render =
            crate::session::RenderState::Loaded(Box::new(om_data::RenderLayout {
                layout: om_layout::Layout::default(),
                field_bindings: Default::default(),
            }));
        app.apply(&context, Action::ZoomIn);
        assert_eq!(app.w.document.zoom.percent(), 150);
        app.apply(&context, Action::ZoomOut);
        assert_eq!(app.w.document.zoom.percent(), 100);
        app.apply(&context, Action::ZoomOut);
        app.apply(&context, Action::ActualSize);
        assert_eq!(app.w.document.zoom.percent(), 100);

        let mut actions = Vec::new();
        let input = egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(Modifiers::COMMAND),
                egui::Event::Key {
                    key: Key::Equals,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::COMMAND,
                },
            ],
            ..Default::default()
        };
        context
            .run_ui(input, |ui| app.shortcuts(ui, &mut actions))
            .drop_without_applying_deltas();
        assert_eq!(actions, [Action::ZoomIn]);
        for action in actions {
            app.apply(&context, action);
        }
        assert_eq!(app.w.document.zoom.percent(), 150);
        assert_eq!(context.zoom_factor(), 1.0);

        app.w.view = ViewMode::Table;
        app.apply(&context, Action::ZoomIn);
        assert_eq!(app.w.document.zoom.percent(), 150);
        app.w.view = ViewMode::Form;
        app.w.session.as_mut().unwrap().render = crate::session::RenderState::Unavailable;
        app.apply(&context, Action::ZoomIn);
        assert_eq!(
            app.w.document.zoom.percent(),
            150,
            "generic fallback keeps its existing scale"
        );
    }

    #[test]
    fn script_zoom_uses_the_renderers_current_percentage() {
        let mut app = OpenMakerApp::with_license_store(
            Box::new(om_data::mock::MockProvider::new()),
            Box::new(NoLicense),
        );
        let context = egui::Context::default();
        app.w.session = Some(Session::new(
            Target {
                host: "test".into(),
                file: "Sample".into(),
                local: None,
            },
            DEFAULT_PAGE_SIZE,
        ));
        app.w.document.zoom.zoom_in();
        let mut window = app.window_state();
        assert_eq!(window.zoom, 150);
        window.zoom = 200;
        app.on_script_event(
            &context,
            crate::scripting::ScriptEvent::Window(Box::new(crate::scripting::WindowUpdate {
                file: 0,
                state: window,
                data_changed: false,
                refreshed: false,
                objects: Vec::new(),
                scroll: Vec::new(),
                fronted: Vec::new(),
                originals: Default::default(),
                found_set: None,
                background: false,
            })),
        );
        assert_eq!(app.w.document.zoom.percent(), 200);
        assert_eq!(app.window_state().zoom, 200);
        app.apply(&context, Action::CloseFile);
        assert_eq!(app.w.document.zoom.percent(), 100);
    }

    /// Layout 1: object 1, a rectangle of `fill`, and object 2, a graphic
    /// of a `size` x `size` PNG. Every snapshot reuses these ids.
    fn card(fill: Color32, size: u32) -> om_data::RenderLayout {
        use om_layout::{style::ComponentStyle, Color, LayoutObject, ObjectStyle, StyleSet};
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(size, size)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let object = |id, object_type| LayoutObject {
            id,
            object_type,
            bounds: Some(om_layout::Rect {
                left: 10.0,
                top: 10.0 + 50.0 * id as f64,
                right: 50.0,
                bottom: 50.0 + 50.0 * id as f64,
                ..Default::default()
            }),
            ..Default::default()
        };
        let [red, green, blue, _] = fill.to_array().map(|c| f32::from(c) / 255.0);
        let rectangle = LayoutObject {
            style: Some(ObjectStyle {
                local_styles: Some(StyleSet {
                    components: vec![ComponentStyle {
                        component: 1,
                        state: 1,
                        properties: vec![om_layout::Property {
                            id: 7,
                            color: Some(Color {
                                red,
                                green,
                                blue,
                                alpha: 1.0,
                            }),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..object(1, 5)
        };
        let graphic = LayoutObject {
            graphic: Some(om_layout::GraphicObject {
                library_key: 9,
                ..Default::default()
            }),
            ..object(2, 3)
        };
        om_data::RenderLayout {
            layout: om_layout::Layout {
                id: 1,
                def: Some(om_layout::LayoutDef {
                    base_width: 200.0,
                    ..Default::default()
                }),
                parts: vec![om_layout::Part {
                    part_type: 4,
                    height: 200.0,
                    ..Default::default()
                }],
                objects: om_layout::ObjectStore {
                    root: vec![1, 2],
                    objects: vec![rectangle, graphic],
                    ..Default::default()
                },
                library_images: [(
                    9,
                    om_layout::LibraryImage {
                        format: om_layout::LibraryImageFormat::Png,
                        data: png.into_inner(),
                    },
                )]
                .into(),
                ..Default::default()
            },
            field_bindings: Default::default(),
        }
    }

    /// The rectangle fills and image sizes Browse paints for the loaded
    /// layout, once egui has decoded its images.
    fn shown(app: &mut OpenMakerApp, ctx: &egui::Context) -> (Vec<Color32>, Vec<[usize; 2]>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut decoded = false;
        loop {
            let session = app.w.session.as_ref().unwrap();
            let crate::session::RenderState::Loaded(layout) = &session.render else {
                panic!("no layout");
            };
            let document = &mut app.w.document;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                crate::document::show(
                    ui,
                    session,
                    layout,
                    document,
                    ViewMode::Form,
                    None,
                    &mut Vec::new(),
                );
            });
            let shapes = std::mem::take(&mut output.shapes);
            output.drop_without_applying_deltas();
            // A texture is made the frame after its decoding finishes.
            if decoded {
                let textures = ctx.tex_manager();
                let textures = textures.read();
                let mut shown = (Vec::new(), Vec::new());
                for shape in shapes {
                    if let egui::Shape::Rect(rect) = shape.shape {
                        match &rect.brush {
                            Some(brush) => shown
                                .1
                                .push(textures.meta(brush.fill_texture_id).unwrap().size),
                            // The rectangle, not the window's background.
                            None if rect.rect.size() == egui::vec2(40.0, 40.0) => {
                                shown.0.push(rect.fill);
                            }
                            None => {}
                        }
                    }
                }
                return shown;
            }
            decoded = !ctx.has_pending_images();
            assert!(std::time::Instant::now() < deadline, "images never loaded");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn every_accepted_layout_snapshot_clears_the_layout_cache() {
        let mut app = OpenMakerApp::with_license_store(
            Box::new(om_data::mock::MockProvider::new()),
            Box::new(NoLicense),
        );
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let open = |file: &str| {
            let mut session = Session::new(
                Target {
                    host: "test".into(),
                    file: file.into(),
                    local: None,
                },
                DEFAULT_PAGE_SIZE,
            );
            let key = session
                .set_layouts(vec![om_data::LayoutInfo {
                    id: 1,
                    name: "Card".into(),
                }])
                .unwrap();
            session
                .accept_page(
                    key,
                    Ok(om_data::RecordPage {
                        versions: Default::default(),
                        fields: Vec::new(),
                        records: vec![om_data::Record {
                            id: 1,
                            values: Vec::new(),
                        }],
                        total: 1,
                    }),
                )
                .unwrap();
            session
        };
        let render = |app: &mut OpenMakerApp, card| {
            app.on_reply(
                &ctx,
                Reply::RenderLayout {
                    layout_id: 1,
                    result: Box::new(Ok(Some(card))),
                },
            );
        };
        let yellow = Color32::from_rgb(255, 255, 0);
        app.w.session = Some(open("First"));
        render(&mut app, card(Color32::RED, 2));
        assert_eq!(shown(&mut app, &ctx), (vec![Color32::RED], vec![[2, 2]]));
        // The same file's layout again, changed under the same ids.
        render(&mut app, card(Color32::GREEN, 3));
        assert_eq!(shown(&mut app, &ctx), (vec![Color32::GREEN], vec![[3, 3]]));
        // Another hosted file whose layout has the same ids.
        app.w.session = Some(open("Second"));
        render(&mut app, card(Color32::BLUE, 4));
        assert_eq!(shown(&mut app, &ctx), (vec![Color32::BLUE], vec![[4, 4]]));
        // Reselecting the layout replaces it before its page arrives; a
        // lost connection then puts the shown snapshot back.
        app.w.session.as_mut().unwrap().select_layout(1);
        render(&mut app, card(yellow, 5));
        assert_eq!(shown(&mut app, &ctx), (vec![yellow], vec![[5, 5]]));
        app.lose_connection(&DataError::connection_lost("The host went away."));
        assert_eq!(shown(&mut app, &ctx), (vec![Color32::BLUE], vec![[4, 4]]));
    }

    #[test]
    fn a_lost_connection_clears_the_restored_layout_cache_in_every_window() {
        let mut app = two_record_windows();
        let contexts = [egui::Context::default(), egui::Context::default()];
        for ctx in &contexts {
            theme::install(ctx);
        }
        let originals = [(Color32::RED, 2), (Color32::BLUE, 3)];
        let replacements = [(Color32::GREEN, 4), (Color32::YELLOW, 5)];
        for (index, ctx) in contexts.iter().enumerate() {
            app.with_window(index as u32 + 1, |app| {
                let (color, size) = originals[index];
                app.on_reply(
                    ctx,
                    Reply::RenderLayout {
                        layout_id: 1,
                        result: Box::new(Ok(Some(card(color, size)))),
                    },
                );
                assert_eq!(shown(app, ctx), (vec![color], vec![[size as usize; 2]]));
                // Warm the cache with a replacement under the same ids while
                // the previous snapshot is still kept for a pending page.
                app.w.session.as_mut().unwrap().select_layout(1);
                let (color, size) = replacements[index];
                app.on_reply(
                    ctx,
                    Reply::RenderLayout {
                        layout_id: 1,
                        result: Box::new(Ok(Some(card(color, size)))),
                    },
                );
                assert_eq!(shown(app, ctx), (vec![color], vec![[size as usize; 2]]));
            })
            .unwrap();
        }
        app.lose_connection(&DataError::connection_lost("The host went away."));
        assert_eq!(app.w.id, 1, "the front window is unchanged");
        for (index, ctx) in contexts.iter().enumerate() {
            app.with_window(index as u32 + 1, |app| {
                let (color, size) = originals[index];
                assert_eq!(shown(app, ctx), (vec![color], vec![[size as usize; 2]]));
                assert!(app.w.session.as_ref().unwrap().pending.is_none());
            })
            .unwrap();
        }
    }

    /// FileMaker Pro 26.0.3 shows a layout the window goes back to in the
    /// view it last had there, not in the window's current view, also when
    /// that view is not the one the layout stores (#805, measured on two
    /// layouts of one table: A stored Form, B stored List).
    #[test]
    fn each_layout_comes_back_in_the_view_it_last_had() {
        let ctx = egui::Context::default();
        let mut app = OpenMakerApp::new(Box::new(om_data::mock::MockProvider::new()));
        app.w = window_with_records(1, &["Ada"]);
        let render = |app: &mut OpenMakerApp, layout_id: u32, flags: u64| {
            let layout = om_data::RenderLayout {
                layout: om_layout::Layout {
                    id: u64::from(layout_id),
                    def: Some(om_layout::LayoutDef {
                        flags,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            app.on_reply(
                &ctx,
                Reply::RenderLayout {
                    layout_id,
                    result: Box::new(Ok(Some(layout))),
                },
            );
        };
        const FORM: u64 = 0;
        const LIST: u64 = 0x200;
        render(&mut app, 1, FORM);
        assert_eq!(app.view(), ViewMode::Form);
        app.apply(&ctx, Action::SelectLayout(2));
        render(&mut app, 2, LIST);
        assert_eq!(app.view(), ViewMode::List);
        // Back on A: Form at once, before its render arrives, and after.
        app.apply(&ctx, Action::SelectLayout(1));
        assert_eq!(app.view(), ViewMode::Form);
        render(&mut app, 1, FORM);
        assert_eq!(app.view(), ViewMode::Form);
        // A in Table, B, then A again: Table, which A does not store.
        app.apply(&ctx, Action::SetView(ViewMode::Table));
        app.apply(&ctx, Action::SelectLayout(2));
        assert_eq!(app.view(), ViewMode::List);
        render(&mut app, 2, LIST);
        app.apply(&ctx, Action::SelectLayout(1));
        render(&mut app, 1, FORM);
        assert_eq!(app.view(), ViewMode::Table);
        // A script starts from the window's views and leaves its own.
        let state = app.window_state();
        assert_eq!(state.views.get(&2), Some(&crate::scripting::View::List));
        assert_eq!(state.views.get(&1), Some(&crate::scripting::View::Table));
        // A view the layout no longer offers (Layout Setup turned Table
        // off) gives way to the one it stores.
        const TABLE_OFF: u64 = 8;
        render(&mut app, 1, FORM | TABLE_OFF);
        assert_eq!(app.view(), ViewMode::Form);
        // A layout left before its first render arrives is not given the
        // view the window still had from the layout before it: C, stored
        // Form, left while B's List showed, then loads in Form.
        app.apply(&ctx, Action::SelectLayout(2));
        render(&mut app, 2, LIST);
        app.apply(&ctx, Action::SelectLayout(3));
        app.apply(&ctx, Action::SelectLayout(2));
        app.apply(&ctx, Action::SelectLayout(3));
        render(&mut app, 3, FORM);
        assert_eq!(app.view(), ViewMode::Form);
        // A lost connection puts back the layout last shown with its
        // records (A) in A's view, not the List of B switched to.
        app.apply(&ctx, Action::SelectLayout(2));
        assert_eq!(app.view(), ViewMode::List);
        app.lose_connection(&DataError::connection_lost("The host went away."));
        assert_eq!(app.w.session.as_ref().unwrap().layout, Some(1));
        assert_eq!(app.view(), ViewMode::Form);
    }

    #[test]
    fn find_requests_add_delete_and_navigate() {
        let mut find = FindState::default();
        assert_eq!(find.len(), 1);
        find.delete();
        assert_eq!(find.len(), 1, "the last request stays");

        find.current_mut().set("Last Name", "Smith");
        find.add(find.current().clone());
        assert_eq!((find.len(), find.current_index()), (2, 1));
        assert_eq!(find.current().get("Last Name"), Some("Smith"));

        find.add(FindRequest::default());
        find.go_to(99);
        assert_eq!(find.current_index(), 2);
        find.delete();
        assert_eq!((find.len(), find.current_index()), (2, 1));
    }

    /// An app with windows 1 (in front), 2 and 3.
    fn three_windows() -> OpenMakerApp {
        let mut app = OpenMakerApp::new(Box::new(om_data::mock::MockProvider::new()));
        app.windows = vec![windows::Doc::new(2), windows::Doc::new(3)];
        app
    }

    #[test]
    fn working_on_another_window_gives_the_front_back_by_id() {
        let mut app = three_windows();
        assert_eq!(app.with_window(3, |app| app.w.id), Some(3));
        assert_eq!(app.window_ids(), [1, 3, 2]);
        // A window the file does not have.
        assert_eq!(app.with_window(9, |_| ()), None);
        // Another window added meanwhile does not move the front.
        app.with_window(2, |app| app.windows.push(windows::Doc::new(4)));
        assert_eq!(app.w.id, 1);
    }

    #[test]
    fn a_window_brought_to_the_front_from_another_window_stays_in_front() {
        let mut app = three_windows();
        app.w.file_id = 1;
        app.windows[0].file_id = 2;
        app.windows[1].file_id = 1;
        app.security = Some(crate::security::ManageSecurity::new(1, "First".into()));
        app.value_lists = Some(crate::value_lists::ManageValueLists::new(1, "First".into()));
        // Window 3's Window menu picks a different file's window 2.
        app.with_window(3, |app| app.bring_to_front(2));
        assert!(app.security.is_none());
        assert!(app.value_lists.is_none());
        assert_eq!(app.w.id, 2);
        assert_eq!(app.window_ids().len(), 3);
    }

    #[test]
    fn security_catalog_replies_belong_to_the_file_and_dialog_request() {
        let mut app = three_windows();
        let ctx = egui::Context::default();
        let (worker, requests) = worker::Worker::capture_jobs();
        app.worker = worker;
        app.w.file_id = 1;
        app.w.session = Some(Session::new(
            Target {
                host: "test".into(),
                file: "First".into(),
                local: None,
            },
            DEFAULT_PAGE_SIZE,
        ));
        app.security = Some(crate::security::ManageSecurity::new(1, "First".into()));
        app.request_security_catalog(&ctx);
        let first = requests.try_recv().unwrap();
        let Request::SecurityCatalog { token: old } = first.request else {
            panic!("expected a security catalog request");
        };
        assert_eq!(first.window, 1);
        app.request_security_catalog(&ctx);
        let next = requests.try_recv().unwrap();
        let Request::SecurityCatalog { token } = next.request else {
            panic!("expected a security catalog request");
        };
        assert_ne!(old, token);
        let refused = |token| Reply::Security {
            token,
            result: Err(DataError::connection_lost("obsolete catalog")),
        };
        app.on_reply(&ctx, refused(old));
        assert!(app.w.connection_lost.is_none());
        app.w.file_id = 2;
        app.request_security_catalog(&ctx);
        assert!(
            requests.try_recv().is_err(),
            "another file cannot refresh the dialog"
        );
        app.on_reply(&ctx, refused(token));
        assert!(app.w.connection_lost.is_none());
        app.w.file_id = 1;
        app.on_reply(&ctx, refused(token));
        assert!(
            app.w.connection_lost.is_some(),
            "the current request reaches its owner"
        );
    }

    /// File 1's Manage Value Lists asks for its catalog and file 2's
    /// dialog replaces it: file 1's late answer, whichever window it comes
    /// back to, must not fill file 2's dialog, nor an answer to a dialog
    /// since closed the one reopened in its place.
    #[test]
    fn a_value_list_reply_fills_only_the_dialog_that_asked_for_it() {
        let mut app = three_windows();
        let ctx = egui::Context::default();
        let (worker, requests) = worker::Worker::capture_jobs();
        app.worker = worker;
        app.w.session = Some(Session::new(
            Target {
                host: "test".into(),
                file: "First".into(),
                local: None,
            },
            DEFAULT_PAGE_SIZE,
        ));
        let ask = |app: &mut OpenMakerApp, file: u64| {
            app.w.file_id = file;
            app.value_lists = Some(crate::value_lists::ManageValueLists::new(
                file,
                "File".into(),
            ));
            app.request_value_lists(&ctx);
            let Request::ValueLists { token } = requests.try_recv().unwrap().request else {
                panic!("expected a value-list catalog request");
            };
            token
        };
        let lost = |token| Reply::ValueLists {
            token,
            result: Err(DataError::connection_lost("an earlier catalog")),
        };
        let first = ask(&mut app, 1);
        let second = ask(&mut app, 2);
        assert_ne!(first, second);
        app.w.file_id = 1;
        app.on_reply(&ctx, lost(first));
        app.w.file_id = 2;
        app.on_reply(&ctx, lost(first));
        assert!(app.w.connection_lost.is_none());
        // Closed and reopened: the earlier answer is stale too.
        let reopened = ask(&mut app, 2);
        app.on_reply(&ctx, lost(second));
        assert!(app.w.connection_lost.is_none());
        app.on_reply(&ctx, lost(reopened));
        assert!(
            app.w.connection_lost.is_some(),
            "the current request reaches its dialog"
        );
    }

    #[test]
    fn closing_the_file_or_the_front_window_from_another_window_does_not_panic() {
        let mut app = three_windows();
        // Window 2 closes the front window: window 2 stays in front.
        app.with_window(2, |app| app.remove_window(1));
        assert_eq!(app.window_ids(), [2, 3]);
        // Window 3 empties the file's windows.
        app.with_window(3, |app| {
            app.windows.clear();
            app.w = windows::Doc::new(windows::FIRST);
        });
        assert_eq!(app.window_ids(), [windows::FIRST]);
    }

    /// Window `id` on a one-field layout showing records named `names`,
    /// the first current, editable.
    fn window_with_records(id: u32, names: &[&str]) -> windows::Doc {
        let mut session = Session::new(
            Target {
                host: "host".into(),
                file: "Tasks.fmp12".into(),
                local: None,
            },
            DEFAULT_PAGE_SIZE,
        );
        let key = session.select_layout(1);
        let page = om_data::RecordPage {
            versions: Default::default(),
            fields: vec![om_data::Field {
                name: "Name".into(),
                kind: om_data::FieldKind::Text,
            }],
            records: names
                .iter()
                .enumerate()
                .map(|(i, name)| om_data::Record {
                    id: i as i64 + 1,
                    values: vec![Some((*name).to_owned())],
                })
                .collect(),
            total: names.len() as u64,
        };
        session.accept_page(key, Ok(page)).unwrap();
        let mut doc = windows::Doc::new(id);
        doc.session = Some(session);
        doc.document.capabilities.update = true;
        doc
    }

    /// Window 1 in front on "Ada", window 2 behind it on "Bob" of its own
    /// found set "Bob", "Cy".
    pub(super) fn two_record_windows() -> OpenMakerApp {
        let mut app = OpenMakerApp::new(Box::new(om_data::mock::MockProvider::new()));
        app.w = window_with_records(1, &["Ada"]);
        app.windows = vec![window_with_records(2, &["Bob", "Cy"])];
        app
    }

    #[test]
    fn a_repeated_field_acknowledgement_keeps_other_drafts_original_revision() {
        for changed in [true, false] {
            let ctx = egui::Context::default();
            let mut app = two_record_windows();
            let session = app.w.session.as_mut().unwrap();
            session.geometry = crate::session::Geometry::Loaded(om_data::LayoutGeometry {
                revision: Some([1; 32]),
                ..Default::default()
            });
            for id in [1, 2] {
                app.w.layout_workspace.stage_field(
                    session,
                    crate::layout_canvas::NewField {
                        left: 20.0,
                        top: id as f64 * 30.0,
                        width: 100.0,
                        height: 22.0,
                        field: Some(crate::layout_canvas::FieldChoice {
                            table: 1,
                            field: id,
                            name: format!("Field{id}"),
                            date: false,
                        }),
                        label: false,
                        control: 0,
                        value_list: None,
                        revision: Some([1; 32]),
                    },
                );
            }
            app.on_response(
                &ctx,
                worker::Response {
                    epoch: app.epoch,
                    window: app.w.id,
                    reply: Reply::LayoutFieldPlaced(Ok(
                        om_data::layout_placement::PlacedLayoutField {
                            changed,
                            field_object: 1,
                            label_object: None,
                            revision: [2; 32],
                        },
                    )),
                },
            );
            let remaining = app.w.layout_workspace.new_field().unwrap();
            assert_eq!(remaining.field.as_ref().unwrap().field, 2);
            assert_eq!(
                remaining.revision,
                Some(if changed { [2; 32] } else { [1; 32] })
            );
        }
    }

    #[test]
    fn close_popover_from_a_script_behind_targets_its_own_document() {
        use crate::scripting::{ScriptEvent, Start};
        use std::time::{Duration, Instant};
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        // Publish both synthetic windows through the ordinary root job before
        // addressing the worker by the background window's id.
        app.start_script(
            &ctx,
            Start::Button {
                step: Box::new(om_data::ScriptStep::new(103)),
                formula: None,
                literal: String::new(),
                portal: None,
            },
        );
        let registered = Instant::now() + Duration::from_secs(30);
        while app.script.running {
            match app.worker.poll() {
                Ok(response) => app.on_response(&ctx, response),
                Err(worker::PollError::Empty) => std::thread::sleep(Duration::from_millis(5)),
                Err(worker::PollError::Stopped) => panic!("worker stopped during setup"),
            }
            assert!(
                Instant::now() < registered,
                "window registration did not finish"
            );
        }
        assert!(app.error.is_none(), "{:?}", app.error);
        for window in [1, 2] {
            app.with_window(window, |app| {
                app.w.document.hold_popover(1, 3);
                app.w.document.hold_popover(7, 9);
                app.w.document.active = Some("Name".into());
                let session = app.w.session.as_ref().unwrap();
                app.w
                    .document
                    .set_field(session, "Name", format!("draft {window}"))
                    .unwrap();
            });
        }
        // Start the actual worker/runtime step in window 2 while window 1
        // stays in front. Its dismissal must be addressed to window 2,
        // even before a later Window/Finished reply brings it forward.
        app.with_window(2, |app| {
            app.start_script(
                &ctx,
                Start::Button {
                    step: Box::new(om_data::ScriptStep::new(169)),
                    formula: None,
                    literal: String::new(),
                    portal: None,
                },
            )
        });
        assert_eq!(app.w.id, 1);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut dismissed = false;
        while app.script.running {
            match app.worker.poll() {
                Ok(response) => {
                    let close = matches!(
                        response.reply,
                        Reply::Script(ScriptEvent::ClosePopover { .. })
                    );
                    if close {
                        assert_eq!(response.window, 2);
                        assert_eq!(app.w.id, 1);
                    }
                    app.on_response(&ctx, response);
                    if close {
                        dismissed = true;
                        assert_eq!(app.w.id, 1);
                        assert_eq!(app.w.document.active.as_deref(), Some("Name"));
                        assert_eq!(app.w.document.popovers().get(&1), Some(&3));
                        app.with_window(2, |app| {
                            assert_eq!(app.w.document.popovers().get(&1), None);
                            assert_eq!(app.w.document.popovers().get(&7), Some(&9));
                            assert_eq!(app.w.document.active, None);
                        });
                    }
                }
                Err(worker::PollError::Empty) => std::thread::sleep(Duration::from_millis(5)),
                Err(worker::PollError::Stopped) => panic!("worker stopped"),
            }
            assert!(
                Instant::now() < deadline,
                "Close Popover script did not finish"
            );
        }
        assert!(dismissed);
        assert_eq!(name_draft(&mut app, 1).as_deref(), Some("draft 1"));
        assert_eq!(name_draft(&mut app, 2).as_deref(), Some("draft 2"));
        assert!(app.error.is_none(), "{:?}", app.error);
        // A late dismissal for a document that closed is discarded.
        app.remove_window(2);
        app.on_response(
            &ctx,
            worker::Response {
                epoch: app.epoch,
                window: 2,
                reply: Reply::Script(ScriptEvent::ClosePopover {
                    layout: 1,
                    popover: 3,
                    button: None,
                }),
            },
        );
        assert_eq!(app.w.document.popovers().get(&1), Some(&3));
        assert_eq!(app.w.document.active.as_deref(), Some("Name"));
    }

    fn name_draft(app: &mut OpenMakerApp, window: u32) -> Option<String> {
        app.with_window(window, |app| {
            let session = app.w.session.as_ref()?;
            app.w
                .document
                .changes(session)
                .get("Name")
                .cloned()
                .flatten()
        })
        .flatten()
    }

    #[test]
    fn a_vetoed_field_exit_gives_the_focus_back_in_its_own_window() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        let owner = app.with_window(2, |app| app.key_owner()).unwrap();
        app.resume_held(
            &ctx,
            2,
            scripts::Held::Exit(("Name".into(), None), false, 1, None, owner, None),
            false,
            None,
        );
        assert_eq!(app.w.document.focus, None);
        assert_eq!(app.windows[0].document.focus.as_deref(), Some("Name"));
    }

    #[test]
    fn a_held_move_goes_on_in_its_window_and_not_in_another_after_it_closed() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        // Window 2's Next, held while window 1 is in front.
        app.resume_held(
            &ctx,
            2,
            scripts::Held::Action(Action::GoToRecord(1), 1),
            true,
            None,
        );
        let current = |app: &OpenMakerApp, i: Option<usize>| {
            let doc = i.map_or(&app.w, |i| &app.windows[i]);
            doc.session.as_ref().map(|s| s.current)
        };
        assert_eq!(current(&app, Some(0)), Some(1));
        assert_eq!(current(&app, None), Some(0));
        // Window 2 closed before its trigger let the move go on: window 1
        // does not take it.
        app.windows.clear();
        app.resume_held(
            &ctx,
            2,
            scripts::Held::Action(Action::GoToRecord(1), 1),
            true,
            None,
        );
        assert_eq!(current(&app, None), Some(0));
        assert_eq!(app.w.deferred_action, None);
    }

    #[test]
    fn a_run_after_a_workspace_save_waits_for_the_front_windows_frame() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        // The save came back to window 2, behind the front one.
        app.with_window(2, |app| {
            app.workspace_after_save(
                &ctx,
                vec![crate::script_workspace::ScriptWorkspaceAction::Run(5)],
            )
        });
        assert!(!app.script.running, "not started outside a frame");
        assert_eq!(app.workspace_followups, [Action::RunScript(5)]);

        // A save's answer is the workspace's that asked. File 1's save is
        // delayed while file 2's window comes to the front and file 2's
        // workspace, with a script of the same id, waits for its own.
        use crate::script_workspace::tests::{drafted, save_draft, state};
        let (worker, requests) = worker::Worker::capture_jobs();
        app.worker = worker;
        app.workspace_followups.clear();
        app.w.file_id = 1;
        app.windows[0].file_id = 2;
        let saves = || match requests.try_recv().unwrap().request {
            Request::SaveScript { workspace, .. } => workspace,
            other => panic!("expected a script save, not {other:?}"),
        };
        let mut first = drafted(1);
        let asked = save_draft(&mut first);
        app.workspace = Some(first);
        app.workspace_actions(&ctx, asked, &mut Vec::new());
        let old = saves();
        app.bring_to_front(2);
        assert!(app.workspace.is_none(), "another file's window drops it");
        let mut second = drafted(2);
        let asked = save_draft(&mut second);
        app.workspace = Some(second);
        app.workspace_actions(&ctx, asked, &mut Vec::new());
        let new = saves();
        assert_ne!(old, new);
        let saved = |workspace| Reply::ScriptSaved {
            workspace,
            result: Ok(om_data::Script {
                id: 2,
                name: "Original".into(),
                ..Default::default()
            }),
        };
        // File 1's answer, in its own window: its scripts take the saved
        // one; file 2's draft, wait and follow-ups stay, also on a failure.
        app.with_window(1, |app| app.on_reply(&ctx, saved(old)));
        assert_eq!(state(app.workspace.as_ref().unwrap()), (true, true, None));
        assert!(app.workspace_followups.is_empty());
        let first_window = app.windows.iter().find(|d| d.id == 1).unwrap();
        assert!(first_window.scripts.script(2).is_some());
        app.with_window(1, |app| {
            app.on_reply(
                &ctx,
                Reply::ScriptSaved {
                    workspace: old,
                    result: Err(DataError::new("refused")),
                },
            )
        });
        assert_eq!(state(app.workspace.as_ref().unwrap()), (true, true, None));
        // File 2's workspace closed and opened again does not take the
        // answer its predecessor waited for.
        app.workspace = Some(drafted(2));
        app.on_reply(&ctx, saved(new));
        assert_eq!(state(app.workspace.as_ref().unwrap()), (false, true, None));
        // Its own answer it takes.
        let asked = save_draft(app.workspace.as_mut().unwrap());
        app.workspace_actions(&ctx, asked, &mut Vec::new());
        let current = saves();
        app.on_reply(&ctx, saved(current));
        assert_eq!(state(app.workspace.as_ref().unwrap()), (false, false, None));
    }

    #[test]
    fn a_scripts_change_to_a_window_behind_leaves_it_behind() {
        use crate::scripting::{ScriptEvent, WindowUpdate};
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        let epoch = app.epoch;
        let update = |state, background| worker::Response {
            epoch,
            window: 1,
            reply: Reply::Script(ScriptEvent::Window(Box::new(WindowUpdate {
                file: 0,
                state,
                data_changed: false,
                refreshed: false,
                objects: Vec::new(),
                scroll: Vec::new(),
                fronted: Vec::new(),
                originals: Default::default(),
                found_set: None,
                background,
            }))),
        };
        // Set Window Title of window 2, from a script in window 1.
        let mut behind = app.with_window(2, |app| app.window_state()).unwrap();
        behind.title = Some("Renamed".into());
        app.on_response(&ctx, update(behind.clone(), true));
        assert_eq!(app.window_ids(), [1, 2]);
        assert_eq!(app.windows[0].title.as_deref(), Some("Renamed"));
        // New Window, then Select Window back: the new window opens behind.
        let opened = crate::scripting::WindowState {
            id: 3,
            ..behind.clone()
        };
        app.on_response(&ctx, update(opened, true));
        assert_eq!(app.window_ids(), [1, 3, 2]);
        // The window the script works in comes to the front.
        app.on_response(&ctx, update(behind, false));
        assert_eq!(app.window_ids()[0], 2);
    }

    #[test]
    fn a_refused_save_behind_cancels_its_click_but_not_the_front_windows_focus() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        let epoch = app.epoch;
        // Window 2's click waits for its save; window 1's editor has the
        // focus of the viewport that takes the reply.
        app.windows[0].document.click_during_save(1, 1, 7);
        let editor = egui::Id::unique("window 1's editor");
        ctx.memory_mut(|memory| memory.request_focus(editor));
        app.on_response(
            &ctx,
            worker::Response {
                epoch,
                window: 2,
                reply: Reply::Updated {
                    layout_id: 1,
                    record_id: 1,
                    result: Err(DataError::new("refused")),
                },
            },
        );
        assert_eq!(app.windows[0].document.pending_click(), None);
        assert_eq!(app.error.as_deref(), Some("refused"));
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(editor));
    }

    #[test]
    fn a_saved_field_shows_its_saved_value_before_the_page_is_read_again() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        let epoch = app.epoch;
        let session = app.w.session.as_ref().unwrap();
        app.w
            .document
            .set_field(session, "Name", "ada l.".into())
            .unwrap();
        app.on_response(
            &ctx,
            worker::Response {
                epoch,
                window: 1,
                reply: Reply::Updated {
                    layout_id: 1,
                    record_id: 1,
                    result: Ok(om_data::Record {
                        id: 1,
                        values: vec![Some("Ada L.".into())],
                    }),
                },
            },
        );
        // The page is being read again; meanwhile the field shows what the
        // save kept, not "Ada" from before it (#783).
        let session = app.w.session.as_ref().unwrap();
        assert!(session.pending.is_some());
        assert!(app.w.document.edited_fields(1, 1).is_empty());
        assert_eq!(
            app.w.document.value(session, "Name").as_deref(),
            Some("Ada L.")
        );
    }

    /// A record page keeps the window's calculation inputs when the
    /// provider read it under the generation they were read under, and
    /// drops them otherwise (#1042): the reply alone decides, as for a
    /// window whose calculations ask for nothing more.
    #[test]
    fn a_page_reply_keeps_calculation_inputs_only_under_their_generation() {
        let ctx = egui::Context::default();
        let mut app = OpenMakerApp::new(Box::new(om_data::mock::MockProvider::new()));
        app.w = window_with_records(1, &["Ada"]);
        let epoch = app.epoch;
        let (table, occurrence, other) = (0x8001, 0xd0_0001, 0x8002);
        let mut schema = om_data::host::CalcSchema::new("Tasks", om_data::calc::Locale::en_us());
        schema.add_table(table, "Tasks");
        schema.add_occurrence(occurrence, "Tasks", table);
        app.w.calcs.set_schema(Some(std::sync::Arc::new(schema)));
        app.w.session.as_mut().unwrap().render =
            crate::session::RenderState::Loaded(Box::new(om_data::RenderLayout {
                layout: om_layout::Layout {
                    def: Some(om_layout::LayoutDef {
                        table_alias_key: om_data::layout_placement::serial_value(occurrence)
                            .unwrap(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                field_bindings: Default::default(),
            }));
        let cache = app.w.calcs.cache.get_mut();
        cache.generation = Some(4);
        cache.records.insert((other, 7), None);
        cache.records.insert((table, 1), None);
        let reply = |generation| worker::Response {
            epoch,
            window: 1,
            reply: Reply::Records {
                key: crate::session::PageKey {
                    revision: 0,
                    layout_id: 1,
                    offset: 0,
                    limit: DEFAULT_PAGE_SIZE,
                },
                result: Ok(om_data::RecordPage {
                    versions: Default::default(),
                    fields: Vec::new(),
                    records: vec![om_data::Record {
                        id: 1,
                        values: Vec::new(),
                    }],
                    total: 1,
                }),
                found: None,
                no_access: Default::default(),
                generation,
            },
        };
        app.on_response(&ctx, reply(Some(4)));
        let records = &app.w.calcs.cache.get_mut().records;
        assert!(records.contains_key(&(other, 7)));
        assert!(!records.contains_key(&(table, 1)), "the page's own record");
        app.on_response(&ctx, reply(Some(5)));
        assert!(app.w.calcs.cache.get_mut().records.is_empty());
    }

    #[test]
    fn a_background_windows_post_save_reload_survives_navigation_in_that_window() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        let epoch = app.epoch;
        app.on_response(
            &ctx,
            worker::Response {
                epoch,
                window: 2,
                reply: Reply::Updated {
                    layout_id: 1,
                    record_id: 1,
                    result: Ok(om_data::Record {
                        id: 1,
                        values: vec![Some("Saved name".into())],
                    }),
                },
            },
        );
        let front = app.w.session.as_ref().unwrap();
        let front_key = front.pending.unwrap();
        let front_page = front.page.as_ref().unwrap().data.clone();
        let behind = app.windows[0].session.as_ref().unwrap();
        let behind_key = behind.pending.unwrap();
        let mut saved_page = behind.page.as_ref().unwrap().data.clone();
        saved_page.records[0].values[0] = Some("Saved name".into());
        // Page keys can be identical across windows; the response's window
        // id decides which session accepts it.
        assert_eq!(front_key, behind_key);
        app.with_window(2, |app| {
            app.apply(&ctx, Action::GoToRecord(1));
            app.apply(&ctx, Action::GoToRecord(0));
            assert_eq!(app.w.session.as_ref().unwrap().pending, Some(behind_key));
        });
        app.on_response(
            &ctx,
            worker::Response {
                epoch,
                window: 2,
                reply: Reply::Records {
                    key: behind_key,
                    result: Ok(saved_page),
                    found: None,
                    no_access: Default::default(),
                    generation: None,
                },
            },
        );
        assert_eq!(app.w.id, 1);
        let front = app.w.session.as_ref().unwrap();
        assert_eq!(front.pending, Some(front_key));
        assert_eq!(front.page.as_ref().unwrap().data, front_page);
        let behind = app.windows[0].session.as_ref().unwrap();
        assert_eq!(behind.pending, None);
        assert_eq!(
            behind.current_record().unwrap().1.values[0].as_deref(),
            Some("Saved name")
        );
    }

    #[test]
    fn accepted_calculation_generations_expire_only_the_owning_report_groups() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        app.w.calcs.cache.borrow_mut().generation = Some(1);
        let remember = |session: &Session| {
            session.accept_report_groups(2, Ok((std::sync::Arc::from([1, 2]), Vec::new())));
        };
        remember(app.w.session.as_ref().unwrap());
        remember(app.windows[0].session.as_ref().unwrap());
        let reply = |epoch, generation| worker::Response {
            epoch,
            window: 1,
            reply: Reply::CalcData {
                needs: Vec::new(),
                result: Box::new(Ok(om_data::host::CalcData {
                    generation: Some(generation),
                    ..Default::default()
                })),
            },
        };
        app.on_response(&ctx, reply(app.epoch, 2));
        assert_eq!(app.w.session.as_ref().unwrap().report_group_record(0), None);
        assert_eq!(
            app.windows[0]
                .session
                .as_ref()
                .unwrap()
                .report_group_record(0),
            Some(1)
        );
        remember(app.w.session.as_ref().unwrap());
        // More data from this snapshot, or a late older reply, must not
        // repeatedly reread groups while paging through the same report.
        for generation in [2, 1] {
            app.on_response(&ctx, reply(app.epoch, generation));
            assert_eq!(
                app.w.session.as_ref().unwrap().report_group_record(0),
                Some(1)
            );
        }
        app.w
            .session
            .as_mut()
            .unwrap()
            .set_paper(Default::default());
        assert_eq!(
            app.w.session.as_ref().unwrap().report_group_record(0),
            Some(1)
        );
    }

    #[test]
    fn a_table_the_file_could_not_read_is_told_once_for_all_its_windows() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        // Window 2 opened as New Window opens it.
        app.windows.clear();
        app.add_window(2);
        let failing = |epoch, window| worker::Response {
            epoch,
            window,
            reply: Reply::CalcData {
                needs: vec![om_data::host::CalcNeed::Records { table: 7 }],
                result: Box::new(Ok(om_data::host::CalcData {
                    failures: [(7, DataError::new("unreadable"))].into(),
                    ..Default::default()
                })),
            },
        };
        app.on_response(&ctx, failing(app.epoch, 1));
        assert!(app.error.take().is_some(), "window 1 tells it");
        app.on_response(&ctx, failing(app.epoch, 2));
        assert_eq!(app.error, None, "window 2 told it again");
        // Each window keeps its own cache.
        let table = |app: &mut OpenMakerApp, window| {
            app.with_window(window, |app| {
                app.w.calcs.cache.borrow().tables.contains_key(&8)
            })
        };
        let read = worker::Response {
            epoch: app.epoch,
            window: 1,
            reply: Reply::CalcData {
                needs: vec![om_data::host::CalcNeed::Records { table: 8 }],
                result: Box::new(Ok(om_data::host::CalcData {
                    tables: [(8, std::sync::Arc::from([1u64]))].into(),
                    ..Default::default()
                })),
            },
        };
        app.on_response(&ctx, read);
        assert_eq!(
            (table(&mut app, 1), table(&mut app, 2)),
            (Some(true), Some(false))
        );
        // The file opened again tells it again.
        app.close_file(&ctx);
        app.on_response(&ctx, failing(app.epoch, 1));
        assert!(app.error.is_some(), "a new file is told again");
    }

    #[test]
    fn closing_a_secondary_window_keeps_pending_hydration_until_the_last_close() {
        use std::sync::mpsc;
        use std::time::Duration;
        struct Pending {
            closes: om_data::CloseRequests,
            started: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl DataProvider for Pending {
            fn list_files(
                &mut self,
                _: &str,
                _: u16,
            ) -> Result<Vec<om_data::HostedFile>, DataError> {
                Ok(Vec::new())
            }
            fn open(&mut self, _: &om_data::Connection) -> Result<(), DataError> {
                Ok(())
            }
            fn layouts(&mut self) -> Result<Vec<om_data::LayoutInfo>, DataError> {
                Ok(Vec::new())
            }
            fn records(
                &mut self,
                _: u32,
                _: u64,
                _: u32,
            ) -> Result<om_data::RecordPage, DataError> {
                Err(DataError::new("no records"))
            }
            fn calc_schema(
                &mut self,
            ) -> Result<Option<std::sync::Arc<om_data::host::CalcSchema>>, DataError> {
                self.started.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(None)
            }
            fn close_requests(&self) -> Option<om_data::CloseRequests> {
                Some(self.closes.clone())
            }
            fn close(&mut self) {}
        }
        let ctx = egui::Context::default();
        let closes = om_data::CloseRequests::default();
        let (started, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let mut app = OpenMakerApp::with_license_store(
            Box::new(Pending {
                closes: closes.clone(),
                started,
                release: resume,
            }),
            Box::new(NoLicense),
        );
        app.w = window_with_records(1, &["Ada"]);
        app.add_window(2);
        app.request(&ctx, Request::CalcSchema);
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        app.apply(&ctx, Action::CloseWindow);
        assert!(
            !closes.pending(),
            "the surviving window still needs hydration and sessions"
        );
        app.apply(&ctx, Action::CloseWindow);
        assert!(
            closes.pending(),
            "the last close cancels the file's pending work"
        );
        release.send(()).unwrap();
    }

    #[test]
    fn related_hydration_and_warnings_belong_to_the_file_until_its_last_window_closes() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        app.windows.clear();
        app.add_window(2);
        // Close the window that requested the schema while hydration is pending.
        app.apply(&ctx, Action::CloseWindow);
        assert!(app.is_open());
        assert_eq!(app.window_ids(), [2]);
        let schema = std::sync::Arc::new(om_data::host::CalcSchema::new(
            "Root",
            om_data::calc::Locale::en_us(),
        ));
        let warning = "Related file “Missing” is unavailable".to_owned();
        let reply = |epoch, window| worker::Response {
            epoch,
            window,
            reply: Reply::CalcSchema {
                result: Ok(Some(schema.clone())),
                warnings: vec![warning.clone()],
                sign_ins: Vec::new(),
            },
        };
        app.on_response(&ctx, reply(app.epoch, 1));
        assert!(
            app.w.calcs.schema().is_some(),
            "the surviving window needs the completed schema"
        );
        assert_eq!(app.error.take(), Some(warning.clone()));
        app.add_window(3);
        app.on_response(&ctx, reply(app.epoch, 3));
        assert_eq!(app.error, None, "no repeat warning in the new window");
        app.apply(&ctx, Action::CloseWindow);
        assert!(app.is_open(), "another window keeps the file alive");
        app.apply(&ctx, Action::CloseWindow);
        assert!(!app.is_open(), "the last window closes the file");
        app.on_response(&ctx, reply(app.epoch, 1));
        assert_eq!(
            app.error,
            Some(warning),
            "a reopened file has new warning history"
        );
    }

    #[test]
    fn a_changed_schema_reloads_every_windows_layout() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        let epoch = app.epoch;
        app.on_response(
            &ctx,
            worker::Response {
                epoch,
                window: 1,
                reply: Reply::Schema {
                    result: Ok(Default::default()),
                    changed: true,
                },
            },
        );
        for doc in std::iter::once(&app.w).chain(&app.windows) {
            let session = doc.session.as_ref().unwrap();
            assert!(session.pending.is_some(), "window {} reloads", doc.id);
        }
        // And asks for the file's calculation schema again, which Layout
        // mode's Field tool lists the fields of (#726).
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut asked = false;
        while !asked && Instant::now() < deadline {
            match app.worker.poll() {
                Ok(response) => asked = matches!(response.reply, Reply::CalcSchema { .. }),
                Err(worker::PollError::Empty) => std::thread::sleep(Duration::from_millis(5)),
                Err(worker::PollError::Stopped) => break,
            }
        }
        assert!(asked, "the changed schema is asked for");
    }

    /// A record's unsaved edits are kept by layout: Delete Layout saves the
    /// window's own record first, refuses while another window of the file
    /// holds edits made on the layout, and a window whose layout another
    /// client deleted says its changes were not saved.
    #[test]
    fn deleting_a_layout_strands_no_unsaved_edits_silently() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        let layouts = vec![
            om_data::LayoutInfo {
                id: 1,
                name: "Tasks".into(),
            },
            om_data::LayoutInfo {
                id: 2,
                name: "Notes".into(),
            },
        ];
        for doc in std::iter::once(&mut app.w).chain(&mut app.windows) {
            doc.session.as_mut().unwrap().layouts = layouts.clone();
        }
        let edit = |app: &mut OpenMakerApp, window, value: &str| {
            app.with_window(window, |app| {
                let session = app.w.session.as_ref().unwrap();
                app.w
                    .document
                    .set_field(session, "Name", value.into())
                    .unwrap();
            });
        };
        // Window 1's own edit is saved before the alert asks.
        edit(&mut app, 1, "Ada edited");
        app.apply(&ctx, Action::DeleteLayout(1));
        assert_eq!(app.w.deferred_action, Some(Action::DeleteLayout(1)));
        assert_eq!(app.w.delete_layout, None);
        app.w.document.revert(app.w.session.as_ref().unwrap());
        app.w.document.pending = false;
        app.w.deferred_action = None;
        // Window 2's edit on the layout refuses the deletion, at the alert
        // too; a layout it holds no edits on can go.
        edit(&mut app, 2, "Bob edited");
        app.apply(&ctx, Action::DeleteLayout(1));
        assert!(app.error.take().is_some_and(|e| e.contains("unsaved")));
        assert_eq!(app.w.delete_layout, None);
        app.w.delete_layout = Some(1);
        app.apply(&ctx, Action::ConfirmDeleteLayout);
        assert!(app.error.take().is_some_and(|e| e.contains("unsaved")));
        assert_eq!(app.w.after_delete, None, "nothing was sent");
        app.apply(&ctx, Action::DeleteLayout(2));
        assert_eq!((app.error.take(), app.w.delete_layout), (None, Some(2)));
        app.w.delete_layout = None;
        // Another client deleted it: window 2 says its edit was not saved.
        app.on_response(
            &ctx,
            worker::Response {
                epoch: app.epoch,
                window: 2,
                reply: Reply::Layouts(Ok((layouts[1..].to_vec(), Vec::new()))),
            },
        );
        let error = app.error.take().unwrap_or_default();
        assert!(error.contains("\"Tasks\" was deleted"), "{error}");
    }

    #[test]
    fn operators_go_into_the_last_focused_field() {
        let mut find = FindState::default();
        find.insert_operator("*");
        find.set_focused_field("Notes::Body");
        find.insert_operator("==");
        find.insert_operator("Smith");
        assert_eq!(
            find.current().criteria,
            [("Notes::Body".to_owned(), "==Smith".to_owned())]
        );
    }
    #[test]
    fn a_peer_loss_and_schema_reply_preserve_the_other_files_windows() {
        let mut app = two_record_windows();
        app.windows[0].file_id = 9;
        app.window_files.insert(2, 9);
        let ctx = egui::Context::default();
        let drafts = app.w.document.unsaved();
        app.on_response(
            &ctx,
            worker::Response {
                epoch: app.epoch,
                window: 2,
                reply: Reply::ConnectionLost(DataError::connection_lost("peer lost")),
            },
        );
        assert!(app.w.connection_lost.is_none());
        assert_eq!(app.w.document.unsaved(), drafts);
        assert!(app.windows[0].connection_lost.is_some());
        let schema = std::sync::Arc::new(om_data::host::CalcSchema::new(
            "Root",
            om_data::calc::Locale::en_us(),
        ));
        app.on_response(
            &ctx,
            worker::Response {
                epoch: app.epoch,
                window: 1,
                reply: Reply::CalcSchema {
                    result: Ok(Some(schema)),
                    warnings: Vec::new(),
                    sign_ins: Vec::new(),
                },
            },
        );
        assert_eq!(app.w.calcs.schema().unwrap().file_name, "Root");
        assert!(app.windows[0].calcs.schema().is_none());
    }
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod preview_tests;
