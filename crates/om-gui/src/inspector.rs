//! FileMaker's four-tab Inspector, populated only with known object metadata.
//! A movable object's frame and a field object's control style and value
//! list are set here as unsaved changes; a field placed with the Field tool,
//! or a button drawn with the Button tool, shows its own section
//! ([`Inspector::new_field`], [`Inspector::new_button`]) until Save Layout
//! stores it.

use egui::{vec2, Align, CornerRadius, RichText, Role, Sense, Stroke, Ui, WidgetInfo};
use om_data::layout_objects::FIELD_CONTROLS;
use om_data::ObjectGeom;

use crate::icons::{self, Icon};
use crate::layout_canvas::{FieldChoice, NewButton, NewField, NewText};
use crate::session::Session;
use crate::{theme, widgets};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Position,
    Styles,
    Appearance,
    Data,
}

impl Tab {
    const ALL: [Self; 4] = [Self::Position, Self::Styles, Self::Appearance, Self::Data];

    fn label(self) -> &'static str {
        match self {
            Self::Position => "Position",
            Self::Styles => "Styles",
            Self::Appearance => "Appearance",
            Self::Data => "Data",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Position => Icon::Position,
            Self::Styles => Icon::Styles,
            Self::Appearance => Icon::Appearance,
            Self::Data => Icon::Data,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Units {
    #[default]
    Points,
    Inches,
    Centimeters,
}

impl Units {
    fn label(self) -> &'static str {
        match self {
            Self::Points => "pt",
            Self::Inches => "in",
            Self::Centimeters => "cm",
        }
    }

    fn display(self, points: f64) -> String {
        match self {
            Self::Points => format!("{points:.2}"),
            Self::Inches => format!("{:.3}", points / 72.0),
            Self::Centimeters => format!("{:.3}", points * 2.54 / 72.0),
        }
    }
}

#[derive(Default)]
pub(crate) struct Inspector {
    tab: Tab,
    units: Units,
}

/// A field object's control as the Data tab shows and sets it.
pub(crate) struct ControlView<'a> {
    /// The control (a display type of [`FIELD_CONTROLS`]) and value list
    /// key the object stores; a new field's are an edit box and none.
    pub stored: (u32, u64),
    /// The unsaved change, if any (`None` list: as stored).
    pub pending: Option<(u32, Option<u64>)>,
    /// Why the control cannot be changed here, if it cannot.
    pub fixed: Option<&'static str>,
    /// A date field, which a drop-down calendar can show.
    pub date: bool,
    /// The file's value lists, by key as layouts store it, in display order.
    pub lists: &'a [(u64, String)],
}

/// What the user set in the Inspector this frame.
#[derive(Default)]
pub(crate) struct InspectorEdit {
    /// The selected object's frame (Position tab).
    pub frame: Option<om_data::GeomRect>,
    /// The selected field object's control and value list (Data tab).
    pub control: Option<(u32, Option<u64>)>,
}

/// Pro's Control style menu ends with Concealed Edit Box (display type 7,
/// layout-model.md 5.4), which OpenMaker does not set.
const CONCEALED: u32 = 7;

/// A control's name, as Pro's Control style menu has it.
fn control_name(control: u32) -> &'static str {
    match control {
        CONCEALED => "Concealed Edit Box",
        _ => FIELD_CONTROLS
            .iter()
            .find(|(display, ..)| *display == control)
            .map_or("Other", |(.., name)| name),
    }
}

