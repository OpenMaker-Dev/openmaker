//! The menu bar, with FileMaker's menus and items (captures 16-25).
//!
//! FileMaker on macOS uses the system menu bar, and so does OpenMaker when it
//! runs in a window of its own there (`system_menu.rs`); elsewhere the bar is
//! drawn in the window. The menus are built through [`MenuRows`], so both
//! show the same rows.
//! Items OpenMaker does not implement yet are present but disabled, so the
//! menus read like FileMaker's and fill in as features land.

use egui::{Key, KeyboardShortcut, Modifiers, Ui};
use om_data::scripts::save_records::Format as SaveFormat;

use crate::app::{shortcuts, steps, Action, EditCommand, FindState, Mode, ViewMode};
use crate::file_history::{Change, History, SavedFile, Tab};
use crate::layout_canvas::DraftEdit;
use crate::session::Session;
use crate::theme;
use crate::widgets::{menu_item, menu_separator, submenu};

/// Everything the menus read.
pub(crate) struct MenuContext<'a> {
    /// The window is OpenMaker Go's (`app/mobile.rs`): its menus leave out
    /// what FileMaker Go has none of, such as the Script Workspace.
    pub go: bool,
    pub mode: Mode,
    pub view: ViewMode,
    pub zoom: u16,
    /// View's zoom commands can change it (`OpenMakerApp::zooms`).
    pub zooms: bool,
    pub session: Option<&'a Session>,
    pub file_history: &'a History,
    /// The front window's file, when Recent can record it.
    pub current_file: Option<&'a SavedFile>,
    pub launch_available: bool,
    /// The window has Home (not an embedding with the Hosts dialog alone).
    pub home: bool,
    pub find: &'a FindState,
    pub toolbar: bool,
    pub toolbar_toggle_available: bool,
    pub formatting: bool,
    pub inspector: bool,
    pub objects: bool,
    pub window_title: &'a str,
    /// The file's windows (id, title, name), front first: the Window
    /// menu's list, with a check at this window (`window`).
    pub windows: &'a [(u32, String, String)],
    /// Window > Show Window's windows (id, title, hidden), every open
    /// file's, hidden ones too.
    pub show_windows: &'a [(u32, String, bool)],
    pub window: u32,
    pub scripts: &'a om_data::ScriptCatalog,
    /// Whether `scripts` is the file's catalog as read: until it is, the
    /// Scripts menu asks for it ([`Action::ReadScripts`], #1053).
    pub scripts_read: bool,
    pub scripts_unread: bool,
    /// The account's privilege set as the host enforces it: the Scripts
    /// menu leaves out the scripts it gives no access.
    pub set_access: Option<&'a om_data::host::security::SetAccess>,
    pub record_create: bool,
    pub record_delete: bool,
    /// Records > Revert Record... (Browse) or Requests > Revert Request...
    /// (Find) can run (`om_data::scripts::revert_record::available`).
    pub revert_record: bool,
    /// Records > Relookup Field Contents can run: a field of the
    /// record's own table is entered in Browse mode on a record with
    /// nothing to commit, no script running or paused and no edit being
    /// prepared (`om_data::scripts::relookup::available`). A related field
    /// leaves it off: the step does not look up through one (3).
    pub relookup: bool,
    /// The account may change the file's design: File > Manage's dialogs,
    /// Layout mode and Script Workspace (`App::design_allowed`).
    pub design: bool,
    pub export_records: bool,
    /// The account may print (File > Print, Page Setup; session-identity.md
    /// 2.3).
    pub print: bool,
    /// The menu commands the account's set makes available (2.3).
    pub menus: om_data::host::privilege_set::MenuCommands,
    /// The Save Records as commands that can run (File > Save/Send Records
    /// As).
    pub save_records: &'a [SaveFormat],
    pub import_records: bool,
    /// A container field of the current record is selected (Browse mode,
    /// no script running): Export Field Contents...
    pub container: bool,
    /// ...and the record may be changed: Insert's Picture and File.
    pub container_insert: bool,
    /// The selection is optimized for interactive content outside portal rows.
    pub container_interactive: bool,
    /// A text field is entered for typing (Browse mode, no script
    /// running): Edit's Cut, Copy and Paste, and Insert's Current Date,
    /// Current Time, Current User Name, From Index and From Last Visited
    /// Record.
    pub text_field: bool,
    /// ...and it is one of the current record's own fields, not a portal
    /// row's or a related field: Edit's Undo, Redo, Clear and Select All.
    pub own_field: bool,
    /// Edit > Undo and Redo have something to do in that field.
    pub field_undo: (bool, bool),
    /// A record is shown in Browse with no field entered: Edit > Copy
    /// copies the record (Copy Record/Request).
    pub record_copy: bool,
    pub client_license: bool,
    /// Layout mode has a field placed with the Field tool and not yet
    /// saved (Save Layout, Revert Layout).
    pub layout_edit: bool,
    /// A draft or a redo branch can be discarded explicitly.
    pub layout_revert: bool,
    pub layout_history: (bool, bool),
    /// The Layout mode Selection tool can take Edit > Select All.
    pub layout_select_all: bool,
    /// Layout mode has an object selected that can be deleted (Edit >
    /// Clear).
    pub layout_delete: bool,
    /// How many of Layout mode's selected objects Arrange's Align,
    /// Distribute and Resize To apply to.
    pub layout_arrange: usize,
}

impl MenuContext<'_> {
    fn open(&self) -> bool {
        self.session.is_some()
    }

    /// The account's set makes all menu commands available.
    fn full_menus(&self) -> bool {
        self.menus == om_data::host::privilege_set::MenuCommands::All
    }

    /// Editing only's commands that Minimum leaves out (FileMaker Pro
    /// 26.0.3, #137's and #651's readings): Show All, Show Omitted Only,
    /// Refresh Window and Export Field Contents. That Minimum leaves out
    /// Show All is inferred [I]: Pro's reading had every record shown.
    /// Layout and record navigation, the bars and zoom stay under both.
    fn editing(&self) -> bool {
        self.menus != om_data::host::privilege_set::MenuCommands::Minimum
    }

    fn total(&self) -> u64 {
        self.session.and_then(Session::total).unwrap_or(0)
    }

    fn current(&self) -> u64 {
        self.session.map_or(0, |session| session.current)
    }
}

