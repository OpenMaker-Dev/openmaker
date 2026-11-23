//! OpenMaker's Home screen. File and history actions use the same dispatch
//! as the File menu. Hosts retains its existing connection form. Its look
//! follows `docs/design.md`: a source-list sidebar, a window title, and a
//! striped list in a grouped frame.

use egui::{
    vec2, Align, Align2, Button, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText,
    Sense, Stroke, Ui,
};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::app::Action;
use crate::file_history::{Change, History, SavedFile, Tab};
use crate::theme;

const SIDEBAR_WIDTH: f32 = 190.0;

#[derive(Default)]
pub(crate) struct Home {
    search: String,
}

impl Home {
    pub fn show(&mut self, ui: &mut Ui, history: &History, busy: bool, actions: &mut Vec<Action>) {
        let tab = history.tab();
        egui::Panel::left("home_navigation")
            .exact_size(SIDEBAR_WIDTH)
            .frame(
                Frame::new()
                    .fill(theme::SIDEBAR_BG)
                    .inner_margin(Margin::symmetric(10, 18)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(title(crate::APP_TITLE, theme::TITLE3));
                });
                ui.add_space(14.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                for section in [Tab::Recent, Tab::Favorites] {
                    if navigation(ui, section.label(), tab == section, !busy) {
                        self.search.clear();
                        actions.push(Action::ShowHome(section));
                    }
                }
                ui.add_space(12.0);
                if navigation(ui, "Hosts", false, !busy) {
                    actions.push(Action::ShowHosts);
                }
            });
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(theme::DIALOG_BG)
                    .inner_margin(Margin::same(20)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(title("Home", theme::TITLE1));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if action_button(ui, "Open File…", true, !busy) {
                            actions.push(Action::ShowOpenFile);
                        }
                        if action_button(ui, "Create…", false, !busy) {
                            actions.push(Action::ShowCreateNew);
                        }
                    });
                });
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    let heading = match tab {
                        Tab::Recent => "Recent files",
                        Tab::Favorites => "Favorite files",
                    };
                    ui.label(title(heading, theme::TITLE3));
                    if !history.files(tab).is_empty() {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let search = ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .hint_text("Search files")
                                    .desired_width(180.0),
                            );
                            crate::widgets::accessible_label(ui, search.id, "Search files");
                        });
                    }
                });
                ui.add_space(8.0);
                if busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Opening file…").color(theme::CAPTION));
                    });
                    ui.add_space(6.0);
                }
                self.files(ui, history, busy, actions);
            });
    }

    fn files(&self, ui: &mut Ui, history: &History, busy: bool, actions: &mut Vec<Action>) {
        let tab = history.tab();
        let files = history.files(tab);
        let query = self.search.trim().to_lowercase();
        let visible: Vec<_> = files
            .iter()
            .filter(|file| {
                query.is_empty()
                    || file.name().to_lowercase().contains(&query)
                    || file.location().to_lowercase().contains(&query)
            })
            .collect();
        Frame::new()
            .fill(theme::CONTROL_BG)
            .stroke(Stroke::new(1.0, theme::HAIRLINE))
            .corner_radius(theme::MENU_RADIUS)
            .inner_margin(Margin::same(4))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                if visible.is_empty() {
                    empty(ui, tab, files.is_empty());
                    return;
                }
                let name_width = (ui.available_width() * 0.40).clamp(125.0, 270.0);
                TableBuilder::new(ui)
                    .id_salt(("home_files", tab.label()))
                    .striped(true)
                    .min_scrolled_height(0.0)
                    .auto_shrink([false, false])
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::exact(name_width).clip(true))
                    .column(Column::remainder().clip(true))
                    .column(Column::exact(34.0))
                    .header(24.0, |mut row| {
                        row.col(|ui| {
                            ui.add_space(8.0);
                            ui.label(caption("Name"));
                        });
                        row.col(|ui| {
                            ui.label(caption("Location"));
                        });
                        row.col(|_| {});
                    })
                    .body(|body| {
                        body.rows(theme::ROW_HEIGHT, visible.len(), |row| {
                            let file = visible[row.index()];
                            file_row(row, history, file, busy, actions);
                        });
                    });
            });
    }
}

/// A window or section title in the semibold UI face.
fn title(text: &str, size: f32) -> RichText {
    RichText::new(text)
        .font(FontId::new(size, theme::semibold()))
        .color(theme::TEXT)
}

