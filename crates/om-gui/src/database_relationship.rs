//! Manage Database's Edit Relationship and Specify Table dialogs, as
//! FileMaker Pro 26 lays them out (reference screenshots of 2026-11-11).
//! Each answers with the `SchemaChange` its OK stages
//! (`om_data::schema_draft`): a new or changed relationship, a new or
//! renamed table occurrence.

use egui::{
    vec2, Align, Color32, CornerRadius, Id, Layout, RichText, Role, Sense, Stroke, WidgetInfo,
};
use om_data::scripts::specs::SortSpec;
use om_data::{
    DatabaseSchema, RelationshipMatch, RelationshipSide, SchemaChange, SchemaOccurrence,
};

use crate::{theme, widgets};

/// The operators of the relationship menu, by code (file-metadata.md 4.2).
const OPERATORS: [&str; 7] = ["=", "≠", "<", "≤", ">", "≥", "×"];

/// What a dialog's buttons answered.
pub(crate) enum Answer {
    /// Still open.
    Open,
    Cancel,
    Ok(SchemaChange),
}

/// Edit Relationship: a new relationship (both occurrences chosen) or an
/// existing one (its occurrences fixed).
pub(crate) struct EditRelationship {
    relationship: Option<u64>,
    left: Option<u64>,
    right: Option<u64>,
    left_field: Option<u64>,
    right_field: Option<u64>,
    operator: u8,
    matches: Vec<RelationshipMatch>,
    selected: Option<usize>,
    left_side: RelationshipSide,
    right_side: RelationshipSide,
    /// A side ("Specify…" or "Sort records" checked; `true` for the right)
    /// whose order Manage Database is asked to open Sort Records for.
    sort_request: Option<bool>,
    /// Sort Records for a side's order, while open.
    sorting: Option<(bool, crate::script_workspace::SortOrder)>,
    /// Whether Sort Records' OK gave the left and the right side an order:
    /// only those are sent, so a stored order nobody touched is kept as it
    /// is stored.
    ordered: [bool; 2],
}

impl EditRelationship {
    /// A new relationship, from `left` to `right` when they are known
    /// (a field dragged onto another, or the selected occurrences).
    pub(crate) fn new(left: Option<u64>, right: Option<u64>) -> Self {
        Self {
            relationship: None,
            left,
            right,
            left_field: None,
            right_field: None,
            operator: 0,
            matches: Vec::new(),
            selected: None,
            left_side: RelationshipSide::default(),
            right_side: RelationshipSide::default(),
            sort_request: None,
            sorting: None,
            ordered: [false; 2],
        }
    }

