//! Layout management chrome populated from the open file's layout catalog.
//!
//! The catalog exposes names, IDs and each layout's table occurrence, so
//! this dialog can find and open layouts and show the occurrence each shows
//! records of (Associated Table), and New opens New Layout/Report
//! (`new_layout.rs`). Editing, deleting and duplicating layouts are still to
//! come, and the menu set shows a dash. No folder or privilege information
//! is inferred.

use egui::{
    pos2, vec2, Align, Align2, CornerRadius, Id, Key, Layout, RichText, Role, Sense, Stroke, Ui,
    WidgetInfo,
};

use crate::session::Session;
use crate::theme;
use crate::widgets;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManageLayoutsResult {
    Cancel,
    Open(u32),
    /// New: New Layout/Report.
    New,
    /// Duplicate: copy the selected layout (Duplicate Layout).
    Duplicate(u32),
    /// Delete: delete the selected layout (Delete Layout).
    Delete(u32),
}

/// Transient state, recreated each time the dialog is opened.
#[derive(Default)]
pub(crate) struct ManageLayouts {
    search: String,
    selected: Option<u32>,
    initialized: bool,
}

impl ManageLayouts {
    /// Draw the modal. Returning a result asks the caller to close it.
    /// `tables` names each layout's table occurrence (Associated Table),
    /// when known.
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        session: &Session,
        tables: &std::collections::BTreeMap<u32, String>,
    ) -> Option<ManageLayoutsResult> {
        if !self.initialized {
            self.selected = session
                .layout
                .or_else(|| session.layouts.first().map(|item| item.id));
            self.initialized = true;
        }
        let width = (ctx.content_rect().width() - 64.0).clamp(320.0, 640.0);
        let list_height = (ctx.content_rect().height() - 240.0).clamp(100.0, 300.0);
        let frame = egui::Frame::new()
            .fill(theme::DIALOG_BG)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(10))
            .shadow(ctx.global_style().visuals.window_shadow)
            .inner_margin(egui::Margin::same(16));
        let mut result = None;
        let response = egui::Modal::new(Id::unique("manage_layouts"))
            .frame(frame)
            .backdrop_color(theme::SCRIM)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Manage Layouts")
                            .font(egui::FontId::new(15.0, theme::semibold())),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(&session.target.file).color(theme::CAPTION));
                    });
                });
                ui.add_space(8.0);
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search Layouts")
                        .desired_width(width),
                );
                widgets::accessible_label(ui, search.id, "Search Layouts");
                let query = self.search.trim().to_lowercase();
                let shown: Vec<_> = session
                    .layouts
                    .iter()
                    .filter(|layout| {
                        query.is_empty() || layout.name.to_lowercase().contains(&query)
                    })
                    .collect();
                if search.changed() && !shown.iter().any(|layout| Some(layout.id) == self.selected)
                {
                    self.selected = shown.first().map(|layout| layout.id);
                }

                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(theme::CONTROL_BG)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let (header, _) = ui.allocate_exact_size(vec2(width, 25.0), Sense::hover());
                        ui.painter()
                            .rect_filled(header, CornerRadius::ZERO, theme::LAYOUT_BAR_BG);
                        for (column, text) in ["Layout Name", "Associated Table", "Menu Set"]
                            .into_iter()
                            .enumerate()
                        {
                            widgets::put_label(
                                ui,
                                column_rect(header, column),
                                text,
                                theme::small(),
                                theme::TEXT,
                                Align::Min,
                            );
                        }
                        egui::ScrollArea::vertical()
                            .id_salt("manage_layouts_list")
                            .min_scrolled_height(list_height)
                            .max_height(list_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_min_width(width);
                                if shown.is_empty() {
                                    ui.add_space(24.0);
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.label(
                                            RichText::new(if session.layouts.is_empty() {
                                                "This file has no layouts."
                                            } else {
                                                "No layouts match the search."
                                            })
                                            .color(theme::WEAK_TEXT),
                                        );
                                    });
                                }
                                for (index, layout) in shown.iter().enumerate() {
                                    let selected = self.selected == Some(layout.id);
                                    let table = tables.get(&layout.id).map(String::as_str);
                                    let response = layout_row(
                                        ui,
                                        &layout.name,
                                        table,
                                        selected,
                                        index % 2 == 1,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(layout.id);
                                    }
                                    if response.double_clicked() {
                                        result = Some(ManageLayoutsResult::Open(layout.id));
                                    }
                                }
                            });
                    });
                ui.add_space(5.0);
                ui.label(
                    RichText::new(if query.is_empty() {
                        format!("{} layouts", session.layouts.len())
                    } else {
                        format!("{} of {} layouts", shown.len(), session.layouts.len())
                    })
                    .font(theme::small())
                    .color(theme::CAPTION),
                );
                ui.add_space(8.0);
                let selected_visible = self
                    .selected
                    .filter(|id| shown.iter().any(|layout| layout.id == *id));
                ui.horizontal(|ui| {
                    if widgets::dialog_button(ui, "New", false, true).clicked() {
                        result = Some(ManageLayoutsResult::New);
                    }
                    widgets::dialog_button(ui, "Edit", false, false);
                    if widgets::dialog_button(ui, "Duplicate", false, selected_visible.is_some())
                        .clicked()
                    {
                        result = selected_visible.map(ManageLayoutsResult::Duplicate);
                    }
                    if widgets::dialog_button(ui, "Delete", false, selected_visible.is_some())
                        .clicked()
                    {
                        result = selected_visible.map(ManageLayoutsResult::Delete);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::dialog_button(ui, "Open", true, selected_visible.is_some())
                            .clicked()
                        {
                            result = selected_visible.map(ManageLayoutsResult::Open);
                        }
                        if widgets::dialog_button(ui, "Close", false, true).clicked() {
                            result = Some(ManageLayoutsResult::Cancel);
                        }
                    });
                });
                if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter)) {
                    result = selected_visible.map(ManageLayoutsResult::Open);
                }
            });
        if response.should_close() {
            Some(ManageLayoutsResult::Cancel)
        } else {
            result
        }
    }
}

