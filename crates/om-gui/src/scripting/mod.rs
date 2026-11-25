//! Scripts in the desktop client (script-runtime.md 2.1, 12): the runtime
//! runs on the provider worker thread, so steps that need the host (finds,
//! record writes, Perform Script on Server) call the provider directly and
//! block, while the UI thread keeps painting.
//!
//! ```text
//! UI thread                          worker thread
//! Request::Script(job) ------------> ScriptSession::start, Runtime::run
//!                                      WorkerHost: provider, window model
//! Reply::Script(Window/Prompt/...) <-- after steps, when it asks the user
//! Request::Answer(id, answer) -----> the waiting host step goes on
//! other requests (pages, layouts) -> served between steps and while the
//!                                    script waits, so the window stays live
//! ```
//!
//! The UI owns the window the user sees; a script works on the worker's
//! [`window::Window`], which starts from the UI's [`WindowState`] and sends
//! it back after every step that changed it (not while the window is
//! frozen). Esc and Cmd-period set the shared abort flag (Allow User
//! Abort).

pub(crate) mod databases;
pub(crate) mod files;
pub(crate) mod host;
pub(crate) mod session;
pub(crate) mod window;

use om_data::calc::eval::Variables;
use om_data::formula::Formula;
use om_data::found::FindRequest;
use om_data::runtime::{
    AlertChoice, CustomDialog, DialogResponse, Mode, TriggerEvent, TriggerInfo,
};
use om_data::{Calculation, FieldChanges, ScriptStep};

pub(crate) use session::ScriptSession;
pub(crate) use window::{Cells, DraftKey, RelatedDraft, Uncommitted};

/// The window's view (View As: 1 Form, 2 List, 3 Table).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum View {
    #[default]
    Form,
    List,
    Table,
}

impl View {
    /// The view a layout's definition stores (layout-model.md 4.2): Table
    /// with flag `0x4000`, else List with `0x200`, else Form.
    pub fn stored(flags: u64) -> Self {
        if flags & 0x4000 != 0 {
            View::Table
        } else if flags & 0x200 != 0 {
            View::List
        } else {
            View::Form
        }
    }

    /// Whether a layout with `flags` offers this view (layout-model.md
    /// 4.2: bit 2, 4 or 8 turns Form, List or Table off).
    pub fn enabled(self, flags: u64) -> bool {
        let off = match self {
            View::Form => 2,
            View::List => 4,
            View::Table => 8,
        };
        flags & off == 0
    }
}

/// Adjust Window [Resize to Fit] (#1115) that waits for the window's
/// content area to be measured: what the step fits, as the step saw it, so
/// that what the script does afterwards does not change it. A later size or
/// Maximize replaces it, as later steps would.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fit {
    /// The layout's width and the bottom of its lowest part, in points.
    pub layout: (f64, f64),
    pub zoom: u16,
    pub toolbar: bool,
    pub menubar: bool,
}