fn command(modifiers: Modifiers, key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(modifiers, key))
}

const CMD: Modifiers = Modifiers::COMMAND;
const SHIFT_CMD: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const ALT_SHIFT_CMD: Modifiers = SHIFT_CMD.plus(Modifiers::ALT);

/// Where a menu's rows go. Every menu is built through it, so that wherever
/// it is shown, its rows read, enable, check and act alike: drawn in the
/// window ([`Drawn`]) or, later, kept for the system's menu bar.
pub(crate) trait MenuRows {
    /// A command row; whether it was chosen.
    fn item(
        &mut self,
        label: &str,
        shortcut: Option<KeyboardShortcut>,
        checked: bool,
        enabled: bool,
    ) -> bool;
    /// A command row of a list that can change while a menu shows it (the
    /// file's windows, scripts, layouts): `key` names what it runs, so that a
    /// choice reaches that and nothing else should the list change before
    /// the choice is read (the system menu bar's). Drawn as [`Self::item`].
    fn keyed_item(
        &mut self,
        key: &str,
        label: &str,
        shortcut: Option<KeyboardShortcut>,
        checked: bool,
        enabled: bool,
    ) -> bool {
        let _ = key;
        self.item(label, shortcut, checked, enabled)
    }
    fn separator(&mut self);
    /// A submenu, whose rows `rows` adds.
    fn submenu(&mut self, label: &str, enabled: bool, rows: &mut dyn FnMut(&mut dyn MenuRows));
    /// A line of text that is not a command (a script folder's name).
    fn note(&mut self, text: &str);
    /// Rows of a list that can be longer than the screen is tall (File >
    /// Favorites' files), which `rows` adds: they scroll where the menu
    /// itself does not.
    fn scrolled(&mut self, rows: &mut dyn FnMut(&mut dyn MenuRows));
    /// The user is looking at these rows now: a menu that reads its
    /// content when shown asks for it.
    fn shown(&self) -> bool;
    /// Whether a menu that reads its content when shown (`what`, the
    /// Scripts menu's catalog) should ask for it now: whenever the rows are
    /// [`Self::shown`], unless the rows say otherwise (the system menu bar
    /// asks once while its window is in front, as it cannot tell when the
    /// menu opens).
    fn ask(&mut self, what: &str) -> bool {
        let _ = what;
        self.shown()
    }
}

/// Rows drawn in an egui menu.
pub(crate) struct Drawn<'a>(pub &'a mut Ui);

impl MenuRows for Drawn<'_> {
    fn item(
        &mut self,
        label: &str,
        shortcut: Option<KeyboardShortcut>,
        checked: bool,
        enabled: bool,
    ) -> bool {
        let text = shortcut.map(|shortcut| shortcut_text(self.0, shortcut));
        menu_item(self.0, label, text.as_deref(), checked, enabled).clicked()
    }

    fn separator(&mut self) {
        menu_separator(self.0);
    }

    fn submenu(&mut self, label: &str, enabled: bool, rows: &mut dyn FnMut(&mut dyn MenuRows)) {
        submenu(self.0, label, enabled, |ui| rows(&mut Drawn(ui)));
    }

    fn note(&mut self, text: &str) {
        self.0.weak(text);
    }

    fn scrolled(&mut self, rows: &mut dyn FnMut(&mut dyn MenuRows)) {
        // The list keeps clear of the screen's edges and of a few rows
        // above it, so the whole menu fits and egui keeps it on screen.
        let room = self.0.ctx().content_rect().height() - 6.0 * theme::MENU_ROW_HEIGHT;
        egui::ScrollArea::vertical()
            .max_height(room.max(3.0 * theme::MENU_ROW_HEIGHT))
            .show(self.0, |ui| rows(&mut Drawn(ui)));
    }

    fn shown(&self) -> bool {
        // An egui menu's rows are built only while it is open.
        true
    }
}

/// The platform's shortcut text, with punctuation keys shown as symbols as
/// FileMaker does (`Ctrl++` rather than `Ctrl+Plus`).
fn shortcut_text(ui: &Ui, shortcut: KeyboardShortcut) -> String {
    let text = ui.ctx().format_shortcut(&shortcut);
    let symbol = match shortcut.logical_key {
        Key::Plus => "+",
        Key::Minus => "-",
        Key::Equals => "=",
        Key::Semicolon => ";",
        Key::Quote => "'",
        Key::Comma => ",",
        _ => return text,
    };
    match text.strip_suffix(shortcut.logical_key.name()) {
        Some(prefix) => format!("{prefix}{symbol}"),
        None => text,
    }
}

/// A menu row for a command OpenMaker does not implement yet.
fn pending(rows: &mut dyn MenuRows, label: &str, shortcut: Option<KeyboardShortcut>) {
    rows.item(label, shortcut, false, false);
}

/// A submenu whose commands are all still to come.
fn pending_submenu(rows: &mut dyn MenuRows, label: &str) {
    rows.submenu(label, false, &mut |_| {});
}

