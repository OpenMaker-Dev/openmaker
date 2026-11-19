//! Layout mode's object list, populated from the selected layout's geometry.
//!
//! FileMaker's LayoutLeftPanel exposes Fields, Objects and Add-ons tabs.
//! Fields lists the current layout's own table; Add-ons stays disabled.

use egui::{vec2, Align2, Color32, CornerRadius, RichText, Role, Sense, Stroke, Ui, WidgetInfo};
use om_data::ObjectGeom;

use crate::icons::{self, Icon};
use crate::session::{Geometry, Session};
use crate::theme;
use crate::widgets;

pub(crate) const WIDTH: f32 = 220.0;

#[derive(Default)]
pub(crate) struct ObjectsPanel {
    search: String,
    field_search: String,
    fields_tab: bool,
}

impl ObjectsPanel {
    /// Return the actual geometry index of a newly clicked object. Rows are
    /// front-to-back, reversing the provider's back-to-front stacking order.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        session: &Session,
        canvas: &crate::layout_canvas::CanvasState,
        fields: FieldList<'_>,
    ) -> Option<usize> {
        let deleted = &canvas.deleted().keys;
        let drafts = canvas.fields();
        let full = ui.max_rect();
        ui.painter()
            .rect_filled(full, CornerRadius::ZERO, theme::INSPECTOR_BG);
        ui.painter().vline(
            full.right() - 0.5,
            full.y_range(),
            Stroke::new(1.0, theme::HAIRLINE),
        );
        let mut clicked = None;
        egui::Frame::new()
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(if self.fields_tab {
                        "Layout Fields"
                    } else {
                        "Layout Objects"
                    })
                    .font(egui::FontId::new(theme::BODY, theme::semibold())),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if ui
                        .add(
                            egui::Button::new("Fields")
                                .selected(self.fields_tab)
                                .frame(false),
                        )
                        .clicked()
                    {
                        self.fields_tab = true;
                    }
                    if ui
                        .add(
                            egui::Button::new("Objects")
                                .selected(!self.fields_tab)
                                .frame(false),
                        )
                        .clicked()
                    {
                        self.fields_tab = false;
                    }
                    ui.add_enabled(false, egui::Button::new("Add-ons").frame(false));
                });
                ui.add_space(5.0);
                if self.fields_tab {
                    self.show_fields(ui, session, fields);
                    return;
                }
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search Objects")
                        .desired_width(ui.available_width()),
                );
                widgets::accessible_label(ui, search.id, "Search Objects");
                ui.add_space(5.0);
                ui.separator();
                ui.add_space(3.0);
                match &session.geometry {
                    Geometry::Loading => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Loading objects...").color(theme::WEAK_TEXT));
                        });
                    }
                    Geometry::Unavailable => {
                        ui.label(
                            RichText::new("Object information is unavailable for this layout.")
                                .color(theme::WEAK_TEXT),
                        );
                    }
                    Geometry::Loaded(geometry) => {
                        // Draft indices follow stored indices; they are never catalog keys.
                        let mut objects = geometry.objects.clone();
                        objects.extend(drafts.iter().map(|field| ObjectGeom {
                            kind_name: "Field".into(),
                            field_name: Some(format!(
                                    "{} (unsaved)",
                                    field
                                        .field
                                        .as_ref()
                                        .map_or("Unbound field", |f| f.name.as_str())
                                )),
                            ..Default::default()
                        }));
                        let query = self.search.trim().to_lowercase();
                        // Objects deleted and not saved yet are not listed.
                        let listed =
                            |object: &ObjectGeom| object.key == 0 || !deleted.contains(&object.key);
                        let total = objects.iter().filter(|o| listed(o)).count();
                        let shown: Vec<_> = objects
                            .iter()
                            .enumerate()
                            .rev()
                            .filter(|(_, object)| listed(object))
                            .map(|(index, object)| (index, object, object_label(&objects, index)))
                            .filter(|(_, _, label)| {
                                query.is_empty() || label.to_lowercase().contains(&query)
                            })
                            .collect();
                        let list_height = (ui.available_height() - 26.0).max(40.0);
                        egui::ScrollArea::vertical()
                            .id_salt("layout_objects_list")
                            .max_height(list_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if shown.is_empty() {
                                    ui.label(
                                        RichText::new(if total == 0 {
                                            "This layout has no objects."
                                        } else {
                                            "No objects match the search."
                                        })
                                        .color(theme::WEAK_TEXT),
                                    );
                                }
                                ui.spacing_mut().item_spacing.y = 1.0;
                                for (index, object, label) in &shown {
                                    let response =
                                        object_row(ui, object, label, canvas.is_selected(*index));
                                    if response.clicked() {
                                        clicked = Some(*index);
                                    }
                                }
                            });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(if query.is_empty() {
                                format!("{total} {}", if total == 1 { "object" } else { "objects" })
                            } else {
                                format!("{} of {total} objects", shown.len())
                            })
                            .font(theme::small())
                            .color(theme::CAPTION),
                        );
                    }
                }
            });
        clicked
    }
}

/// A field drag is tied to a document, layout revision and actual base table.
#[derive(Clone)]
pub(crate) struct FieldDrag {
    target: crate::session::Target,
    layout: u32,
    revision: om_data::layout_placement::Revision,
    pub field: crate::layout_canvas::FieldChoice,
}

