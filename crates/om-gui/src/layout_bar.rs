//! The layout bar under the status toolbar (25 pt, `#DEDEDF`, ruled above and
//! below) and the optional formatting bar.
//!
//! Browse: `Layout:` popup | `View As:` Form/List/Table | Preview, with the
//! text-formatting toggle and Edit Layout at the right. Find adds Matching
//! Records Include/Omit and Insert Operators; Preview swaps Preview for Exit
//! Preview; Layout mode shows the layout's table and theme and Exit Layout.

use egui::{pos2, vec2, Align, Align2, CornerRadius, Layout, Rect, Sense, Stroke, Ui};

use crate::app::{Action, FindState, Mode, ViewMode};
use crate::icons::Icon;
use crate::session::Session;
use crate::theme;
use crate::widgets::{self, bar_button, bar_divider, bar_label, glyph_toggle, popup_button};

/// FileMaker's find operators: the symbol inserted and its description.
const OPERATORS: [(&str, &str); 16] = [
    ("<", "less than"),
    ("≤", "less than or equal"),
    (">", "greater than"),
    ("≥", "greater than or equal"),
    ("=", "exact match"),
    ("==", "match entire field"),
    ("!", "duplicate values"),
    ("//", "today's date"),
    ("?", "invalid date or time"),
    ("@", "one character"),
    ("#", "one digit"),
    ("*", "zero or more characters"),
    ("\\", "literal text"),
    ("\"\"", "match phrase"),
    ("~", "relaxed search"),
    ("...", "range"),
];

pub(crate) struct LayoutBarContext<'a> {
    pub mode: Mode,
    pub view: ViewMode,
    pub session: &'a Session,
    pub schema: Option<&'a om_data::host::CalcSchema>,
    pub find: &'a FindState,
    pub formatting: bool,
    /// The account may enter Layout mode (`App::design_allowed`).
    pub design: bool,
    /// The layout menu can be used: not under Minimum menus
    /// (session-identity.md 2.3), which leave the bar nothing.
    pub layouts: bool,
    /// View As, Preview and Exit Preview are available: all menu commands;
    /// Editing only leaves them out and keeps layout navigation (FileMaker
    /// Pro 26.0.3, #137's reading).
    pub modes: bool,
}

pub(crate) fn show(ui: &mut Ui, context: &LayoutBarContext<'_>, actions: &mut Vec<Action>) {
    let frame = egui::Frame::new()
        .fill(theme::LAYOUT_BAR_BG)
        .inner_margin(egui::Margin {
            left: 9,
            right: 9,
            top: 0,
            bottom: 0,
        });
    egui::Panel::top("layout_bar")
        .frame(frame)
        .exact_size(theme::LAYOUT_BAR_HEIGHT)
        .show_separator_line(false)
        .resizable(false)
        .show(ui, |ui| {
            if !context.layouts {
                ui.disable();
            }
            let rect = ui.max_rect().expand2(vec2(9.0, 0.0));
            let painter = ui.painter();
            painter.hline(
                rect.x_range(),
                rect.top() + 0.5,
                Stroke::new(1.0, theme::LAYOUT_BAR_TOP_RULE),
            );
            painter.hline(
                rect.x_range(),
                rect.bottom() - 0.5,
                Stroke::new(1.0, theme::LAYOUT_BAR_BOTTOM_RULE),
            );
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                left(ui, context, actions);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    right(ui, context, actions);
                });
            });
        });
}

