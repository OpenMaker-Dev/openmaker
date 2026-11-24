//! Options for Field's "Lookup for Field" dialog, opened by "Looked-up
//! value" or its Specify… button: the occurrence the lookup starts with
//! (one of the field's table), the related occurrence it looks up from,
//! the field it copies, what to do without an exact match, and "Don't copy
//! contents if empty". OK hands the lookup back to Options for Field,
//! whose OK stages it; the draft refuses what OpenMaker cannot perform
//! (`om_data::schema_draft`). The layout follows Claris's help for the
//! dialog; it was not compared with a FileMaker Pro 26 capture.

use std::collections::{BTreeSet, VecDeque};

use egui::{Align, CornerRadius, Id, Layout, RichText, Stroke};
use om_data::field_options::{Lookup, NoMatch};
use om_data::{DatabaseSchema, SchemaFieldKind};

use crate::{theme, widgets};

pub(crate) enum Answer {
    Cancel,
    Ok(Lookup),
}

/// "If no exact match, then".
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    DoNotCopy,
    NextLower,
    NextHigher,
    Constant,
}

pub(crate) struct LookupDialog {
    title: String,
    schema: DatabaseSchema,
    table_id: u64,
    from: u64,
    to: Option<u64>,
    copied: Option<u64>,
    /// "Don't copy contents if empty": `copy_empty` off.
    skip_empty: bool,
    choice: Choice,
    constant: String,
    error: Option<String>,
}

impl LookupDialog {
    /// The dialog for field `field` of table `table_id`, on `current` or,
    /// for a new lookup, starting with occurrence `context`.
    pub(crate) fn new(
        field: &str,
        schema: DatabaseSchema,
        table_id: u64,
        context: u64,
        current: Option<&Lookup>,
    ) -> Self {
        let (choice, constant) = match current.map(|l| &l.no_match) {
            Some(NoMatch::NextLower) => (Choice::NextLower, String::new()),
            Some(NoMatch::NextHigher) => (Choice::NextHigher, String::new()),
            Some(NoMatch::Constant(text)) => (Choice::Constant, text.clone()),
            _ => (Choice::DoNotCopy, String::new()),
        };
        Self {
            title: format!("Lookup for Field “{field}”"),
            schema,
            table_id,
            from: current.map_or(context, |l| l.from),
            to: current.map(|l| l.to),
            copied: current.map(|l| l.copied),
            // FileMaker Pro offers "Don't copy contents if empty" checked
            // [I].
            skip_empty: current.is_none_or(|l| !l.copy_empty),
            choice,
            constant,
            error: None,
        }
    }

    fn name_of(&self, occurrence: u64) -> String {
        self.schema
            .occurrences
            .iter()
            .find(|o| o.id == occurrence)
            .map_or_else(|| "<unknown>".to_owned(), |o| o.name.clone())
    }

    /// The occurrences the graph relates to `from`, nearest first.
    fn related(&self) -> Vec<u64> {
        let mut seen = BTreeSet::from([self.from]);
        let mut queue = VecDeque::from([self.from]);
        let mut related = Vec::new();
        while let Some(at) = queue.pop_front() {
            for r in &self.schema.relationships {
                let next = if r.left_occurrence == at {
                    r.right_occurrence
                } else if r.right_occurrence == at {
                    r.left_occurrence
                } else {
                    continue;
                };
                if seen.insert(next) {
                    related.push(next);
                    queue.push_back(next);
                }
            }
        }
        related
    }

