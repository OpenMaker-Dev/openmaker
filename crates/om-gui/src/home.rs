//! OpenMaker's Home window, laid out as FileMaker Pro 26.1.1's Home
//! (docs/re/client-ui.md "Home window"): the app's name in a header band;
//! Create, Open and the page list in a sidebar; and the page itself, with
//! Favorites and Recent tabs over a Name and Location table. Its commands
//! use the same actions as the File menu. Geometry and colors are measured
//! from FileMaker; where FileMaker has Claris's FileMaker Cloud and
//! Resources pages, OpenMaker lists Hosts (docs/desktop-client.md).

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Galley, Key, Layout,
    Modifiers, Rect, Response, Sense, Stroke, Ui, UiBuilder,
};

use crate::app::Action;
use crate::file_history::{Change, History, SavedFile, Tab};
use crate::icons::{self, Icon};
use crate::theme;

// FileMaker's Home, in points from the top-left of the window's content
// (below the title bar). Main-pane x positions are from the sidebar's edge;
// the title's and tabs' allow for Inter's side bearings, so their ink lands
// where FileMaker's does.
const HEADER_HEIGHT: f32 = 51.0;
const WORDMARK_X: f32 = 22.0;
const SIDEBAR_WIDTH: f32 = 198.0;
const CAPSULE: Rect = Rect::from_min_max(pos2(25.0, 84.0), pos2(169.0, 112.0));
const CAPSULE_PITCH: f32 = 40.0;
const SIDEBAR_RULE_Y: f32 = 179.5;
const NAV: Rect = Rect::from_min_max(pos2(20.0, 198.0), pos2(176.0, 228.0));
const NAV_PITCH: f32 = 38.0;
const NAV_ICON_X: f32 = 40.0;
const NAV_ICON: f32 = 17.0;
const NAV_TEXT_X: f32 = 58.5;
const TITLE_X: f32 = 27.0;
const TITLE_Y: f32 = 103.75;
const TAB_X: f32 = 21.0;
const TAB_GAP: f32 = 12.0;
const TAB_Y: f32 = 140.75;
const TAB_RULE_Y: f32 = 152.0;
const TAB_RULE: f32 = 2.0;
const TABLE_X: f32 = 13.0;
const TABLE_RIGHT_INSET: f32 = 38.0;
const COLUMN_RULE_TOP: f32 = 167.5;
const HEADER_RULE_Y: f32 = 189.5;
const HEADER_TEXT_Y: f32 = 175.5;
const NAME_COLUMN: f32 = 252.0;
const TRAILING_COLUMN: f32 = 13.0;
const HEADER_NAME_X: f32 = 21.0;
const HEADER_LOCATION_X: f32 = 14.0;
const ROWS_TOP: f32 = 194.5;
const ROW_HEIGHT: f32 = 50.0;
const ROW_ICON_X: f32 = 19.0;
const ROW_ICON: f32 = 32.0;
const ROW_NAME_X: f32 = 59.5;
const ROW_LOCATION_X: f32 = 11.0;
const CELL_PADDING: f32 = 11.0;

/// What Home shows and may do this frame.
pub(crate) struct HomeView<'a> {
    pub history: &'a History,
    /// A file is opening: Home's commands wait for it.
    pub busy: bool,
    /// No dialog or alert is in front of Home: its controls and keys work.
    pub in_front: bool,
    /// Why Recent and Favorites could not be read or saved, when they could
    /// not; the lists then last until the app quits.
    pub history_error: Option<&'a str>,
}

/// Home's own state: the selected row of the shown list.
#[derive(Default)]
pub(crate) struct Home {
    selected: Option<SavedFile>,
    reveal_selected: bool,
}

impl Home {
    pub fn show(&mut self, ui: &mut Ui, view: HomeView<'_>, actions: &mut Vec<Action>) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::CONTENT_BG))
            .show(ui, |ui| self.contents(ui, &view, actions));
    }

