//! The macOS menu bar ([#55](https://github.com/OpenMaker-Dev/openmaker/issues/55)).
//! FileMaker Pro's menus are in the system menu bar at the top of the
//! screen, the first named after the app (client-ui.md, "Window regions").
//! When the app runs in a window of its own on macOS, the menus the front
//! window's menu bar would draw are built into the system menu bar instead,
//! through muda, and the windows draw none.
//!
//! The menus are built each frame through [`MenuRows`] (`menubar.rs`), as
//! the window's bar builds them: a [`Recorder`] keeps the rows. When they
//! differ from what the bar shows only in which rows are enabled or
//! checked, the bar's items change in place; otherwise the bar is built
//! again. A row the user chooses comes back as a muda event with the row's
//! id; the next frame's recording answers `true` for that row, so its
//! action runs as a click in the window's menu would.
//!
//! The app menu also gets macOS's own Services, Hide, Hide Others and Show
//! All, and the Help menu the system's search field.
//! Enabled rows can install key equivalents. Editor and zoom shortcuts
//! stay with the viewport so they follow its focus and event order.

use std::collections::HashMap;

use egui::{Event, Key, KeyboardShortcut, Modifiers};
use muda::accelerator::{Key as MudaKey, KeyAccelerator, Modifiers as MudaModifiers, NamedKey};
use muda::{CheckMenuItem, Menu, MenuEvent, PredefinedMenuItem, Submenu};

use crate::app::{EditCommand, FieldMenuTarget};
use crate::menubar::MenuRows;

/// A row of a menu as recorded.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    Item {
        id: String,
        label: String,
        shortcut: Option<KeyboardShortcut>,
        checked: bool,
        enabled: bool,
    },
    Separator,
    Submenu {
        id: String,
        label: String,
        enabled: bool,
        rows: Vec<Row>,
    },
}

impl Row {
    /// The row without its enabled and checked states: two menus of the same
    /// shape differ only in what changes in place.
    fn shape(&self) -> Row {
        match self {
            Row::Item {
                id,
                label,
                shortcut,
                ..
            } => Row::Item {
                id: id.clone(),
                label: label.clone(),
                shortcut: *shortcut,
                checked: false,
                enabled: false,
            },
            Row::Separator => Row::Separator,
            Row::Submenu {
                id, label, rows, ..
            } => Row::Submenu {
                id: id.clone(),
                label: label.clone(),
                enabled: false,
                rows: rows.iter().map(Row::shape).collect(),
            },
        }
    }
}

/// The rows of one menu bar's menus, in order: title and rows.
type Menus = Vec<(String, Vec<Row>)>;

fn shape(menus: &Menus) -> Vec<(String, Vec<Row>)> {
    menus
        .iter()
        .map(|(title, rows)| (title.clone(), rows.iter().map(Row::shape).collect()))
        .collect()
}

/// Keeps the rows a menu adds, for the system menu bar. `chosen` is the id
/// of the row the user chose since the last frame, answered once.
pub(crate) struct Recorder<'a> {
    path: String,
    rows: Vec<Row>,
    /// A dialog is over the window: no row runs.
    blocked: bool,
    /// The window's menus may ask for what they read when shown: no read
    /// failed since the window came to the front.
    may_ask: bool,
    /// How many rows of each label this menu has had so far: ids stay
    /// distinct when two rows read alike (two scripts of one name).
    seen: HashMap<String, usize>,
    chosen: &'a mut Option<String>,
}

impl<'a> Recorder<'a> {
    fn new(path: String, chosen: &'a mut Option<String>, may_ask: bool, blocked: bool) -> Self {
        Recorder {
            path,
            rows: Vec::new(),
            blocked,
            may_ask,
            seen: HashMap::new(),
            chosen,
        }
    }

    fn id(&mut self, label: &str) -> String {
        let count = self.seen.entry(label.to_owned()).or_default();
        let id = format!("{}\u{1f}{label}\u{1f}{count}", self.path);
        *count += 1;
        id
    }
}

