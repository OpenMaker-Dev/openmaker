//! The fields pane of Specify Calculation (42): the table popup (the
//! current table, then the related and the unrelated table occurrences),
//! a search field, the sort menu and the table's fields with their type
//! icons. A double click inserts the field's reference at the cursor.

use egui::epaint::{PathShape, PathStroke};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    pos2, vec2, Align2, Color32, FontId, Painter, Rect, Role, Sense, Stroke, Ui, WidgetInfo,
};
use om_data::calc::ValueType;
use om_data::host::{CalcSchema, FieldClass, FieldDef};

use super::syntax::Insertion;
use super::{bar_chart, popup_button, LIST_FONT};
use crate::{theme, widgets};

/// Row height of the list (42: 24 pt apart).
const ROW: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FieldSort {
    Creation,
    Name,
    Type,
}

pub(super) struct FieldsPane {
    /// The occurrence listed; `None` for the current table.
    pub occurrence: Option<u64>,
    pub search: String,
    pub sort: FieldSort,
    pub selected: Option<u64>,
}

impl Default for FieldsPane {
    fn default() -> Self {
        Self {
            occurrence: None,
            search: String::new(),
            sort: FieldSort::Creation,
            selected: None,
        }
    }
}

/// How a field is written in a formula (`Table::Field` when qualified,
/// `${...}` where needed): the same text name completion inserts.
pub(super) use om_data::formula_editor::field_reference as reference;

/// FileMaker's field type order (Manage Database): the data types, then
/// calculations and summaries.
fn type_rank(field: &FieldDef) -> u8 {
    match (field.class, field.value_type) {
        (FieldClass::Calculation, _) => 6,
        (FieldClass::Summary, _) => 7,
        (_, ValueType::Text) => 0,
        (_, ValueType::Number) => 1,
        (_, ValueType::Date) => 2,
        (_, ValueType::Time) => 3,
        (_, ValueType::Timestamp) => 4,
        (_, ValueType::Container) => 5,
    }
}