fn left(ui: &mut Ui, context: &LayoutBarContext<'_>, actions: &mut Vec<Action>) {
    layout_popup(ui, context.session, context.mode, actions);
    if context.mode == Mode::Layout {
        let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
        crate::icons::paint(
            ui.painter(),
            rect.shrink(1.0),
            Icon::Pencil,
            theme::ICON,
            theme::LAYOUT_BAR_BG,
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Layout Setup"));
        if response.clicked() {
            actions.push(Action::LayoutSetup);
        }
        ui.add_space(2.0);
        bar_divider(ui);
        ui.add_space(2.0);
        bar_label(ui, "Table:");
        let table = crate::calc::layout_occurrence(context.session)
            .and_then(|own| context.schema?.table_of(own));
        if let Some(table) = table {
            bar_label(ui, &table.name);
        } else {
            weak_value(ui, "-");
        }
        return;
    }

    ui.add_space(2.0);
    bar_divider(ui);
    ui.add_space(2.0);
    bar_label(ui, "View As:");
    ui.spacing_mut().item_spacing.x = 5.0;
    for (view, icon, label) in [
        (ViewMode::Form, Icon::ViewForm, "Form View"),
        (ViewMode::List, Icon::ViewList, "List View"),
        (ViewMode::Table, Icon::ViewTable, "Table View"),
    ] {
        let selected = context.view == view;
        if glyph_toggle(
            ui,
            icon,
            label,
            selected,
            context.session.view_enabled(view) && context.modes,
        )
        .on_hover_text(format!("View as {}", view.label()))
        .clicked()
            && !selected
        {
            actions.push(Action::SetView(view));
        }
    }
    ui.spacing_mut().item_spacing.x = 6.0;
    ui.add_space(3.0);
    bar_divider(ui);
    ui.add_space(2.0);

    match context.mode {
        Mode::Browse => {
            if bar_button(ui, "Preview", context.modes).clicked() {
                actions.push(Action::SetMode(Mode::Preview));
            }
        }
        Mode::Preview => {
            // Back to Browse from Preview a script entered: kept under
            // Editing only, as it grants nothing.
            if bar_button(ui, "Exit Preview", true).clicked() {
                actions.push(Action::SetMode(Mode::Browse));
            }
        }
        Mode::Find => find_controls(ui, context.find, actions),
        Mode::Layout => {}
    }
}

fn weak_value(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::small())
                .color(theme::WEAK_TEXT),
        )
        .selectable(false),
    )
    .on_hover_text("Not reported by the data provider yet");
}

fn find_controls(ui: &mut Ui, find: &FindState, actions: &mut Vec<Action>) {
    bar_label(ui, "Matching Records:");
    let omit = find.current().omit;
    if let Some(index) = widgets::segmented(ui, &["Include", "Omit"], usize::from(omit)) {
        actions.push(Action::SetOmit(index == 1));
    }
    ui.add_space(12.0);
    bar_divider(ui);
    ui.add_space(2.0);
    bar_label(ui, "Insert:");
    let response = bar_button(ui, "Operators", true);
    egui::Popup::menu(&response)
        .style(theme::menu_popup_style)
        .show(|ui| {
            for (symbol, description) in OPERATORS {
                let label = format!("{symbol}   {description}");
                if widgets::menu_item(ui, &label, None, false, true).clicked() {
                    actions.push(Action::InsertOperator(symbol));
                }
            }
        });
}

fn right(ui: &mut Ui, context: &LayoutBarContext<'_>, actions: &mut Vec<Action>) {
    // Find and Preview show nothing at the right (captures 13 and 15).
    let exit = match context.mode {
        Mode::Browse => ("Edit Layout", Mode::Layout),
        Mode::Layout => ("Exit Layout", Mode::Browse),
        Mode::Find | Mode::Preview => return,
    };
    if bar_button(ui, exit.0, exit.1 != Mode::Layout || context.design).clicked() {
        actions.push(Action::SetMode(exit.1));
    }
    ui.add_space(4.0);
    if formatting_toggle(ui, context.formatting, context.modes).clicked() {
        actions.push(Action::ToggleFormattingBar);
    }
    if context.mode == Mode::Layout {
        ui.add_space(4.0);
        // TODO(layout-model): om-data does not report the layout's theme yet.
        weak_value(ui, "-");
        bar_label(ui, "Theme:");
    }
}

/// The `Aᵃ` Show/Hide Text Formatting toggle.
fn formatting_toggle(ui: &mut Ui, on: bool, enabled: bool) -> egui::Response {
    let enabled = enabled && ui.is_enabled();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 18.0), sense);
    let color = if !enabled {
        theme::DISABLED
    } else if on {
        theme::SELECTED
    } else {
        theme::CAPTION
    };
    let painter = ui.painter();
    painter.text(
        pos2(rect.left() + 3.0, rect.bottom() - 1.0),
        Align2::LEFT_BOTTOM,
        "A",
        egui::FontId::proportional(14.0),
        color,
    );
    painter.text(
        pos2(rect.left() + 13.0, rect.top() + 1.0),
        Align2::LEFT_TOP,
        "a",
        egui::FontId::proportional(9.0),
        color,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, true, on, "Show/Hide Text Formatting")
    });
    response
}