impl FieldDrag {
    pub(crate) fn matches(
        &self,
        session: &Session,
        fields: &[crate::layout_canvas::FieldChoice],
    ) -> bool {
        self.target.host == session.target.host
            && self.target.file == session.target.file
            && self.target.local == session.target.local
            && session.layout == Some(self.layout)
            && matches!(&session.geometry, Geometry::Loaded(g) if g.revision == Some(self.revision))
            && fields.contains(&self.field)
    }
}

pub(crate) struct FieldList<'a> {
    pub choices: &'a [crate::layout_canvas::FieldChoice],
    pub table: Option<&'a str>,
    pub frozen: bool,
}

impl ObjectsPanel {
    fn show_fields(&mut self, ui: &mut Ui, session: &Session, fields: FieldList<'_>) {
        ui.label(format!("Table: {}", fields.table.unwrap_or("Unavailable")));
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.field_search)
                .hint_text("Search Fields")
                .desired_width(ui.available_width()),
        );
        widgets::accessible_label(ui, search.id, "Search Fields");
        ui.separator();
        let revision = match &session.geometry {
            Geometry::Loaded(g) => g.revision,
            _ => None,
        };
        let query = self.field_search.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("layout_fields_list")
            .max_height((ui.available_height() - 60.0).max(40.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let shown: Vec<_> = fields
                    .choices
                    .iter()
                    .filter(|f| query.is_empty() || f.name.to_lowercase().contains(&query))
                    .collect();
                if shown.is_empty() {
                    ui.label(if fields.table.is_none() {
                        "The layout's table is unavailable."
                    } else if fields.choices.is_empty() {
                        "This table has no placeable fields."
                    } else {
                        "No fields match the search."
                    });
                }
                for field in shown {
                    let enabled = !fields.frozen && revision.is_some();
                    let response = ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(&field.name)
                                .frame(false)
                                .sense(Sense::drag())
                                .min_size(vec2(ui.available_width(), 27.0)),
                        )
                        .on_hover_cursor(egui::CursorIcon::Grab);
                    if let (Some(layout), Some(revision)) = (session.layout, revision) {
                        response.dnd_set_drag_payload(FieldDrag {
                            target: session.target.clone(),
                            layout,
                            revision,
                            field: field.clone(),
                        });
                    }
                }
            });
        ui.label(
            RichText::new("Drag onto the layout. Save Layout keeps the additions.")
                .font(theme::small())
                .color(theme::WEAK_TEXT),
        );
        ui.label(
            RichText::new(
                "Own-table edit boxes only; containers and related fields are not listed.",
            )
            .font(theme::small())
            .color(theme::WEAK_TEXT),
        );
    }
}

fn object_label(objects: &[ObjectGeom], index: usize) -> String {
    let object = &objects[index];
    let Some(field) = object.field_name.as_deref().filter(|name| !name.is_empty()) else {
        return format!("{} {}", object.kind_name, index + 1);
    };
    let label = format!("{} - {field}", object.kind_name);
    if objects
        .iter()
        .filter(|other| {
            other.kind_name == object.kind_name && other.field_name.as_deref() == Some(field)
        })
        .count()
        > 1
    {
        format!("{label} {}", index + 1)
    } else {
        label
    }
}

fn object_row(ui: &mut Ui, object: &ObjectGeom, label: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 27.0), Sense::click());
    let fill = if selected {
        theme::DEFAULT_BUTTON
    } else if response.hovered() {
        theme::CURRENT_ROW
    } else {
        theme::INSPECTOR_BG
    };
    let color = if selected {
        Color32::WHITE
    } else {
        theme::TEXT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    let icon = match object.kind_name.as_str() {
        "Field" => Icon::FieldTool,
        "Text" => Icon::TextTool,
        "Button" | "Button Bar" | "Popover Button" => Icon::ButtonTool,
        "Portal" => Icon::Portal,
        "Tab Control" | "Tab Panel" | "Slide Control" | "Slide Panel" | "Popover" => {
            Icon::PanelTool
        }
        "Web Viewer" => Icon::WebViewer,
        "Chart" => Icon::Chart,
        "Rectangle" => Icon::Rectangle,
        "Rounded Rectangle" => Icon::RoundedRectangle,
        "Oval" => Icon::Oval,
        _ => Icon::ShapeTool,
    };
    icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(rect.left_center() + vec2(13.0, 0.0), vec2(15.0, 15.0)),
        icon,
        color,
        fill,
    );
    let text_rect = egui::Rect::from_min_max(rect.min + vec2(27.0, 0.0), rect.max - vec2(5.0, 0.0));
    ui.painter()
        .with_clip_rect(ui.clip_rect().intersect(text_rect))
        .text(
            text_rect.left_center(),
            Align2::LEFT_CENTER,
            label,
            theme::small(),
            color,
        );
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, label));
    response.on_hover_text(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_labels_distinguish_duplicate_fields_and_unnamed_objects() {
        let field = ObjectGeom {
            kind_name: "Field".into(),
            rect: Default::default(),
            field_name: Some("Name".into()),
            ..Default::default()
        };
        let unnamed = ObjectGeom {
            field_name: None,
            ..field.clone()
        };
        assert_eq!(
            object_label(std::slice::from_ref(&field), 0),
            "Field - Name"
        );
        let objects = [field.clone(), field, unnamed];
        assert_eq!(object_label(&objects, 0), "Field - Name 1");
        assert_eq!(object_label(&objects, 1), "Field - Name 2");
        assert_eq!(object_label(&objects, 2), "Field 3");
    }
}