/// Draw the menu bar. Returns whether this frame's pointer is on the bar or
/// on one of its open menus, where a click leaves the active field entered
/// (`OpenMakerApp::draw_window_with`).
pub(crate) fn show(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) -> bool {
    let mut open = false;
    let frame = egui::Frame::new()
        .fill(theme::TOOLBAR_BG)
        .inner_margin(egui::Margin::symmetric(6, 0));
    let bar = egui::Panel::top("menubar")
        .frame(frame)
        .exact_size(theme::MENU_BAR_HEIGHT)
        .show_separator_line(false)
        .resizable(false)
        .show(ui, |ui| {
            let rect = ui.max_rect().expand2(egui::vec2(6.0, 0.0));
            ui.painter().hline(
                rect.x_range(),
                rect.bottom() - 0.5,
                egui::Stroke::new(1.0, theme::MENU_BAR_RULE),
            );
            egui::MenuBar::new()
                .style(theme::menu_bar_style)
                .config(egui::containers::menu::MenuConfig::new().style(theme::menu_popup_style))
                .ui(ui, |ui| {
                    // Under Editing only or Minimum menus (session-identity.md
                    // 2.3) each menu gates its own items, so that a row's
                    // look and its click agree: a whole menu disabled at
                    // its start drew some rows as enabled.
                    menus(context, actions, &mut |title, rows| {
                        let opened = if title == crate::APP_TITLE {
                            let title = egui::RichText::new(title)
                                .font(egui::FontId::new(theme::BODY, theme::semibold()));
                            ui.menu_button(title, |ui| rows(&mut Drawn(ui)))
                        } else {
                            ui.menu_button(title, |ui| rows(&mut Drawn(ui)))
                        };
                        open |= opened.inner.is_some();
                    });
                });
        });
    // The bar, or an open menu's popup (menus are foreground areas over the
    // window, their submenus too).
    let ctx = ui.ctx();
    ctx.pointer_interact_pos().is_some_and(|pos| {
        bar.response.rect.contains(pos)
            || (open
                && ctx
                    .layer_id_at(pos)
                    .is_some_and(|layer| layer.order == egui::Order::Foreground))
    })
}

/// The menu bar's menus in order, each with its title: the window's bar
/// draws them, the system menu bar keeps them (`system_menu.rs`). Under
/// Editing only or Minimum menus (session-identity.md 2.3) each menu gates
/// its own items, so that a row's look and its click agree: a whole menu
/// disabled at its start drew some rows as enabled.
/// What adds a menu's rows.
pub(crate) type MenuContents<'a> = dyn FnMut(&mut dyn MenuRows) + 'a;
/// What takes each menu: its title and what adds its rows.
pub(crate) type MenuSink<'a> = dyn FnMut(&str, &mut MenuContents<'_>) + 'a;

pub(crate) fn menus(context: &MenuContext<'_>, actions: &mut Vec<Action>, menu: &mut MenuSink<'_>) {
    menu(crate::APP_TITLE, &mut |rows| {
        app_menu(rows, context, actions)
    });
    menu("File", &mut |rows| file_menu(rows, context, actions));
    menu("Edit", &mut |rows| edit_menu(rows, context, actions));
    menu("View", &mut |rows| view_menu(rows, context, actions));
    menu("Insert", &mut |rows| insert_menu(rows, context, actions));
    menu("Format", &mut |rows| format_menu(rows));
    match context.mode {
        Mode::Browse | Mode::Preview => {
            menu("Records", &mut |rows| records_menu(rows, context, actions));
        }
        Mode::Find => menu("Requests", &mut |rows| {
            requests_menu(rows, context, actions)
        }),
        Mode::Layout => {
            menu("Layouts", &mut |rows| layouts_menu(rows, context, actions));
            menu("Arrange", &mut |rows| arrange_menu(rows, context, actions));
        }
    }
    menu("Scripts", &mut |rows| scripts_menu(rows, context, actions));
    menu("Tools", &mut |rows| tools_menu(rows));
    menu("Window", &mut |rows| window_menu(rows, context, actions));
    menu("Help", &mut |rows| help_menu(rows));
}

/// OpenMaker Go's menus (`app/mobile.rs`), which its native toolbars
/// show: the layout menu, Browse's Records or Find's Requests, a View
/// menu of the modes and views FileMaker Go has, and Scripts. Each is the
/// window's own, so its rows read, enable and act as they do there.
pub(crate) fn go_menus(
    context: &MenuContext<'_>,
    actions: &mut Vec<Action>,
    menu: &mut MenuSink<'_>,
) {
    menu("Layouts", &mut |rows| {
        if let Some(session) = context.session {
            crate::layout_menu::show(rows, session, context.mode, actions);
        }
    });
    match context.mode {
        Mode::Find => menu("Requests", &mut |rows| {
            requests_menu(rows, context, actions)
        }),
        _ => menu("Records", &mut |rows| records_menu(rows, context, actions)),
    }
    menu("View", &mut |rows| {
        mode_rows(rows, context, &[Mode::Browse, Mode::Find], actions);
        rows.separator();
        view_as_rows(rows, context, actions);
    });
    menu("Scripts", &mut |rows| scripts_menu(rows, context, actions));
}

fn app_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    pending(rows, &format!("About {}", crate::APP_TITLE), None);
    rows.separator();
    if rows.item("Settings...", Some(shortcuts::SETTINGS), false, true) {
        actions.push(Action::ShowSettings);
    }
    if rows.item("FileMaker License...", None, false, context.client_license) {
        actions.push(Action::ManageLicense);
    }
    rows.separator();
    if rows.item(
        &format!("Quit {}", crate::APP_TITLE),
        Some(shortcuts::QUIT),
        false,
        true,
    ) {
        actions.push(Action::Quit);
    }
}

