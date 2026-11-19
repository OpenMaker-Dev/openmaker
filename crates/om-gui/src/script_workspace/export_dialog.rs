//! Export Records' export order (script-steps.md 11.4): FileMaker's
//! Specify Field Order for Export dialog for a stored order (Claris Help,
//! "Exporting data"). The table pop-up and its fields are at the left; Move,
//! Move All, Clear and Clear All between them; the field export order at the
//! right (dragged into a new order); the group-by fields, the output file's
//! character set and "Apply current layout's data formatting to exported
//! data" below.
//!
//! The dialog edits a decoded [`ExportOptions`]. What an entry stores and
//! the dialog does not show (a summary field's break field `sum_by`, a
//! repetition), the group-by fields and `ExpUseNameAlias` are kept. The
//! controls are Claris's; their sizes and places follow Sort Records'
//! dialog, not a measurement of FileMaker Pro's.

use std::sync::Arc;

use egui::{pos2, vec2, FontId, Rect, Sense, Stroke};
use om_data::host::CalcSchema;
use om_transfer::spec::ExportField;
use om_transfer::{ExportCharset, ExportOptions, FieldSpec};

use super::pickers::{
    self, checkbox, dialog, left_label, list_box, list_box_with, popup_menu, push_button, ListRow,
    Picked, BUTTON_WIDTH, CONTROL_HEIGHT,
};
use crate::theme;

/// The export order a step gets when none is stored: no fields, UTF-8,
/// FileMaker Pro 26's `ExpUseNameAlias` byte as its XML importer writes it
/// (script-steps.md 11.4) [I: a new order from FileMaker Pro's dialog was
/// not compared].
pub(crate) fn new_order() -> ExportOptions {
    ExportOptions {
        fields: Vec::new(),
        group_by: Vec::new(),
        formatting: false,
        charset: ExportCharset::Utf8,
        use_name_alias: Some(1),
    }
}

/// The Specify Field Order for Export dialog of a step's export order.
pub(crate) struct ExportOrder {
    names: Option<Arc<CalcSchema>>,
    order: ExportOptions,
    /// The table occurrence whose fields the left list shows.
    occurrence: Option<u64>,
    /// The field selected in the left list.
    field: Option<u64>,
    /// The entry selected in the export order.
    entry: Option<usize>,
}

impl ExportOrder {
    pub fn new(order: ExportOptions, names: Option<Arc<CalcSchema>>) -> Self {
        let occurrence = order
            .fields
            .first()
            .and_then(|f| f.field.ids())
            .map(|(o, _)| o)
            .or_else(|| {
                names
                    .as_ref()
                    .and_then(|n| n.occurrences_in_order().first().map(|o| o.id))
            });
        Self {
            names,
            order,
            occurrence,
            field: None,
            entry: None,
        }
    }

    /// `Table::Field` of a stored field spec.
    fn field_name(&self, spec: &FieldSpec) -> String {
        let names = self.names.as_deref();
        let Some((occurrence, field)) = spec.ids() else {
            return om_data::formula::FIELD_MISSING.to_owned();
        };
        let table = names
            .and_then(|n| n.occurrence(occurrence))
            .map_or(om_data::formula::TABLE_MISSING, |o| o.name.as_str());
        let name = names
            .and_then(|n| n.field(occurrence, field))
            .map_or(om_data::formula::FIELD_MISSING, |f| f.name.as_str());
        let mut text = format!("{table}::{name}");
        if spec.repetition > 1 {
            text.push_str(&format!("[{}]", spec.repetition));
        }
        text
    }

    /// The fields of the chosen occurrence's table.
    fn fields(&self) -> Vec<(u64, String)> {
        let (Some(names), Some(occurrence)) = (&self.names, self.occurrence) else {
            return Vec::new();
        };
        names
            .table_of(occurrence)
            .map(|t| t.fields.values().map(|f| (f.id, f.name.clone())).collect())
            .unwrap_or_default()
    }

    /// The entry of a field of the chosen occurrence, when it is in the
    /// order.
    fn position(&self, field: u64) -> Option<usize> {
        let occurrence = self.occurrence?;
        self.order
            .fields
            .iter()
            .position(|f| f.field.ids() == Some((occurrence, field)))
    }

    /// Add a field of the chosen occurrence to the export order (Move);
    /// one already in it is selected instead.
    fn add(&mut self, field: u64) {
        let Some(occurrence) = self.occurrence else {
            return;
        };
        if let Some(i) = self.position(field) {
            self.entry = Some(i);
            return;
        }
        // The order's count is a u16 (script-steps.md 11.4).
        if self.order.fields.len() >= usize::from(u16::MAX) {
            return;
        }
        self.order.fields.push(ExportField {
            field: FieldSpec::new(occurrence, field),
            sum_by: FieldSpec::none(),
        });
        self.entry = Some(self.order.fields.len() - 1);
    }