    /// The side whose order Sort Records should open for, once.
    pub(crate) fn take_sort_request(&mut self) -> Option<(bool, u64, SortSpec)> {
        let right = self.sort_request.take()?;
        let (occurrence, side) = if right {
            (self.right?, &self.right_side)
        } else {
            (self.left?, &self.left_side)
        };
        let spec = side.order.clone().unwrap_or(SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: Vec::new(),
        });
        Some((right, occurrence, spec))
    }

    /// Show Sort Records over the dialog for a side's order.
    pub(crate) fn open_sort(&mut self, right: bool, dialog: crate::script_workspace::SortOrder) {
        self.sorting = Some((right, dialog));
    }

    /// Relationship `id` of `schema`, to change.
    pub(crate) fn edit(schema: &DatabaseSchema, id: u64) -> Option<Self> {
        let relationship = schema.relationships.iter().find(|r| r.id == id)?;
        Some(Self {
            relationship: Some(id),
            left: Some(relationship.left_occurrence),
            right: Some(relationship.right_occurrence),
            left_field: None,
            right_field: None,
            operator: 0,
            matches: relationship
                .matches
                .iter()
                .map(|m| RelationshipMatch {
                    left_field: m.left_field,
                    operator: m.operator,
                    right_field: m.right_field,
                })
                .collect(),
            selected: None,
            left_side: relationship.left(),
            right_side: relationship.right(),
            sort_request: None,
            sorting: None,
            ordered: [false; 2],
        })
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context, schema: &DatabaseSchema) -> Answer {
        let mut answer = Answer::Open;
        let occurrence = |id: Option<u64>| -> Option<&SchemaOccurrence> {
            schema.occurrences.iter().find(|o| Some(o.id) == id)
        };
        let fields = |id: Option<u64>| {
            occurrence(id)
                .and_then(|o| o.table_id)
                .and_then(|t| schema.table(t))
                .map(|t| {
                    t.fields
                        .iter()
                        .map(|f| (f.id, f.name.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let field_name = |occurrence_id: Option<u64>, field: u64| {
            fields(occurrence_id)
                .into_iter()
                .find(|(id, _)| *id == field)
                .map_or_else(|| "<Field Missing>".to_owned(), |(_, name)| name)
        };
        let name = |id: Option<u64>| occurrence(id).map_or_else(String::new, |o| o.name.clone());
        egui::Modal::new(Id::unique("edit_relationship")).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.label(RichText::new("Edit Relationship").strong());
            ui.add_space(6.0);
            ui.label(
                "A relationship defines the set of matching related records in one table for \
                 each record in another table. Select the pair of fields to be used to find \
                 matching records. To create complex relationship criteria, use additional \
                 pairs of fields.",
            );
            ui.add_space(8.0);
            let fixed = self.relationship.is_some();
            ui.columns(3, |columns| {
                for (column, side) in [(0usize, false), (2, true)] {
                    let ui = &mut columns[column];
                    let (chosen, field) = if side {
                        (&mut self.right, &mut self.right_field)
                    } else {
                        (&mut self.left, &mut self.left_field)
                    };
                    ui.horizontal(|ui| {
                        ui.label("Table:");
                        let label = if side { "Right Table" } else { "Left Table" };
                        ui.add_enabled_ui(!fixed, |ui| {
                            egui::ComboBox::from_id_salt(label)
                                .width(170.0)
                                .selected_text(name(*chosen))
                                .show_ui(ui, |ui| {
                                    for o in &schema.occurrences {
                                        ui.selectable_value(chosen, Some(o.id), &o.name);
                                    }
                                })
                                .response
                                .widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, label));
                        });
                    });
                    let list = fields(*chosen);
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, theme::FIELD_BORDER))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt(("relationship_fields", side))
                                .max_height(150.0)
                                .min_scrolled_height(150.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for (id, field_name) in list {
                                        if row(ui, &field_name, *field == Some(id)) {
                                            *field = Some(id);
                                        }
                                    }
                                });
                        });
                }
                let ui = &mut columns[1];
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    egui::ComboBox::from_id_salt("relationship_operator")
                        .width(48.0)
                        .selected_text(OPERATORS[usize::from(self.operator)])
                        .show_ui(ui, |ui| {
                            for (code, symbol) in OPERATORS.iter().enumerate() {
                                ui.selectable_value(&mut self.operator, code as u8, *symbol);
                            }
                        })
                        .response
                        .widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, "Operator"));
                });
            });
            let pair = match (self.left_field, self.right_field) {
                (Some(left_field), Some(right_field)) => Some(RelationshipMatch {
                    left_field,
                    operator: self.operator,
                    right_field,
                }),
                _ => None,
            };
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::dialog_button(
                        ui,
                        "Change",
                        false,
                        pair.is_some() && self.selected.is_some(),
                    )
                    .clicked()
                    {
                        if let (Some(pair), Some(at)) = (pair, self.selected) {
                            self.matches[at] = pair;
                        }
                    }
                    if widgets::dialog_button(ui, "Add", false, pair.is_some()).clicked() {
                        if let Some(pair) = pair {
                            self.matches.push(pair);
                            self.selected = Some(self.matches.len() - 1);
                        }
                    }
                });
            });
            // The two tables' names over their columns of the pairs.
            let (heading, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
            let third = heading.width() / 3.0;
            for (column, title) in [(0.0, name(self.left)), (2.0, name(self.right))] {
                let cell = egui::Rect::from_min_size(
                    heading.min + vec2(third * column, 0.0),
                    vec2(third, heading.height()),
                );
                widgets::put_label(ui, cell, &title, theme::body(), theme::TEXT, Align::Center);
            }
            egui::Frame::new()
                .fill(Color32::WHITE)
                .stroke(Stroke::new(1.0, theme::FIELD_BORDER))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("relationship_pairs")
                        .max_height(80.0)
                        .min_scrolled_height(80.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (at, pair) in self.matches.iter().enumerate() {
                                let cells = [
                                    field_name(self.left, pair.left_field),
                                    OPERATORS
                                        .get(usize::from(pair.operator))
                                        .unwrap_or(&"?")
                                        .to_string(),
                                    field_name(self.right, pair.right_field),
                                ];
                                if pair_row(ui, &cells, self.selected == Some(at)) {
                                    self.selected = Some(at);
                                    self.left_field = Some(pair.left_field);
                                    self.right_field = Some(pair.right_field);
                                    self.operator = pair.operator;
                                }
                            }
                        });
                });
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::dialog_button(ui, "Delete", false, self.selected.is_some())
                        .clicked()
                    {
                        if let Some(at) = self.selected.take() {
                            self.matches.remove(at);
                        }
                    }
                    if widgets::dialog_button(ui, "Duplicate", false, self.selected.is_some())
                        .clicked()
                    {
                        if let Some(at) = self.selected {
                            self.matches.insert(at + 1, self.matches[at]);
                            self.selected = Some(at + 1);
                        }
                    }
                });
            });
            ui.separator();
            ui.columns(2, |columns| {
                for (column, side) in [(0usize, false), (1, true)] {
                    let ui = &mut columns[column];
                    let (title, options) = if side {
                        (name(self.right), &mut self.right_side)
                    } else {
                        (name(self.left), &mut self.left_side)
                    };
                    ui.label(title);
                    ui.checkbox(
                        &mut options.allow_creation,
                        "Allow creation of records in this table via this relationship",
                    );
                    ui.checkbox(
                        &mut options.delete_related,
                        "Delete related records in this table when a record is deleted in the \
                         other table",
                    );
                    ui.horizontal(|ui| {
                        // Checking "Sort records" without an order opens
                        // Sort Records, as "Specify…" does.
                        let empty = options.order.as_ref().is_none_or(|o| o.fields.is_empty());
                        let toggled = ui.checkbox(&mut options.sort, "Sort records").changed();
                        if toggled && options.sort && empty {
                            self.sort_request = Some(side);
                        }
                        if widgets::dialog_button(ui, "Specify…", false, options.sort).clicked() {
                            self.sort_request = Some(side);
                        }
                    });
                }
            });
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ready = self.left.is_some() && self.right.is_some() && !self.matches.is_empty();
                if widgets::dialog_button(ui, "OK", true, ready).clicked() {
                    // An order Sort Records did not give is kept as stored.
                    let send = |side: &RelationshipSide, ordered: bool| RelationshipSide {
                        order: side.order.clone().filter(|_| ordered),
                        ..side.clone()
                    };
                    let left = send(&self.left_side, self.ordered[0]);
                    let right = send(&self.right_side, self.ordered[1]);
                    answer = Answer::Ok(match self.relationship {
                        Some(relationship_id) => SchemaChange::ChangeRelationship {
                            relationship_id,
                            matches: self.matches.clone(),
                            left: left.clone(),
                            right: right.clone(),
                        },
                        None => SchemaChange::CreateRelationship {
                            left_occurrence: self.left.expect("ready"),
                            right_occurrence: self.right.expect("ready"),
                            matches: self.matches.clone(),
                            left: left.clone(),
                            right: right.clone(),
                        },
                    });
                }
                if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                    answer = Answer::Cancel;
                }
            });
        });
        // Sort Records over the dialog: OK takes its order for the side
        // ("Sort records" stays on only with fields to sort by); Cancel
        // leaves the side as it was, without an order to sort by.
        if let Some((right, mut dialog)) = self.sorting.take() {
            let side = if right {
                &mut self.right_side
            } else {
                &mut self.left_side
            };
            match dialog.show(ctx, true) {
                Some(crate::script_workspace::Picked::Ok(spec)) => {
                    side.sort = !spec.fields.is_empty();
                    side.order = Some(spec);
                    self.ordered[usize::from(right)] = true;
                }
                Some(crate::script_workspace::Picked::Cancel) => {
                    if side.order.as_ref().is_none_or(|o| o.fields.is_empty()) {
                        side.sort = false;
                    }
                }
                None => self.sorting = Some((right, dialog)),
            }
            if self.sorting.is_some() {
                return Answer::Open;
            }
        }
        answer
    }
}