    fn contents(&mut self, ui: &mut Ui, view: &HomeView<'_>, actions: &mut Vec<Action>) {
        let area = ui.max_rect();
        let at = |x: f32, y: f32| area.min + vec2(x, y);
        let painter = ui.painter().clone();
        let rule = Stroke::new(1.0, theme::SEPARATOR);
        let enabled = !view.busy && view.in_front;

        // The header band, with the app's name where FileMaker has its logo.
        painter.text(
            at(WORDMARK_X, HEADER_HEIGHT / 2.0),
            Align2::LEFT_CENTER,
            crate::APP_TITLE,
            FontId::new(theme::TITLE2, theme::semibold()),
            theme::TEXT,
        );
        painter.hline(area.x_range(), at(0.0, HEADER_HEIGHT + 0.5).y, rule);

        // The sidebar: Create and Open, a rule, then the pages.
        painter.vline(
            at(SIDEBAR_WIDTH, 0.0).x,
            (area.top() + HEADER_HEIGHT + 1.0)..=area.bottom(),
            rule,
        );
        for (index, (label, action)) in [
            ("Create", Action::ShowCreateNew),
            ("Open", Action::ShowOpenFile),
        ]
        .into_iter()
        .enumerate()
        {
            let rect =
                CAPSULE.translate(area.min.to_vec2() + vec2(0.0, CAPSULE_PITCH * index as f32));
            if capsule(ui, rect, label, enabled).clicked() {
                actions.push(action);
            }
        }
        painter.hline(
            area.left()..=at(SIDEBAR_WIDTH - 0.5, 0.0).x,
            at(0.0, SIDEBAR_RULE_Y).y,
            rule,
        );
        let tab = view.history.tab();
        for (index, (label, icon, selected, action)) in [
            ("Home", Icon::Home, true, Action::ShowHome(tab)),
            ("Hosts", Icon::Server, false, Action::ShowHosts),
        ]
        .into_iter()
        .enumerate()
        {
            let rect = NAV.translate(area.min.to_vec2() + vec2(0.0, NAV_PITCH * index as f32));
            if navigation(ui, rect, label, icon, selected, enabled).clicked() {
                actions.push(action);
            }
        }

        // The page: its title, the tabs, and the selected list.
        let pane = Rect::from_min_max(at(SIDEBAR_WIDTH, HEADER_HEIGHT), area.max);
        let px = |x: f32| pane.left() + x;
        painter.text(
            pos2(px(TITLE_X), at(0.0, TITLE_Y).y),
            Align2::LEFT_CENTER,
            "Home",
            FontId::new(theme::PAGE_TITLE, theme::semibold()),
            theme::TEXT,
        );
        let mut x = px(TAB_X);
        for shown in [Tab::Favorites, Tab::Recent] {
            let (response, text) = tab_button(
                ui,
                pos2(x, at(0.0, TAB_Y).y),
                at(0.0, TAB_RULE_Y).y,
                shown,
                shown == tab,
                enabled,
            );
            if response.clicked() && shown != tab {
                actions.push(Action::ShowHome(shown));
            }
            x = text.right() + TAB_GAP;
        }
        let table = Rect::from_min_max(
            pos2(px(TABLE_X), at(0.0, COLUMN_RULE_TOP).y),
            pos2(area.right() - TABLE_RIGHT_INSET, area.bottom()),
        );
        status(ui, view, pos2(table.right(), at(0.0, TAB_Y).y));
        self.table(ui, table, view, actions);
    }

