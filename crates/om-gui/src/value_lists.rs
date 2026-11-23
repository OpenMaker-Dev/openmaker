//! File > Manage > Value Lists: the open file's value-list catalog
//! (`om_data::value_lists`), read through the provider's
//! `value_list_catalog`, the operation the authoring service's
//! `get_value_lists` also runs. The dialog only presents it.
//!
//! Value lists are read only: New, Edit, Duplicate and Delete stay disabled
//! with the catalog's reason, and nothing is saved. The dialog's columns
//! follow FileMaker Pro's Manage Value Lists (name, source, values); its
//! exact layout has not been compared with FileMaker's.
//!
//! The dialog belongs to the file whose window opened it (`owner`, the
//! window's file id) and to its latest catalog request (`token`): the app
//! shows it only over a window of that file and gives it only the reply to
//! that request, so a late reply for a dialog since closed, or for another
//! file's, never fills it.

use egui::{
    pos2, vec2, Align, Align2, CornerRadius, Id, Layout, RichText, Role, Sense, Stroke, Ui,
    WidgetInfo,
};
use om_data::value_lists::{CatalogList, ListSource, ValueListCatalog};
use om_data::DataError;

use crate::theme;
use crate::widgets;

/// The dialog's state while it is open.
pub(crate) struct ManageValueLists {
    file: String,
    owner: u64,
    /// The catalog request this dialog waits for or shows (0: none yet).
    token: u64,
    /// `None` while the read is outstanding.
    catalog: Option<Result<ValueListCatalog, String>>,
    selected: Option<u64>,
}

impl ManageValueLists {
    pub(crate) fn new(owner: u64, file: String) -> Self {
        Self {
            file,
            owner,
            token: 0,
            catalog: None,
            selected: None,
        }
    }

    pub(crate) fn owns(&self, file: u64) -> bool {
        self.owner == file
    }

    /// Catalog request `token` was sent for this dialog: it waits for that
    /// one alone.
    pub(crate) fn requested(&mut self, token: u64) {
        self.token = token;
        self.catalog = None;
        self.selected = None;
    }

    /// Whether the reply to request `token`, answered in a window of file
    /// `file`, is this dialog's.
    pub(crate) fn accepts(&self, file: u64, token: u64) -> bool {
        self.owns(file) && self.token == token
    }

    pub(crate) fn accept(&mut self, result: Result<ValueListCatalog, DataError>) {
        self.selected = result
            .as_ref()
            .ok()
            .and_then(|catalog| catalog.lists.first())
            .map(|list| list.id);
        self.catalog = Some(result.map_err(|error| error.message));
    }

    /// Draw the modal; `true` asks the caller to close it.
    pub(crate) fn show(&mut self, ctx: &egui::Context) -> bool {
        let width = (ctx.content_rect().width() - 64.0).clamp(420.0, 680.0);
        let list_height = (ctx.content_rect().height() - 300.0).clamp(100.0, 300.0);
        let frame = egui::Frame::new()
            .fill(theme::DIALOG_BG)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(10))
            .shadow(ctx.global_style().visuals.window_shadow)
            .inner_margin(egui::Margin::same(16));
        let mut close = false;
        let response = egui::Modal::new(Id::unique("manage_value_lists"))
            .frame(frame)
            .backdrop_color(theme::SCRIM)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.label(
                    RichText::new(format!("Manage Value Lists for “{}”", self.file))
                        .font(egui::FontId::new(15.0, theme::semibold())),
                );
                ui.add_space(10.0);
                let lists = match &self.catalog {
                    Some(Ok(catalog)) => &catalog.lists[..],
                    _ => &[],
                };
                egui::Frame::new()
                    .fill(theme::CONTROL_BG)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let (header, _) = ui.allocate_exact_size(vec2(width, 25.0), Sense::hover());
                        ui.painter()
                            .rect_filled(header, CornerRadius::ZERO, theme::LAYOUT_BAR_BG);
                        for (column, text) in ["Value List Name", "Source", "Values"]
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
                            .id_salt("manage_value_lists_list")
                            .min_scrolled_height(list_height)
                            .max_height(list_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_min_width(width);
                                let empty = match &self.catalog {
                                    None => Some("Reading value lists…".to_owned()),
                                    Some(Err(error)) => Some(error.clone()),
                                    Some(Ok(_)) if lists.is_empty() => {
                                        Some("This file has no value lists.".to_owned())
                                    }
                                    Some(Ok(_)) => None,
                                };
                                if let Some(text) = empty {
                                    ui.add_space(24.0);
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.label(RichText::new(text).color(theme::WEAK_TEXT));
                                    });
                                }
                                for (index, list) in lists.iter().enumerate() {
                                    let selected = self.selected == Some(list.id);
                                    if list_row(ui, list, selected, index % 2 == 1).clicked() {
                                        self.selected = Some(list.id);
                                    }
                                }
                            });
                    });
                ui.add_space(5.0);
                ui.label(
                    RichText::new(match lists.len() {
                        1 => "1 value list".to_owned(),
                        n => format!("{n} value lists"),
                    })
                    .font(theme::small())
                    .color(theme::CAPTION),
                );
                if let Some(list) = lists.iter().find(|list| Some(list.id) == self.selected) {
                    for line in details(list) {
                        ui.label(RichText::new(line).font(theme::small()).color(theme::TEXT));
                    }
                }
                let read_only = match &self.catalog {
                    Some(Ok(catalog)) => catalog.read_only.map(|reason| reason.message()),
                    _ => Some("Value lists cannot be changed until they are read."),
                };
                if let Some(reason) = read_only {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(reason)
                            .font(theme::small())
                            .color(theme::CAPTION),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    for label in ["New...", "Edit...", "Duplicate", "Delete"] {
                        let button = widgets::dialog_button(ui, label, false, read_only.is_none());
                        if let Some(reason) = read_only {
                            button.on_hover_text(reason);
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::dialog_button(ui, "Close", true, true).clicked() {
                            close = true;
                        }
                    });
                });
            });
        close || response.should_close()
    }
}