/// A sidebar row as macOS draws a source list: the selected row filled with
/// the accent, the hovered row with the hover fill.
fn navigation(ui: &mut Ui, label: &str, selected: bool, enabled: bool) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), theme::ROW_HEIGHT), sense);
    let fill = if selected {
        theme::SELECTED
    } else if enabled && response.hovered() {
        theme::HOVER_FILL
    } else {
        Color32::TRANSPARENT
    };
    let text = match (selected, enabled) {
        (true, _) => theme::ON_ACCENT,
        (false, true) => theme::TEXT,
        (false, false) => theme::DISABLED,
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(theme::CONTROL_RADIUS), fill);
    painter.text(
        rect.left_center() + vec2(8.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        theme::body(),
        text,
    );
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, label));
    response.clicked()
}

/// A large push button; the primary one is the default button.
fn action_button(ui: &mut Ui, label: &str, primary: bool, enabled: bool) -> bool {
    let (fill, text, stroke) = if primary {
        (theme::DEFAULT_BUTTON, theme::ON_ACCENT, Stroke::NONE)
    } else {
        (
            theme::CONTROL_BG,
            theme::TEXT,
            Stroke::new(1.0, theme::BUTTON_BORDER),
        )
    };
    ui.add_enabled(
        enabled,
        Button::new(RichText::new(label).color(text))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(theme::CONTROL_RADIUS)
            .min_size(vec2(88.0, theme::LARGE_CONTROL_HEIGHT)),
    )
    .clicked()
}

/// A column header, as macOS table headers: small secondary text.
fn caption(text: &str) -> RichText {
    RichText::new(text)
        .font(FontId::new(theme::SMALL, theme::semibold()))
        .color(theme::CAPTION)
}

fn empty(ui: &mut Ui, tab: Tab, empty_list: bool) {
    let (heading, detail) = match (empty_list, tab) {
        (false, _) => ("No matching files", "Try a different name or location."),
        (true, Tab::Recent) => ("No recent files", "Open or create a file to see it here."),
        (true, Tab::Favorites) => (
            "No favorite files",
            "Use the star beside a recent file to keep it here.",
        ),
    };
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.30).max(18.0));
        ui.label(title(heading, theme::TITLE3));
        ui.add_space(4.0);
        ui.label(RichText::new(detail).color(theme::CAPTION));
    });
}

fn file_row(
    mut row: TableRow<'_, '_>,
    history: &History,
    file: &SavedFile,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let favorite = history.files(Tab::Favorites).contains(file);
    let name = file.name();
    let location = file.location();
    let pin = if favorite {
        "Remove from Favorites"
    } else {
        "Add to Favorites"
    };
    row.col(|ui| {
        // The name opens the file; hovering shows the hover fill, not a
        // button bezel, as a list row would.
        let widgets = &mut ui.visuals_mut().widgets;
        for state in [&mut widgets.hovered, &mut widgets.active] {
            state.weak_bg_fill = theme::HOVER_FILL;
            state.bg_fill = theme::HOVER_FILL;
            state.bg_stroke = Stroke::NONE;
        }
        let open = ui.add_enabled(
            !busy,
            Button::new(RichText::new(&name).color(theme::TEXT))
                .right_text(egui::Atom::grow())
                .truncate()
                .min_size(vec2(ui.available_width(), theme::ROW_HEIGHT - 4.0))
                .frame_when_inactive(false),
        );
        if open.clicked() {
            actions.push(Action::OpenSaved(file.clone()));
        }
        open.on_hover_text(&location).context_menu(|ui| {
            if ui.button(pin).clicked() {
                actions.push(Action::EditHistory(Change::SetFavorite(
                    file.clone(),
                    !favorite,
                )));
                ui.close();
            }
            if history.tab() == Tab::Recent && ui.button("Remove from Recent").clicked() {
                actions.push(Action::EditHistory(Change::RemoveRecent(file.clone())));
                ui.close();
            }
        });
    });
    row.col(|ui| {
        ui.add(
            egui::Label::new(RichText::new(&location).color(theme::CAPTION))
                .truncate()
                .selectable(false),
        )
        .on_hover_text(&location);
    });
    row.col(|ui| {
        let (star, color) = if favorite {
            ("★", theme::ACCENT)
        } else {
            ("☆", theme::WEAK_TEXT)
        };
        let button = ui.add_enabled(
            !busy,
            Button::new(RichText::new(star).size(theme::TITLE3).color(color))
                .min_size(vec2(26.0, theme::ROW_HEIGHT - 4.0))
                .frame_when_inactive(false),
        );
        crate::widgets::accessible_label(ui, button.id, &format!("{pin}: {name}"));
        if button.on_hover_text(pin).clicked() {
            actions.push(Action::EditHistory(Change::SetFavorite(
                file.clone(),
                !favorite,
            )));
        }
    });
}