/// The formatting bar FileMaker shows under the layout bar when text
/// formatting is toggled on. Its controls apply to the active field, so they
/// stay disabled until field editing exists.
pub(crate) fn formatting_bar(ui: &mut Ui) {
    let frame = egui::Frame::new()
        .fill(theme::TOOLBAR_BG)
        .inner_margin(egui::Margin {
            left: 9,
            right: 9,
            top: 0,
            bottom: 0,
        });
    egui::Panel::top("formatting_bar")
        .frame(frame)
        .exact_size(theme::FORMATTING_BAR_HEIGHT)
        .show_separator_line(false)
        .resizable(false)
        .show(ui, |ui| {
            let rect = ui.max_rect().expand2(vec2(9.0, 0.0));
            ui.painter().hline(
                rect.x_range(),
                rect.bottom() - 0.5,
                Stroke::new(1.0, theme::LAYOUT_BAR_BOTTOM_RULE),
            );
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                popup_button(ui, "Font", "", 150.0, false);
                popup_button(ui, "Font Size", "", 52.0, false);
                ui.add_space(4.0);
                for label in ["B", "I", "U"] {
                    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 16.0), Sense::hover());
                    let font = match label {
                        "B" => egui::FontId::new(12.0, theme::semibold()),
                        _ => egui::FontId::proportional(12.0),
                    };
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        label,
                        font,
                        theme::DISABLED,
                    );
                    if label == "U" {
                        ui.painter().hline(
                            rect.center().x - 4.0..=rect.center().x + 4.0,
                            rect.bottom() - 2.0,
                            Stroke::new(1.0, theme::DISABLED),
                        );
                    }
                }
                ui.add_space(4.0);
                bar_divider(ui);
                // Left, center, right and justify.
                for inset in [[0.0, 5.0], [2.5, 2.5], [5.0, 0.0], [0.0, 0.0]] {
                    alignment_glyph(ui, inset);
                }
                ui.add_space(4.0);
                bar_divider(ui);
                let (swatch, _) = ui.allocate_exact_size(vec2(22.0, 14.0), Sense::hover());
                ui.painter().rect(
                    swatch,
                    CornerRadius::same(2),
                    theme::BAR_BG,
                    Stroke::new(1.0, theme::DISABLED),
                    egui::StrokeKind::Inside,
                );
            });
        });
}

/// A text-alignment glyph: three lines, the middle one inset by `[left,
/// right]` to show left, center, right or justified text.
fn alignment_glyph(ui: &mut Ui, [left, right]: [f32; 2]) {
    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 16.0), Sense::hover());
    let stroke = Stroke::new(1.2, theme::DISABLED);
    let lines = Rect::from_center_size(rect.center(), vec2(14.0, 10.0));
    for (y, [left, right]) in [(0.0, [0.0, 0.0]), (5.0, [left, right]), (10.0, [0.0, 0.0])] {
        ui.painter().hline(
            lines.left() + left..=lines.right() - right,
            lines.top() + y,
            stroke,
        );
    }
}

/// The `Layout:` label and popup listing the file's layouts, with a
/// checkmark on the current one.
fn layout_popup(ui: &mut Ui, session: &Session, mode: Mode, actions: &mut Vec<Action>) {
    bar_label(ui, "Layout:");
    // A layout the account may not see has no name in the menu (#137's
    // reading), except after Go to Related Record took the window there
    // (#696's reading).
    let name = session
        .layout
        .filter(|id| session.shows_layout(*id) || session.hidden_named)
        .and_then(|_| session.layout_name())
        .unwrap_or("");
    let enabled = !session.layouts.is_empty();
    let response = popup_button(ui, "Layout", name, 132.0, enabled);
    egui::Popup::menu(&response)
        .style(theme::menu_popup_style)
        .show(|ui| {
            crate::layout_menu::show(&mut crate::menubar::Drawn(ui), session, mode, actions);
        });
}