/// The window as a script sees it: what the UI shows when a script starts
/// or resumes, and what the script leaves.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowState {
    /// The window (app/windows.rs); the file's first window is 1.
    pub id: u32,
    pub hidden: bool,
    pub layout: Option<u32>,
    /// Each layout's open popover in this document. Steps update this
    /// snapshot and send targeted dismissal events to its UI.
    pub popovers: std::collections::BTreeMap<u64, u64>,
    /// The panel each tab control shows, by (layout, tab control): what
    /// Print and Save Records as PDF print.
    pub panels: std::collections::BTreeMap<(u64, u64), u64>,
    /// Zero-based position of the current record (or request) in the found
    /// set.
    pub index: u64,
    /// Physical report page, independent of index.
    pub preview_page: u64,
    pub mode: Mode,
    pub view: View,
    /// The view the window last showed each layout in, the current one's
    /// included: going to a layout shows it in its entry, else in the view
    /// the file stores for it (FileMaker Pro 26 keeps a view per layout in
    /// each window, #805).
    pub views: std::collections::BTreeMap<u32, View>,
    /// Find mode's requests and the current one.
    pub find: Vec<FindRequest>,
    pub find_index: usize,
    /// The current record's id and its unsaved field values.
    pub record: Option<i64>,
    /// Version from this command's accepted page, never a later refresh.
    pub record_version: Option<u64>,
    /// Stored Related versions already accepted by the UI before this command.
    pub related_versions: std::collections::BTreeMap<(u64, u64), u64>,
    pub drafts: FieldChanges,
    /// The field that has the focus, and whether its contents are
    /// selected.
    pub focus: Option<String>,
    /// The focused layout instance, when known (duplicate field names differ).
    pub object: Option<u64>,
    /// The numeric editor's object and repetition; a named editor has none.
    pub numeric_focus: Option<(u64, u32)>,
    /// A script requested a field by name. The UI resolves its visible
    /// instance anew, even when another instance of that name was active.
    /// UI snapshots set this false to preserve the actual clicked object.
    pub field_focus: bool,
    pub select: bool,
    /// The active portal row (Go to Portal Row, Go to Object, a field in a
    /// portal).
    pub portal: Option<PortalRow>,
    /// What scripts left on the current record that no commit has kept.
    pub uncommitted: window::Uncommitted,
    pub title: Option<String>,
    pub toolbar: bool,
    pub menubar: bool,
    /// Set Zoom Level, in percent, and whether the user may change it.
    pub zoom: u16,
    pub zoom_locked: bool,
    /// The focused field's selection, 1-based start and length; `None`
    /// puts the insertion point after its text (`Get (
    /// ActiveSelectionStart )`).
    pub selection: Option<(u64, u64)>,
    /// The focused field's draft is being typed (typing, Insert Text): it
    /// counts as a modified field only once the field is left (6.3 O2).
    pub typing: Option<String>,
    /// Find mode's requests when the window last left Find mode; 0 before
    /// any (`Get ( RequestCount )` keeps the count).
    pub requests: u64,
    /// The found set is sorted.
    pub sorted: bool,
    /// Where the window is, as the UI last drew it.
    pub placement: Option<crate::desktop::Placement>,
    /// `Get ( WindowUUID )`: new for each window.
    pub uuid: String,
    /// The modifier keys held when the script started (`Get (
    /// ActiveModifierKeys )`).
    pub modifiers: i64,
    /// How the window shows: New Window's style.
    pub style: WindowStyle,
    /// Go to Related Record took the window to a layout the account's
    /// set hides: the toolbar names it there, as FileMaker Pro 26.0.3 did
    /// (#696), where Go to Layout leaves the name blank.
    pub hidden_named: bool,
}

/// How a window shows (New Window's style, script-steps.md 11.8; Claris,
/// "About window styles").
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum WindowStyle {
    #[default]
    Document,
    /// In front of every other window, which take no input until it
    /// closes; closing it returns to the window it was opened from.
    Dialog { opener: u32 },
    /// Drawn over its parent's layout area, which takes no input until
    /// the card closes; closing it returns to the parent.
    Card(Card),
}

/// A card: its parent window, options and where it shows on the parent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Card {
    pub parent: u32,
    /// Dim parent window: the parent's layout area is darkened.
    pub dim: bool,
    /// The card has a close box.
    pub close: bool,
    /// From the parent's layout area's top left, in points (`None`
    /// centres the card on it), and its size (`None`: the area's).
    pub left: Option<f64>,
    pub top: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

impl WindowStyle {
    /// `Get ( WindowStyle )`: 0 document, 2 dialog, 3 card.
    pub fn number(&self) -> i64 {
        match self {
            WindowStyle::Document => 0,
            WindowStyle::Dialog { .. } => 2,
            WindowStyle::Card(_) => 3,
        }
    }

    /// The window focus returns to when this one closes: a card's
    /// parent, a dialog's opener.
    pub fn parent(&self) -> Option<u32> {
        match self {
            WindowStyle::Document => None,
            WindowStyle::Dialog { opener } => Some(*opener),
            WindowStyle::Card(card) => Some(card.parent),
        }
    }

    pub fn card(&self) -> Option<&Card> {
        match self {
            WindowStyle::Card(card) => Some(card),
            _ => None,
        }
    }

    pub fn is_dialog(&self) -> bool {
        matches!(self, WindowStyle::Dialog { .. })
    }
}