impl Recorder<'_> {
    fn row(
        &mut self,
        id: String,
        label: &str,
        shortcut: Option<KeyboardShortcut>,
        checked: bool,
        enabled: bool,
    ) -> bool {
        let enabled = enabled && !self.blocked;
        let chosen = enabled && self.chosen.as_deref() == Some(id.as_str());
        if chosen {
            *self.chosen = None;
        }
        let shortcut = shortcut.filter(|shortcut| !editor_equivalent(&self.path, *shortcut));
        self.rows.push(Row::Item {
            id,
            label: label.to_owned(),
            shortcut,
            checked,
            enabled,
        });
        chosen
    }
}

impl MenuRows for Recorder<'_> {
    fn keyed_item(
        &mut self,
        key: &str,
        label: &str,
        shortcut: Option<KeyboardShortcut>,
        checked: bool,
        enabled: bool,
    ) -> bool {
        // What the row runs, not where it is: a list that changed before
        // the choice was read cannot send it elsewhere.
        let id = format!("{}\u{1f}#{key}", self.path);
        self.row(id, label, shortcut, checked, enabled)
    }

    fn item(
        &mut self,
        label: &str,
        shortcut: Option<KeyboardShortcut>,
        checked: bool,
        enabled: bool,
    ) -> bool {
        let id = self.id(label);
        self.row(id, label, shortcut, checked, enabled)
    }

    fn separator(&mut self) {
        self.rows.push(Row::Separator);
    }

    fn submenu(&mut self, label: &str, enabled: bool, rows: &mut dyn FnMut(&mut dyn MenuRows)) {
        let id = self.id(label);
        let enabled = enabled && !self.blocked;
        let mut inner = Recorder::new(id.clone(), &mut *self.chosen, self.may_ask, !enabled);
        rows(&mut inner);
        let rows = inner.rows;
        self.rows.push(Row::Submenu {
            id,
            label: label.to_owned(),
            enabled,
            rows,
        });
    }

    fn note(&mut self, text: &str) {
        let id = self.id(text);
        self.rows.push(Row::Item {
            id,
            label: text.to_owned(),
            shortcut: None,
            checked: false,
            enabled: false,
        });
    }

    fn shown(&self) -> bool {
        // The system menu says nothing before it opens.
        true
    }

    fn ask(&mut self, _: &str) -> bool {
        // As long as no read failed since the window came to the front: one
        // that failed is asked again when it next does, not every frame.
        self.may_ask && !self.blocked
    }
}

/// An item of the bar that changes in place.
enum Handle {
    Item(CheckMenuItem),
    Submenu(Submenu),
}

/// The system menu bar while the app shows it.
pub(crate) struct SystemMenuBar {
    menu: Option<Menu>,
    shown: Menus,
    handles: HashMap<String, Handle>,
    /// The window (session epoch and window id) whose menus the bar shows.
    owner: Option<Owner>,
    field_target: Option<FieldMenuTarget>,
    /// The rows the user chose, in order, each with the window whose menus
    /// showed it, until a recording answers or drops it.
    chosen: std::collections::VecDeque<Choice>,
    /// A choice came: the bar's check marks may differ from the rows (muda
    /// flips a check item's mark itself when it is chosen).
    reconcile: bool,
    /// Windows whose menus' reads failed since they came to the front.
    failed: std::collections::HashSet<Owner>,
    events: std::sync::mpsc::Receiver<String>,
}

/// A window, as the system bar tells windows apart.
pub(crate) type Owner = (u64, u32);

struct Choice {
    owner: Owner,
    id: String,
    target: FieldMenuTarget,
}

/// The most choices that wait for the app at once.
const CHOICES: usize = 16;

