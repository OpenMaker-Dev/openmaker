//! The menu bar, with FileMaker's menus and items (captures 16-25).
//!
//! FileMaker on macOS uses the system menu bar; OpenMaker draws it in the
//! window on every platform for now (TODO(platform): native macOS menus).
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

/// A menu row; returns whether it was chosen.
fn item(
    ui: &mut Ui,
    label: &str,
    shortcut: Option<KeyboardShortcut>,
    checked: bool,
    enabled: bool,
) -> bool {
    let text = shortcut.map(|shortcut| shortcut_text(ui, shortcut));
    menu_item(ui, label, text.as_deref(), checked, enabled).clicked()
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
fn pending(ui: &mut Ui, label: &str, shortcut: Option<KeyboardShortcut>) {
    item(ui, label, shortcut, false, false);
}

/// A submenu whose commands are all still to come.
fn pending_submenu(ui: &mut Ui, label: &str) {
    submenu(ui, label, false, |_ui| {});
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
                        |ui| app_menu(ui, context, actions),
                    );
                    // Under Editing only or Minimum menus (session-identity.md
                    // 2.3) each menu gates its own items, so that a row's
                    // look and its click agree: a whole menu disabled at
                    // its start drew some rows as enabled.
                    ui.menu_button("File", |ui| file_menu(ui, context, actions));
                    ui.menu_button("Edit", |ui| edit_menu(ui, context, actions));
                    ui.menu_button("View", |ui| view_menu(ui, context, actions));
                    ui.menu_button("Insert", |ui| insert_menu(ui, context, actions));
                    ui.menu_button("Format", format_menu);
                    match context.mode {
                        Mode::Browse | Mode::Preview => {
                            ui.menu_button("Records", |ui| records_menu(ui, context, actions));
                        }
                        Mode::Find => {
                            ui.menu_button("Requests", |ui| requests_menu(ui, context, actions));
                        }
                        Mode::Layout => {
                            ui.menu_button("Layouts", |ui| layouts_menu(ui, context, actions));
                            ui.menu_button("Arrange", arrange_menu);
                        }
                    }
                    ui.menu_button("Scripts", |ui| scripts_menu(ui, context, actions));
                    ui.menu_button("Tools", tools_menu);
                    ui.menu_button("Window", |ui| window_menu(ui, context, actions));
                    ui.menu_button("Help", help_menu);
                });
        });
}

fn app_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    pending(ui, &format!("About {}", crate::APP_TITLE), None);
    menu_separator(ui);
    if item(ui, "Settings...", Some(shortcuts::SETTINGS), false, true) {
        actions.push(Action::ShowSettings);
    }
    if item(
        ui,
        "FileMaker License...",
        None,
        false,
        context.client_license,
    ) {
        actions.push(Action::ManageLicense);
    }
    menu_separator(ui);
    if item(
        ui,
        &format!("Quit {}", crate::APP_TITLE),
        Some(shortcuts::QUIT),
        false,
        true,
    ) {
        actions.push(Action::Quit);
    }
}

fn file_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if item(ui, om_data::new_file::CREATE_FILE_LABEL, None, false, true) {
        actions.push(Action::ShowCreateNew);
    }
    menu_separator(ui);
    pending(ui, "Sign In to Claris ID", None);
    pending_submenu(ui, "Favorites");
    pending_submenu(ui, "Recent");
    submenu(ui, "Hosts", true, |ui| {
        if item(ui, "Show Hosts...", None, false, true) {
            actions.push(Action::CloseFile);
        }
    });
    if item(ui, "Open...", Some(shortcuts::OPEN), false, true) {
        actions.push(Action::ShowOpenFile);
    }
    pending(ui, "Open Quickly...", command(CMD, Key::K));
    if item(ui, "Close", Some(shortcuts::CLOSE), false, context.open()) {
        actions.push(Action::CloseFile);
    }
    menu_separator(ui);
    // Under Editing only and Minimum the Manage, Import Records and
    // Save/Send Records As submenus open, every command in them disabled
    // (FileMaker Pro 26.0.3, #651's reading).
    submenu(ui, "Manage", context.open(), |ui| {
        manage_items(ui, context.design && context.full_menus(), actions)
    });
    menu_separator(ui);
    pending_submenu(ui, "Sharing");
    menu_separator(ui);
    pending(ui, "File Options...", None);
    // The account a local file is open with (#830); a hosted file's is
    // not changed yet (#831).
    let local = context
        .session
        .is_some_and(|session| session.target.local.is_some());
    // Minimum disables it (#651's reading); Editing only is read as doing
    // the same, since Pro's comparison of the two named no difference here
    // [I].
    if item(
        ui,
        "Change Password...",
        None,
        false,
        context.open() && local && context.full_menus(),
    ) {
        actions.push(Action::ChangePassword);
    }
    menu_separator(ui);
    if item(
        ui,
        "Page Setup...",
        None,
        false,
        context.open() && context.print && context.full_menus(),
    ) {
        actions.push(Action::Step(42, 0));
    }
    if item(
        ui,
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
    menu_separator(ui);
    // The submenu follows whether the window can import (not in Find
    // mode, for one); its command follows the menu level.
    submenu(ui, "Import Records", context.import_records, |ui| {
        if item(ui, "File...", None, false, context.full_menus()) {
            actions.push(Action::ImportRecords);
        }
        pending(ui, "Folder...", None);
        pending(ui, "ODBC Data Source...", None);
    });
    if item(
        ui,
        "Export Records...",
        None,
        false,
        context.export_records && context.full_menus(),
    ) {
        actions.push(Action::ExportRecords);
    }
    submenu(ui, "Save/Send Records As", context.open(), |ui| {
        for format in SaveFormat::ALL {
            let enabled = context.save_records.contains(&format) && context.full_menus();
            if item(ui, format.menu_path()[2], None, false, enabled) {
                actions.push(Action::SaveRecordsAs(format));
            }
        }
        // Save Records as Snapshot Link (152) has no implementation.
        pending(ui, "Snapshot Link...", None);
    });
    pending_submenu(ui, "Send");
    pending(ui, "Save a Copy As...", None);
    pending(ui, "Recover...", None);
}