impl Default for WindowState {
    fn default() -> Self {
        WindowState {
            id: 1,
            hidden: false,
            layout: None,
            popovers: Default::default(),
            panels: Default::default(),
            index: 0,
            preview_page: 0,
            mode: Mode::Browse,
            view: View::Form,
            views: Default::default(),
            find: vec![FindRequest::default()],
            find_index: 0,
            record: None,
            record_version: None,
            related_versions: Default::default(),
            drafts: FieldChanges::new(),
            focus: None,
            object: None,
            numeric_focus: None,
            field_focus: false,
            select: false,
            portal: None,
            uncommitted: window::Uncommitted::default(),
            title: None,
            toolbar: true,
            menubar: true,
            zoom: 100,
            zoom_locked: false,
            selection: None,
            typing: None,
            requests: 0,
            sorted: false,
            placement: None,
            uuid: om_data::host::window::new_window_uuid(),
            modifiers: 0,
            style: WindowStyle::Document,
            hidden_named: false,
        }
    }
}

/// A portal's active row (script-steps.md, Go to Portal Row): the row
/// stays active until the record is left or committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PortalRow {
    /// The portal object on the layout.
    pub object: u64,
    /// The portal's table occurrence.
    pub occurrence: u64,
    /// The row, from 1.
    pub row: u64,
    /// Identity resolved when the row was entered, never a later re-read of
    /// its numeric position. Sorted/filtered rows remain unknown/refused.
    pub identity: Option<PortalIdentity>,
    /// The window identified this row's record (a button in it was
    /// clicked, or the user's focus is in one of its fields): the row is
    /// that record in any portal order; never substitute a row by position.
    pub clicked: bool,
    /// The field object in the portal that has the focus; `None` when the
    /// whole row is selected.
    pub field: Option<u64>,
    /// The repetition of that field that has the focus, from 1 (Go to
    /// Object's or Go to Field's repetition).
    pub repetition: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PortalIdentity {
    Record(u64),
    /// A record a script or the user is creating through the portal (a
    /// related draft, by its key, which other drafts going leave as it is).
    New(DraftKey),
    Creation,
}

/// Who the session is, for `Get` selectors.
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionContext {
    pub host: String,
    pub file: String,
    pub account: String,
    /// The file's path, for a file opened from this computer (no host).
    pub local: Option<std::path::PathBuf>,
}

/// A request to run a script.
#[derive(Clone, Debug)]
pub(crate) struct ScriptJob {
    pub start: Start,
    /// The window the script starts in, and the file's other windows, the
    /// one in front last.
    pub window: WindowState,
    pub others: Vec<WindowState>,
    pub context: SessionContext,
}

/// How a script starts.
#[derive(Clone, Debug)]
pub(crate) enum Start {
    /// A Scripts menu item or the Script Workspace.
    Script { id: u64 },
    /// A layout button's single step, with its parameter: a calculation
    /// evaluated on the button's record, or literal text. A button in a
    /// portal row makes that row active first (`host::portals`, a click
    /// in a portal row): its identity is the row's related record.
    Button {
        step: Box<ScriptStep>,
        formula: Option<Box<Formula>>,
        literal: String,
        portal: Option<PortalRow>,
    },
    /// A layout or object trigger an event in the window fired. `action`
    /// is the UI's token for the event's default action, which the UI
    /// performs when the script lets it (`ScriptEvent::Action`).
    /// `prepare`: the event is an ordinary commit's OnObjectSave, so the
    /// records begun in portals' creation rows are previewed before the
    /// script reads them (`WorkerHost::prepare_begun_records`, #1047).
    Trigger {
        event: TriggerEvent,
        script: u64,
        file_reference: Option<u64>,
        parameter: Option<Calculation>,
        modes: u32,
        action: Option<u64>,
        info: Box<TriggerInfo>,
        prepare: bool,
    },
    /// Records the user committed in the window: the file's
    /// OnWindowTransaction script, with its parameter's JSON.
    WindowTransaction { script: u64, parameter: String },
    /// Install OnTimer Script's interval elapsed.
    Timer,
    /// The script `FM.RunScriptIdle` scheduled is due (`plugin_ui`): run
    /// it in this window, the one whose script scheduled it.
    Idle,
    /// A Perform Script on Server with Callback result arrived.
    Callback { id: u32, error: i32, result: String },
    /// The user saved the window's record, which scripts left changes on
    /// (`window::Uncommitted`): the worker commits all of it.
    Commit,
    /// The user reverted that record: the worker drops what scripts left
    /// on it and deletes a record New Record made.
    Revert,
    /// A page in a web viewer of window `window` called
    /// `FileMaker.PerformScript`: a script of that window's file, by name.
    WebViewer {
        window: u32,
        call: crate::web_viewer::ScriptCall,
    },
}

