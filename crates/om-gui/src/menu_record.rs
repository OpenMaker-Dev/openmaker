//! Menus recorded as data rather than drawn: the rows that
//! `menubar.rs` adds through [`MenuRows`], kept with stable ids so that a
//! row chosen outside egui (the macOS system menu bar, a phone or tablet's
//! native menus) runs on the next frame as a click in the window's own
//! menu would, with the rows' enabled state at that moment.

use std::collections::HashMap;

use egui::{Key, KeyboardShortcut};

use crate::menubar::MenuRows;

/// A row of a menu as recorded.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
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
    #[cfg(target_os = "macos")]
    pub(crate) fn shape(&self) -> Row {
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
#[cfg(target_os = "macos")]
pub(crate) type Menus = Vec<(String, Vec<Row>)>;

#[cfg(target_os = "macos")]
pub(crate) fn shape(menus: &Menus) -> Vec<(String, Vec<Row>)> {
    menus
        .iter()
        .map(|(title, rows)| (title.clone(), rows.iter().map(Row::shape).collect()))
        .collect()
}

/// Keeps the rows a menu adds, for the system menu bar. `chosen` is the id
/// of the row the user chose since the last frame, answered once.
pub(crate) struct Recorder<'a> {
    path: String,
    pub(crate) rows: Vec<Row>,
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
    pub(crate) fn new(
        path: String,
        chosen: &'a mut Option<String>,
        may_ask: bool,
        blocked: bool,
    ) -> Self {
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
        // AppKit can consume an equivalent on a disabled item. Keep the
        // effective shortcut in the row's shape so a change rebuilds the
        // item: muda's setter leaves the old AppKit equivalent on None.
        let shortcut =
            shortcut.filter(|shortcut| enabled && !viewport_shortcut(&self.path, *shortcut));
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

    fn scrolled(&mut self, rows: &mut dyn FnMut(&mut dyn MenuRows)) {
        // The system's menus and OpenMaker Go's scroll on their own.
        rows(self);
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

/// These keys go through the viewport even when the menu row is enabled.
/// Keep Save Layout in the same input stream as Undo and Redo so AppKit
/// cannot reorder a file write around a draft edit.
fn viewport_shortcut(path: &str, shortcut: KeyboardShortcut) -> bool {
    matches!(
        (path.split('\u{1f}').next(), shortcut.logical_key),
        (Some("Edit" | "Insert"), _)
            | (Some("View"), Key::Plus | Key::Minus)
            | (Some("Layouts"), Key::S)
    )
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
    fn save_layout_has_no_appkit_key_equivalent() {
        let mut chosen = None;
        let mut recorder = Recorder::new("Layouts".into(), &mut chosen, true, false);
        recorder.item(
            "Save Layout",
            Some(KeyboardShortcut::new(egui::Modifiers::COMMAND, Key::S)),
            false,
            true,
        );
        assert!(matches!(
            recorder.rows[0],
            Row::Item {
                enabled: true,
                shortcut: None,
                ..
            }
        ));
    }
}
