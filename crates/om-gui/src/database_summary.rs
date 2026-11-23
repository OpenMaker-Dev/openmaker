//! Manage Database's "Options for Summary Field": the kind of summary
//! (Total of, Average of, Count of, Minimum, Maximum, Standard Deviation
//! of, Fraction of Total of, List of), the field it summarizes, the kind's
//! option (running total, weighted average, running count, by population,
//! subtotaled), restarting a running summary for each sorted group and the
//! field that groups it, and how repetitions are summarized. OK stages
//! `SchemaChange::CreateSummaryField` or `ChangeSummary`. The layout
//! follows Claris's help for the dialog; it was not compared with a FileMaker
//! Pro 26 capture.

use egui::{Align, CornerRadius, Id, Layout, RichText, Stroke};
use om_data::meta::{summary_flag as flag, SummaryDef, SummaryField, SummaryKind};
use om_data::{SchemaChange, SchemaField, SchemaFieldKind};

use crate::{theme, widgets};

/// The kinds in the dialog's order, with their labels and the label of
/// their option, if they have one.
const KINDS: [(SummaryKind, &str, Option<&str>); 8] = [
    (SummaryKind::Total, "Total of", Some("Running total")),
    (SummaryKind::Average, "Average of", Some("Weighted average")),
    (SummaryKind::Count, "Count of", Some("Running count")),
    (SummaryKind::Minimum, "Minimum", None),
    (SummaryKind::Maximum, "Maximum", None),
    (
        SummaryKind::StdDeviation,
        "Standard Deviation of",
        Some("By population"),
    ),
    (
        SummaryKind::Fraction,
        "Fraction of Total of",
        Some("Subtotaled"),
    ),
    (SummaryKind::List, "List of", None),
];

pub(crate) enum Answer {
    Cancel,
    Ok(SchemaChange),
}

/// What the dialog defines: a new field's name and comment, or a field.
pub(crate) enum Target {
    New {
        table_id: u64,
        name: String,
        comment: String,
    },
    Existing {
        table_id: u64,
        field_id: u64,
    },
}

pub(crate) struct SummaryDialog {
    title: String,
    target: Target,
    /// The table's fields and the occurrence a second field is read
    /// through (its first occurrence).
    fields: Vec<SchemaField>,
    occurrence: u64,
    kind: SummaryKind,
    field: Option<u64>,
    /// The kind's option: running, weighted, by population, subtotaled.
    option: bool,
    restart: bool,
    /// The weight, subtotal or restart field.
    second: Option<u64>,
    individually: bool,
    /// Why OK was refused, by the dialog or by the draft.
    pub(crate) error: Option<String>,
    /// OK was pressed and the change is staged on the provider.
    pub(crate) waiting: bool,
}

impl SummaryDialog {
    pub(crate) fn new(
        name: &str,
        target: Target,
        fields: Vec<SchemaField>,
        occurrence: u64,
        current: Option<&SummaryDef>,
    ) -> Self {
        let def = current.copied();
        let kind = def.map_or(SummaryKind::Total, |d| d.kind);
        let flags = def.map_or(0, |d| d.flags);
        Self {
            title: format!("Options for Summary Field “{name}”"),
            target,
            fields: fields
                .into_iter()
                .filter(|f| f.kind != SchemaFieldKind::Summary)
                .collect(),
            occurrence,
            kind,
            field: def.and_then(|d| d.field).map(|f| f.field),
            option: flags & (flag::OPTION | flag::RUNNING) != 0,
            restart: flags & flag::RESTART != 0,
            second: def.and_then(|d| d.additional).map(|f| f.field),
            individually: flags & flag::INDIVIDUAL_REPETITIONS != 0,
            error: None,
            waiting: false,
        }
    }

    /// The definition the dialog shows.
    fn definition(&self) -> Result<SummaryDef, String> {
        let field = self
            .field
            .ok_or_else(|| "Choose the field to summarize.".to_owned())?;
        let running = matches!(self.kind, SummaryKind::Total | SummaryKind::Count);
        let mut flags = 0;
        if self.option {
            flags |= if running { flag::RUNNING } else { flag::OPTION };
        }
        let restart = running && self.option && self.restart;
        if restart {
            flags |= flag::RESTART;
        }
        if self.individually {
            flags |= flag::INDIVIDUAL_REPETITIONS;
        }
        let needs_second = restart
            || (self.option && matches!(self.kind, SummaryKind::Average | SummaryKind::Fraction));
        let additional = if needs_second {
            let second = self
                .second
                .ok_or_else(|| "Choose the second field.".to_owned())?;
            Some(SummaryField {
                occurrence: Some(self.occurrence),
                field: second,
                repetition: 1,
            })
        } else {
            None
        };
        Ok(SummaryDef {
            flags,
            additional,
            ..SummaryDef::new(self.kind, field)
        })
    }