impl SystemMenuBar {
    /// The system menu bar, when the app's root window is an AppKit window
    /// (not a test harness's).
    pub(crate) fn start(
        ctx: &egui::Context,
        frame: &impl raw_window_handle::HasWindowHandle,
    ) -> Option<Self> {
        use raw_window_handle::RawWindowHandle;
        let Ok(RawWindowHandle::AppKit(_)) = frame.window_handle().map(|h| h.as_raw()) else {
            return None;
        };
        // A choice wakes the app, which reads it on its next frame.
        let (sender, events) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if sender.send(event.id.0).is_ok() {
                ctx.request_repaint();
            }
        }));
        Some(SystemMenuBar {
            menu: None,
            shown: Vec::new(),
            handles: HashMap::new(),
            owner: None,
            field_target: None,
            chosen: std::collections::VecDeque::new(),
            reconcile: false,
            failed: std::collections::HashSet::new(),
            events,
        })
    }

    /// The start of a frame: take the row the user chose.
    pub(crate) fn begin(&mut self) {
        while let Ok(id) = self.events.try_recv() {
            self.reconcile = true;
            if self.chosen.len() >= CHOICES {
                continue;
            }
            if let Some((owner, target)) = self.owner.zip(self.field_target.as_ref()) {
                self.chosen.push_back(Choice {
                    owner,
                    id,
                    target: target.clone(),
                });
            }
        }
    }

    /// Keep only the choices whose window is still open (`live`).
    pub(crate) fn retain_choices(&mut self, live: impl Fn(Owner) -> bool) {
        self.chosen.retain(|choice| live(choice.owner));
    }

    /// Drop the choices made in `owner`'s menus: it cannot take them (a card
    /// covers it).
    pub(crate) fn drop_choices_for(&mut self, owner: Owner) {
        self.chosen.retain(|choice| choice.owner != owner);
    }

    /// Whether a choice made in `owner`'s menus waits for it.
    pub(crate) fn has_choice_for(&self, owner: Owner) -> bool {
        self.chosen.iter().any(|choice| choice.owner == owner)
    }

    /// Whether choices wait for any window.
    pub(crate) fn has_choices(&self) -> bool {
        !self.chosen.is_empty()
    }

    /// `owner`'s menus read something that failed (the Scripts catalog):
    /// they ask again only when the window next comes to the front.
    pub(crate) fn read_failed(&mut self, owner: Owner) {
        self.failed.insert(owner);
    }

    /// `owner` is not in front: what failed may be asked again when it is.
    pub(crate) fn behind(&mut self, owner: Owner) {
        self.failed.remove(&owner);
    }

    /// Build `owner`'s menus: `menus` adds each menu with its title,
    /// `blocked` when a dialog is over the window. When the window is in
    /// front (`shown`) the bar shows them; otherwise they only answer a
    /// choice made while they were shown. The oldest choice made in this
    /// window's menus is answered, with the rows' state now; one that no
    /// enabled row answers (the row went) is dropped.
    pub(crate) fn record(
        &mut self,
        owner: Owner,
        shown: bool,
        blocked: bool,
        target: &FieldMenuTarget,
        menus: impl FnOnce(&mut crate::menubar::MenuSink<'_>),
    ) -> Option<FieldMenuTarget> {
        let mine = self.chosen.iter().position(|choice| choice.owner == owner);
        let (mut chosen, chosen_target) = match mine.and_then(|at| self.chosen.remove(at)) {
            Some(choice) => (Some(choice.id), Some(choice.target)),
            None => (None, None),
        };
        let may_ask = shown && !self.failed.contains(&owner);
        let mut recorded: Menus = Vec::new();
        menus(&mut |title, rows| {
            let mut recorder = Recorder::new(title.to_owned(), &mut chosen, may_ask, blocked);
            rows(&mut recorder);
            recorded.push((title.to_owned(), recorder.rows));
        });
        if !shown {
            return chosen_target;
        }
        self.owner = Some(owner);
        self.field_target = Some(target.clone());
        let reconcile = std::mem::take(&mut self.reconcile);
        if recorded == self.shown && !reconcile {
            return chosen_target;
        }
        if self.menu.is_some() && shape(&recorded) == shape(&self.shown) {
            self.update(&recorded);
        } else {
            self.build(&recorded);
        }
        self.shown = recorded;
        chosen_target
    }

    fn update(&self, menus: &Menus) {
        fn walk(handles: &HashMap<String, Handle>, rows: &[Row]) {
            for row in rows {
                match row {
                    Row::Item {
                        id,
                        checked,
                        enabled,
                        shortcut,
                        ..
                    } => {
                        if let Some(Handle::Item(item)) = handles.get(id) {
                            let _ = item.set_key_accelerator(key_equivalent(*enabled, *shortcut));
                            item.set_enabled(*enabled);
                            item.set_checked(*checked);
                        }
                    }
                    Row::Submenu {
                        id, enabled, rows, ..
                    } => {
                        if let Some(Handle::Submenu(submenu)) = handles.get(id) {
                            submenu.set_enabled(*enabled);
                        }
                        walk(handles, rows);
                    }
                    Row::Separator => {}
                }
            }
        }
        for (_, rows) in menus {
            walk(&self.handles, rows);
        }
    }

    fn build(&mut self, menus: &Menus) {
        let menu = Menu::new();
        let mut handles = HashMap::new();
        let last = menus.len().saturating_sub(1);
        for (index, (title, rows)) in menus.iter().enumerate() {
            let submenu = Submenu::with_id(title.clone(), title, true);
            if index == 0 {
                app_menu(&submenu, rows, &mut handles);
            } else {
                append(&submenu, rows, &mut handles);
            }
            let _ = menu.append(&submenu);
            if index == last && title == "Help" {
                submenu.set_as_help_menu_for_nsapp();
            }
        }
        menu.init_for_nsapp();
        self.menu = Some(menu);
        self.handles = handles;
    }
}