fn file_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if rows.item(om_data::new_file::CREATE_FILE_LABEL, None, false, true) {
        actions.push(Action::ShowCreateNew);
    }
    rows.separator();
    pending(rows, "Sign In to Claris ID", None);
    // Claris documents Show Favorites, Add Current File to Favorites and
    // Show Recent, and the files listed in each; their order is unmeasured.
    for tab in [Tab::Favorites, Tab::Recent] {
        rows.submenu(tab.label(), context.launch_available, &mut |rows| {
            let show = match tab {
                Tab::Favorites => "Show Favorites",
                Tab::Recent => "Show Recent",
            };
            if rows.item(show, None, false, context.home) {
                actions.push(Action::ShowHome(tab));
            }
            if tab == Tab::Favorites {
                let current = context
                    .current_file
                    .filter(|file| !context.file_history.files(Tab::Favorites).contains(file));
                if rows.item(
                    "Add Current File to Favorites",
                    None,
                    false,
                    current.is_some(),
                ) {
                    if let Some(file) = current {
                        actions.push(Action::EditHistory(Change::SetFavorite(file.clone(), true)));
                    }
                }
            }
            let files = context.file_history.files(tab);
            if !files.is_empty() {
                rows.separator();
                // Favorites have no limit; Recent keeps 20.
                rows.scrolled(&mut |rows| {
                    for file in files {
                        let name = file.name();
                        let label = if files.iter().filter(|entry| entry.name() == name).count() > 1
                        {
                            format!("{name} ({})", file.location())
                        } else {
                            name
                        };
                        if rows.item(&label, None, false, true) {
                            actions.push(Action::OpenSaved(file.clone()));
                        }
                    }
                });
            }
        });
    }
    rows.submenu("Hosts", true, &mut |rows| {
        if rows.item(
            "Show Hosts...",
            None,
            false,
            context.launch_available && context.home,
        ) {
            actions.push(Action::ShowHosts);
        }
        // No shortcut: FileMaker gives Shift-Command-O to Show Hosts.
        if rows.item(
            "Open Network File...",
            None,
            false,
            context.launch_available,
        ) {
            actions.push(Action::OpenNetworkFile(String::new()));
        }
    });
    if rows.item("Open...", Some(shortcuts::OPEN), false, true) {
        actions.push(Action::ShowOpenFile);
    }
    pending(rows, "Open Quickly...", command(CMD, Key::K));
    if rows.item("Close", Some(shortcuts::CLOSE), false, context.open()) {
        actions.push(Action::CloseFile);
    }
    rows.separator();
    // Under Editing only and Minimum the Manage, Import Records and
    // Save/Send Records As submenus open, every command in them disabled
    // (FileMaker Pro 26.0.3, #651's reading).
    rows.submenu("Manage", context.open(), &mut |rows| {
        manage_items(rows, context.design && context.full_menus(), actions)
    });
    rows.separator();
    pending_submenu(rows, "Sharing");
    rows.separator();
    pending(rows, "File Options...", None);
    // The account a local file is open with (#830); a hosted file's is
    // not changed yet (#831).
    let local = context
        .session
        .is_some_and(|session| session.target.local.is_some());
    // Minimum disables it (#651's reading); Editing only is read as doing
    // the same, since Pro's comparison of the two named no difference here
    // [I].
    if rows.item(
        "Change Password...",
        None,
        false,
        context.open() && local && context.full_menus(),
    ) {
        actions.push(Action::ChangePassword);
    }
    rows.separator();
    if rows.item(
        "Page Setup...",
        None,
        false,
        context.open() && context.print && context.full_menus(),
    ) {
        actions.push(Action::Step(42, 0));
    }
    if rows.item(
        "Print...",
        Some(shortcuts::PRINT),
        false,
        context.open()
            && context.print
            && context.full_menus()
            && matches!(context.mode, Mode::Browse | Mode::Preview),
    ) {
        actions.push(Action::Step(43, 0));
    }
    rows.separator();
    // The submenu follows whether the window can import (not in Find
    // mode, for one); its command follows the menu level.
    rows.submenu("Import Records", context.import_records, &mut |rows| {
        if rows.item("File...", None, false, context.full_menus()) {
            actions.push(Action::ImportRecords);
        }
        pending(rows, "Folder...", None);
        pending(rows, "ODBC Data Source...", None);
    });
    if rows.item(
        "Export Records...",
        None,
        false,
        context.export_records && context.full_menus(),
    ) {
        actions.push(Action::ExportRecords);
    }
    rows.submenu("Save/Send Records As", context.open(), &mut |rows| {
        for format in SaveFormat::ALL {
            let enabled = context.save_records.contains(&format) && context.full_menus();
            if rows.item(format.menu_path()[2], None, false, enabled) {
                actions.push(Action::SaveRecordsAs(format));
            }
        }
        // Save Records as Snapshot Link (152) has no implementation.
        pending(rows, "Snapshot Link...", None);
    });
    pending_submenu(rows, "Send");
    pending(rows, "Save a Copy As...", None);
    pending(rows, "Recover...", None);
}

/// File > Manage, also behind Layout mode's Manage toolbar button.
///
/// TODO(authoring): Manage Database, Security, Value Lists, Layouts, Scripts
/// (Script Workspace), External Data Sources, Containers, Custom Functions,
/// Custom Menus and Themes dialogs (captures 32-41). Manage Layouts currently
/// supports searching and opening layouts and Manage Value Lists shows the
/// catalog; their authoring commands are disabled.
pub(crate) fn manage_items(rows: &mut dyn MenuRows, design: bool, actions: &mut Vec<Action>) {
    if rows.item("Database...", command(SHIFT_CMD, Key::D), false, design) {
        actions.push(Action::ManageDatabase);
    }
    if rows.item("Security...", None, false, design) {
        actions.push(Action::ManageSecurity);
    }
    if rows.item("Value Lists...", None, false, design) {
        actions.push(Action::ManageValueLists);
    }
    if rows.item("Layouts...", None, false, design) {
        actions.push(Action::ManageLayouts);
    }
    if rows.item("Scripts...", None, false, design) {
        actions.push(Action::ScriptWorkspace);
    }
    pending(rows, "External Data Sources...", None);
    pending(rows, "Containers...", None);
    pending(rows, "Custom Functions...", None);
    pending(rows, "Custom Menus...", None);
    pending(rows, "Themes...", None);
}

