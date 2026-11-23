//! Portal Setup and Add Fields to Portal, which the Portal tool opens for
//! the portal it drew, as FileMaker Pro 26 opens them (re/client-ui.md,
//! "Portal Setup"), their controls where Pro's `IDD_PORTAL_SETUP.nib` and
//! `IDD_FIELD_ORDER.nib` put them. What OpenMaker places (authoring
//! operation 21, `om_data::layout_placement::plan_portal`) is the portal's
//! related occurrence, its number of rows and the fields in its first row;
//! Portal Setup's other options are shown at the defaults Pro gives a new
//! portal, not offered.

use egui::{vec2, CornerRadius, Rect, Stroke, UiBuilder};
use om_data::layout_placement::{portal_field_frames, PortalPlacement, MAX_PORTAL_FIELDS};

use crate::part_setup::dialog;
use crate::{theme, widgets};

const NOT_YET: &str = "OpenMaker places a portal with this option as FileMaker Pro sets it for a \
                       new portal; it does not change it yet.";

/// The rows Portal Setup offers at first for a portal drawn this high: as
/// many rows of 39 points as fit, 7 for the 275-point portal of Pro's twin
/// (layout-model.md 10.17) [OpenMaker's choice: how Pro counts them is not
/// measured].
pub(crate) fn default_rows(height: f64) -> u32 {
    ((height / 39.0).floor() as u32).clamp(1, om_data::layout_placement::MAX_PORTAL_ROWS)
}

/// An occurrence a portal can show: one the relationship graph joins to the
/// layout's own, with its table's fields.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PortalOccurrence {
    pub id: u64,
    pub name: String,
    pub table: u64,
    pub fields: Vec<PortalField>,
}

/// A field of a portal's occurrence: its id and name, and whether
/// OpenMaker places it in a portal (not a container).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PortalField {
    pub id: u64,
    pub name: String,
    pub placeable: bool,
}

/// The occurrences the window's schema joins to `own`, the layout's, in the
/// graph's order, with their tables' fields in id order.
pub(crate) fn related_occurrences(
    schema: &om_data::host::CalcSchema,
    own: u64,
) -> Vec<PortalOccurrence> {
    let mut reached = std::collections::HashSet::from([own]);
    let mut pending = vec![own];
    while let Some(occurrence) = pending.pop() {
        for relationship in schema.relationships() {
            let other = if relationship.left == occurrence {
                relationship.right
            } else if relationship.right == occurrence {
                relationship.left
            } else {
                continue;
            };
            if reached.insert(other) {
                pending.push(other);
            }
        }
    }
    schema
        .occurrences_in_order()
        .into_iter()
        .filter(|o| o.id != own && reached.contains(&o.id))
        .filter_map(|o| {
            let table = schema.table_of(o.id)?;
            Some(PortalOccurrence {
                id: o.id,
                name: o.name.clone(),
                table: table.id,
                fields: table
                    .fields
                    .values()
                    .map(|f| PortalField {
                        id: f.id,
                        name: f.name.clone(),
                        placeable: f.value_type != om_data::calc::ValueType::Container,
                    })
                    .collect(),
            })
        })
        .collect()
}