impl FieldsPane {
    /// Draw the pane in `rect`. `current` is the context occurrence;
    /// fields of the current table are inserted without their table when
    /// `unqualified_current` (field definitions), otherwise every field is
    /// qualified, as FileMaker writes fields in script steps.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        schema: Option<&CalcSchema>,
        current: Option<u64>,
        unqualified_current: bool,
    ) -> Option<Insertion> {
        let mut insert = None;
        let listed = self.occurrence.or(current);
        let name_of = |o: u64| {
            schema
                .and_then(|s| s.occurrence(o))
                .map_or_else(String::new, |o| o.name.clone())
        };
        let current_label = current.map(|c| format!("Current Table (\"{}\")", name_of(c)));
        let label = match listed {
            Some(o) if Some(o) == current => current_label.clone().unwrap_or_default(),
            Some(o) => name_of(o),
            None => String::new(),
        };
        // 42: the popup 10 pt in and 5 down, the search field under it.
        let popup = Rect::from_min_size(rect.min + vec2(10.0, 5.0), vec2(202.0, 18.0));
        let response = popup_button(ui, popup, "Table", &label, schema.is_some());
        if let Some(schema) = schema {
            egui::Popup::menu(&response)
                .style(theme::menu_popup_style)
                .show(|ui| {
                    if let Some(label) = &current_label {
                        if widgets::menu_item(ui, label, None, listed == current, true).clicked() {
                            self.occurrence = None;
                            self.selected = None;
                        }
                    }
                    let others: Vec<(u64, String, bool)> = schema
                        .occurrences_in_order()
                        .into_iter()
                        .filter(|o| Some(o.id) != current)
                        .map(|o| {
                            let related = current.is_some_and(|c| schema.path(c, o.id).is_some());
                            (o.id, o.name.clone(), related)
                        })
                        .collect();
                    for (heading, related) in
                        [("Related Tables", true), ("Unrelated Tables", false)]
                    {
                        let group: Vec<&(u64, String, bool)> =
                            others.iter().filter(|o| o.2 == related).collect();
                        if group.is_empty() {
                            continue;
                        }
                        widgets::menu_separator(ui);
                        widgets::menu_item(ui, heading, None, false, false);
                        for (id, name, _) in group {
                            let on = listed == Some(*id);
                            if widgets::menu_item(ui, name, None, on, true).clicked() {
                                self.occurrence = Some(*id);
                                self.selected = None;
                            }
                        }
                    }
                });
        }
        let search = Rect::from_min_max(
            pos2(rect.left() + 10.0, popup.bottom() + 8.0),
            pos2(rect.right() - 28.0, popup.bottom() + 26.0),
        );
        super::search_box(ui, search, &mut self.search, "Search Fields");
        let sort_button = Rect::from_center_size(
            pos2(rect.right() - 16.5, search.center().y + 1.5),
            vec2(18.0, 18.0),
        );
        self.sort_menu(ui, sort_button);
        let (schema, occurrence) = schema.zip(listed)?;
        let query = self.search.trim().to_lowercase();
        let mut fields: Vec<&FieldDef> = schema
            .table_of(occurrence)
            .into_iter()
            .flat_map(|t| t.fields.values())
            .filter(|f| query.is_empty() || f.name.to_lowercase().contains(&query))
            .collect();
        match self.sort {
            FieldSort::Creation => {}
            FieldSort::Name => fields.sort_by_cached_key(|f| f.name.to_lowercase()),
            FieldSort::Type => fields.sort_by_key(|f| type_rank(f)),
        }
        let list = Rect::from_min_max(pos2(rect.left(), search.bottom() + 15.0), rect.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list));
        child.set_clip_rect(list.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
            .id_salt("calculation_fields")
            .auto_shrink([false, false])
            .show_rows(&mut child, ROW, fields.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for field in &fields[range] {
                    let (row, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                    let selected = self.selected == Some(field.id);
                    if selected {
                        ui.painter().rect_filled(
                            Rect::from_min_max(
                                pos2(row.left() + 6.0, row.top() + 1.0),
                                pos2(row.right() - 6.0, row.bottom() - 1.0),
                            ),
                            4.0,
                            theme::SELECTED,
                        );
                    }
                    let (text, icon) = if selected {
                        (Color32::WHITE, Color32::WHITE)
                    } else {
                        (theme::TEXT, FIELD_ICON)
                    };
                    type_icon(
                        ui.painter(),
                        Rect::from_center_size(
                            pos2(row.left() + 22.0, row.center().y),
                            vec2(16.0, 16.0),
                        ),
                        field,
                        icon,
                    );
                    ui.painter().text(
                        pos2(row.left() + 34.0, row.center().y),
                        Align2::LEFT_CENTER,
                        &field.name,
                        FontId::proportional(LIST_FONT),
                        text,
                    );
                    response.widget_info(|| {
                        WidgetInfo::selected(Role::ListItem, true, selected, &field.name)
                    });
                    if response.clicked() || response.double_clicked() {
                        self.selected = Some(field.id);
                    }
                    if response.double_clicked() {
                        let qualified = !(unqualified_current && Some(occurrence) == current);
                        insert = Some(Insertion::caret_after(reference(
                            schema, occurrence, field.id, qualified,
                        )));
                    }
                }
            });
        insert
    }

    /// The bars button: the list's order.
    fn sort_menu(&mut self, ui: &mut Ui, rect: Rect) {
        let response = ui.interact(rect, ui.scope_id().with("field_sort"), Sense::click());
        bar_chart(ui.painter(), rect, super::BARS);
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Field Order"));
        egui::Popup::menu(&response)
            .style(theme::menu_popup_style)
            .show(|ui| {
                for (label, sort) in [
                    ("Creation Order", FieldSort::Creation),
                    ("Field Name", FieldSort::Name),
                    ("Field Type", FieldSort::Type),
                ] {
                    if widgets::menu_item(ui, label, None, self.sort == sort, true).clicked() {
                        self.sort = sort;
                    }
                }
            });
    }
}

/// The gray of the type icons (42).
const FIELD_ICON: Color32 = Color32::from_rgb(0x70, 0x70, 0x70);