fn edit_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    // The field the user is typing in: its editor's own commands, which
    // its keys also run (a click on the menus leaves it entered). That
    // Editing only keeps them, as it keeps the editors' keys, is inferred.
    let text = context.text_field && context.editing();
    // Undo, Redo, Clear and Select All change the record's own field's
    // text here; a portal row's editor takes only its keys.
    let own = text && context.own_field;
    let layout = context.mode == Mode::Layout;
    let (undo, redo) = if layout {
        context.layout_history
    } else {
        let (undo, redo) = context.field_undo;
        (own && undo, own && redo)
    };
    let undo_label = if undo { "Undo" } else { "Can't Undo" };
    if rows.item(undo_label, Some(shortcuts::UNDO), false, undo) {
        actions.push(if layout {
            Action::EditLayoutDraft(DraftEdit::Undo)
        } else {
            Action::EditField(EditCommand::Undo)
        });
    }
    let redo_label = if redo { "Redo" } else { "Can't Redo" };
    if rows.item(redo_label, Some(shortcuts::REDO), false, redo) {
        actions.push(if layout {
            Action::EditLayoutDraft(DraftEdit::Redo)
        } else {
            Action::EditField(EditCommand::Redo)
        });
    }
    rows.separator();
    if rows.item("Cut", command(CMD, Key::X), false, text) {
        actions.push(Action::EditField(EditCommand::Cut));
    }
    // With no field entered, Copy copies the record (Copy Record/Request).
    let record_copy = context.record_copy && context.editing();
    if rows.item("Copy", command(CMD, Key::C), false, text || record_copy) {
        actions.push(if text {
            Action::EditField(EditCommand::Copy)
        } else {
            Action::Step(steps::COPY_RECORD, 0)
        });
    }
    // The editors paste text only: Paste Text Only pastes as Paste does.
    for (label, shortcut) in [
        ("Paste", command(CMD, Key::V)),
        ("Paste Text Only", command(ALT_SHIFT_CMD, Key::V)),
    ] {
        if rows.item(label, shortcut, false, text) {
            actions.push(Action::EditField(EditCommand::Paste));
        }
    }
    // A field's selection, or in Layout mode the selected object, as
    // Delete deletes them.
    let layout_clear = context.layout_delete && context.full_menus();
    if rows.item("Clear", None, false, own || layout_clear) {
        actions.push(if own {
            Action::EditField(EditCommand::Clear)
        } else {
            Action::DeleteLayoutObject
        });
    }
    rows.separator();
    if context.mode == Mode::Layout {
        if rows.keyed_item(
            "select-all-layout-objects",
            "Select All",
            Some(shortcuts::SELECT_ALL),
            false,
            context.layout_select_all,
        ) {
            actions.push(Action::SelectAllLayoutObjects);
        }
    } else if rows.item("Select All", Some(shortcuts::SELECT_ALL), false, own) {
        actions.push(Action::EditField(EditCommand::SelectAll));
    }
    rows.separator();
    pending_submenu(rows, "Find/Replace");
    pending_submenu(rows, "Spelling");
    rows.separator();
    if rows.item(
        "Export Field Contents...",
        None,
        false,
        context.container && context.editing(),
    ) {
        actions.push(Action::Step(steps::EXPORT_FIELD_CONTENTS, 0));
    }
}

fn view_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    let open = context.open();
    mode_rows(rows, context, &Mode::ALL, actions);
    rows.separator();
    rows.submenu("Go to Layout", open, &mut |rows| {
        if let Some(session) = context.session {
            crate::layout_menu::show(rows, session, context.mode, actions);
        }
    });
    rows.separator();
    view_as_rows(rows, context, actions);
    rows.separator();
    if rows.item(
        "Status Toolbar",
        Some(shortcuts::STATUS_TOOLBAR),
        open && context.toolbar,
        open && context.toolbar_toggle_available,
    ) {
        actions.push(Action::ToggleToolbar);
    }
    pending(rows, "Customize Status Toolbar...", None);
    if rows.item("Formatting Bar", None, open && context.formatting, open) {
        actions.push(Action::ToggleFormattingBar);
    }
    rows.separator();
    if context.mode == Mode::Layout {
        if rows.item("Objects", None, context.objects, open) {
            actions.push(Action::ToggleObjects);
        }
        if rows.item("Inspector", None, context.inspector, open) {
            actions.push(Action::ToggleInspector);
        }
    }
    pending(rows, "Ruler", None);
    rows.separator();
    let zoom_enabled = context.zooms;
    if rows.item(
        "Actual Size",
        None,
        false,
        zoom_enabled && context.zoom != 100,
    ) {
        actions.push(Action::ActualSize);
    }
    if rows.item(
        "Zoom In",
        Some(shortcuts::ZOOM_IN),
        false,
        zoom_enabled && context.zoom < 400,
    ) {
        actions.push(Action::ZoomIn);
    }
    if rows.item(
        "Zoom Out",
        Some(shortcuts::ZOOM_OUT),
        false,
        zoom_enabled && context.zoom > 25,
    ) {
        actions.push(Action::ZoomOut);
    }
}

