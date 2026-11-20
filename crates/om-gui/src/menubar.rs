//! The menu bar, with FileMaker's menus and items (captures 16-25).
//!
//! FileMaker on macOS uses the system menu bar; OpenMaker draws it in the
//! window on every platform for now ([#55](https://github.com/OpenMaker-Dev/openmaker/issues/55)). The menus are built
//! through [`MenuRows`], so that another place to show them shows the same.
//! Items OpenMaker does not implement yet are present but disabled, so the
//! menus read like FileMaker's and fill in as features land.

use egui::{Key, KeyboardShortcut, Modifiers, Ui};
use om_data::scripts::save_records::Format as SaveFormat;

use crate::app::{shortcuts, steps, Action, FindState, Mode, ViewMode};
use crate::session::Session;
use crate::theme;
use crate::widgets::{menu_item, menu_separator, submenu};

/// Everything the menus read.
pub(crate) struct MenuContext<'a> {
    pub mode: Mode,
    pub view: ViewMode,
    pub zoom: u16,
    pub session: Option<&'a Session>,
    pub find: &'a FindState,
    pub toolbar: bool,
    pub formatting: bool,
    pub inspector: bool,
    pub objects: bool,
    pub window_title: &'a str,
    /// The file's windows (id, title, name), front first: the Window
    /// menu's list, with a check at this window (`window`).
    pub windows: &'a [(u32, String, String)],
    pub window: u32,
    pub scripts: &'a om_data::ScriptCatalog,
    /// Whether `scripts` is the file's catalog as read: until it is, the
    /// Scripts menu asks for it ([`Action::ReadScripts`], #1053).
    pub scripts_read: bool,
    /// The account's privilege set as the host enforces it: the Scripts
    /// menu leaves out the scripts it gives no access.
    pub set_access: Option<&'a om_data::host::security::SetAccess>,
    pub record_create: bool,
    pub record_delete: bool,
    /// Records > Revert Record... (Browse) or Requests > Revert Request...
    /// (Find) can run (`om_data::scripts::revert_record::available`).
    pub revert_record: bool,
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
    pub client_license: bool,
    /// Layout mode has a field placed with the Field tool and not yet
    /// saved (Save Layout, Revert Layout).
    pub layout_edit: bool,
    /// Layout mode has an object selected that can be deleted (Edit >
    /// Clear).
    pub layout_delete: bool,
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
    fn separator(&mut self);
    /// A submenu, whose rows `rows` adds.
    fn submenu(&mut self, label: &str, enabled: bool, rows: &mut dyn FnMut(&mut dyn MenuRows));
    /// A line of text that is not a command (a script folder's name).
    fn note(&mut self, text: &str);
    /// The user is looking at these rows now: a menu that reads its
    /// content when shown asks for it.
    fn shown(&self) -> bool;
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