/// Specify Table: a new occurrence of a table of this file, or another
/// name for an existing one.
pub(crate) struct SpecifyTable {
    occurrence: Option<u64>,
    table: Option<u64>,
    name: String,
    file: String,
}

impl SpecifyTable {
    pub(crate) fn new(file: String) -> Self {
        Self {
            occurrence: None,
            table: None,
            name: String::new(),
            file,
        }
    }

    pub(crate) fn edit(file: String, occurrence: &SchemaOccurrence) -> Self {
        Self {
            occurrence: Some(occurrence.id),
            table: occurrence.table_id,
            name: occurrence.name.clone(),
            file,
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context, schema: &DatabaseSchema) -> Answer {
        let mut answer = Answer::Open;
        egui::Modal::new(Id::unique("specify_table")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(RichText::new("Specify Table").strong());
            ui.add_space(6.0);
            ui.label(
                "Choose a table to include in the graph from this file or another data \
                 source.  The same table can be included in the graph more than once.",
            );
            ui.horizontal(|ui| {
                ui.label("Data Source:");
                ui.add_enabled(
                    false,
                    egui::Button::new(format!("Current File (“{}”)", self.file)),
                )
                .on_disabled_hover_text("OpenMaker adds occurrences of this file's tables only.");
            });
            let editing = self.occurrence.is_some();
            egui::Frame::new()
                .fill(Color32::WHITE)
                .stroke(Stroke::new(1.0, theme::FIELD_BORDER))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("specify_table_tables")
                        .max_height(150.0)
                        .min_scrolled_height(150.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for table in &schema.tables {
                                let on = self.table == Some(table.id);
                                // Another table for an existing occurrence
                                // is not saved yet.
                                let clicked = ui
                                    .add_enabled_ui(!editing || on, |ui| row(ui, &table.name, on))
                                    .inner;
                                if clicked && !editing {
                                    self.table = Some(table.id);
                                    self.name = om_data::unused_name(
                                        &table.name,
                                        schema.occurrences.iter().map(|o| &o.name),
                                    );
                                }
                            }
                        });
                });
            ui.label("Name");
            let response = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(300.0));
            widgets::accessible_label(ui, response.id, "Name");
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ready = self.table.is_some() && !self.name.trim().is_empty();
                if widgets::dialog_button(ui, "OK", true, ready).clicked() {
                    answer = Answer::Ok(match self.occurrence {
                        Some(occurrence_id) => SchemaChange::RenameOccurrence {
                            occurrence_id,
                            name: self.name.trim().to_owned(),
                        },
                        None => SchemaChange::CreateOccurrence {
                            table_id: self.table.expect("ready"),
                            name: self.name.trim().to_owned(),
                        },
                    });
                }
                if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                    answer = Answer::Cancel;
                }
            });
        });
        answer
    }
}

/// A match pair's row: the left field under the left table, the operator
/// between, the right field under the right table; true when clicked.
fn pair_row(ui: &mut egui::Ui, cells: &[String; 3], selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::DEFAULT_BUTTON);
    }
    let color = if selected {
        Color32::WHITE
    } else {
        theme::TEXT
    };
    let third = rect.width() / 3.0;
    for (column, text) in cells.iter().enumerate() {
        let cell = egui::Rect::from_min_size(
            rect.min + vec2(third * column as f32, 0.0),
            vec2(third, rect.height()),
        );
        widgets::put_label(ui, cell, text, theme::body(), color, Align::Center);
    }
    let label = cells.join("   ");
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &label));
    response.clicked()
}

/// A selectable list row; true when clicked.
fn row(ui: &mut egui::Ui, text: &str, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::DEFAULT_BUTTON);
    }
    widgets::put_label(
        ui,
        rect.shrink2(vec2(8.0, 0.0)),
        text,
        theme::body(),
        if selected {
            Color32::WHITE
        } else {
            theme::TEXT
        },
        Align::Min,
    );
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, text));
    response.clicked()
}
