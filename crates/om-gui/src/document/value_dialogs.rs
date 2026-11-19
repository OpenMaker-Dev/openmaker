//! The dialog a pop-up menu's “Other…” opens (#869), as FileMaker Pro
//! 26.0.3 shows it on the Contacts starter's Title: titled “Other…”, a
//! multi-line text box and no message, Cancel on the left and OK, the
//! default, on the right. The box is empty, or holds the field's value
//! when no item of the list stores it, selected. OK enters the text
//! (`om_data::choice::other_value`); Cancel and Escape change nothing.
//!
//! “Edit…” opens Edit Value List as Pro 26.0.3 shows it on Content
//! Management's Type (#869): titled with the list's name in curly quotes,
//! one multi-line editor with an item per line and `-` for a divider, no
//! message, Cancel on the left and OK, the default, on the right. OK saves
//! the list's values (`DataProvider::set_value_list_values`); Cancel
//! leaves the list as it was.
//!
//! This module only presents the dialogs and reports the answers; the
//! document and the window apply them.

use egui::{Align, Id, Key, Layout, RichText};
use om_layout::LayoutObject;

use super::RelatedField;
use crate::widgets;

/// Where the entered value goes.
#[derive(Clone, Debug)]
pub(super) enum OtherTarget {
    /// The record's own field: object `object` of `layout`, on `record`.
    Own {
        layout: u64,
        record: i64,
        object: u64,
    },
    /// A related record's field (a portal row's, or a related field), with
    /// what its edit starts from.
    Related {
        field: RelatedField,
        occurrence: u64,
        version: Option<u64>,
        stored: String,
    },
}

/// An open Other… dialog.
#[derive(Clone, Debug)]
pub(super) struct OtherDialog {
    pub target: OtherTarget,
    /// The pop-up menu, whose flags offer Other….
    pub object: LayoutObject,
    pub text: String,
    /// The text box has had the focus, with its text selected.
    focused: bool,
}

impl OtherDialog {
    pub fn new(target: OtherTarget, object: LayoutObject, text: String) -> Self {
        OtherDialog {
            target,
            object,
            text,
            focused: false,
        }
    }
}

/// The user's answer to the dialog.
pub(super) enum Answer {
    Ok(String),
    Cancel,
}

/// Show `dialog`; its answer, once given.
pub(super) fn other(ctx: &egui::Context, dialog: &mut OtherDialog) -> Option<Answer> {
    let mut answer = None;
    egui::Modal::new(Id::unique("value_list_other")).show(ctx, |ui| {
        ui.set_width(300.0);
        let title = ui.label(RichText::new("Other…").strong());
        ui.add_space(6.0);
        let id = Id::unique("value_list_other_text");
        let edit = egui::TextEdit::multiline(&mut dialog.text)
            .id(id)
            .desired_rows(4)
            .desired_width(f32::INFINITY);
        let response = ui.add(edit).labelled_by(title.id);
        if !dialog.focused {
            dialog.focused = true;
            response.request_focus();
            let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(dialog.text.chars().count()),
                )));
            state.store(ui.ctx(), id);
        }
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::dialog_button(ui, "OK", true, true).clicked() {
                answer = Some(Answer::Ok(dialog.text.clone()));
            }
            if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                answer = Some(Answer::Cancel);
            }
        });
        if ui.input(|input| input.key_pressed(Key::Escape)) {
            answer = Some(Answer::Cancel);
        }
    });
    answer
}

/// An open Edit Value List dialog: custom list `list` as it was read when
/// the dialog opened (`DataProvider::custom_value_list`).
pub(crate) struct EditDialog {
    pub list: u64,
    pub name: String,
    pub revision: [u8; 32],
    pub text: String,
    /// OK was pressed and the change is on its way.
    pub pending: bool,
    focused: bool,
}

impl EditDialog {
    pub fn new(list: u64, custom: om_data::value_lists::CustomList) -> Self {
        EditDialog {
            list,
            name: custom.name,
            revision: custom.revision,
            text: custom.values.join("\n"),
            pending: false,
            focused: false,
        }
    }

    /// The editor's lines as the list's values: one item per line, a
    /// Windows line end taken as one, and the empty line a final return
    /// leaves dropped (inferred: what Pro stores for a final return is
    /// unmeasured).
    pub fn values(&self) -> Vec<String> {
        let text = self.text.replace("\r\n", "\n");
        let text = text.strip_suffix('\n').unwrap_or(&text);
        if text.is_empty() {
            return Vec::new();
        }
        text.split('\n').map(str::to_owned).collect()
    }
}

/// The user's answer to Edit Value List.
pub(crate) enum EditAnswer {
    Ok(Vec<String>),
    Cancel,
}

/// Show `dialog`; its answer, once given.
pub(crate) fn edit(ctx: &egui::Context, dialog: &mut EditDialog) -> Option<EditAnswer> {
    let mut answer = None;
    egui::Modal::new(Id::unique("value_list_edit")).show(ctx, |ui| {
        ui.set_width(360.0);
        let title = ui.label(RichText::new(format!("Edit Value List “{}”", dialog.name)).strong());
        ui.add_space(6.0);
        let id = Id::unique("value_list_edit_text");
        let response = egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                ui.add_enabled(
                    !dialog.pending,
                    egui::TextEdit::multiline(&mut dialog.text)
                        .id(id)
                        .desired_rows(12)
                        .desired_width(f32::INFINITY),
                )
                .labelled_by(title.id)
            })
            .inner;
        if !dialog.focused {
            dialog.focused = true;
            response.request_focus();
        }
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::dialog_button(ui, "OK", true, !dialog.pending).clicked() {
                answer = Some(EditAnswer::Ok(dialog.values()));
            }
            if widgets::dialog_button(ui, "Cancel", false, !dialog.pending).clicked() {
                answer = Some(EditAnswer::Cancel);
            }
        });
        if !dialog.pending && ui.input(|input| input.key_pressed(Key::Escape)) {
            answer = Some(EditAnswer::Cancel);
        }
    });
    answer
}