    fn table(&mut self, ui: &mut Ui, table: Rect, view: &HomeView<'_>, actions: &mut Vec<Action>) {
        let painter = ui.painter().clone();
        let rule = Stroke::new(1.0, theme::SEPARATOR);
        let header_rule = table.top() + (HEADER_RULE_Y - COLUMN_RULE_TOP);
        let name_rule = table.left() + NAME_COLUMN;
        let trailing_rule = table.right() - TRAILING_COLUMN;
        for x in [name_rule, trailing_rule] {
            painter.vline(x, table.top()..=header_rule, rule);
        }
        painter.hline(table.x_range(), header_rule, rule);
        let header_y = table.top() + (HEADER_TEXT_Y - COLUMN_RULE_TOP);
        for (x, label) in [
            (table.left() + HEADER_NAME_X, "Name"),
            (name_rule + HEADER_LOCATION_X, "Location"),
        ] {
            painter.text(
                pos2(x, header_y),
                Align2::LEFT_CENTER,
                label,
                theme::small(),
                theme::TEXT,
            );
        }

        let tab = view.history.tab();
        let files = view.history.files(tab);
        let rows = Rect::from_min_max(
            pos2(table.left(), table.top() + (ROWS_TOP - COLUMN_RULE_TOP)),
            table.max,
        );
        let mut rows_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(rows)
                .layout(Layout::top_down(Align::Min)),
        );
        if files.is_empty() {
            empty(&mut rows_ui, tab);
            return;
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|file| !files.contains(file))
        {
            self.selected = None;
        }
        let enabled = !view.busy && view.in_front;
        if enabled {
            self.keys(ui, files, tab, actions);
        }
        let columns = Columns {
            name: name_rule - table.left(),
            location: trailing_rule - name_rule,
        };
        egui::ScrollArea::vertical()
            .id_salt(("home_files", tab.label()))
            .auto_shrink([false, false])
            .show(&mut rows_ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for file in files {
                    let selected = self.selected.as_ref() == Some(file);
                    let response =
                        row(ui, view.history, file, selected, &columns, enabled, actions);
                    if response.clicked() || response.secondary_clicked() {
                        self.selected = Some(file.clone());
                    }
                    if response.double_clicked() && enabled {
                        actions.push(Action::OpenSaved(file.clone()));
                    }
                    if selected && std::mem::take(&mut self.reveal_selected) {
                        response.scroll_to_me(None);
                    }
                }
            });
    }

    /// The list's keys, as a macOS table takes them: arrows move the
    /// selection, Return opens the selected file and Delete removes it.
    fn keys(&mut self, ui: &Ui, files: &[SavedFile], tab: Tab, actions: &mut Vec<Action>) {
        let free = ui
            .ctx()
            .memory(|memory| memory.focused().is_none() && memory.top_modal_layer().is_none());
        if !free {
            return;
        }
        let index = self
            .selected
            .as_ref()
            .and_then(|selected| files.iter().position(|file| file == selected));
        let (down, up, open, remove) = ui.input_mut(|input| {
            (
                input.consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::ArrowUp),
                input.consume_key(Modifiers::NONE, Key::Enter),
                input.consume_key(Modifiers::NONE, Key::Delete)
                    || input.consume_key(Modifiers::NONE, Key::Backspace),
            )
        });
        let moved = match (index, down, up) {
            (None, true, _) => Some(0),
            (None, _, true) => Some(files.len() - 1),
            (Some(index), true, _) => Some((index + 1).min(files.len() - 1)),
            (Some(index), _, true) => Some(index.saturating_sub(1)),
            _ => None,
        };
        if let Some(moved) = moved {
            self.selected = Some(files[moved].clone());
            self.reveal_selected = true;
        }
        let Some(selected) = self.selected.clone() else {
            return;
        };
        if open {
            actions.push(Action::OpenSaved(selected));
        } else if remove {
            actions.push(Action::EditHistory(removal(tab, selected)));
        }
    }
}

/// What Remove does in each list: Recent forgets the file, Favorites
/// unpins it. Neither touches the file.
fn removal(tab: Tab, file: SavedFile) -> Change {
    match tab {
        Tab::Recent => Change::RemoveRecent(file),
        Tab::Favorites => Change::SetFavorite(file, false),
    }
}

/// A capsule button: Home's Create and Open.
fn capsule(ui: &mut Ui, rect: Rect, label: &str, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, ui.scope_id().with(("home_capsule", label)), sense);
    let fill = if enabled && response.is_pointer_button_down_on() {
        theme::HOVER_FILL
    } else {
        theme::CAPSULE_FILL
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), fill);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::body(),
        if enabled {
            theme::TEXT
        } else {
            theme::DISABLED
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    response
}

/// A page in the sidebar, as FileMaker draws it: a glyph and gray text, the
/// selected page on a light gray fill.
fn navigation(
    ui: &mut Ui,
    rect: Rect,
    label: &str,
    icon: Icon,
    selected: bool,
    enabled: bool,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, ui.scope_id().with(("home_navigation", label)), sense);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::CONTROL_RADIUS),
            theme::NAV_SELECTION,
        );
    }
    let color = if enabled {
        theme::CAPTION
    } else {
        theme::DISABLED
    };
    icons::paint(
        painter,
        Rect::from_center_size(
            pos2(rect.left() + (NAV_ICON_X - NAV.left()), rect.center().y),
            vec2(NAV_ICON, NAV_ICON),
        ),
        icon,
        if enabled {
            theme::ICON
        } else {
            theme::DISABLED
        },
        if selected {
            theme::NAV_SELECTION
        } else {
            theme::CONTENT_BG
        },
    );
    painter.text(
        pos2(rect.left() + (NAV_TEXT_X - NAV.left()), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        theme::body(),
        color,
    );
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, label));
    response
}

/// One of the page's tabs: gray text, the selected one underlined in the
/// accent from `rule_top`. Also returns where its text is.
fn tab_button(
    ui: &mut Ui,
    left_center: egui::Pos2,
    rule_top: f32,
    tab: Tab,
    selected: bool,
    enabled: bool,
) -> (Response, Rect) {
    let label = tab.label();
    let galley = ui
        .painter()
        .layout_no_wrap(label.into(), theme::body(), theme::CAPTION);
    let text = Rect::from_min_size(
        left_center - vec2(0.0, galley.size().y / 2.0),
        galley.size(),
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(
        text.expand2(vec2(4.0, 6.0)),
        ui.scope_id().with(("home_tab", label)),
        sense,
    );
    let painter = ui.painter();
    painter.galley(text.min, galley, theme::CAPTION);
    if selected {
        painter.rect_filled(
            Rect::from_min_max(
                pos2(text.left() - 2.0, rule_top),
                pos2(text.right(), rule_top + TAB_RULE),
            ),
            CornerRadius::ZERO,
            theme::ACCENT,
        );
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, enabled, selected, label));
    (response, text)
}