/// The Data tab's Control style pop-up and, for a list control, its Values
/// from pop-up (FileMaker Pro's FieldInspector.nib and its options views:
/// re/client-ui.md, "Inspector: a field's Data settings"). Returns the
/// control and list chosen this frame; a new control keeps the list chosen
/// before, if any.
fn control_rows(
    ui: &mut Ui,
    salt: &str,
    view: &ControlView<'_>,
    enabled: bool,
) -> Option<(u32, Option<u64>)> {
    let pending_list = view.pending.and_then(|(_, list)| list);
    let (control, list) = match view.pending {
        Some((control, list)) => (control, list.unwrap_or(view.stored.1)),
        None => view.stored,
    };
    let editable = enabled && view.fixed.is_none();
    let mut chosen = None;
    ui.add_space(6.0);
    ui.label("Control style");
    let combo = ui
        .add_enabled_ui(editable, |ui| {
            egui::ComboBox::from_id_salt((salt, "control_style"))
                .selected_text(control_name(control))
                .width(CONTROL_WIDTH)
                .show_ui(ui, |ui| {
                    for (display, _, name) in FIELD_CONTROLS {
                        let allowed = display != 6 || view.date;
                        let item = ui.add_enabled(
                            allowed,
                            egui::Button::selectable(display == control, name),
                        );
                        if item.clicked() {
                            chosen = Some((display, pending_list));
                        }
                        item.on_disabled_hover_text("A drop-down calendar shows a date field.");
                    }
                    ui.add_enabled(
                        false,
                        egui::Button::selectable(control == CONCEALED, control_name(CONCEALED)),
                    )
                    .on_disabled_hover_text("OpenMaker does not conceal a field yet.");
                })
                .response
        })
        .inner;
    widgets::accessible_label(ui, combo.id, "Control style");
    if let Some(why) = view.fixed {
        combo.on_disabled_hover_text(why);
    }
    if !(1..=4).contains(&control) {
        return chosen;
    }
    ui.add_space(6.0);
    ui.label("Values from:");
    let shown = view
        .lists
        .iter()
        .find(|(key, _)| *key == list && list != 0)
        .map_or("", |(_, name)| name.as_str());
    let combo = ui
        .add_enabled_ui(editable, |ui| {
            egui::ComboBox::from_id_salt((salt, "values_from"))
                .selected_text(shown)
                .width(CONTROL_WIDTH)
                .show_ui(ui, |ui| {
                    for (key, name) in view.lists {
                        if ui
                            .add(egui::Button::selectable(*key == list, name.as_str()))
                            .clicked()
                        {
                            chosen = Some((control, Some(*key)));
                        }
                    }
                })
                .response
        })
        .inner;
    widgets::accessible_label(ui, combo.id, "Values from");
    if list == 0 && editable {
        ui.label(
            RichText::new("Choose the value list the field shows.")
                .font(theme::small())
                .color(theme::WEAK_TEXT),
        );
    }
    chosen
}

/// The Data tab's pop-up menus' width.
const CONTROL_WIDTH: f32 = 180.0;

