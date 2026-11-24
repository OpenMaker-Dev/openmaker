//! What the native chrome shows of a document: its state and commands
//! (`om_gui::ChromeState`, `om_gui::ChromeCommand`) and FileMaker's menus
//! as `om_gui::ChromeMenu` records them.

use om_gui::{ChromeCommand, ChromeMenu, Mode, OpenMakerApp};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DocumentMode {
    Browse,
    Find,
    /// Preview and Layout mode, which only a script can leave the window
    /// in: the chrome shows no record or Find controls.
    Other,
}

/// The document as the chrome shows it.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct DocumentState {
    /// The file in front, by its id in the window: [`crate::Command::Close`]
    /// names it.
    pub file: u64,
    pub title: String,
    pub mode: DocumentMode,
    pub layout: String,
    /// Browse's record (from 0) of `count` found, or Find's request (from
    /// 0) of `count` requests.
    pub position: u64,
    pub count: u64,
    /// Find's current request omits the records it matches.
    pub omitting: bool,
    /// A script runs, or a record or page is on its way.
    pub busy: bool,
    /// The record has changes not yet saved, or their save is on its way.
    pub unsaved: bool,
    /// A field takes typing: the keyboard shows.
    pub keyboard: bool,
    /// Where the field's insertion point is, for the platform's input
    /// method (its marked text and candidates show there).
    pub caret: Option<Caret>,
    /// The file closed: the chrome returns to the Launch Center.
    pub closed: bool,
    /// Why the document stopped, when it stopped on an error of
    /// OpenMaker's rather than by closing its file.
    pub failure: Option<String>,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_go_to_record: bool,
    pub can_find: bool,
    pub can_perform_find: bool,
    pub can_cancel_find: bool,
    pub can_toggle_omit: bool,
    pub can_quick_find: bool,
    pub can_done: bool,
    pub can_close: bool,
}

impl DocumentState {
    /// A document that stopped on an error: closed, every control off.
    pub(crate) fn stopped(failure: String) -> Self {
        DocumentState {
            file: 0,
            title: String::new(),
            mode: DocumentMode::Other,
            layout: String::new(),
            position: 0,
            count: 0,
            omitting: false,
            busy: false,
            unsaved: false,
            keyboard: false,
            caret: None,
            closed: true,
            failure: Some(failure),
            can_previous: false,
            can_next: false,
            can_go_to_record: false,
            can_find: false,
            can_perform_find: false,
            can_cancel_find: false,
            can_toggle_omit: false,
            can_quick_find: false,
            can_done: false,
            can_close: false,
        }
    }

    pub(crate) fn of(app: &OpenMakerApp, caret: Option<Caret>) -> Self {
        let state = app.chrome_state();
        let can = |command: ChromeCommand| app.chrome_command_available(&command);
        DocumentState {
            file: app.front_file(),
            title: state.title,
            mode: match state.mode {
                Mode::Browse => DocumentMode::Browse,
                Mode::Find => DocumentMode::Find,
                Mode::Preview | Mode::Layout => DocumentMode::Other,
            },
            layout: state.layout,
            position: state.position,
            count: state.count,
            omitting: state.omitting,
            busy: state.busy,
            unsaved: state.unsaved,
            keyboard: caret.is_some(),
            caret,
            closed: !app.is_open(),
            failure: None,
            can_previous: can(ChromeCommand::Previous),
            can_next: can(ChromeCommand::Next),
            can_go_to_record: can(ChromeCommand::GoToRecord(1)),
            can_find: can(ChromeCommand::Find),
            can_perform_find: can(ChromeCommand::PerformFind),
            can_cancel_find: can(ChromeCommand::CancelFind),
            can_toggle_omit: can(ChromeCommand::ToggleOmit),
            // Any words: the layout and mode allow Quick Find.
            can_quick_find: can(ChromeCommand::QuickFind("*".into())),
            can_done: can(ChromeCommand::Done),
            can_close: can(ChromeCommand::Close),
        }
    }
}

/// The insertion point of the field that takes typing, in points from the
/// canvas's top left.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct Caret {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// The field takes a password: the keyboard's secure entry, with no
    /// suggestions or saved text.
    pub password: bool,
}

/// One of FileMaker's menus, for the chrome to show: the layout menu,
/// Records or Requests, View and Scripts.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Menu {
    pub title: String,
    /// The rows after a submenu row whose depth is greater than its own are
    /// that submenu's.
    pub rows: Vec<MenuRow>,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MenuRow {
    /// What [`crate::Input::Menu`] sends back when the row is chosen.
    pub id: String,
    pub depth: u32,
    pub kind: MenuRowKind,
    pub label: String,
    pub enabled: bool,
    pub checked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MenuRowKind {
    Command,
    Submenu,
    Separator,
}

pub(crate) fn menus(menus: &[ChromeMenu]) -> Vec<Menu> {
    menus
        .iter()
        .map(|menu| Menu {
            title: menu.title.clone(),
            rows: menu
                .rows
                .iter()
                .map(|row| MenuRow {
                    id: row.id.clone(),
                    depth: row.depth,
                    kind: match row.kind {
                        om_gui::MenuRowKind::Command => MenuRowKind::Command,
                        om_gui::MenuRowKind::Submenu => MenuRowKind::Submenu,
                        om_gui::MenuRowKind::Separator => MenuRowKind::Separator,
                    },
                    label: row.label.clone(),
                    enabled: row.enabled,
                    checked: row.checked,
                })
                .collect(),
        })
        .collect()
}