    fn change(&self, summary: SummaryDef) -> SchemaChange {
        match &self.target {
            Target::New {
                table_id,
                name,
                comment,
            } => SchemaChange::CreateSummaryField {
                table_id: *table_id,
                name: name.clone(),
                comment: comment.clone(),
                summary,
            },
            Target::Existing { table_id, field_id } => SchemaChange::ChangeSummary {
                table_id: *table_id,
                field_id: *field_id,
                summary,
            },
        }
    }

    /// A list of the fields `kind` takes, choosing into `chosen`.
    fn field_list(
        ui: &mut egui::Ui,
        id: &str,
        fields: &[SchemaField],
        kind: Option<SummaryKind>,
        chosen: &mut Option<u64>,
    ) {
        egui::Frame::new()
            .fill(theme::CONTROL_BG)
            .stroke(Stroke::new(1.0, theme::DIVIDER))
            .show(ui, |ui| {
                ui.set_width(200.0);
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .max_height(220.0)
                    .min_scrolled_height(220.0)
                    .show(ui, |ui| {
                        ui.set_width(196.0);
                        for field in fields {
                            let fits = kind.is_none_or(|k| {
                                om_data::summary_field::kind_result(k, field.data_type).is_ok()
                            });
                            let on = *chosen == Some(field.id);
                            let response = ui.add_enabled(
                                fits,
                                egui::Button::selectable(on, field.name.as_str()),
                            );
                            if response.clicked() {
                                *chosen = Some(field.id);
                            }
                        }
                    });
            });
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<Answer> {
        let mut answer = None;
        egui::Modal::new(Id::unique("database_summary_options"))
            .frame(
                egui::Frame::new()
                    .fill(theme::DIALOG_BG)
                    .stroke(Stroke::new(1.0, theme::DIVIDER))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(18)
                    .shadow(ctx.global_style().visuals.window_shadow),
            )
            .show(ctx, |ui| {
                ui.set_width(620.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(&self.title).font(egui::FontId::new(13.0, theme::semibold())),
                    );
                });
                ui.add_space(10.0);
                let option = KINDS
                    .iter()
                    .find(|(k, ..)| *k == self.kind)
                    .and_then(|(.., o)| *o);
                let running = matches!(self.kind, SummaryKind::Total | SummaryKind::Count);
                let second = self.option
                    && (matches!(self.kind, SummaryKind::Average | SummaryKind::Fraction)
                        || (running && self.restart));
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(170.0);
                        ui.label(RichText::new("Summary").strong());
                        for (kind, label, _) in KINDS {
                            if ui.radio(self.kind == kind, label).clicked() && self.kind != kind {
                                self.kind = kind;
                                self.option = false;
                                self.restart = false;
                                // A field that kind does not take is cleared.
                                let fits = self
                                    .field
                                    .and_then(|id| self.fields.iter().find(|f| f.id == id));
                                if fits.is_some_and(|f| {
                                    om_data::summary_field::kind_result(kind, f.data_type).is_err()
                                }) {
                                    self.field = None;
                                }
                            }
                        }
                    });
                    ui.vertical(|ui| {
                        ui.label("Summarize field:");
                        Self::field_list(
                            ui,
                            "summary_fields",
                            &self.fields,
                            Some(self.kind),
                            &mut self.field,
                        );
                    });
                    if second {
                        ui.vertical(|ui| {
                            ui.label(match self.kind {
                                SummaryKind::Average => "Weighted by:",
                                SummaryKind::Fraction => "When sorted by:",
                                _ => "Restart for each group of:",
                            });
                            let kind =
                                (self.kind == SummaryKind::Average).then_some(SummaryKind::Total);
                            Self::field_list(
                                ui,
                                "summary_second",
                                &self.fields,
                                kind,
                                &mut self.second,
                            );
                        });
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(option.is_some(), |ui| {
                        ui.checkbox(&mut self.option, option.unwrap_or("Running total"));
                    });
                    if running {
                        ui.add_enabled_ui(self.option, |ui| {
                            ui.checkbox(&mut self.restart, "Restart summary for each sorted group");
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Summarize repetitions:");
                    let label = if self.individually {
                        "Individually"
                    } else {
                        "All together"
                    };
                    egui::ComboBox::from_id_salt("Summarize Repetitions")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.individually, false, "All together");
                            ui.selectable_value(&mut self.individually, true, "Individually");
                        });
                });
                if let Some(error) = &self.error {
                    ui.add_space(6.0);
                    ui.colored_label(theme::RED, error);
                }
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::dialog_button(ui, "OK", true, !self.waiting).clicked() {
                        match self.definition() {
                            Ok(summary) => answer = Some(Answer::Ok(self.change(summary))),
                            Err(error) => self.error = Some(error),
                        }
                    }
                    if widgets::dialog_button(ui, "Cancel", false, !self.waiting).clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
            });
        answer
    }
}