/// The app menu's rows, with macOS's own Services, Hide, Hide Others and
/// Show All before its last row (Quit).
fn app_menu(submenu: &Submenu, rows: &[Row], handles: &mut HashMap<String, Handle>) {
    let (quit, rest) = match rows.split_last() {
        Some((quit @ Row::Item { .. }, rest)) => (Some(quit), rest),
        _ => (None, rows),
    };
    append(submenu, rest, handles);
    let system = [
        PredefinedMenuItem::services(None),
        PredefinedMenuItem::separator(),
        PredefinedMenuItem::hide(None),
        PredefinedMenuItem::hide_others(None),
        PredefinedMenuItem::show_all(None),
        PredefinedMenuItem::separator(),
    ];
    for item in &system {
        let _ = submenu.append(item);
    }
    if let Some(quit) = quit {
        append(submenu, std::slice::from_ref(quit), handles);
    }
}

fn append(submenu: &Submenu, rows: &[Row], handles: &mut HashMap<String, Handle>) {
    for row in rows {
        match row {
            Row::Item {
                id,
                label,
                shortcut,
                checked,
                enabled,
            } => {
                let item = CheckMenuItem::with_id(id.clone(), label, *enabled, *checked, None);
                if let Some(accelerator) = key_equivalent(*enabled, *shortcut) {
                    let _ = item.set_key_accelerator(Some(accelerator));
                }
                let _ = submenu.append(&item);
                handles.insert(id.clone(), Handle::Item(item));
            }
            Row::Separator => {
                let _ = submenu.append(&PredefinedMenuItem::separator());
            }
            Row::Submenu {
                id,
                label,
                enabled,
                rows,
            } => {
                let inner = Submenu::with_id(id.clone(), label, *enabled);
                append(&inner, rows, handles);
                let _ = submenu.append(&inner);
                handles.insert(id.clone(), Handle::Submenu(inner));
            }
        }
    }
}

/// These keys go through the viewport even when the menu row is enabled.
fn editor_equivalent(path: &str, shortcut: KeyboardShortcut) -> bool {
    matches!(
        (path.split('\u{1f}').next(), shortcut.logical_key),
        (Some("Edit" | "Insert"), _) | (Some("View"), Key::Plus | Key::Minus)
    )
}