/// What the dialogs set for the portal.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PortalChoice {
    pub occurrence: u64,
    pub table: u64,
    pub name: String,
    pub rows: u32,
    /// The fields in its first row, left to right: id and name.
    pub fields: Vec<(u64, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PortalSetupAnswer {
    Open,
    Cancel,
    Done(PortalChoice),
}

/// Portal Setup, then Add Fields to Portal, for a portal drawn at `frame`.
pub(crate) struct PortalSetup {
    occurrences: Vec<PortalOccurrence>,
    occurrence: Option<usize>,
    rows: String,
    frame: om_layout::Rect,
    /// The occurrence and fields (ids) set before, when the dialogs are
    /// opened again: Add Fields to Portal starts with those fields if the
    /// same occurrence is still chosen.
    kept: Option<(u64, Vec<u64>)>,
    adding: Option<AddFields>,
}

/// Add Fields to Portal: the fields included, in order (indices into the
/// occurrence's fields), and the row chosen in each list.
struct AddFields {
    included: Vec<usize>,
    available_row: Option<usize>,
    included_row: Option<usize>,
    problem: Option<String>,
}

impl PortalSetup {
    /// Portal Setup over a portal drawn at `frame`, set as `current` set it
    /// before, if it was set up already.
    pub(crate) fn new(
        occurrences: Vec<PortalOccurrence>,
        frame: om_layout::Rect,
        current: Option<&PortalChoice>,
    ) -> Self {
        let occurrence = current
            .and_then(|c| occurrences.iter().position(|o| o.id == c.occurrence))
            .or((!occurrences.is_empty()).then_some(0));
        let rows = current.map_or_else(|| default_rows(frame.height()), |c| c.rows);
        let mut setup = Self {
            occurrences,
            occurrence,
            rows: rows.to_string(),
            frame,
            kept: None,
            adding: None,
        };
        setup.kept = current.map(|current| {
            (
                current.occurrence,
                current.fields.iter().map(|(id, _)| *id).collect(),
            )
        });
        setup
    }

    fn rows(&self) -> Option<u32> {
        self.rows
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=om_data::layout_placement::MAX_PORTAL_ROWS).contains(n))
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> PortalSetupAnswer {
        if self.adding.is_some() {
            self.show_add_fields(ctx)
        } else {
            self.show_setup(ctx)
        }
    }

    fn show_setup(&mut self, ctx: &egui::Context) -> PortalSetupAnswer {
        let mut answer = PortalSetupAnswer::Open;
        let mut next = false;
        let rows = self.rows();
        // Rows that leave no room for a field are refused here, where they
        // can be changed: Add Fields to Portal cannot.
        let room = rows.map(|rows| {
            portal_field_frames(&PortalPlacement {
                occurrence: 0,
                frame: self.frame.clone(),
                rows,
                fields: vec![0],
            })
        });
        let ready = self.occurrence.is_some() && matches!(room, Some(Ok(_)));
        let escaped = dialog(
            ctx,
            "portal_setup",
            "Portal Setup",
            vec2(473.0, 371.0),
            |ui, at| {
                let place = |ui: &mut egui::Ui, rect: Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.scope_builder(UiBuilder::new().max_rect(rect), add);
                };
                place(ui, at(20.0, 21.0, 125.0, 17.0), &mut |ui| {
                    ui.label("Show records from:");
                });
                place(ui, at(145.0, 19.0, 257.0, 22.0), &mut |ui| {
                    let selected = self
                        .occurrence
                        .and_then(|i| self.occurrences.get(i))
                        .map_or("", |o| o.name.as_str())
                        .to_owned();
                    let combo = egui::ComboBox::from_id_salt("portal_setup_occurrence")
                        .width(251.0)
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (index, occurrence) in self.occurrences.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.occurrence,
                                    Some(index),
                                    occurrence.name.as_str(),
                                );
                            }
                        });
                    widgets::accessible_label(ui, combo.response.id, "Show records from");
                    if self.occurrences.is_empty() {
                        combo.response.on_hover_text(
                            "No table occurrence is related to the layout's in the \
                             relationship graph.",
                        );
                    }
                });
                // The options, as Pro sets them for a new portal (flags
                // 0x10: no scrolling, deletion, sorting or filtering).
                for (label, left, top, width) in [
                    ("Sort portal records", 107.0, 54.0, 155.0),
                    ("Filter portal records", 107.0, 81.0, 155.0),
                    ("Allow deletion of portal records", 107.0, 106.0, 240.0),
                    ("Allow vertical scrolling", 107.0, 130.0, 181.0),
                    ("Reset scroll bar when exiting record", 124.0, 178.0, 268.0),
                    ("Use alternate row state", 107.0, 269.0, 181.0),
                    ("Use active row state", 107.0, 289.0, 181.0),
                ] {
                    place(ui, at(left, top, width, 18.0), &mut |ui| {
                        ui.add_enabled(false, egui::Checkbox::new(&mut false, label))
                            .on_disabled_hover_text(NOT_YET);
                    });
                }
                for top in [49.0, 76.0] {
                    place(ui, at(268.0, top + 5.0, 86.0, 22.0), &mut |ui| {
                        ui.add_enabled_ui(false, |ui| {
                            ui.add_sized(vec2(86.0, 22.0), egui::Button::new("Specify…"))
                        })
                        .inner
                        .on_disabled_hover_text(NOT_YET);
                    });
                }
                place(ui, at(125.0, 154.0, 104.0, 17.0), &mut |ui| {
                    ui.add_enabled(false, egui::Label::new("Show scroll bar:"));
                });
                place(ui, at(229.0, 152.0, 153.0, 22.0), &mut |ui| {
                    ui.add_enabled_ui(false, |ui| {
                        egui::ComboBox::from_id_salt("portal_setup_scroll_bar")
                            .width(147.0)
                            .selected_text("Always")
                            .show_ui(ui, |_| {});
                    });
                });
                place(ui, at(20.0, 210.0, 52.0, 17.0), &mut |ui| {
                    ui.label("Format");
                });
                let line = at(73.0, 219.0, 376.0, 1.0);
                ui.painter().hline(
                    line.x_range(),
                    line.top(),
                    Stroke::new(1.0, theme::HAIRLINE),
                );
                place(ui, at(101.0, 242.0, 72.0, 17.0), &mut |ui| {
                    ui.label("Initial row:");
                });
                place(ui, at(180.0, 240.0, 35.0, 22.0), &mut |ui| {
                    let field = ui.add_enabled(
                        false,
                        egui::TextEdit::singleline(&mut "1".to_owned()).desired_width(35.0),
                    );
                    widgets::accessible_label(ui, field.id, "Initial row");
                });
                place(ui, at(229.0, 242.0, 110.0, 17.0), &mut |ui| {
                    ui.label("Number of rows:");
                });
                place(ui, at(345.0, 240.0, 35.0, 22.0), &mut |ui| {
                    let field =
                        ui.add(egui::TextEdit::singleline(&mut self.rows).desired_width(35.0));
                    widgets::accessible_label(ui, field.id, "Number of rows");
                });
                let right = egui::Layout::right_to_left(egui::Align::Center);
                place(ui, at(380.0, 331.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        let ok = widgets::dialog_button(ui, "OK", true, ready);
                        if !ready {
                            ok.on_disabled_hover_text(match &room {
                                Some(Err(refusal)) => refusal.to_string(),
                                _ => format!(
                                    "Choose the related records to show, and from 1 to {} rows.",
                                    om_data::layout_placement::MAX_PORTAL_ROWS
                                ),
                            });
                        } else if ok.clicked() {
                            next = true;
                        }
                    });
                });
                place(ui, at(301.0, 331.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                            answer = PortalSetupAnswer::Cancel;
                        }
                    });
                });
            },
        );
        if next {
            // The fields kept, by id, only for the occurrence they are of.
            let chosen = self.occurrence.and_then(|i| self.occurrences.get(i));
            let included = match (self.kept.take(), chosen) {
                (Some((kept, ids)), Some(chosen)) if kept == chosen.id => ids
                    .iter()
                    .filter_map(|id| chosen.fields.iter().position(|f| f.id == *id))
                    .collect(),
                _ => Vec::new(),
            };
            self.adding = Some(AddFields {
                included,
                available_row: None,
                included_row: None,
                problem: None,
            });
        } else if escaped && answer == PortalSetupAnswer::Open {
            answer = PortalSetupAnswer::Cancel;
        }
        answer
    }

    fn show_add_fields(&mut self, ctx: &egui::Context) -> PortalSetupAnswer {
        let mut answer = PortalSetupAnswer::Open;
        let Some(occurrence) = self
            .occurrence
            .and_then(|i| self.occurrences.get(i))
            .cloned()
        else {
            self.adding = None;
            return answer;
        };
        let rows = self.rows().unwrap_or(1);
        let frame = self.frame.clone();
        let Some(adding) = self.adding.as_mut() else {
            return answer;
        };
        let fields = &occurrence.fields;
        let mut done = false;
        let escaped = dialog(
            ctx,
            "portal_add_fields",
            "Add Fields to Portal",
            vec2(507.0, 286.0),
            |ui, at| {
                let place = |ui: &mut egui::Ui, rect: Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.scope_builder(UiBuilder::new().max_rect(rect), add);
                };
                place(ui, at(20.0, 20.0, 120.0, 17.0), &mut |ui| {
                    ui.label("Available fields");
                });
                place(ui, at(18.0, 38.0, 179.0, 22.0), &mut |ui| {
                    ui.add_enabled_ui(false, |ui| {
                        egui::ComboBox::from_id_salt("portal_add_fields_occurrence")
                            .width(173.0)
                            .selected_text(occurrence.name.as_str())
                            .show_ui(ui, |_| {});
                    });
                });
                place(ui, at(312.0, 43.0, 120.0, 17.0), &mut |ui| {
                    ui.label("Included fields");
                });
                let mut moved = None;
                let mut cleared = None;
                let before = (adding.available_row, adding.included_row);
                list(
                    ui,
                    at(20.0, 67.0, 175.0, 164.0),
                    "Available fields",
                    fields.iter().enumerate().filter_map(|(index, field)| {
                        (!adding.included.contains(&index)).then_some((
                            index,
                            field.name.as_str(),
                            field.placeable,
                        ))
                    }),
                    &mut adding.available_row,
                    &mut moved,
                );
                list(
                    ui,
                    at(312.0, 67.0, 175.0, 164.0),
                    "Included fields",
                    adding
                        .included
                        .iter()
                        .filter_map(|&index| Some((index, fields.get(index)?.name.as_str(), true))),
                    &mut adding.included_row,
                    &mut cleared,
                );
                // One field chosen at a time, in either list.
                if adding.available_row != before.0 && adding.available_row.is_some() {
                    adding.included_row = None;
                } else if adding.included_row != before.1 && adding.included_row.is_some() {
                    adding.available_row = None;
                }
                // Move, or Clear while an included field is chosen.
                let clearing = adding.included_row.is_some();
                let full = adding.included.len() >= MAX_PORTAL_FIELDS;
                // Pro's push buttons: 101 x 32 frames, bezels included.
                let size = vec2(89.0, 22.0);
                place(ui, at(209.0, 107.0, 89.0, 22.0), &mut |ui| {
                    let (label, enabled) = if clearing {
                        ("Clear", true)
                    } else {
                        (
                            "Move",
                            !full
                                && adding.available_row.is_some_and(|index| {
                                    fields.get(index).is_some_and(|f| f.placeable)
                                }),
                        )
                    };
                    let button = ui
                        .add_enabled_ui(enabled, |ui| ui.add_sized(size, egui::Button::new(label)));
                    if button.inner.clicked() {
                        if clearing {
                            cleared = adding.included_row;
                        } else {
                            moved = adding.available_row;
                        }
                    }
                });
                place(ui, at(209.0, 139.0, 89.0, 22.0), &mut |ui| {
                    if ui.add_sized(size, egui::Button::new("Move All")).clicked() {
                        for (index, field) in fields.iter().enumerate() {
                            if field.placeable
                                && !adding.included.contains(&index)
                                && adding.included.len() < MAX_PORTAL_FIELDS
                            {
                                adding.included.push(index);
                            }
                        }
                        adding.available_row = None;
                    }
                });
                place(ui, at(209.0, 171.0, 89.0, 22.0), &mut |ui| {
                    let any = !adding.included.is_empty();
                    let clear = ui.add_enabled_ui(any, |ui| {
                        ui.add_sized(size, egui::Button::new("Clear All"))
                    });
                    if clear.inner.clicked() {
                        adding.included.clear();
                        adding.included_row = None;
                    }
                });
                if let Some(index) = moved {
                    if fields.get(index).is_some_and(|f| f.placeable)
                        && !full
                        && !adding.included.contains(&index)
                    {
                        adding.included.push(index);
                    }
                    adding.available_row = None;
                }
                if let Some(index) = cleared {
                    adding.included.retain(|&i| i != index);
                    adding.included_row = None;
                }
                if let Some(problem) = &adding.problem {
                    place(ui, at(20.0, 237.0, 467.0, 17.0), &mut |ui| {
                        ui.colored_label(ui.visuals().error_fg_color, problem);
                    });
                }
                let ready = !adding.included.is_empty();
                let right = egui::Layout::right_to_left(egui::Align::Center);
                place(ui, at(418.0, 251.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        let ok = widgets::dialog_button(ui, "OK", true, ready);
                        if !ready {
                            ok.on_disabled_hover_text("Include at least one field.");
                        } else if ok.clicked() {
                            done = true;
                        }
                    });
                });
                place(ui, at(337.0, 251.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                            answer = PortalSetupAnswer::Cancel;
                        }
                    });
                });
            },
        );
        if done {
            let chosen: Vec<&PortalField> = adding
                .included
                .iter()
                .filter_map(|&i| fields.get(i))
                .collect();
            let ids: Vec<u64> = chosen.iter().map(|f| f.id).collect();
            // The fields must fit the portal's rows and width as operation 21
            // places them.
            let fits = portal_field_frames(&PortalPlacement {
                occurrence: occurrence.id,
                frame,
                rows,
                fields: ids.clone(),
            });
            match fits {
                Err(refusal) => adding.problem = Some(refusal.to_string()),
                Ok(_) => {
                    answer = PortalSetupAnswer::Done(PortalChoice {
                        occurrence: occurrence.id,
                        table: occurrence.table,
                        name: occurrence.name.clone(),
                        rows,
                        fields: chosen.iter().map(|f| (f.id, f.name.clone())).collect(),
                    });
                }
            }
        } else if escaped && answer == PortalSetupAnswer::Open {
            answer = PortalSetupAnswer::Cancel;
        }
        answer
    }
}

/// A one-column list in a white box at `rect`, accessible as `label`: each
/// row's index, name and whether it can be chosen. A click chooses a row; a
/// double click also reports it in `activated`.
fn list<'a>(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    rows: impl Iterator<Item = (usize, &'a str, bool)>,
    chosen: &mut Option<usize>,
    activated: &mut Option<usize>,
) {
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::CONTROL_BG);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );
    ui.scope_builder(UiBuilder::new().max_rect(rect.shrink(2.0)), |ui| {
        egui::ScrollArea::vertical()
            .id_salt(label)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let column = egui::Layout::top_down_justified(egui::Align::LEFT);
                ui.with_layout(column, |ui| {
                    for (index, name, enabled) in rows {
                        let on = *chosen == Some(index);
                        let row = ui.add_enabled(enabled, egui::Button::selectable(on, name));
                        if row.clicked() {
                            *chosen = Some(index);
                        }
                        if row.double_clicked() {
                            *activated = Some(index);
                        }
                    }
                });
            });
    });
    let id = ui.scope_id().with(label);
    let response = ui.interact(rect, id, egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::List, true, label));
}