/// Vertical scrolling belongs to the window a step ran in. Retain the
/// context so navigation later in a frozen worker slice cannot scroll a
/// different layout or view when the UI catches up. Ordinary Form movement
/// belongs to that viewport; To Selection also belongs to its record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollContext {
    pub layout: Option<u32>,
    pub mode: Mode,
    pub view: View,
    pub index: u64,
    pub preview_page: u64,
}
impl From<&WindowState> for ScrollContext {
    fn from(state: &WindowState) -> Self {
        Self {
            layout: state.layout,
            mode: state.mode,
            view: state.view,
            index: state.index,
            preview_page: state.preview_page,
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct ScrollRequest {
    pub choice: u16,
    pub context: ScrollContext,
    pub field: Option<String>,
    pub field_focus: bool,
}
impl ScrollRequest {
    pub fn matches_context(&self, context: ScrollContext) -> bool {
        self.context.layout == context.layout
            && self.context.mode == context.mode
            && self.context.view == context.view
            && (context.mode != Mode::Preview || self.context.preview_page == context.preview_page)
            && (self.context.index == context.index
                || (self.choice != 5 && context.view == View::Form))
    }
    pub fn matches(&self, state: &WindowState) -> bool {
        self.matches_context(state.into())
    }
}

/// What a script changed in the window.
#[derive(Clone, Debug)]
pub(crate) struct WindowUpdate {
    pub file: u64,
    pub state: WindowState,
    /// Records were written or the found set changed: pages are stale.
    pub data_changed: bool,
    /// Refresh Window reloads only its owning window, without a record write.
    pub refreshed: bool,
    pub objects: Vec<ObjectRefresh>,
    pub scroll: Vec<ScrollRequest>,
    /// The tab panels steps brought to the front since the last update, in
    /// order: layout, tab control and panel (Go to Object). Only these
    /// controls' panels change in the window, never one the user chose since
    /// the script began; Freeze Window holds them with the rest.
    pub fronted: Vec<(u64, u64, u64)>,
    /// The current record's stored values of the drafted fields, so the UI
    /// can show the drafts before its page arrives.
    pub originals: FieldChanges,
    /// A find, omit or sort changed the found set.
    pub found_set: Option<om_data::found::FoundSet>,
    /// A window the script does not work in now (Set Window Title of a
    /// named window, the window Select Window or New Window leaves): the
    /// UI shows the change where the window is, without bringing it to
    /// the front.
    pub background: bool,
}

/// A targeted refresh on the owning window. No record/page change is implied.
#[derive(Clone, Debug)]
pub(crate) struct ObjectRefresh {
    pub layout: u32,
    pub objects: Vec<u64>,
    /// Zero refreshes every repetition; visibility always refreshes.
    pub repetition: u32,
    pub join: Option<(u64, u64, u64)>,
    pub generation: Option<u64>,
}

/// Something the worker asks the user; the UI answers with
/// `Request::Answer`.
#[derive(Clone, Debug)]
pub(crate) enum Prompt {
    /// A script error with error capture off (script-runtime.md 4.4):
    /// Continue/Cancel for confirm errors, OK otherwise; Cancel only while
    /// Allow User Abort is On.
    Alert {
        message: String,
        detail: String,
        confirm: bool,
        can_cancel: bool,
    },
    /// Show Custom Dialog.
    Dialog(CustomDialog),
    /// The MBS plug-in subset's `Dialog.Run` alert, answered `Timeout`
    /// once `deadline` passes.
    PluginDialog {
        dialog: om_data::runtime::plugins::PluginDialog,
        deadline: Option<std::time::Instant>,
    },
    /// An account for file `file`, which refused this file's (#214), as
    /// FileMaker Pro's Open File asks: Account Name filled with `account`,
    /// and why the last answer was refused.
    SignIn {
        file: String,
        account: String,
        error: Option<String>,
    },
    /// A step's "With dialog" confirmation (Delete Record, Delete All
    /// Records, Replace Field Contents, Truncate Table, Revert Record):
    /// Cancel, then `ok`. Return chooses `ok` when it is the `default`
    /// button; without one Return does nothing, as in FileMaker Pro's
    /// Revert Record... alert (#871). Escape cancels.
    Confirm {
        title: String,
        message: String,
        ok: String,
        default: bool,
    },
    /// The Sort Records dialog for the window's layout: its name, its
    /// occurrence and the fields it shows, and the current order.
    Sort {
        layout: String,
        occurrence: Option<u64>,
        fields: Vec<(om_data::specs::SpecField, String)>,
        current: om_data::specs::SortSpec,
    },
    /// Insert from Index's View Index dialog: the field's index values.
    Index {
        field: String,
        values: Vec<String>,
    },
    /// Export Records' and Save Records as Excel's output file and export
    /// order.
    Export(ExportDialog),
    Pdf(PdfDialog),
    PageSetup(crate::page_setup::Settings),
    Print(crate::printing::Dialog),
    /// Import Records' field mapping and options.
    Import(ImportDialog),
    /// Source selection, before reading or parsing a no-path import.
    ImportSource(ImportSourceDialog),
    /// The file an Insert step without a path puts into its field, or the
    /// file Export Field Contents without a path writes.
    ContainerFile(ContainerFileDialog),
    /// The Import Summary after an import shown with dialogs.
    ImportSummary {
        total: usize,
        added: usize,
        updated: usize,
        skipped: usize,
    },
}

/// The export dialog's choices (FileMaker Pro's Specify Output File and
/// Specify Export Order in one).
#[derive(Clone, Debug)]
pub(crate) struct ExportDialog {
    /// The output path list line (`file:Contacts.tab`, relative to the
    /// Documents folder).
    pub path: String,
    pub file_type: om_transfer::FileType,
    /// The types offered.
    pub types: Vec<om_transfer::FileType>,
    pub charset: om_transfer::ExportCharset,
    /// "Apply current layout's data formatting to exported data".
    pub formatting: bool,
    /// The fields that can be exported (`Occurrence::Field` for related).
    pub available: Vec<String>,
    /// The export order: indexes into `available`.
    pub chosen: Vec<usize>,
    /// Save Records as Excel's records choice: `Some(true)` for the current
    /// record; `None` for Export Records, which has none.
    pub current_record: Option<bool>,
}

/// Save Records as PDF's dialog: where to save, as a save panel offers it
/// (`chooser`), and which records. `path` is the native path chosen.
#[derive(Clone, Debug)]
pub(crate) struct PdfDialog {
    pub chooser: crate::open_file::SaveChooser,
    pub path: String,
    pub current_record: bool,
}

/// Explicit source choices for the menu and no-path script steps. Keep the
/// offered formats bounded to these supported shared parser profiles.
#[derive(Clone, Debug)]
pub(crate) struct ImportSourceDialog {
    pub path: String,
    pub target: String,
    pub file_type: om_transfer::FileType,
    pub charset: om_transfer::ImportCharset,
    /// One-based worksheet position. Named ranges are not offered.
    pub sheet: u32,
}

impl ImportSourceDialog {
    pub const TYPES: [om_transfer::FileType; 3] = [
        om_transfer::FileType::TabSeparated,
        om_transfer::FileType::CommaSeparated,
        om_transfer::FileType::Excel2007,
    ];
}

/// The file of a container step that names none (host::containers):
/// chosen by path, absolute or relative to the Documents folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContainerFileDialog {
    /// The step's name: "Insert Picture", "Export Field Contents".
    pub title: String,
    /// The field, as `Table::Field`.
    pub field: String,
    /// Export: the file is written; otherwise it is read.
    pub export: bool,
    pub path: String,
    /// The file the platform's open panel chose (`file_panel`), as the
    /// panel named it: used as is, never trimmed or read as a path list.
    pub chosen: Option<std::path::PathBuf>,
}

/// The import dialog's choices (FileMaker Pro's Specify Import Order).
#[derive(Clone, Debug)]
pub(crate) struct ImportDialog {
    /// The source file.
    pub path: String,
    pub target: String,
    /// The source's columns: their names, or the first row's values.
    pub columns: Vec<String>,
    /// The target table's fields.
    pub targets: Vec<String>,
    /// Per column: the target field (an index into `targets`), or none.
    pub map: Vec<Option<usize>>,
    /// Per source column: use the selected target as a match key rather
    /// than an imported value when updating matching records.
    pub match_fields: Vec<bool>,
    pub action: om_transfer::ImportAction,
    pub add_remaining: bool,
    /// "Don't import first record (contains field names)".
    pub names_row: bool,
    /// "Perform auto-enter options while importing".
    pub auto_enter: bool,
}

/// An account and password typed for [`Prompt::SignIn`]. Its `Debug`
/// leaves the password out.
#[derive(Clone)]
pub(crate) struct SignIn {
    pub account: String,
    pub password: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn")
            .field("account", &self.account)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// The user's answer to a [`Prompt`] or a pause.
#[derive(Clone, Debug)]
pub(crate) enum Answer {
    Alert(AlertChoice),
    /// The account typed for [`Prompt::SignIn`]; `None` cancels.
    SignIn(Option<SignIn>),
    Dialog(DialogResponse),
    PluginDialog(om_data::runtime::plugins::PluginDialogAnswer),
    Confirm(bool),
    /// The Sort Records dialog's Sort (the order), Unsort or Cancel.
    Sort(crate::script_workspace::BrowseSort),
    /// The index value to paste; `None` cancels.
    Index(Option<String>),
    /// The export dialog as the user left it; `None` cancels.
    Export(Option<ExportDialog>),
    Pdf(Option<PdfDialog>),
    PageSetup(Option<crate::page_setup::Settings>),
    Print(Option<crate::printing::Options>),
    /// The import dialog as the user left it; `None` cancels.
    Import(Option<ImportDialog>),
    ImportSource(Option<ImportSourceDialog>),
    /// The container file dialog as the user left it; `None` cancels.
    ContainerFile(Option<ContainerFileDialog>),
    /// A web viewer command's outcome: `Err` holds the step's error code.
    WebViewer(Result<(), i32>),
    /// Continue a paused script from the window as the user left it.
    Resume(Box<WindowState>),
    /// Cancel a paused script (Esc while paused).
    CancelPause,
}

/// A separately owned hosted desktop file; no credentials cross to the UI.
#[derive(Debug)]
pub(crate) struct OpenedFile {
    pub id: u64,
    pub hidden: bool,
    pub context: SessionContext,
    pub window: WindowState,
    pub layouts: Vec<om_data::LayoutInfo>,
    /// The layout menus' catalog (flags, folders, separators), which a file
    /// opened from Home reads with its layouts (`Request::Layouts`).
    pub layout_menu: Vec<om_data::LayoutMenuEntry>,
    /// The file's whole script catalog when the session has read it; a
    /// file a script opens reads none (#1053), and its windows' Scripts
    /// menu asks for it.
    pub catalog: Option<om_data::ScriptCatalog>,
    pub schema: Option<std::sync::Arc<om_data::host::CalcSchema>>,
    pub host_version: String,
    pub privileges: Option<om_data::host::Privileges>,
}

/// What the worker tells the UI while scripts run.
#[derive(Debug)]
pub(crate) enum ScriptEvent {
    PaperSettings {
        file: u64,
        paper: om_layout::paper::PaperSettings,
    },
    FileOpened(Box<OpenedFile>),
    FileClosed(u64),
    Globals {
        file: u64,
        globals: Box<Variables>,
    },
    /// Each window's save that may be partly applied as a script left it
    /// (`DataProvider::unsettled_saves`, #1006), sent just before
    /// [`ScriptEvent::Finished`]: those of `files` not listed have none.
    Unsettled {
        files: Vec<u64>,
        saves: Vec<(u32, Vec<om_data::FileCommit>)>,
    },
    /// A script started; the window takes no input until it ends or pauses.
    Started,
    Window(Box<WindowUpdate>),
    /// A user-invoked commit on the worker was refused. Keep its error
    /// code until the final window update so UI-origin edits can recover.
    SaveRefused {
        layout: u32,
        record: i64,
        error: om_data::DataError,
    },
    Prompt {
        id: u64,
        prompt: Prompt,
    },
    /// Pause/Resume Script or a pausing mode step: the window takes input
    /// and shows Continue/Cancel. `message`: what the user started while
    /// paused (a save of the record) failed.
    Paused {
        seconds: Option<f64>,
        message: Option<String>,
    },
    /// Every script ended. `globals` are the file's `$$` variables (layout
    /// calculations read them); `message` explains a halt the user must
    /// see (a step the client lacks).
    Finished {
        window: Box<WindowUpdate>,
        globals: Box<Variables>,
        message: Option<String>,
    },
    /// The trigger the UI fired for its token ended: perform the held
    /// action or drop it. `record`: the current record of the trigger's
    /// window as its script left it (`Some(None)`: none, an empty found
    /// set), when that window is still the current one and its record is
    /// known; the UI's page may not show it yet (#1073).
    Action {
        token: u64,
        proceed: bool,
        record: Option<Option<i64>>,
    },
    /// Install OnTimer Script: start [`Start::Timer`] every so many
    /// seconds; `None` stops.
    Timer(Option<f64>),
    /// `FM.RunScriptIdle`: start [`Start::Idle`] once the app has been
    /// idle so many seconds; `None` stops it (`FM.StopScriptIdle`).
    Idle(Option<f64>),
    /// A server script's callback is due: start [`Start::Callback`].
    Callback {
        id: u32,
        error: i32,
        result: String,
    },
    /// Close Window closed this window (the file's others stay open).
    CloseWindow(u32),
    /// Close only this popover in the response's owning document, after
    /// its pre-event triggers have allowed it.
    ClosePopover {
        layout: u64,
        popover: u64,
        button: Option<u64>,
    },
    /// Set Web Viewer or Perform JavaScript in Web Viewer on a web viewer
    /// of the window; the UI answers with `Answer::WebViewer`.
    WebViewer {
        id: u64,
        command: crate::web_viewer::Command,
    },
    /// Effects the UI performs.
    OpenMenu(u16),
    OpenUrl(String),
    Copy(String),
    Viewport(egui::ViewportCommand),
    /// Adjust Window [Resize to Fit] before the window's content area, or a
    /// bar the step shows, was drawn: the window fits once it is.
    Fit(Fit),
    Beep,
    Quit,
    CloseFile,
    /// The file's calculation schema changed (a related file signed in
    /// to, #214): its windows read it again.
    SchemaChanged,
    /// A panic stopped the session's scripts, and their end could not be
    /// told as [`Self::Finished`] (no run had taken the UI's windows, or
    /// telling its end panicked too): the windows stay as the UI shows
    /// them.
    Failed(String),
}

/// The trigger record of `event` on a layout.
pub(crate) fn layout_trigger(
    layout: &om_layout::Layout,
    event: TriggerEvent,
) -> Option<&om_layout::ScriptTrigger> {
    layout.triggers.iter().find(|t| t.event == event.id())
}

/// The trigger record of `event` on a layout object.
pub(crate) fn object_trigger(
    layout: &om_layout::Layout,
    object: u64,
    event: TriggerEvent,
) -> Option<&om_layout::ScriptTrigger> {
    layout
        .objects
        .get(object)?
        .triggers
        .iter()
        .find(|t| t.event == event.id())
}

/// The runtime's trigger for a stored record, of an event in window
/// `window`; the worker resolves an external reference when the event fires.
pub(crate) fn make_trigger(
    event: TriggerEvent,
    record: &om_layout::ScriptTrigger,
    (window, mode): (u32, Mode),
    action: Option<u64>,
    from_step: bool,
) -> Option<om_data::runtime::Trigger> {
    if record.external && record.file_reference.is_none() {
        return None;
    }
    Some(om_data::runtime::Trigger {
        file_reference: record.file_reference,
        parameter: record.parameter.as_ref().map(host::parameter),
        window: Some(u64::from(window)),
        modes: record.modes,
        mode,
        action,
        from_step,
        ..om_data::runtime::Trigger::new(event, 0, record.script?)
    })
}

/// The field object of the layout's table that shows `field`.
pub(crate) fn field_object(render: &om_data::RenderLayout, field: &str) -> Option<u64> {
    let alias = render.layout.def.as_ref().map_or(0, |d| d.table_alias_key);
    render
        .layout
        .objects
        .objects
        .iter()
        .find(|o| {
            o.field_spec().is_some_and(|s| s.table_key == alias)
                && render.field_bindings.get(&o.id).map(String::as_str) == Some(field)
        })
        .map(|o| o.id)
}

/// The fields the layout shows, top to bottom and left to right (the tab
/// order when the layout sets none).
pub(crate) fn tab_order(render: &om_data::RenderLayout) -> Vec<String> {
    let mut fields: Vec<(f64, f64, String)> = render
        .field_bindings
        .iter()
        .filter_map(|(id, name)| {
            let bounds = render.layout.absolute_bounds(*id)?;
            Some((bounds.top, bounds.left, name.clone()))
        })
        .collect();
    fields.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut names: Vec<String> = Vec::new();
    for (_, _, name) in fields {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}