fn key_equivalent(enabled: bool, shortcut: Option<KeyboardShortcut>) -> Option<KeyAccelerator> {
    // AppKit can consume a key equivalent even on a disabled menu item.
    shortcut.filter(|_| enabled).and_then(accelerator)
}

fn clipboard_paste() -> Option<Event> {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;

    let plain_text = NSString::from_str("public.utf8-plain-text");
    let text = NSPasteboard::generalPasteboard().stringForType(&plain_text)?;
    let text = text.to_string().replace("\r\n", "\n");
    (!text.is_empty()).then_some(Event::Paste(text))
}

/// Paste Text Only also enters the viewport's input stream in key order.
pub(crate) fn paste_text_only(input: &mut egui::RawInput) {
    for event in &mut input.events {
        if matches!(event, Event::Key { key: Key::V, pressed: true, modifiers, .. }
            if modifiers.matches_exact(Modifiers::MAC_CMD | Modifiers::SHIFT | Modifiers::ALT))
        {
            if let Some(paste) = clipboard_paste() {
                *event = paste;
            }
        }
    }
}

fn field_edit_id(ctx: &egui::Context) -> egui::Id {
    egui::Id::unique("native_field_edit").with(ctx.viewport_id())
}

pub(crate) fn edit_field(ctx: &egui::Context, editor: egui::Id, command: EditCommand) {
    let key = |key, modifiers| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    // Clear deletes a selection only. A bare Delete would also remove the
    // character at the insertion point.
    let has_selection = || {
        egui::TextEdit::load_state(ctx, editor)
            .and_then(|state| state.cursor.char_range())
            .is_some_and(|range| !range.is_empty())
    };
    let event = match command {
        EditCommand::Cut => Event::Cut,
        EditCommand::Copy => Event::Copy,
        EditCommand::Paste => {
            let Some(paste) = clipboard_paste() else {
                return;
            };
            paste
        }
        EditCommand::Undo => key(Key::Z, Modifiers::COMMAND),
        EditCommand::Redo => key(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT),
        EditCommand::Clear if has_selection() => key(Key::Delete, Modifiers::NONE),
        EditCommand::Clear => return,
        EditCommand::SelectAll => key(Key::A, Modifiers::COMMAND),
    };
    let slot = field_edit_id(ctx);
    ctx.data_mut(|data| data.insert_temp(slot, (editor, event)));
}

/// Only the original, still-focused editor gets the event. It processes
/// the command through TextEdit and reports modifications through the same
/// draft and trigger path as typing.
pub(crate) fn with_field_edit<R>(
    ui: &mut egui::Ui,
    editor: egui::Id,
    draw: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let slot = field_edit_id(ui.ctx());
    let event = ui.ctx().data_mut(|data| {
        let (target, event) = data.get_temp::<(egui::Id, Event)>(slot)?;
        if target != editor {
            return None;
        }
        data.remove::<(egui::Id, Event)>(slot);
        Some(event)
    });
    let Some(event) = event.filter(|_| ui.memory(|memory| memory.has_focus(editor))) else {
        return draw(ui);
    };
    ui.input_mut(|input| input.events.insert(0, event.clone()));
    let response = draw(ui);
    ui.input_mut(|input| {
        if input.events.first() == Some(&event) {
            input.events.remove(0);
        }
    });
    response
}

pub(crate) fn clear_field_edit(ctx: &egui::Context) {
    let slot = field_edit_id(ctx);
    ctx.data_mut(|data| data.remove::<(egui::Id, Event)>(slot));
}