/// What the Source column says.
fn source_name(list: &CatalogList) -> &'static str {
    match &list.content {
        Ok(definition) => match definition.source {
            ListSource::Custom { .. } => "Custom Values",
            ListSource::Field { .. } => "Field",
            ListSource::External { .. } => "Another File",
            ListSource::Calculation => "Calculation",
        },
        Err(_) => "Unreadable",
    }
}

/// What the Values column says: the custom values, or the field(s).
fn values_text(list: &CatalogList) -> String {
    match &list.content {
        Ok(definition) => match &definition.source {
            ListSource::Custom { values } => values.join(" "),
            ListSource::Field {
                primary, secondary, ..
            } => match secondary {
                Some(second) => format!("{}, {}", primary.label(), second.label()),
                None => primary.label(),
            },
            ListSource::External { .. } => "Another file's value list".to_owned(),
            ListSource::Calculation => String::new(),
        },
        Err(unreadable) => unreadable.message(),
    }
}

/// The selected list's options, one line each.
fn details(list: &CatalogList) -> Vec<String> {
    let Ok(definition) = &list.content else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let ListSource::Field {
        secondary,
        related_from,
        sort_by_second,
        second_only,
        ..
    } = &definition.source
    {
        if let Some(occurrence) = related_from {
            lines.push(format!(
                "Include only related values starting from {}",
                occurrence.name.as_deref().unwrap_or("<Table Missing>")
            ));
        }
        if secondary.is_some() {
            if *second_only {
                lines.push("Show values only from second field".to_owned());
            }
            if *sort_by_second {
                lines.push("Sort values using second field".to_owned());
            }
        }
    }
    if let ListSource::External { file, list: key } = &definition.source {
        lines.push(format!(
            "Stored file reference {} and list key {} (not resolved)",
            file.map_or("none".to_owned(), |id| id.to_string()),
            key.map_or("none".to_owned(), |id| id.to_string())
        ));
    }
    if let Some(language) = &definition.sort_language {
        let name = if language.name.is_empty() {
            format!("language {}", language.id)
        } else {
            language.name.clone()
        };
        lines.push(format!("Re-sort values based on {name}"));
    }
    if definition.unknown_options != 0 {
        lines.push(format!(
            "Options OpenMaker does not know: 0x{:04x}",
            definition.unknown_options
        ));
    }
    lines
}

fn column_rect(rect: egui::Rect, column: usize) -> egui::Rect {
    let edges = [0.0, 0.36, 0.56, 1.0];
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

fn list_row(ui: &mut Ui, list: &CatalogList, selected: bool, alternate: bool) -> egui::Response {
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
    let (color, weak) = if selected {
        (theme::ON_ACCENT, theme::ON_ACCENT)
    } else {
        (theme::TEXT, theme::WEAK_TEXT)
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    let name = list.name.as_deref().unwrap_or("(no name)");
    let values = values_text(list);
    for (column, text, font, color) in [
        (
            0,
            name,
            theme::body(),
            if list.name.is_some() { color } else { weak },
        ),
        (1, source_name(list), theme::small(), color),
        (2, values.as_str(), theme::small(), color),
    ] {
        let cell = column_rect(rect, column);
        ui.painter()
            .with_clip_rect(ui.clip_rect().intersect(cell))
            .text(cell.left_center(), Align2::LEFT_CENTER, text, font, color);
    }
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, name));
    response
}