/// At the tabs' right end: a file opening, or why the lists can't be saved.
fn status(ui: &mut Ui, view: &HomeView<'_>, right_center: egui::Pos2) {
    let rect = Rect::from_min_max(
        right_center - vec2(320.0, 10.0),
        right_center + vec2(0.0, 10.0),
    );
    let mut status_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if view.busy {
        status_ui.label(
            egui::RichText::new("Opening…")
                .font(theme::small())
                .color(theme::CAPTION),
        );
        status_ui.spinner();
    } else if let Some(error) = view.history_error {
        status_ui
            .label(
                egui::RichText::new("Recent and Favorites can't be saved")
                    .font(theme::small())
                    .color(theme::YELLOW),
            )
            .on_hover_text(error);
    }
}

fn empty(ui: &mut Ui, tab: Tab) {
    let (heading, detail) = match tab {
        Tab::Recent => ("No recent files", "Files you open appear here."),
        Tab::Favorites => (
            "No favorite files",
            "Right-click a recent file and choose Add to Favorites.",
        ),
    };
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.3).max(24.0));
        ui.label(
            egui::RichText::new(heading)
                .font(theme::body())
                .color(theme::CAPTION),
        );
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(detail)
                .font(theme::small())
                .color(theme::CAPTION),
        );
    });
}

/// The widths of the Name and Location columns.
struct Columns {
    name: f32,
    location: f32,
}

/// One file: its glyph and name, and where it is. A click selects it, a
/// double click opens it, and its context menu pins or removes it.
fn row(
    ui: &mut Ui,
    history: &History,
    file: &SavedFile,
    selected: bool,
    columns: &Columns,
    enabled: bool,
    actions: &mut Vec<Action>,
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let name = file.name();
    let location = file.location();
    let (ink, glyph) = if selected {
        (theme::ON_ACCENT, theme::ON_ACCENT)
    } else {
        (theme::TEXT, theme::ICON)
    };
    let painter = ui.painter();
    if selected {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::CONTROL_RADIUS),
            theme::SELECTED,
        );
    }
    let icon = match file {
        SavedFile::Local { .. } => Icon::File,
        SavedFile::Hosted { .. } => Icon::Server,
    };
    icons::paint(
        painter,
        Rect::from_min_size(
            pos2(rect.left() + ROW_ICON_X, rect.center().y - ROW_ICON / 2.0),
            vec2(ROW_ICON, ROW_ICON),
        ),
        icon,
        glyph,
        if selected {
            theme::SELECTED
        } else {
            theme::CONTENT_BG
        },
    );
    let name_x = rect.left() + ROW_NAME_X;
    let location_x = rect.left() + columns.name + ROW_LOCATION_X;
    for (x, text, width) in [
        (name_x, &name, columns.name - ROW_NAME_X - CELL_PADDING),
        (
            location_x,
            &location,
            columns.location - ROW_LOCATION_X - CELL_PADDING,
        ),
    ] {
        let galley = truncated(painter, text, ink, width);
        let top = rect.center().y - galley.size().y / 2.0;
        painter.galley(pos2(x, top), galley, ink);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::ListBoxOption, enabled, selected, &name)
    });
    let response = response.on_hover_text(&location);
    if enabled {
        let favorite = history.files(Tab::Favorites).contains(file);
        response.context_menu(|ui| {
            if history.tab() == Tab::Recent {
                let pin = if favorite {
                    "Remove from Favorites"
                } else {
                    "Add to Favorites"
                };
                if ui.button(pin).clicked() {
                    actions.push(Action::EditHistory(Change::SetFavorite(
                        file.clone(),
                        !favorite,
                    )));
                    ui.close();
                }
            }
            if ui.button("Remove").clicked() {
                actions.push(Action::EditHistory(removal(history.tab(), file.clone())));
                ui.close();
            }
        });
    }
    response
}

/// `text` on one line, cut with an ellipsis at `width`.
fn truncated(painter: &egui::Painter, text: &str, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat::simple(FontId::new(theme::BODY, FontFamily::Proportional), color),
    );
    job.wrap = TextWrapping::truncate_at_width(width.max(0.0));
    painter.layout_job(job)
}