/// A field's type icon (42: `Tt` text, a picture for containers, `fx` for
/// calculations, a clock over a calendar for timestamps). OpenMaker's own
/// drawings on a 16 pt square, lines on pixel centers.
fn type_icon(painter: &Painter, rect: Rect, field: &FieldDef, color: Color32) {
    let o = rect.min;
    let p = |x: f32, y: f32| o + vec2(x, y);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(p(x0, y0), p(x1, y1));
    let stroke = Stroke::new(1.0, color);
    let letters = |text: &str, size: f32, italics: bool, bold: bool| {
        let mut job = LayoutJob::default();
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: if bold {
                    FontId::new(size, theme::semibold())
                } else {
                    FontId::proportional(size)
                },
                color,
                italics,
                extra_letter_spacing: -0.5,
                ..TextFormat::default()
            },
        );
        let galley = painter.layout_job(job);
        painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    };
    let dots = |xs: &[f32], ys: &[f32], skip: &dyn Fn(f32, f32) -> bool| {
        for &y in ys {
            for &x in xs {
                if !skip(x, y) {
                    painter.rect_filled(r(x, y, x + 1.0, y + 1.0), 0.0, color);
                }
            }
        }
    };
    let clock = |cx: f32, cy: f32, radius: f32, fill: Color32| {
        painter.circle(p(cx, cy), radius, fill, stroke);
        painter.add(PathShape::line(
            vec![
                p(cx, cy - radius * 0.65),
                p(cx, cy),
                p(cx + radius * 0.55, cy),
            ],
            PathStroke::new(1.0, color),
        ));
    };
    match (field.class, field.value_type) {
        (FieldClass::Calculation, _) => letters("fx", 16.0, true, false),
        (FieldClass::Summary, _) => letters("\u{3a3}", 15.0, false, true),
        (_, ValueType::Text) => letters("Tt", 16.0, false, true),
        (_, ValueType::Number) => letters("#", 15.0, false, true),
        (_, ValueType::Date) => {
            painter.rect_stroke(
                r(1.5, 2.5, 14.5, 14.5),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.rect_filled(r(1.0, 2.0, 15.0, 5.0), 0.0, color);
            dots(&[4.0, 6.5, 9.0, 11.5], &[7.0, 9.5, 12.0], &|_, _| false);
        }
        (_, ValueType::Time) => clock(8.0, 8.0, 6.0, Color32::TRANSPARENT),
        (_, ValueType::Timestamp) => {
            painter.rect_stroke(
                r(1.5, 4.5, 15.5, 15.5),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.rect_filled(r(1.0, 4.0, 16.0, 7.0), 0.0, color);
            dots(&[6.0, 8.5, 11.0, 13.5], &[9.0, 11.5, 14.0], &|x, y| {
                x < 8.0 && y < 10.0
            });
            clock(4.5, 4.5, 4.0, theme::DIALOG_BG);
        }
        (_, ValueType::Container) => {
            painter.rect_stroke(
                r(0.5, 2.5, 15.5, 13.5),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.rect_stroke(r(2.5, 4.5, 4.5, 6.5), 0.0, stroke, egui::StrokeKind::Middle);
            // A ridge rising to the right, in convex pieces.
            let ridge = [
                (2.0, 12.0),
                (5.0, 10.0),
                (6.5, 11.0),
                (10.0, 8.5),
                (12.0, 9.5),
                (14.0, 7.5),
            ];
            for pair in ridge.windows(2) {
                let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
                painter.add(PathShape::convex_polygon(
                    vec![p(x0, 12.0), p(x0, y0), p(x1, y1), p(x1, 12.0)],
                    color,
                    Stroke::NONE,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::calc::Locale;

    #[test]
    fn references_are_qualified_and_quoted_as_filemaker_prints_them() {
        let mut schema = CalcSchema::new("f", Locale::en_us());
        schema.add_table(0x8002, "Contacts");
        schema.add_field(0x8002, FieldDef::data(1, "First Name", ValueType::Text));
        schema.add_field(0x8002, FieldDef::data(2, "a+b", ValueType::Text));
        schema.add_occurrence(0xd00002, "Contacts", 0x8002);
        schema.add_occurrence(0xd00003, "Contacts 2", 0x8002);
        assert_eq!(
            reference(&schema, 0xd00002, 1, true),
            "Contacts::First Name"
        );
        assert_eq!(reference(&schema, 0xd00002, 1, false), "First Name");
        assert_eq!(reference(&schema, 0xd00003, 2, true), "Contacts 2::${a+b}");
    }
}