/// Picture, Audio/Video, PDF and File insert into the selected container
/// field: the script steps without a path, which ask for the file and store
/// it as the steps with a path do, typed by its name (what FileMaker Pro
/// stores for each is not measured, containers.md "Desktop menus and file
/// dialogs").
fn insert_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    // Editing only and Minimum leave both out (FileMaker Pro 26.0.3,
    // #651's reading).
    let insert = context.container_insert && context.full_menus();
    let insert_interactive = insert && context.container_interactive;
    for (label, step, enabled) in [
        ("Picture...", steps::INSERT_PICTURE, insert),
        (
            "Audio/Video...",
            steps::INSERT_AUDIO_VIDEO,
            insert_interactive,
        ),
        ("PDF...", steps::INSERT_PDF, insert_interactive),
        ("File...", steps::INSERT_FILE, insert),
    ] {
        if rows.item(label, None, false, enabled) {
            actions.push(Action::Step(step, 0));
        }
    }
    rows.separator();
    // The field's steps (script-steps.md: 13, 14, 60, 11, 12), on the field
    // entered: inserted at its insertion point, replacing its selection.
    // That Editing only keeps them, as it keeps Export Field Contents, is
    // inferred.
    let text = context.text_field && context.editing();
    for (label, shortcut, step) in [
        (
            "Current Date",
            command(CMD, Key::Minus),
            steps::INSERT_CURRENT_DATE,
        ),
        (
            "Current Time",
            command(CMD, Key::Semicolon),
            steps::INSERT_CURRENT_TIME,
        ),
        (
            "Current User Name",
            command(SHIFT_CMD, Key::N),
            steps::INSERT_CURRENT_USER_NAME,
        ),
    ] {
        if rows.item(label, shortcut, false, text) {
            actions.push(Action::Step(step, 0));
        }
    }
    rows.separator();
    for (label, shortcut, step) in [
        (
            "From Index...",
            command(CMD, Key::I),
            steps::INSERT_FROM_INDEX,
        ),
        (
            "From Last Visited Record",
            command(CMD, Key::Quote),
            steps::INSERT_FROM_LAST_VISITED,
        ),
    ] {
        if rows.item(label, shortcut, false, text) {
            actions.push(Action::Step(step, 0));
        }
    }
}

fn format_menu(rows: &mut dyn MenuRows) {
    for label in [
        "Font",
        "Size",
        "Style",
        "Align Text",
        "Line Spacing",
        "Text Color",
    ] {
        pending_submenu(rows, label);
    }
}

/// View's mode rows, of `modes`.
fn mode_rows(
    rows: &mut dyn MenuRows,
    context: &MenuContext<'_>,
    modes: &[Mode],
    actions: &mut Vec<Action>,
) {
    let open = context.open();
    for &mode in modes {
        if rows.item(
            mode.menu_label(),
            Some(mode.shortcut()),
            open && context.mode == mode,
            open && context.full_menus() && (mode != Mode::Layout || context.design),
        ) && context.mode != mode
        {
            actions.push(Action::SetMode(mode));
        }
    }
}

/// View's View as Form, List and Table.
fn view_as_rows(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    // Editing only and Minimum leave out the views, still checking the
    // current one, and keep the bars and zoom (FileMaker Pro 26.0.3, #651's
    // reading).
    let views_shown = context.open() && context.mode != Mode::Layout;
    for view in ViewMode::ALL {
        let label = format!("View as {}", view.label());
        if rows.item(
            &label,
            None,
            views_shown && context.view == view,
            views_shown
                && context.full_menus()
                && context
                    .session
                    .is_some_and(|session| session.view_enabled(view)),
        ) && context.view != view
        {
            actions.push(Action::SetView(view));
        }
    }
}

fn records_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if context.mode == Mode::Preview {
        let position = context.session.and_then(Session::preview_position);
        let (current, total) = position.unwrap_or((0, 0));
        record_navigation(rows, current, total, true, actions);
        return;
    }
    let total = context.total();
    let current = context.current();
    if rows.item(
        "New Record",
        command(CMD, Key::N),
        false,
        context.open() && context.record_create && context.full_menus(),
    ) {
        actions.push(Action::NewRecord);
    }
    if rows.item(
        "Duplicate Record",
        command(CMD, Key::D),
        false,
        total > 0 && context.record_create && context.full_menus(),
    ) {
        actions.push(Action::DuplicateRecord);
    }
    if rows.item(
        "Delete Record...",
        command(CMD, Key::E),
        false,
        total > 0 && context.record_delete && context.full_menus(),
    ) {
        actions.push(Action::DeleteRecord);
    }
    rows.separator();
    if rows.item(
        "Delete All Records...",
        None,
        false,
        total > 0 && context.record_delete && context.full_menus(),
    ) {
        actions.push(Action::Step(steps::DELETE_ALL, 0));
    }
    rows.separator();
    record_navigation(rows, current, total, false, actions);
    if rows.item(
        "Refresh Window",
        Some(shortcuts::REFRESH),
        false,
        context.open() && context.editing(),
    ) {
        actions.push(Action::Refresh);
    }
    rows.separator();
    let open = context.open() && context.editing();
    for (label, shortcut, step, enabled) in [
        (
            "Show All Records",
            command(CMD, Key::J),
            steps::SHOW_ALL,
            open,
        ),
        ("Show Omitted Only", None, steps::SHOW_OMITTED, open),
        (
            "Omit Record",
            command(CMD, Key::T),
            steps::OMIT_RECORD,
            total > 0 && context.full_menus(),
        ),
    ] {
        if rows.item(label, shortcut, false, enabled) {
            actions.push(Action::Step(step, 0));
        }
    }
    // Omit Multiple Records with its dialog (how many to omit).
    if rows.item(
        "Omit Multiple...",
        command(SHIFT_CMD, Key::T),
        false,
        total > 0 && context.full_menus(),
    ) {
        actions.push(Action::Step(steps::OMIT_MULTIPLE, 0));
    }
    rows.separator();
    if rows.item(
        "Modify Last Find",
        command(CMD, Key::R),
        false,
        open && context.full_menus(),
    ) {
        actions.push(Action::Step(steps::MODIFY_LAST_FIND, 0));
    }
    pending_submenu(rows, "Saved Finds");
    rows.separator();
    if rows.item(
        "Sort Records...",
        command(CMD, Key::S),
        false,
        total > 0 && context.full_menus(),
    ) {
        actions.push(Action::Step(steps::SORT_RECORDS, 0));
    }
    if rows.item("Unsort", None, false, total > 0 && context.full_menus()) {
        actions.push(Action::Step(steps::UNSORT, 0));
    }
    rows.separator();
    pending(rows, "Replace Field Contents...", command(CMD, Key::Equals));
    // Relookup Field Contents (40) with its dialog, on the field entered,
    // as a script runs it (`om_data::scripts::relookup`). Under Editing
    // only and Minimum it stays off, as Sort Records and Revert Record do
    // there [I].
    let [_, label] = om_data::scripts::relookup::MENU_PATH;
    if rows.item(label, None, false, context.relookup && context.full_menus()) {
        actions.push(Action::Step(om_data::scripts::relookup::STEP_ID, 0));
    }
    rows.separator();
    // Revert Record/Request (51) with its dialog, as a script runs it
    // (`om_data::scripts::revert_record`). Under Editing only and Minimum
    // it stays disabled: Pro's readings had no record changed [I].
    let [_, label] = om_data::scripts::revert_record::MENU_PATH;
    if rows.item(
        label,
        None,
        false,
        context.revert_record && context.full_menus(),
    ) {
        actions.push(Action::Step(om_data::scripts::revert_record::STEP_ID, 0));
    }
}