/// File > Manage, also behind Layout mode's Manage toolbar button.
///
/// TODO(authoring): Manage Database, Security, Value Lists, Layouts, Scripts
/// (Script Workspace), External Data Sources, Containers, Custom Functions,
/// Custom Menus and Themes dialogs (captures 32-41). Manage Layouts currently
/// supports searching and opening layouts and Manage Value Lists shows the
/// catalog; their authoring commands are disabled.
pub(crate) fn manage_items(ui: &mut Ui, design: bool, actions: &mut Vec<Action>) {
    if item(ui, "Database...", command(SHIFT_CMD, Key::D), false, design) {
        actions.push(Action::ManageDatabase);
    }
    if item(ui, "Security...", None, false, design) {
        actions.push(Action::ManageSecurity);
    }
    if item(ui, "Value Lists...", None, false, design) {
        actions.push(Action::ManageValueLists);
    }
    if item(ui, "Layouts...", None, false, design) {
        actions.push(Action::ManageLayouts);
    }
    if item(ui, "Scripts...", None, false, design) {
        actions.push(Action::ScriptWorkspace);
    }
    pending(ui, "External Data Sources...", None);
    pending(ui, "Containers...", None);
    pending(ui, "Custom Functions...", None);
    pending(ui, "Custom Menus...", None);
    pending(ui, "Themes...", None);
}

fn edit_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    pending(ui, "Can't Undo", command(CMD, Key::Z));
    pending(ui, "Can't Redo", command(SHIFT_CMD, Key::Z));
    menu_separator(ui);
    pending(ui, "Cut", command(CMD, Key::X));
    pending(ui, "Copy", command(CMD, Key::C));
    pending(ui, "Paste", command(CMD, Key::V));
    pending(ui, "Paste Text Only", command(ALT_SHIFT_CMD, Key::V));
    // Layout mode: delete the selected object, as Delete does.
    if item(
        ui,
        "Clear",
        None,
        false,
        context.layout_delete && context.full_menus(),
    ) {
        actions.push(Action::DeleteLayoutObject);
    }
    menu_separator(ui);
    pending(ui, "Select All", command(CMD, Key::A));
    menu_separator(ui);
    pending_submenu(ui, "Find/Replace");
    pending_submenu(ui, "Spelling");
    menu_separator(ui);
    if item(
        ui,
        "Export Field Contents...",
        None,
        false,
        context.container && context.editing(),
    ) {
        actions.push(Action::Step(steps::EXPORT_FIELD_CONTENTS, 0));
    }
}