impl Inspector {
    pub(crate) fn multiple(&mut self, ui: &mut Ui, count: usize, notice: Option<&str>) {
        ui.painter()
            .rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::INSPECTOR_BG);
        ui.painter().vline(
            ui.max_rect().left(),
            ui.max_rect().y_range(),
            Stroke::new(1.0, theme::DIVIDER),
        );
        egui::Frame::new().inner_margin(12).show(ui, |ui| {
            ui.label(RichText::new(format!("{count} objects selected")).strong());
            ui.label("Drag to move the selection. Delete or Clear removes the selection when every object supports deletion.");
            ui.label("Select one object to edit its frame, binding or control in Inspector. Multiple-object Inspector edits are not supported yet.");
            if let Some(notice) = notice { ui.label(notice); }
        });
    }

    /// `frame`: the selected object's frame as shown (its unsaved change,
    /// if any), editable in the Position tab when `editable`. `control`: a
    /// selected field object's control, for the Data tab. Returns what the
    /// user set this frame.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        session: &Session,
        object: Option<&ObjectGeom>,
        frame: Option<om_data::GeomRect>,
        (editable, control): (bool, Option<&ControlView<'_>>),
    ) -> InspectorEdit {
        let mut changed = InspectorEdit::default();
        ui.painter()
            .rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::INSPECTOR_BG);
        ui.painter().vline(
            ui.max_rect().left(),
            ui.max_rect().y_range(),
            Stroke::new(1.0, theme::DIVIDER),
        );
        ui.spacing_mut().item_spacing = vec2(0.0, 4.0);
        ui.horizontal(|ui| {
            let width = ui.available_width() / 4.0;
            for tab in Tab::ALL {
                let (rect, response) = ui.allocate_exact_size(vec2(width, 34.0), Sense::click());
                let selected = self.tab == tab;
                if response.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::ZERO, theme::TOOLBAR_BG);
                }
                icons::paint(
                    ui.painter(),
                    egui::Rect::from_center_size(rect.center(), vec2(18.0, 18.0)),
                    tab.icon(),
                    if selected {
                        theme::SELECTED
                    } else {
                        theme::ICON
                    },
                    theme::INSPECTOR_BG,
                );
                if selected {
                    ui.painter().hline(
                        rect.x_range().shrink(12.0),
                        rect.bottom() - 1.5,
                        Stroke::new(2.0, theme::SELECTED),
                    );
                }
                response
                    .widget_info(|| WidgetInfo::selected(Role::Tab, true, selected, tab.label()));
                if response.on_hover_text(tab.label()).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("inspector_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_min_width((theme::INSPECTOR_WIDTH - 24.0).max(0.0));
                        match self.tab {
                            Tab::Position => {
                                changed.frame = self.position(ui, object, frame, editable)
                            }
                            Tab::Styles => {
                                section(ui, "Styles", |ui| {
                                    ui.label(
                                        RichText::new(object.map_or("No object selected", |o| {
                                            o.kind_name.as_str()
                                        }))
                                        .color(theme::CAPTION),
                                    );
                                    ui.label("Style information is unavailable.");
                                });
                            }
                            Tab::Appearance => {
                                section(ui, "Graphic", |ui| {
                                    ui.label("Appearance information is unavailable.");
                                });
                                section(ui, "Text", |ui| {
                                    ui.label("Text formatting is unavailable.");
                                });
                            }
                            Tab::Data => {
                                section(ui, "Field", |ui| {
                                    if let Some(name) = object.and_then(|o| o.field_name.as_deref())
                                    {
                                        ui.label("Display data from:");
                                        ui.label(RichText::new(name).strong());
                                        if let Some(field) =
                                            session.page.as_ref().and_then(|page| {
                                                page.data
                                                    .fields
                                                    .iter()
                                                    .find(|field| field.name == name)
                                            })
                                        {
                                            ui.label(format!("Type: {:?}", field.kind));
                                        }
                                    } else if control.is_none() {
                                        ui.label("No field selected");
                                    }
                                    // A related field has no name in the
                                    // record page's bindings but still has
                                    // a control.
                                    if let Some(view) = control {
                                        changed.control =
                                            control_rows(ui, "selected_field", view, true);
                                    }
                                });
                                section(ui, "Behavior", |ui| {
                                    ui.label("Behavior information is unavailable.");
                                });
                                section(ui, "Data Formatting", |ui| {
                                    ui.label("Data formatting is unavailable.");
                                });
                            }
                        }
                    });
            });
        changed
    }

    /// The Field tool's new field, before Save Layout: the field it shows,
    /// one of `fields` (the layout's own table's, by id and name), and its
    /// frame in whole points. Only these are set here; Save Layout plans
    /// and stores them as the host does, or refuses.
    /// `frozen`: Save Layout's placement is with the host; the section
    /// shows what was sent and its controls take no input until it answers.
    pub(crate) fn new_field(
        &mut self,
        ui: &mut Ui,
        field: &mut NewField,
        (fields, lists): (&[FieldChoice], &[(u64, String)]),
        frozen: bool,
    ) {
        ui.painter()
            .rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::INSPECTOR_BG);
        ui.painter().vline(
            ui.max_rect().left(),
            ui.max_rect().y_range(),
            Stroke::new(1.0, theme::DIVIDER),
        );
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_min_width((theme::INSPECTOR_WIDTH - 24.0).max(0.0));
                section(ui, "New Field", |ui| {
                    // A disabled Ui takes no pointer, focus or keyboard input.
                    if frozen {
                        ui.disable();
                    }
                    let shown = field
                        .field
                        .as_ref()
                        .map_or("Choose a field", |choice| choice.name.as_str())
                        .to_owned();
                    egui::ComboBox::from_id_salt("new_field_field")
                        .selected_text(shown)
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for choice in fields {
                                let chosen = field.field.as_ref() == Some(choice);
                                if ui.selectable_label(chosen, &choice.name).clicked() {
                                    field.field = Some(choice.clone());
                                }
                            }
                        })
                        .response
                        .widget_info(|| {
                            WidgetInfo::labeled(Role::ComboBox, true, "Field to Place")
                        });
                    ui.add_space(6.0);
                    frame_grid(
                        ui,
                        "New Field",
                        [
                            &mut field.left,
                            &mut field.top,
                            &mut field.width,
                            &mut field.height,
                        ],
                    );
                    ui.add_space(6.0);
                    // The label: the field's name, left of it.
                    let room = field.label_frame().is_some();
                    let create =
                        ui.add_enabled(room, egui::Checkbox::new(&mut field.label, "Create label"));
                    if !room {
                        create.on_disabled_hover_text(
                            "There is no room left of the field for its label.",
                        );
                    }
                    let date = field.field.as_ref().is_some_and(|choice| choice.date);
                    if field.control == 6 && !date {
                        field.control = 0;
                    }
                    let view = ControlView {
                        stored: (0, 0),
                        pending: Some((field.control, field.value_list)),
                        fixed: None,
                        date,
                        lists,
                    };
                    if let Some((control, list)) = control_rows(ui, "new_field", &view, true) {
                        field.control = control;
                        field.value_list = list;
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Save Layout places the field, and its label, on the layout; \
                             Revert Layout discards them.",
                        )
                        .font(theme::small())
                        .color(theme::WEAK_TEXT),
                    );
                });
            });
    }

    /// The Text tool's new text, before Save Layout: its frame in whole
    /// points (the text is typed on the canvas). `frozen` as for
    /// [`Self::new_field`].
    pub(crate) fn new_text(&mut self, ui: &mut Ui, text: &mut NewText, frozen: bool) {
        ui.painter()
            .rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::INSPECTOR_BG);
        ui.painter().vline(
            ui.max_rect().left(),
            ui.max_rect().y_range(),
            Stroke::new(1.0, theme::DIVIDER),
        );
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_min_width((theme::INSPECTOR_WIDTH - 24.0).max(0.0));
                section(ui, "New Text", |ui| {
                    if frozen {
                        ui.disable();
                    }
                    frame_grid(
                        ui,
                        "New Text",
                        [
                            &mut text.left,
                            &mut text.top,
                            &mut text.width,
                            &mut text.height,
                        ],
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Type the text on the layout. Save Layout places it; Revert Layout \
                             discards it.",
                        )
                        .font(theme::small())
                        .color(theme::WEAK_TEXT),
                    );
                });
            });
    }

    /// The Button tool's new button, before Save Layout: its label and
    /// action as Button Setup set them, and its frame in whole points.
    /// Returns whether Button Setup was asked for. `frozen` as for
    /// [`Self::new_field`].
    pub(crate) fn new_button(&mut self, ui: &mut Ui, button: &mut NewButton, frozen: bool) -> bool {
        ui.painter()
            .rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::INSPECTOR_BG);
        ui.painter().vline(
            ui.max_rect().left(),
            ui.max_rect().y_range(),
            Stroke::new(1.0, theme::DIVIDER),
        );
        let mut setup = false;
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_min_width((theme::INSPECTOR_WIDTH - 24.0).max(0.0));
                section(ui, "New Button", |ui| {
                    if frozen {
                        ui.disable();
                    }
                    let (label, label_color) = if button.label.is_empty() {
                        ("No label", theme::WEAK_TEXT)
                    } else {
                        (button.label.as_str(), theme::TEXT)
                    };
                    ui.label(RichText::new(label).color(label_color));
                    let (action, action_color) = if button.step.is_none() {
                        ("No action", theme::WEAK_TEXT)
                    } else {
                        (button.action.as_str(), theme::CAPTION)
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(action)
                                .font(theme::small())
                                .color(action_color),
                        )
                        .wrap(),
                    );
                    ui.add_space(4.0);
                    setup = ui.button("Button Setup…").clicked();
                    ui.add_space(6.0);
                    frame_grid(
                        ui,
                        "New Button",
                        [
                            &mut button.left,
                            &mut button.top,
                            &mut button.width,
                            &mut button.height,
                        ],
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Save Layout places the button on the layout; Revert Layout \
                             discards it.",
                        )
                        .font(theme::small())
                        .color(theme::WEAK_TEXT),
                    );
                });
            });
        setup && !frozen
    }

    /// The Position section: the object's frame, editable in points for a
    /// movable object (whole points, as the object index holds them);
    /// returns the frame set.
    fn position(
        &mut self,
        ui: &mut Ui,
        object: Option<&ObjectGeom>,
        frame: Option<om_data::GeomRect>,
        editable: bool,
    ) -> Option<om_data::GeomRect> {
        let mut changed = None;
        section(ui, "Position", |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(object.map_or("No object selected", |o| o.kind_name.as_str()))
                        .color(theme::CAPTION),
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("inspector_units")
                        .selected_text(self.units.label())
                        .width(48.0)
                        .show_ui(ui, |ui| {
                            for units in [Units::Points, Units::Inches, Units::Centimeters] {
                                ui.selectable_value(&mut self.units, units, units.label());
                            }
                        })
                        .response
                        .widget_info(|| {
                            WidgetInfo::labeled(Role::ComboBox, true, "Measurement Units")
                        });
                });
            });
            ui.add_space(6.0);
            let rect = frame.or(object.map(|object| object.rect));
            // Set in points only; a line keeps its empty side.
            let editing = editable && self.units == Units::Points;
            egui::Grid::new("inspector_position")
                .num_columns(3)
                .spacing(vec2(8.0, 5.0))
                .show(ui, |ui| {
                    for (label, value) in [
                        ("Left", rect.map(|r| r.left)),
                        ("Top", rect.map(|r| r.top)),
                        ("Right", rect.map(|r| r.right)),
                        ("Bottom", rect.map(|r| r.bottom)),
                        ("Width", rect.map(|r| r.width())),
                        ("Height", rect.map(|r| r.height())),
                    ] {
                        ui.label(RichText::new(label).font(theme::small()));
                        let settable = matches!(label, "Left" | "Top" | "Width" | "Height");
                        let field = match (rect, editing && settable) {
                            (Some(mut r), true) => {
                                let mut v = value.unwrap_or_default();
                                let empty = match label {
                                    "Width" => r.width() == 0.0,
                                    "Height" => r.height() == 0.0,
                                    _ => false,
                                };
                                let min = if matches!(label, "Width" | "Height") {
                                    1.0
                                } else {
                                    0.0
                                };
                                // As wide as the read-only values, in their format.
                                let response = ui
                                    .add_enabled_ui(!empty, |ui| {
                                        ui.add_sized(
                                            [92.0, 22.0],
                                            egui::DragValue::new(&mut v)
                                                .speed(1.0)
                                                .range(
                                                    min..=om_data::layout_placement::MAX_COORDINATE,
                                                )
                                                .fixed_decimals(2),
                                        )
                                    })
                                    .inner;
                                if response.changed() {
                                    let v = v.round();
                                    let (w, h) = (r.width(), r.height());
                                    match label {
                                        "Left" => {
                                            r.left = v;
                                            r.right = v + w;
                                        }
                                        "Top" => {
                                            r.top = v;
                                            r.bottom = v + h;
                                        }
                                        "Width" => r.right = r.left + v,
                                        _ => r.bottom = r.top + v,
                                    }
                                    changed = Some(r);
                                }
                                response
                            }
                            _ => {
                                let mut text = value
                                    .map(|value| self.units.display(value))
                                    .unwrap_or_default();
                                ui.add(
                                    egui::TextEdit::singleline(&mut text)
                                        .desired_width(92.0)
                                        .min_size(vec2(92.0, 22.0))
                                        .align(egui::Align2::CENTER_CENTER)
                                        .interactive(false),
                                )
                            }
                        };
                        widgets::accessible_label(ui, field.id, &format!("Object {label}"));
                        ui.label(
                            RichText::new(self.units.label())
                                .font(theme::small())
                                .color(theme::WEAK_TEXT),
                        );
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            ui.label(
                RichText::new(if editing {
                    "Save Layout stores the new position; Revert Layout discards it."
                } else if editable {
                    "Set the position in points (pt)."
                } else {
                    "Read only"
                })
                .font(theme::small())
                .color(theme::WEAK_TEXT),
            );
        });
        section(ui, "Arrange & Align", |ui| {
            for row in [
                ["Bring to Front", "Send to Back"],
                ["Group", "Ungroup"],
                ["Lock", "Unlock"],
            ] {
                ui.horizontal(|ui| {
                    for label in row {
                        ui.add_enabled(
                            false,
                            egui::Button::new(RichText::new(label).font(theme::small())),
                        );
                    }
                });
            }
        });
        section(ui, "Autosizing", |ui| {
            ui.label("Anchor information is unavailable.");
        });
        changed
    }
}