fn column_rect(rect: egui::Rect, column: usize) -> egui::Rect {
    let edges = [0.0, 0.46, 0.78, 1.0];
    egui::Rect::from_min_max(
        pos2(
            rect.left() + rect.width() * edges[column] + 12.0,
            rect.top(),
        ),
        pos2(
            rect.left() + rect.width() * edges[column + 1] - 8.0,
            rect.bottom(),
        ),
    )
}

/// One layout's row: its name, its table occurrence (a dash when unknown)
/// and its menu set (a dash: the provider does not expose it).
fn layout_row(
    ui: &mut Ui,
    name: &str,
    table: Option<&str>,
    selected: bool,
    alternate: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    let fill = if selected {
        theme::DEFAULT_BUTTON
    } else if response.hovered() {
        theme::CURRENT_ROW
    } else if alternate {
        theme::ALTERNATE_ROW
    } else {
        theme::CONTROL_BG
    };
    let color = if selected {
        theme::ON_ACCENT
    } else {
        theme::TEXT
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    let name_rect = column_rect(rect, 0);
    ui.painter()
        .with_clip_rect(ui.clip_rect().intersect(name_rect))
        .text(
            name_rect.left_center(),
            Align2::LEFT_CENTER,
            name,
            theme::body(),
            color,
        );
    let table_rect = column_rect(rect, 1);
    ui.painter()
        .with_clip_rect(ui.clip_rect().intersect(table_rect))
        .text(
            table_rect.left_center(),
            Align2::LEFT_CENTER,
            table.unwrap_or("-"),
            theme::body(),
            color,
        );
    // The menu set column exists in FileMaker's ManageLayouts.nib, but the
    // provider does not expose it. A dash preserves that gap.
    ui.painter().text(
        column_rect(rect, 2).left_center(),
        Align2::LEFT_CENTER,
        "-",
        theme::small(),
        color,
    );
    // The row's label names its table too, as the columns show it.
    let label = match table {
        Some(table) => format!("{name}, {table}"),
        None => name.to_owned(),
    };
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &label));
    response
}