fn view_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    let open = context.open();
    for mode in Mode::ALL {
        if item(
            ui,
            mode.menu_label(),
            Some(mode.shortcut()),
            open && context.mode == mode,
            open && context.full_menus() && (mode != Mode::Layout || context.design),
        ) && context.mode != mode
        {
            actions.push(Action::SetMode(mode));
        }
    }
    menu_separator(ui);
    submenu(ui, "Go to Layout", open, |ui| {
        if let Some(session) = context.session {
            crate::layout_menu::show(ui, session, context.mode, actions);
        }
    });
    menu_separator(ui);
    // Editing only and Minimum leave out the views, still checking the
    // current one, and keep the bars and zoom (FileMaker Pro 26.0.3, #651's
    // reading).
    let views_shown = open && context.mode != Mode::Layout;
    for view in ViewMode::ALL {
        let label = format!("View as {}", view.label());
        if item(
            ui,
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
    menu_separator(ui);
    if item(
        ui,
        "Status Toolbar",
        Some(shortcuts::STATUS_TOOLBAR),
        open && context.toolbar,
        open,
    ) {
        actions.push(Action::ToggleToolbar);
    }
    pending(ui, "Customize Status Toolbar...", None);
    if item(ui, "Formatting Bar", None, open && context.formatting, open) {
        actions.push(Action::ToggleFormattingBar);
    }
    menu_separator(ui);
    if context.mode == Mode::Layout {
        if item(ui, "Objects", None, context.objects, open) {
            actions.push(Action::ToggleObjects);
        }
        if item(ui, "Inspector", None, context.inspector, open) {
            actions.push(Action::ToggleInspector);
        }
    }
    pending(ui, "Ruler", None);
    menu_separator(ui);
    let zoom_enabled = context
        .session
        .is_some_and(|session| matches!(session.render, crate::session::RenderState::Loaded(_)))
        && ((context.mode == Mode::Browse && context.view == ViewMode::Form)
            || context.mode == Mode::Preview);
    if item(
        ui,
        "Actual Size",
        None,
        false,
        zoom_enabled && context.zoom != 100,
    ) {
        actions.push(Action::ActualSize);
    }
    if item(
        ui,
        "Zoom In",
        Some(shortcuts::ZOOM_IN),
        false,
        zoom_enabled && context.zoom < 400,
    ) {
        actions.push(Action::ZoomIn);
    }
    if item(
        ui,
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
fn insert_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    // Editing only and Minimum leave both out (FileMaker Pro 26.0.3,
    // #651's reading).
    let insert = context.container_insert && context.full_menus();
    if item(ui, "Picture...", None, false, insert) {
        actions.push(Action::Step(steps::INSERT_PICTURE, 0));
    }
    pending(ui, "Audio/Video...", None);
    pending(ui, "PDF...", None);
    if item(ui, "File...", None, false, insert) {
        actions.push(Action::Step(steps::INSERT_FILE, 0));
    }
    menu_separator(ui);
    pending(ui, "Current Date", command(CMD, Key::Minus));
    pending(ui, "Current Time", command(CMD, Key::Semicolon));
    pending(ui, "Current User Name", command(SHIFT_CMD, Key::N));
    menu_separator(ui);
    pending(ui, "From Index...", command(CMD, Key::I));
    pending(ui, "From Last Visited Record", command(CMD, Key::Quote));
}

fn format_menu(ui: &mut Ui) {
    for label in [
        "Font",
        "Size",
        "Style",
        "Align Text",
        "Line Spacing",
        "Text Color",
    ] {
        pending_submenu(ui, label);
    }
}

fn records_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if context.mode == Mode::Preview {
        let position = context.session.and_then(Session::preview_position);
        let (current, total) = position.unwrap_or((0, 0));
        record_navigation(ui, current, total, true, actions);
        return;
    }
    let total = context.total();
    let current = context.current();
    if item(
        ui,
        "New Record",
        command(CMD, Key::N),
        false,
        context.open() && context.record_create && context.full_menus(),
    ) {
        actions.push(Action::NewRecord);
    }
    if item(
        ui,
        "Duplicate Record",
        command(CMD, Key::D),
        false,
        total > 0 && context.record_create && context.full_menus(),
    ) {
        actions.push(Action::DuplicateRecord);
    }
    if item(
        ui,
        "Delete Record...",
        command(CMD, Key::E),
        false,
        total > 0 && context.record_delete && context.full_menus(),
    ) {
        actions.push(Action::DeleteRecord);
    }
    menu_separator(ui);
    if item(
        ui,
        "Delete All Records...",
        None,
        false,
        total > 0 && context.record_delete && context.full_menus(),
    ) {
        actions.push(Action::Step(steps::DELETE_ALL, 0));
    }
    menu_separator(ui);
    record_navigation(ui, current, total, false, actions);
    if item(
        ui,
        "Refresh Window",
        Some(shortcuts::REFRESH),
        false,
        context.open() && context.editing(),
    ) {
        actions.push(Action::Refresh);
    }
    menu_separator(ui);
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
        if item(ui, label, shortcut, false, enabled) {
            actions.push(Action::Step(step, 0));
        }
    }
    // Omit Multiple Records with its dialog (how many to omit).
    if item(
        ui,
        "Omit Multiple...",
        command(SHIFT_CMD, Key::T),
        false,
        total > 0 && context.full_menus(),
    ) {
        actions.push(Action::Step(steps::OMIT_MULTIPLE, 0));
    }
    menu_separator(ui);
    if item(
        ui,
        "Modify Last Find",
        command(CMD, Key::R),
        false,
        open && context.full_menus(),
    ) {
        actions.push(Action::Step(steps::MODIFY_LAST_FIND, 0));
    }
    pending_submenu(ui, "Saved Finds");
    menu_separator(ui);
    if item(
        ui,
        "Sort Records...",
        command(CMD, Key::S),
        false,
        total > 0 && context.full_menus(),
    ) {
        actions.push(Action::Step(steps::SORT_RECORDS, 0));
    }
    if item(ui, "Unsort", None, false, total > 0 && context.full_menus()) {
        actions.push(Action::Step(steps::UNSORT, 0));
    }
    menu_separator(ui);
    pending(ui, "Replace Field Contents...", command(CMD, Key::Equals));
    pending(ui, "Relookup Field Contents", None);
    menu_separator(ui);
    // Revert Record/Request (51) with its dialog, as a script runs it
    // (`om_data::scripts::revert_record`). Under Editing only and Minimum
    // it stays disabled: Pro's readings had no record changed [I].
    let [_, label] = om_data::scripts::revert_record::MENU_PATH;
    if item(
        ui,
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
fn requests_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    let full = context.full_menus();
    if item(
        ui,
        "Add New Request",
        Some(shortcuts::NEW_REQUEST),
        false,
        full,
    ) {
        actions.push(Action::NewRequest);
    }
    if item(ui, "Duplicate Request", command(CMD, Key::D), false, full) {
        actions.push(Action::DuplicateRequest);
    }
    let several = context.find.len() > 1;
    if item(
        ui,
        "Delete Request",
        Some(shortcuts::DELETE_REQUEST),
        false,
        several && full,
    ) {
        actions.push(Action::DeleteRequest);
    }
    menu_separator(ui);
    if item(ui, "Show All Records", command(CMD, Key::J), false, full) {
        actions.push(Action::Step(steps::SHOW_ALL, 0));
    }
    menu_separator(ui);
    if item(ui, "Perform Find", None, false, full) {
        actions.push(Action::PerformFind);
    }
    if item(ui, "Constrain Found Set", None, false, full) {
        actions.push(Action::Step(steps::CONSTRAIN, 0));
    }
    if item(ui, "Extend Found Set", None, false, full) {
        actions.push(Action::Step(steps::EXTEND, 0));
    }
    menu_separator(ui);
    // Revert Record/Request (51) with its dialog, as a script runs it
    // (`om_data::scripts::revert_record`).
    let [_, label] = om_data::scripts::revert_record::REQUEST_MENU_PATH;
    if item(ui, label, None, false, context.revert_record && full) {
        actions.push(Action::Step(om_data::scripts::revert_record::STEP_ID, 0));
    }
}

/// New Layout / Report adds a Form layout (`new_layout.rs`); the rest of
/// layout management is still to come. Save Layout and Revert Layout save
/// or drop a field placed with the Field tool (#226).
fn layouts_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if item(
        ui,
        "New Layout / Report...",
        command(CMD, Key::N),
        false,
        context.open(),
    ) {
        actions.push(Action::NewLayout);
    }
    // Copies the layout the window shows, which then shows the copy.
    let shown = context.session.and_then(|session| session.layout);
    if item(
        ui,
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
    if item(
        ui,
        "Delete Layout...",
        command(CMD, Key::E),
        false,
        context.design && shown.is_some(),
    ) {
        if let Some(layout) = shown {
            actions.push(Action::DeleteLayout(layout));
        }
    }
    menu_separator(ui);
    if item(
        ui,
        "Layout Setup...",
        None,
        false,
        context.design && shown.is_some(),
    ) {
        actions.push(Action::LayoutSetup);
    }
    if item(ui, "Manage Layouts...", None, false, true) {
        actions.push(Action::ManageLayouts);
    }
    if item(
        ui,
        "Part Setup...",
        None,
        false,
        context.design && shown.is_some(),
    ) {
        actions.push(Action::PartSetup);
    }
    pending(ui, "Set Tab Order...", None);
    pending(ui, "Set Sliding/Printing...", None);
    menu_separator(ui);
    if item(
        ui,
        "Save Layout",
        command(CMD, Key::S),
        false,
        context.layout_edit,
    ) {
        actions.push(Action::SaveLayout);
    }
    if item(ui, "Revert Layout", None, false, context.layout_edit) {
        actions.push(Action::RevertLayout);
    }
}

fn arrange_menu(ui: &mut Ui) {
    pending(ui, "Group", command(CMD, Key::R));
    pending(ui, "Ungroup", command(SHIFT_CMD, Key::R));
    pending(ui, "Lock", command(CMD, Key::L));
    pending(ui, "Unlock", command(SHIFT_CMD, Key::L));
    menu_separator(ui);
    pending(ui, "Bring to Front", None);
    pending(ui, "Bring Forward", None);
    pending(ui, "Send to Back", None);
    pending(ui, "Send Backward", None);
    menu_separator(ui);
    pending_submenu(ui, "Align");
    pending_submenu(ui, "Distribute");
    pending_submenu(ui, "Resize To");
}

/// Script Workspace and the scripts supplied by this file's catalog.
fn scripts_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    if item(
        ui,
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
            ui.separator();
            item(ui, "Loading Scripts…", None, false, false);
            actions.push(Action::ReadScripts);
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
    menu_separator(ui);
    for script in listed {
        match script.kind {
            om_data::ScriptKind::Script => {
                if item(ui, &script.name, None, false, context.open()) {
                    actions.push(Action::RunScript(script.id));
                }
            }
            om_data::ScriptKind::Folder => {
                ui.weak(&script.name);
            }
            om_data::ScriptKind::Separator => menu_separator(ui),
            om_data::ScriptKind::FolderEnd => {}
        }
    }
}

/// TODO(authoring): developer tools (capture 23, 43).
fn tools_menu(ui: &mut Ui) {
    pending(ui, "Script Debugger", None);
    pending_submenu(ui, "Debugging Controls");
    pending(ui, "Data Viewer", None);
    menu_separator(ui);
    pending_submenu(ui, "Custom Menus");
    menu_separator(ui);
    pending(ui, "Save a Copy as XML...", None);
    pending(ui, "Database Design Report...", None);
    pending(ui, "Developer Utilities...", None);
    pending(ui, "Tools Marketplace", None);
    pending(ui, "Create Add-on...", None);
}

fn window_menu(ui: &mut Ui, context: &MenuContext<'_>, actions: &mut Vec<Action>) {
    // Editing only and Minimum disable New Window (FileMaker Pro Help,
    // "Editing other privileges").
    if item(
        ui,
        "New Window",
        None,
        false,
        context.open() && context.full_menus(),
    ) {
        // As the step does: a window like this one, named after it.
        actions.push(Action::Step(crate::app::steps::NEW_WINDOW, 0));
    }
    pending_submenu(ui, "Show Window");
    pending(ui, "Hide Window", None);
    if item(
        ui,
        "Minimize Window",
        Some(shortcuts::MINIMIZE),
        false,
        true,
    ) {
        actions.push(Action::Minimize);
    }
    menu_separator(ui);
    pending(ui, "Tile Horizontally", None);
    pending(ui, "Tile Vertically", None);
    pending(ui, "Cascade Windows", None);
    pending(ui, "Bring All To Front", None);
    if context.open() {
        menu_separator(ui);
        for (id, title, _) in context.windows {
            let this = *id == context.window;
            if item(ui, title, None, this, true) && !this {
                actions.push(Action::SelectWindow(*id));
            }
        }
        if context.windows.is_empty() {
            item(ui, context.window_title, None, true, true);
        }
    }
}

fn help_menu(ui: &mut Ui) {
    pending(ui, &format!("{} Help", crate::APP_TITLE), None);
    pending(ui, "Keyboard Shortcuts", None);
    menu_separator(ui);
    pending(ui, "Product Documentation", None);
    menu_separator(ui);
    pending(ui, "Check for Updates...", None);
}

fn record_navigation(
    ui: &mut Ui,
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
    submenu(ui, "Go to Record", total > 0, |ui| {
        if item(ui, "First", None, false, current > 0) {
            actions.push(navigate(0));
        }
        if item(
            ui,
            "Previous",
            Some(shortcuts::PREVIOUS),
            false,
            current > 0,
        ) {
            actions.push(navigate(current.saturating_sub(1)));
        }
        if item(
            ui,
            "Next",
            Some(shortcuts::NEXT),
            false,
            current + 1 < total,
        ) {
            actions.push(navigate(current + 1));
        }
        if item(ui, "Last", None, false, current + 1 < total) {
            actions.push(navigate(total.saturating_sub(1)));
        }
    });
}