/// Under Editing only and Minimum menus the Requests menu offers nothing,
/// for the Find mode a script leaves (unmeasured in FileMaker Pro [I]).
fn requests_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    let full = context.full_menus();
    if rows.item("Add New Request", Some(shortcuts::NEW_REQUEST), false, full) {
        actions.push(Action::NewRequest);
    }
    if rows.item("Duplicate Request", command(CMD, Key::D), false, full) {
        actions.push(Action::DuplicateRequest);
    }
    let several = context.find.len() > 1;
    if rows.item(
        "Delete Request",
        Some(shortcuts::DELETE_REQUEST),
        false,
        several && full,
    ) {
        actions.push(Action::DeleteRequest);
    }
    rows.separator();
    if rows.item("Show All Records", command(CMD, Key::J), false, full) {
        actions.push(Action::Step(steps::SHOW_ALL, 0));
    }
    rows.separator();
    if rows.item("Perform Find", None, false, full) {
        actions.push(Action::PerformFind);
    }
    if rows.item("Constrain Found Set", None, false, full) {
        actions.push(Action::Step(steps::CONSTRAIN, 0));
    }
    if rows.item("Extend Found Set", None, false, full) {
        actions.push(Action::Step(steps::EXTEND, 0));
    }
    rows.separator();
    // Revert Record/Request (51) with its dialog, as a script runs it
    // (`om_data::scripts::revert_record`).
    let [_, label] = om_data::scripts::revert_record::REQUEST_MENU_PATH;
    if rows.item(label, None, false, context.revert_record && full) {
        actions.push(Action::Step(om_data::scripts::revert_record::STEP_ID, 0));
    }
}

/// New Layout / Report adds a Form layout (`new_layout.rs`); the rest of
/// layout management is still to come. Save Layout and Revert Layout save
/// or drop a field placed with the Field tool (#226).
fn layouts_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if rows.item(
        "New Layout / Report...",
        command(CMD, Key::N),
        false,
        context.open(),
    ) {
        actions.push(Action::NewLayout);
    }
    // Copies the layout the window shows, which then shows the copy.
    let shown = context.session.and_then(|session| session.layout);
    if rows.item(
        "Duplicate Layout",
        None,
        false,
        context.design && shown.is_some(),
    ) {
        if let Some(layout) = shown {
            actions.push(Action::DuplicateLayout(layout));
        }
    }
    // Deletes the layout the window shows, after its alert.
    if rows.item(
        "Delete Layout...",
        command(CMD, Key::E),
        false,
        context.design && shown.is_some(),
    ) {
        if let Some(layout) = shown {
            actions.push(Action::DeleteLayout(layout));
        }
    }
    rows.separator();
    if rows.item(
        "Layout Setup...",
        None,
        false,
        context.design && shown.is_some(),
    ) {
        actions.push(Action::LayoutSetup);
    }
    if rows.item("Manage Layouts...", None, false, true) {
        actions.push(Action::ManageLayouts);
    }
    if rows.item(
        "Part Setup...",
        None,
        false,
        context.design && shown.is_some(),
    ) {
        actions.push(Action::PartSetup);
    }
    pending(rows, "Set Tab Order...", None);
    pending(rows, "Set Sliding/Printing...", None);
    rows.separator();
    if rows.item(
        "Save Layout",
        command(CMD, Key::S),
        false,
        context.layout_edit,
    ) {
        actions.push(Action::SaveLayout);
    }
    if rows.item("Revert Layout", None, false, context.layout_revert) {
        actions.push(Action::RevertLayout);
    }
}

fn arrange_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    pending(rows, "Group", command(CMD, Key::R));
    pending(rows, "Ungroup", command(SHIFT_CMD, Key::R));
    pending(rows, "Lock", command(CMD, Key::L));
    pending(rows, "Unlock", command(SHIFT_CMD, Key::L));
    rows.separator();
    pending(rows, "Bring to Front", None);
    pending(rows, "Bring Forward", None);
    pending(rows, "Send to Back", None);
    pending(rows, "Send Backward", None);
    rows.separator();
    // Align, Distribute and Resize To: `om_data::layout_arrange`, the
    // operation `om-author`'s arrange_objects runs.
    use om_data::layout_arrange::Arrangement;
    for submenu in ["Align", "Distribute", "Resize To"] {
        let items: Vec<Arrangement> = Arrangement::ALL
            .into_iter()
            .filter(|a| a.menu().0 == submenu)
            .collect();
        let enabled = |a: &Arrangement| context.layout_arrange >= a.minimum();
        rows.submenu(submenu, items.iter().any(enabled), &mut |rows| {
            for arrangement in &items {
                if rows.item(arrangement.menu().1, None, false, enabled(arrangement)) {
                    actions.push(Action::Arrange(*arrangement));
                }
            }
        });
    }
}