/// A shortcut as a key equivalent: the key the shortcut names, with its
/// modifiers (⌘ for the command key). `None` for a key a menu item cannot
/// show.
fn accelerator(shortcut: KeyboardShortcut) -> Option<KeyAccelerator> {
    let mut mods = MudaModifiers::empty();
    let m = shortcut.modifiers;
    if m.command || m.mac_cmd {
        mods |= MudaModifiers::META;
    }
    if m.ctrl && !m.command {
        mods |= MudaModifiers::CONTROL;
    }
    if m.alt {
        mods |= MudaModifiers::ALT;
    }
    if m.shift {
        mods |= MudaModifiers::SHIFT;
    }
    let key = match shortcut.logical_key {
        Key::ArrowUp => MudaKey::Named(NamedKey::ArrowUp),
        Key::ArrowDown => MudaKey::Named(NamedKey::ArrowDown),
        Key::ArrowLeft => MudaKey::Named(NamedKey::ArrowLeft),
        Key::ArrowRight => MudaKey::Named(NamedKey::ArrowRight),
        Key::Enter => MudaKey::Named(NamedKey::Enter),
        Key::Tab => MudaKey::Named(NamedKey::Tab),
        Key::Escape => MudaKey::Named(NamedKey::Escape),
        Key::Backspace => MudaKey::Named(NamedKey::Backspace),
        Key::Delete => MudaKey::Named(NamedKey::Delete),
        key => {
            let symbol = match key {
                Key::Plus => "+",
                Key::Minus => "-",
                Key::Equals => "=",
                Key::Semicolon => ";",
                Key::Quote => "'",
                Key::Comma => ",",
                Key::Period => ".",
                Key::Slash => "/",
                _ => key.name(),
            };
            let mut chars = symbol.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            MudaKey::Character(c.to_ascii_lowercase().to_string())
        }
    };
    Some(KeyAccelerator::new(mods, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(chosen: Option<&str>, f: impl FnOnce(&mut dyn MenuRows) -> bool) -> (Vec<Row>, bool) {
        let mut chosen = chosen.map(str::to_owned);
        let mut recorder = Recorder::new("File".into(), &mut chosen, true, false);
        let answered = f(&mut recorder);
        (recorder.rows, answered)
    }

    #[test]
    fn a_chosen_row_answers_once_and_rows_that_read_alike_keep_ids_of_their_own() {
        let (rows, _) = record(None, |rows| {
            rows.item("Run", None, false, true);
            rows.item("Run", None, false, true);
            false
        });
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Item { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        let second = ids[1].to_owned();
        let (_, answered) = record(Some(&second), |rows| {
            let first = rows.item("Run", None, false, true);
            let second = rows.item("Run", None, false, true);
            assert!(!first);
            second
        });
        assert!(answered);
        // A keyed row's id is what it runs, wherever it is in the list.
        let (rows, _) = record(None, |rows| {
            rows.keyed_item("script 7", "Run", None, false, true);
            false
        });
        assert!(matches!(&rows[0], Row::Item { id, .. } if id.ends_with("#script 7")));
        // A disabled row does not run, whatever the event says.
        let (_, answered) = record(Some(&second), |rows| {
            rows.item("Run", None, false, true);
            rows.item("Run", None, false, false)
        });
        assert!(!answered);
    }

    #[test]
    fn shortcuts_become_key_equivalents() {
        let shortcut = |m, k| KeyboardShortcut::new(m, k);
        let cmd = egui::Modifiers::COMMAND;
        assert_eq!(
            accelerator(shortcut(cmd, Key::N)),
            Some(KeyAccelerator::new(
                MudaModifiers::META,
                MudaKey::Character("n".into())
            ))
        );
        assert_eq!(
            accelerator(shortcut(cmd, Key::Plus)),
            Some(KeyAccelerator::new(
                MudaModifiers::META,
                MudaKey::Character("+".into())
            ))
        );
        assert_eq!(
            accelerator(shortcut(egui::Modifiers::CTRL, Key::ArrowUp)),
            Some(KeyAccelerator::new(
                MudaModifiers::CONTROL,
                MudaKey::Named(NamedKey::ArrowUp)
            ))
        );
    }
}