    fn lookup(&self) -> Result<Lookup, String> {
        let to = self
            .to
            .ok_or_else(|| "Choose the related table to look up from.".to_owned())?;
        let copied = self
            .copied
            .ok_or_else(|| "Choose the field to copy.".to_owned())?;
        Ok(Lookup {
            from: self.from,
            to,
            copied,
            copy_empty: !self.skip_empty,
            no_match: match self.choice {
                Choice::DoNotCopy => NoMatch::DoNotCopy,
                Choice::NextLower => NoMatch::NextLower,
                Choice::NextHigher => NoMatch::NextHigher,
                Choice::Constant => NoMatch::Constant(self.constant.clone()),
            },
        })
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<Answer> {
        let mut answer = None;
        let modal = egui::Modal::new(Id::unique("database_lookup"))
            .frame(
                egui::Frame::new()
                    .fill(theme::DIALOG_BG)
                    .stroke(Stroke::new(1.0, theme::DIVIDER))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(18)
                    .shadow(ctx.global_style().visuals.window_shadow),
            )
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(&self.title).font(egui::FontId::new(13.0, theme::semibold())),
                    );
                });
                ui.add_space(10.0);
                self.tables(ui);
                ui.add_space(8.0);
                self.fields(ui);
                ui.add_space(8.0);
                self.no_match(ui);
                ui.add_space(6.0);
                ui.checkbox(&mut self.skip_empty, "Don't copy contents if empty");
                if let Some(error) = &self.error {
                    ui.add_space(6.0);
                    ui.colored_label(theme::RED_INK, error);
                }
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::dialog_button(ui, "OK", true, true).clicked() {
                        match self.lookup() {
                            Ok(lookup) => answer = Some(Answer::Ok(lookup)),
                            Err(error) => self.error = Some(error),
                        }
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

    fn tables(&mut self, ui: &mut egui::Ui) {
        let starts: Vec<(u64, String)> = self
            .schema
            .occurrences
            .iter()
            .filter(|o| o.table_id == Some(self.table_id))
            .map(|o| (o.id, o.name.clone()))
            .collect();
        let related: Vec<(u64, String)> = self
            .related()
            .into_iter()
            .map(|o| (o, self.name_of(o)))
            .collect();
        egui::Grid::new("database_lookup_tables")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("Starting with table:");
                let from = self.name_of(self.from);
                egui::ComboBox::from_id_salt("Lookup Starting Table")
                    .width(240.0)
                    .selected_text(from)
                    .show_ui(ui, |ui| {
                        for (id, name) in &starts {
                            if ui.selectable_label(self.from == *id, name).clicked() {
                                self.from = *id;
                                self.to = None;
                                self.copied = None;
                            }
                        }
                    })
                    .response
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::ComboBox,
                            true,
                            "Lookup Starting Table",
                        )
                    });
                ui.end_row();
                ui.label("Lookup from related table:");
                let to = self
                    .to
                    .map_or_else(|| "<none>".to_owned(), |o| self.name_of(o));
                egui::ComboBox::from_id_salt("Lookup Related Table")
                    .width(240.0)
                    .selected_text(to)
                    .show_ui(ui, |ui| {
                        for (id, name) in &related {
                            if ui.selectable_label(self.to == Some(*id), name).clicked()
                                && self.to != Some(*id)
                            {
                                self.to = Some(*id);
                                self.copied = None;
                            }
                        }
                    })
                    .response
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::ComboBox,
                            true,
                            "Lookup Related Table",
                        )
                    });
                ui.end_row();
            });
    }

    fn fields(&mut self, ui: &mut egui::Ui) {
        ui.label("Copy value from field:");
        let table = self.to.and_then(|to| {
            self.schema
                .occurrences
                .iter()
                .find(|o| o.id == to)
                .and_then(|o| o.table_id)
        });
        let fields: Vec<(u64, String, bool)> = table
            .and_then(|t| self.schema.table(t))
            .map(|t| {
                t.fields
                    .iter()
                    .map(|f| (f.id, f.name.clone(), f.kind != SchemaFieldKind::Summary))
                    .collect()
            })
            .unwrap_or_default();
        egui::Frame::new()
            .fill(theme::CONTROL_BG)
            .stroke(Stroke::new(1.0, theme::DIVIDER))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("database_lookup_fields")
                    .auto_shrink([false, false])
                    .max_height(160.0)
                    .min_scrolled_height(160.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for (id, name, offered) in &fields {
                            let on = self.copied == Some(*id);
                            if ui
                                .add_enabled(
                                    *offered,
                                    egui::Button::selectable(on, format!("::{name}")),
                                )
                                .clicked()
                            {
                                self.copied = Some(*id);
                            }
                        }
                    });
            });
    }

    fn no_match(&mut self, ui: &mut egui::Ui) {
        ui.label("If no exact match, then:");
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.radio_value(&mut self.choice, Choice::DoNotCopy, "do not copy");
                ui.radio_value(&mut self.choice, Choice::NextLower, "copy next lower value");
                ui.radio_value(
                    &mut self.choice,
                    Choice::NextHigher,
                    "copy next higher value",
                );
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.choice, Choice::Constant, "use");
                    let constant = ui.add_enabled(
                        self.choice == Choice::Constant,
                        egui::TextEdit::singleline(&mut self.constant).desired_width(200.0),
                    );
                    widgets::accessible_label(ui, constant.id, "Lookup Constant");
                });
            });
        });
    }
}