/// Script Workspace and the scripts supplied by this file's catalog.
fn scripts_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if !context.go
        && rows.item(
            "Script Workspace...",
            command(SHIFT_CMD, Key::S),
            false,
            context.open() && context.design,
        )
    {
        actions.push(Action::ScriptWorkspace);
    }
    // The catalog is read the first time the menu shows it.
    if !context.scripts_read {
        if context.open() {
            rows.separator();
            rows.item("Loading Scripts…", None, false, false);
            if context.scripts_unread && rows.ask("scripts") {
                actions.push(Action::ReadScripts);
            }
        }
        return;
    }
    // Members left out of the menu (script-steps.md 1: member flag 0x8)
    // are not listed; a folder left out hides what it holds.
    let mut hidden_depth: Option<usize> = None;
    let mut depth = 0usize;
    let mut listed = Vec::new();
    for script in &context.scripts.scripts {
        match script.kind {
            om_data::ScriptKind::Folder => {
                if hidden_depth.is_none() && !script.in_scripts_menu() {
                    hidden_depth = Some(depth);
                }
                if hidden_depth.is_none() {
                    listed.push(script);
                }
                depth += 1;
            }
            om_data::ScriptKind::FolderEnd => {
                depth = depth.saturating_sub(1);
                if hidden_depth == Some(depth) {
                    hidden_depth = None;
                }
            }
            _ if hidden_depth.is_none()
                && script.in_scripts_menu()
                && context
                    .set_access
                    .is_none_or(|access| access.runs_script(script.id)) =>
            {
                listed.push(script)
            }
            _ => {}
        }
    }
    if listed.is_empty() {
        return;
    }
    rows.separator();
    for script in listed {
        match script.kind {
            om_data::ScriptKind::Script => {
                let key = format!("script {}", script.id);
                if rows.keyed_item(&key, &script.name, None, false, context.open()) {
                    actions.push(Action::RunScript(script.id));
                }
            }
            om_data::ScriptKind::Folder => {
                rows.note(&script.name);
            }
            om_data::ScriptKind::Separator => rows.separator(),
            om_data::ScriptKind::FolderEnd => {}
        }
    }
}

/// TODO(authoring): developer tools (capture 23, 43).
fn tools_menu(rows: &mut dyn MenuRows) {
    pending(rows, "Script Debugger", None);
    pending_submenu(rows, "Debugging Controls");
    pending(rows, "Data Viewer", None);
    rows.separator();
    pending_submenu(rows, "Custom Menus");
    rows.separator();
    pending(rows, "Save a Copy as XML...", None);
    pending(rows, "Database Design Report...", None);
    pending(rows, "Developer Utilities...", None);
    pending(rows, "Tools Marketplace", None);
    pending(rows, "Create Add-on...", None);
}

fn window_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    // Editing only and Minimum disable New Window (FileMaker Pro Help,
    // "Editing other privileges").
    if rows.item(
        "New Window",
        None,
        false,
        context.open() && context.full_menus(),
    ) {
        // As the step does: a window like this one, named after it.
        actions.push(Action::Step(crate::app::steps::NEW_WINDOW, 0));
    }
    // Every window, a hidden one in parentheses: choosing it shows it
    // (Claris documents Show Window > window name for a hidden window;
    // the rest is inferred, client-ui.md). With Home in front it is the
    // way to a file a script opened hidden (#679).
    rows.submenu(
        "Show Window",
        !context.show_windows.is_empty(),
        // A script can open many files hidden: the list scrolls, as File
        // > Favorites' does, so none is out of reach.
        &mut |rows| {
            rows.scrolled(&mut |rows| {
                for (id, title, hidden) in context.show_windows {
                    let this = context.open() && *id == context.window;
                    let label = if *hidden {
                        format!("({title})")
                    } else {
                        title.clone()
                    };
                    if rows.keyed_item(&format!("window {id}"), &label, None, this, true)
                        && (!this || *hidden)
                    {
                        actions.push(Action::SelectWindow(*id));
                    }
                }
            });
        },
    );
    pending(rows, "Hide Window", None);
    if rows.item("Minimize Window", Some(shortcuts::MINIMIZE), false, true) {
        actions.push(Action::Minimize);
    }
    rows.separator();
    pending(rows, "Tile Horizontally", None);
    pending(rows, "Tile Vertically", None);
    pending(rows, "Cascade Windows", None);
    pending(rows, "Bring All To Front", None);
    if context.open() {
        rows.separator();
        for (id, title, _) in context.windows {
            let this = *id == context.window;
            if rows.keyed_item(&format!("window {id}"), title, None, this, true) && !this {
                actions.push(Action::SelectWindow(*id));
            }
        }
        if context.windows.is_empty() {
            rows.item(context.window_title, None, true, true);
        }
    }
}

fn help_menu(rows: &mut dyn MenuRows) {
    pending(rows, &format!("{} Help", crate::APP_TITLE), None);
    pending(rows, "Keyboard Shortcuts", None);
    rows.separator();
    pending(rows, "Product Documentation", None);
    rows.separator();
    pending(rows, "Check for Updates...", None);
}

fn record_navigation(
    rows: &mut dyn MenuRows,
    current: u64,
    total: u64,
    preview: bool,
    actions: &mut Vec<Action>,
) {
    let navigate = |index| {
        if preview {
            Action::GoToPage(index)
        } else {
            Action::GoToRecord(index)
        }
    };
    rows.submenu("Go to Record", total > 0, &mut |rows| {
        if rows.item("First", None, false, current > 0) {
            actions.push(navigate(0));
        }
        if rows.item("Previous", Some(shortcuts::PREVIOUS), false, current > 0) {
            actions.push(navigate(current.saturating_sub(1)));
        }
        if rows.item("Next", Some(shortcuts::NEXT), false, current + 1 < total) {
            actions.push(navigate(current + 1));
        }
        if rows.item("Last", None, false, current + 1 < total) {
            actions.push(navigate(total.saturating_sub(1)));
        }
    });
}