/// A new object's left, top, width and height in whole points, each
/// labelled `<name> Left` and so on for accessibility.
fn frame_grid(ui: &mut Ui, name: &str, values: [&mut f64; 4]) {
    egui::Grid::new((name, "position"))
        .num_columns(3)
        .spacing(vec2(8.0, 5.0))
        .show(ui, |ui| {
            let max = om_data::layout_placement::MAX_COORDINATE;
            for ((label, min), value) in
                [("Left", 0.0), ("Top", 0.0), ("Width", 1.0), ("Height", 1.0)]
                    .into_iter()
                    .zip(values)
            {
                ui.label(RichText::new(label).font(theme::small()));
                let response = ui.add(
                    egui::DragValue::new(value)
                        .speed(1.0)
                        .range(min..=max)
                        .fixed_decimals(0),
                );
                // The layout's object index holds whole points.
                if response.changed() {
                    *value = value.round();
                }
                widgets::accessible_label(ui, response.id, &format!("{name} {label}"));
                ui.label(
                    RichText::new("pt")
                        .font(theme::small())
                        .color(theme::WEAK_TEXT),
                );
                ui.end_row();
            }
        });
}

fn section(ui: &mut Ui, title: &str, contents: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.set_max_width(theme::INSPECTOR_WIDTH - 30.0);
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            contents(ui);
        });
    ui.add_space(5.0);
    ui.separator();
}

#[cfg(test)]
mod tests {
    use super::Units;
    #[test]
    fn measurements_convert_from_layout_points() {
        assert_eq!(Units::Points.display(72.0), "72.00");
        assert_eq!(Units::Inches.display(72.0), "1.000");
        assert_eq!(Units::Centimeters.display(72.0), "2.540");
    }
}
