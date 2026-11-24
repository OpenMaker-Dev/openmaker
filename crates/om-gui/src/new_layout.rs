//! Layouts > New Layout / Report... and Manage Layouts' New: FileMaker
//! Pro's New Layout/Report dialog for the one kind OpenMaker makes, a layout
//! for a computer in Form view (Claris: "New Layout/Report"; layout-model.md
//! 10.6). It names the table occurrence the layout shows records of and the
//! layout's name; the other devices and views are shown but not offered.
//! Finish asks the shared operation (`DataProvider::create_layout`) to add
//! the layout; the window then opens it in Layout mode, as FileMaker Pro
//! opens a new Form layout.
//!
//! The name starts as the occurrence's and follows the occurrence until the
//! user types one (OpenMaker's choice; what FileMaker Pro fills in is not
//! measured).

use egui::{Align, CornerRadius, Id, Layout, RichText, Stroke};
use om_data::layout_catalog::{LayoutRequest, MAX_NAME_CHARS};

use crate::theme;
use crate::widgets;

/// What the dialog's user did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NewLayoutAnswer {
    Open,
    Cancel,
    Finish(LayoutRequest),
}

/// The dialog's state, made when it opens.
pub(crate) struct NewLayout {
    /// The file whose window opened it: it closes when a window of another
    /// file comes to the front, so a layout goes only into its own file.
    pub(crate) file: u64,
    occurrence: Option<u64>,
    name: String,
    /// The user typed the name: it no longer follows the occurrence.
    named: bool,
    /// The layout whose theme and width the new one takes.
    like: Option<u32>,
}

/// Why the views other than Form are not offered.
const FORM_ONLY: &str = "OpenMaker makes Form layouts for a computer only.";

impl NewLayout {
    /// The dialog over a window of file `file` showing layout `like`,
    /// whose occurrence `occurrence` it starts on.
    pub(crate) fn new(file: u64, occurrence: Option<u64>, like: Option<u32>) -> Self {
        Self {
            file,
            occurrence,
            name: String::new(),
            named: false,
            like,
        }
    }

    /// Draw the modal over `ctx`, offering `occurrences` (id and name, in
    /// the file's order).
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        occurrences: &[(u64, String)],
    ) -> NewLayoutAnswer {
        if self
            .occurrence
            .is_none_or(|id| !occurrences.iter().any(|(o, _)| *o == id))
        {
            self.occurrence = occurrences.first().map(|(id, _)| *id);
        }
        let shown = |id: Option<u64>| {
            occurrences
                .iter()
                .find(|(o, _)| Some(*o) == id)
                .map(|(_, name)| name.clone())
                .unwrap_or_default()
        };
        if !self.named {
            self.name = shown(self.occurrence);
        }
        let mut answer = NewLayoutAnswer::Open;
        let frame = egui::Frame::new()
            .fill(theme::DIALOG_BG)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(10))
            .shadow(ctx.global_style().visuals.window_shadow)
            .inner_margin(egui::Margin::same(16));
        let response = egui::Modal::new(Id::unique("new_layout"))
            .frame(frame)
            .backdrop_color(theme::SCRIM)
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.label(
                    RichText::new("New Layout/Report")
                        .font(egui::FontId::new(15.0, theme::semibold())),
                );
                ui.add_space(12.0);
                egui::Grid::new("new_layout_fields")
                    .num_columns(2)
                    .spacing([8.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Show records from:");
                        let combo = egui::ComboBox::from_id_salt("new_layout_occurrence")
                            .width(280.0)
                            .selected_text(shown(self.occurrence))
                            .show_ui(ui, |ui| {
                                for (id, name) in occurrences {
                                    ui.selectable_value(&mut self.occurrence, Some(*id), name);
                                }
                            });
                        widgets::accessible_label(ui, combo.response.id, "Show records from");
                        ui.end_row();
                        ui.label("Layout Name:");
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.name)
                                .char_limit(MAX_NAME_CHARS)
                                .desired_width(280.0),
                        );
                        widgets::accessible_label(ui, edit.id, "Layout Name");
                        if edit.changed() {
                            self.named = true;
                        }
                        ui.end_row();
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    choice(ui, "Computer", true, true, "");
                    for device in ["Touch Device", "Printer"] {
                        choice(ui, device, false, false, FORM_ONLY);
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    choice(ui, "Form", true, true, "");
                    for view in ["List", "Table", "Report"] {
                        choice(ui, view, false, false, FORM_ONLY);
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Display records in Form View. In Browse mode and Find mode, you see \
                         one record, or form, at a time.",
                    )
                    .font(theme::small())
                    .color(theme::CAPTION),
                );
                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ready = self.occurrence.is_some() && !self.name.trim().is_empty();
                    if widgets::dialog_button(ui, "Finish", true, ready).clicked() {
                        answer = NewLayoutAnswer::Finish(LayoutRequest {
                            name: self.name.clone(),
                            occurrence: self.occurrence.expect("ready"),
                            like: self.like,
                        });
                    }
                    if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                        answer = NewLayoutAnswer::Cancel;
                    }
                });
            });
        if response.should_close() && answer == NewLayoutAnswer::Open {
            answer = NewLayoutAnswer::Cancel;
        }
        answer
    }
}

/// One of the dialog's device or view choices: `on` the chosen one;
/// `enabled` false for one OpenMaker does not make, with `why` on hover.
fn choice(ui: &mut egui::Ui, label: &str, on: bool, enabled: bool, why: &str) {
    let response = ui.add_enabled(enabled, egui::Button::selectable(on, label));
    if !enabled {
        response.on_disabled_hover_text(why);
    }
}
