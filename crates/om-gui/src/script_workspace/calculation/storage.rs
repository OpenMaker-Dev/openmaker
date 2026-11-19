//! Storage Options for a calculation field, opened from Specify
//! Calculation's "Storage Options…" button: global storage, "Do not store
//! calculation results -- recalculate when needed", indexing and automatic
//! indexes. The sections and wording follow the Storage tab of Options for
//! Field (`database_options`); this dialog's own layout was not compared
//! with a FileMaker Pro 26 capture.

use egui::{Align, CornerRadius, Id, Layout, RichText, Stroke};
use om_data::calc::ValueType;
use om_data::calc_field::CalcStorage;
use om_data::field_options::Indexing;

use crate::{theme, widgets};

pub(super) enum Answer {
    Cancel,
    Ok(CalcStorage),
}

pub(super) struct StorageOptions {
    field: String,
    storage: CalcStorage,
    /// The calculation's result type as Specify Calculation has it.
    result: ValueType,
}

impl StorageOptions {
    pub(super) fn new(field: &str, storage: CalcStorage, result: ValueType) -> Self {
        Self {
            field: field.to_owned(),
            storage,
            result,
        }
    }

    pub(super) fn show(&mut self, ctx: &egui::Context) -> Option<Answer> {
        let mut answer = None;
        let modal = egui::Modal::new(Id::unique("calculation_storage_options"))
            .frame(
                egui::Frame::new()
                    .fill(theme::DIALOG_BG)
                    .stroke(Stroke::new(1.0, theme::DIVIDER))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(18)
                    .shadow(ctx.global_style().visuals.window_shadow),
            )
            .show(ctx, |ui| {
                let width = 460.0;
                ui.set_width(width);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(format!("Storage Options for Field “{}”", self.field))
                            .font(egui::FontId::new(13.0, theme::semibold())),
                    );
                });
                ui.add_space(8.0);
                self.sections(ui);
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::dialog_button(ui, "OK", true, true).clicked() {
                        answer = Some(Answer::Ok(self.storage.clone()));
                    }
                    if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
            });
        if answer.is_none() && modal.is_top_modal && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            answer = Some(Answer::Cancel);
        }
        answer
    }

    fn sections(&mut self, ui: &mut egui::Ui) {
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).size(13.5));
                let rest = ui.available_rect_before_wrap();
                ui.painter().hline(
                    (rest.left() + 8.0)..=rest.right(),
                    rest.center().y,
                    Stroke::new(1.0, theme::DIVIDER),
                );
            });
        };
        let note = |ui: &mut egui::Ui, text: &str| {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(text).size(11.5)).wrap());
                });
            });
        };
        heading(ui, "Global Storage");
        note(
            ui,
            "A field that uses global storage contains only one value that is shared across \
             all records.  A global field can also be used as a temporary storage location \
             (for example, in scripts).",
        );
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            if ui
                .checkbox(
                    &mut self.storage.global,
                    "Use global storage (one value for all records)",
                )
                .changed()
                && self.storage.global
            {
                self.storage.unstored = false;
                self.storage.indexing = Indexing::None;
            }
        });
        heading(ui, "Indexing");
        note(
            ui,
            "Indexing improves performance for some operations like finds and supports \
             functionality like relational joins and field value uniqueness at the cost of \
             increased file size and time spent indexing.",
        );
        let global = self.storage.global;
        ui.add_enabled_ui(!global, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                if ui
                    .checkbox(
                        &mut self.storage.unstored,
                        "Do not store calculation results -- recalculate when needed",
                    )
                    .changed()
                    && self.storage.unstored
                {
                    self.storage.indexing = Indexing::None;
                }
            });
        });
        let text = self.result == ValueType::Text;
        let indexable = !global && !self.storage.unstored && self.result != ValueType::Container;
        ui.add_enabled_ui(indexable, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label("Indexing:");
                ui.add_space(8.0);
                ui.radio_value(&mut self.storage.indexing, Indexing::None, "None");
                ui.add_space(50.0);
                ui.add_enabled_ui(text, |ui| {
                    ui.radio_value(&mut self.storage.indexing, Indexing::Minimal, "Minimal");
                });
                ui.add_space(50.0);
                ui.radio_value(&mut self.storage.indexing, Indexing::All, "All");
            });
            ui.horizontal(|ui| {
                ui.add_space(80.0);
                ui.checkbox(
                    &mut self.storage.automatic_indexes,
                    "Automatically create indexes as needed",
                );
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label("Default language:");
            let language = match self.storage.language {
                21 | 77 => "English".to_owned(),
                other => format!("Language {other}"),
            };
            ui.add_enabled_ui(false, |ui| {
                egui::ComboBox::from_id_salt("Calculation Default Language")
                    .width(200.0)
                    .selected_text(language)
                    .show_ui(ui, |_| {});
            })
            .response
            .on_hover_text(
                "OpenMaker does not change the index language yet: it does not rebuild \
                 the index in the new language.",
            );
        });
        note(
            ui,
            "Applies to text indexing, sorting, and default display font.",
        );
        // A result that cannot be indexed loses the choice it had.
        if !indexable || (!text && self.storage.indexing == Indexing::Minimal) {
            if !indexable {
                self.storage.indexing = Indexing::None;
            } else {
                self.storage.indexing = Indexing::All;
            }
        }
    }
}