    /// Move All: every field of the chosen occurrence not yet in the order.
    fn add_all(&mut self) {
        let entry = self.entry;
        for (field, _) in self.fields() {
            if self.position(field).is_none() {
                self.add(field);
            }
        }
        self.entry = entry;
    }

    pub fn show(&mut self, ctx: &egui::Context, on_top: bool) -> Option<Picked<ExportOptions>> {
        let mut outcome = None;
        let mut double_field = None;
        let shown = dialog(
            ctx,
            "export_order",
            "Specify Field Order for Export",
            600.0,
            on_top,
            |ui| {
                let full = ui.available_width();
                let left = ui.cursor().left();
                let list_width = 220.0;
                let middle = 112.0;
                let right_x = left + list_width + middle;
                ui.add_space(14.0);
                // The table pop-up over the fields, the order's title.
                let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
                let occurrences: Vec<(u64, String)> = self
                    .names
                    .as_ref()
                    .map(|n| {
                        n.occurrences_in_order()
                            .into_iter()
                            .map(|o| (o.id, o.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let current = occurrences
                    .iter()
                    .find(|(id, _)| Some(*id) == self.occurrence)
                    .map_or_else(String::new, |(_, n)| n.clone());
                let items: Vec<String> = occurrences.iter().map(|(_, n)| n.clone()).collect();
                let popup = Rect::from_min_size(row.min, vec2(list_width, CONTROL_HEIGHT));
                if let Some(i) = popup_menu(ui, popup, "Table", &current, &items, true) {
                    if self.occurrence != Some(occurrences[i].0) {
                        self.occurrence = Some(occurrences[i].0);
                        self.field = None;
                    }
                }
                left_label(
                    ui,
                    Rect::from_min_max(pos2(right_x, row.top()), row.max),
                    "Field export order",
                    FontId::new(theme::BODY, theme::semibold()),
                    theme::TEXT,
                );
                ui.add_space(6.0);
                let (lists, _) = ui.allocate_exact_size(vec2(full, 210.0), Sense::hover());
                // The table's fields.
                let fields = self.fields();
                let rows: Vec<ListRow> = fields.iter().map(|(_, n)| ListRow::item(n)).collect();
                let selected = fields.iter().position(|(id, _)| Some(*id) == self.field);
                let field_list = Rect::from_min_size(lists.min, vec2(list_width, lists.height()));
                let click = list_box(ui, field_list, "export_fields", &rows, selected, true);
                if let Some(i) = click.clicked.or(click.double_clicked) {
                    self.field = Some(fields[i].0);
                    self.entry = None;
                }
                if let Some(i) = click.double_clicked {
                    double_field = Some(fields[i].0);
                }
                // Move, Move All, Clear, Clear All.
                let button_x = lists.left() + list_width + (middle - BUTTON_WIDTH) / 2.0;
                let button = |n: usize| {
                    Rect::from_min_size(
                        pos2(button_x, lists.top() + 40.0 + n as f32 * 32.0),
                        vec2(BUTTON_WIDTH, CONTROL_HEIGHT),
                    )
                };
                let can_move = self.field.is_some();
                if push_button(ui, button(0), "Move", "Move", false, can_move).clicked() {
                    if let Some(field) = self.field {
                        self.add(field);
                    }
                }
                if let Some(field) = double_field {
                    self.add(field);
                }
                if push_button(
                    ui,
                    button(1),
                    "Move All",
                    "Move All",
                    false,
                    !fields.is_empty(),
                )
                .clicked()
                {
                    self.add_all();
                }
                let can_clear = self.entry.is_some();
                if push_button(ui, button(2), "Clear", "Clear", false, can_clear).clicked() {
                    if let Some(i) = self.entry.take() {
                        self.order.fields.remove(i);
                    }
                }
                let has_entries = !self.order.fields.is_empty();
                if push_button(ui, button(3), "Clear All", "Clear All", false, has_entries)
                    .clicked()
                {
                    self.order.fields.clear();
                    self.entry = None;
                }
                // The export order.
                let order: Vec<ListRow> = self
                    .order
                    .fields
                    .iter()
                    .map(|f| ListRow::item(self.field_name(&f.field)))
                    .collect();
                let order_list = Rect::from_min_max(pos2(right_x, lists.top()), lists.max);
                let click = list_box_with(
                    ui,
                    order_list,
                    "export_order",
                    &order,
                    self.entry,
                    true,
                    true,
                );
                if let Some(i) = click.clicked {
                    self.entry = Some(i);
                    self.field = None;
                }
                if let Some(i) = click.double_clicked {
                    // A double click takes a field out, as in Sort Records.
                    self.order.fields.remove(i);
                    self.entry = None;
                }
                if let Some((from, to)) = click.moved {
                    if from < self.order.fields.len() {
                        let entry = self.order.fields.remove(from);
                        let to = if to > from { to - 1 } else { to }.min(self.order.fields.len());
                        self.order.fields.insert(to, entry);
                        self.entry = Some(to);
                    }
                }
                ui.add_space(12.0);
                // Group by: the stored break fields, kept as they are.
                let (row, _) = ui.allocate_exact_size(vec2(full, 22.0), Sense::hover());
                left_label(
                    ui,
                    row,
                    "Group by",
                    FontId::new(theme::BODY, theme::semibold()),
                    theme::TEXT,
                );
                let groups: Vec<String> = self
                    .order
                    .group_by
                    .iter()
                    .map(|(f, _)| self.field_name(f))
                    .collect();
                let (row, _) = ui.allocate_exact_size(vec2(full, 22.0), Sense::hover());
                left_label(
                    ui,
                    row,
                    &if groups.is_empty() {
                        "Unsorted".to_owned()
                    } else {
                        groups.join(", ")
                    },
                    pickers::body(),
                    theme::CAPTION,
                );
                ui.add_space(8.0);
                ui.painter().hline(
                    left..=left + full,
                    ui.cursor().top(),
                    Stroke::new(1.0, theme::HAIRLINE),
                );
                ui.add_space(8.0);
                // The output file's character set and the formatting.
                let (row, _) = ui.allocate_exact_size(vec2(full, 26.0), Sense::hover());
                let label = "Output file character set:";
                left_label(
                    ui,
                    Rect::from_min_size(row.min, vec2(180.0, row.height())),
                    label,
                    pickers::body(),
                    theme::TEXT,
                );
                let items: Vec<String> = ExportCharset::ALL
                    .iter()
                    .map(|c| c.name().to_owned())
                    .collect();
                let popup = Rect::from_min_size(
                    pos2(row.left() + 184.0, row.center().y - CONTROL_HEIGHT / 2.0),
                    vec2(220.0, CONTROL_HEIGHT),
                );
                if let Some(n) = popup_menu(
                    ui,
                    popup,
                    "Output file character set",
                    self.order.charset.name(),
                    &items,
                    true,
                ) {
                    self.order.charset = ExportCharset::ALL[n];
                }
                let (row, _) = ui.allocate_exact_size(vec2(full, 24.0), Sense::hover());
                checkbox(
                    ui,
                    row,
                    &mut self.order.formatting,
                    "Apply current layout's data formatting to exported data",
                    true,
                );
                ui.add_space(18.0);
                let (cancel, ok) = pickers::cancel_ok(ui, true);
                if cancel {
                    outcome = Some(Picked::Cancel);
                } else if ok {
                    outcome = Some(Picked::Ok(self.order.clone()));
                }
            },
        );
        if outcome.is_none() {
            if shown.escape {
                outcome = Some(Picked::Cancel);
            } else if shown.enter {
                outcome = Some(Picked::Ok(self.order.clone()));
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_fields_builds_the_order_once_each_and_keeps_what_it_does_not_show() {
        let mut stored = new_order();
        // A summary field grouped by a break field, as FileMaker stores one.
        stored.fields.push(ExportField {
            field: FieldSpec::new(0xd00002, 9),
            sum_by: FieldSpec::new(0xd00002, 4),
        });
        stored.use_name_alias = None;
        let mut dialog = ExportOrder::new(stored, None);
        assert_eq!(dialog.occurrence, Some(0xd00002));
        dialog.add(8);
        // Moving it again selects it rather than adding it twice.
        dialog.add(8);
        dialog.add(9);
        assert_eq!(dialog.order.fields.len(), 2);
        assert_eq!(dialog.entry, Some(0));
        assert_eq!(dialog.order.fields[0].sum_by, FieldSpec::new(0xd00002, 4));
        assert_eq!(dialog.order.fields[1].field, FieldSpec::new(0xd00002, 8));
        assert_eq!(dialog.order.fields[1].sum_by, FieldSpec::none());
        assert_eq!(dialog.order.use_name_alias, None);
        // It encodes as FileMaker reads it back.
        let bytes = dialog.order.encode();
        assert_eq!(ExportOptions::decode(&bytes).unwrap(), dialog.order);
    }
}