pub(crate) fn show(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    let frame = egui::Frame::new()
        .fill(theme::TOOLBAR_BG)
        .inner_margin(egui::Margin::symmetric(6, 0));
    egui::Panel::top("menubar")
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
                    ui.menu_button(
                        egui::RichText::new(crate::APP_TITLE)
                            .font(egui::FontId::new(theme::BODY, theme::semibold())),
                        |ui| app_menu(&mut Drawn(ui), context, actions),
                    );
                    // Under Editing only or Minimum menus (session-identity.md
                    // 2.3) each menu gates its own items, so that a row's
                    // look and its click agree: a whole menu disabled at
                    // its start drew some rows as enabled.
                    ui.menu_button("File", |ui| file_menu(&mut Drawn(ui), context, actions));
                    ui.menu_button("Edit", |ui| edit_menu(&mut Drawn(ui), context, actions));
                    ui.menu_button("View", |ui| view_menu(&mut Drawn(ui), context, actions));
                    ui.menu_button("Insert", |ui| insert_menu(&mut Drawn(ui), context, actions));
                    ui.menu_button("Format", |ui| format_menu(&mut Drawn(ui)));
                    match context.mode {
                        Mode::Browse | Mode::Preview => {
                            ui.menu_button("Records", |ui| {
                                records_menu(&mut Drawn(ui), context, actions)
                            });
                        }
                        Mode::Find => {
                            ui.menu_button("Requests", |ui| {
                                requests_menu(&mut Drawn(ui), context, actions)
                            });
                        }
                        Mode::Layout => {
                            ui.menu_button("Layouts", |ui| {
                                layouts_menu(&mut Drawn(ui), context, actions)
                            });
                            ui.menu_button("Arrange", |ui| arrange_menu(&mut Drawn(ui)));
                        }
                    }
                    ui.menu_button("Scripts", |ui| {
                        scripts_menu(&mut Drawn(ui), context, actions)
                    });
                    ui.menu_button("Tools", |ui| tools_menu(&mut Drawn(ui)));
                    ui.menu_button("Window", |ui| window_menu(&mut Drawn(ui), context, actions));
                    ui.menu_button("Help", |ui| help_menu(&mut Drawn(ui)));
                });
        });
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
    pending_submenu(rows, "Favorites");
    pending_submenu(rows, "Recent");
    rows.submenu("Hosts", true, &mut |rows| {
        if rows.item("Show Hosts...", None, false, true) {
            actions.push(Action::CloseFile);
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
    pending(rows, "Can't Undo", command(CMD, Key::Z));
    pending(rows, "Can't Redo", command(SHIFT_CMD, Key::Z));
    rows.separator();
    pending(rows, "Cut", command(CMD, Key::X));
    pending(rows, "Copy", command(CMD, Key::C));
    pending(rows, "Paste", command(CMD, Key::V));
    pending(rows, "Paste Text Only", command(ALT_SHIFT_CMD, Key::V));
    // Layout mode: delete the selected object, as Delete does.
    if rows.item(
        "Clear",
        None,
        false,
        context.layout_delete && context.full_menus(),
    ) {
        actions.push(Action::DeleteLayoutObject);
    }
    rows.separator();
    pending(rows, "Select All", command(CMD, Key::A));
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
    for mode in Mode::ALL {
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
    rows.separator();
    rows.submenu("Go to Layout", open, &mut |rows| {
        if let Some(session) = context.session {
            crate::layout_menu::show(rows, session, context.mode, actions);
        }
    });
    rows.separator();
    // Editing only and Minimum leave out the views, still checking the
    // current one, and keep the bars and zoom (FileMaker Pro 26.0.3, #651's
    // reading).
    let views_shown = open && context.mode != Mode::Layout;
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
    rows.separator();
    if rows.item(
        "Status Toolbar",
        Some(shortcuts::STATUS_TOOLBAR),
        open && context.toolbar,
        open,
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
    let zoom_enabled = context
        .session
        .is_some_and(|session| matches!(session.render, crate::session::RenderState::Loaded(_)))
        && ((context.mode == Mode::Browse && context.view == ViewMode::Form)
            || context.mode == Mode::Preview);
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

/// Picture and File insert into the selected container field (the
/// script steps without a path, which ask for the file). Audio/Video and
/// PDF stay disabled: OpenMaker plays no media and shows no PDF, and how
/// FileMaker Pro stores what those commands insert is not measured.
fn insert_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    // Editing only and Minimum leave both out (FileMaker Pro 26.0.3,
    // #651's reading).
    let insert = context.container_insert && context.full_menus();
    if rows.item("Picture...", None, false, insert) {
        actions.push(Action::Step(steps::INSERT_PICTURE, 0));
    }
    pending(rows, "Audio/Video...", None);
    pending(rows, "PDF...", None);
    if rows.item("File...", None, false, insert) {
        actions.push(Action::Step(steps::INSERT_FILE, 0));
    }
    rows.separator();
    pending(rows, "Current Date", command(CMD, Key::Minus));
    pending(rows, "Current Time", command(CMD, Key::Semicolon));
    pending(rows, "Current User Name", command(SHIFT_CMD, Key::N));
    rows.separator();
    pending(rows, "From Index...", command(CMD, Key::I));
    pending(rows, "From Last Visited Record", command(CMD, Key::Quote));
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
    pending(rows, "Relookup Field Contents", None);
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
    if rows.item("Revert Layout", None, false, context.layout_edit) {
        actions.push(Action::RevertLayout);
    }
}

fn arrange_menu(rows: &mut dyn MenuRows) {
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
    pending_submenu(rows, "Align");
    pending_submenu(rows, "Distribute");
    pending_submenu(rows, "Resize To");
}

/// Script Workspace and the scripts supplied by this file's catalog.
fn scripts_menu(rows: &mut dyn MenuRows, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if rows.item(
        "Script Workspace...",
        command(SHIFT_CMD, Key::S),
        false,
        context.open() && context.design,
    ) {
        actions.push(Action::ScriptWorkspace);
    }
    // The catalog is read the first time the menu shows it.
    if !context.scripts_read {
        if context.open() {
            rows.separator();
            rows.item("Loading Scripts…", None, false, false);
            if rows.shown() {
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
                if rows.item(&script.name, None, false, context.open()) {
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
    pending_submenu(rows, "Show Window");
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
            if rows.item(title, None, this, true) && !this {
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
