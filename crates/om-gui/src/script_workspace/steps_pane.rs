//! The steps pane (capture 35): search, FileMaker's categories with
//! disclosure triangles, Favorites, the alphabetical list, and the
//! selected step's description. Double-click or Return inserts the step
//! after the editor's selection.

use std::collections::BTreeSet;

use egui::{pos2, vec2, Align2, Color32, FontId, Key, Rect, Role, Sense, Ui, WidgetInfo};
use om_data::scripts::{self, step_compat, Client};

use super::editor::offered_steps;
use super::glyphs::{self, Glyph};
use crate::theme;

/// FileMaker's categories in the order the pane lists them (35).
pub(super) const CATEGORIES: [&str; 15] = [
    "Favorites",
    "Control",
    "Navigation",
    "Editing",
    "Fields",
    "Records",
    "Found Sets",
    "Windows",
    "Files",
    "Accounts",
    "Artificial Intelligence",
    "Spelling",
    "PDF Files",
    "Open Menu Item",
    "Miscellaneous",
];

const ROW: f32 = 19.0;
/// Height of the description panel (35: from its rule to the window's
/// bottom).
const DESCRIPTION: f32 = 96.0;

#[derive(Default)]
pub(super) struct StepsPane {
    pub search: String,
    open: BTreeSet<&'static str>,
    pub alphabetical: bool,
    pub favorites: Vec<u16>,
    pub selected: Option<u16>,
    /// Button Setup's list: only the steps FileMaker Pro allows on a
    /// button (the step catalog's `not_button_compatible`).
    pub buttons: bool,
}

/// A category's steps, alphabetically (the Claris reference's order; the
/// order FileMaker's pane uses within a category is not documented).
pub(super) fn category_steps(category: &str) -> Vec<u16> {
    let mut steps: Vec<u16> = offered_steps()
        .filter(|id| scripts::step_info(*id).is_some_and(|i| i.category == category))
        .collect();
    steps.sort_by_key(|id| scripts::step_name(*id).unwrap_or_default().to_lowercase());
    steps
}

impl StepsPane {
    /// A row with a step name: selection, double-click inserts.
    fn step_row(
        &mut self,
        ui: &mut Ui,
        id: u16,
        indent: f32,
        compatibility: Option<Client>,
        insert: &mut Option<u16>,
    ) {
        let name = scripts::step_name(id).unwrap_or_default();
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
        let selected = self.selected == Some(id);
        if selected {
            ui.painter()
                .rect_filled(rect.shrink2(vec2(4.0, 0.0)), 4.0, theme::SELECTED);
        }
        let supported = compatibility.is_none_or(|c| step_compat(id).is_none_or(|s| s.supports(c)));
        let color = match (selected, supported) {
            (true, _) => Color32::WHITE,
            (false, true) => theme::TEXT,
            (false, false) => theme::WEAK_TEXT,
        };
        ui.painter().text(
            pos2(rect.left() + indent, rect.center().y),
            Align2::LEFT_CENTER,
            name,
            theme::small(),
            color,
        );
        response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, name));
        if response.clicked() {
            self.selected = Some(id);
        }
        if response.double_clicked() {
            *insert = Some(id);
        }
    }

    /// Whether the pane lists `id`.
    fn lists(&self, id: u16) -> bool {
        !self.buttons || step_compat(id).is_some_and(|c| c.button_compatible())
    }

    /// Draw the pane; returns a step to insert.
    pub fn ui(&mut self, ui: &mut Ui, compatibility: Option<Client>, focused: bool) -> Option<u16> {
        let mut insert = None;
        let full = ui.available_rect_before_wrap();
        // Header: "Steps", the favorite star and the alphabetical toggle.
        let header = Rect::from_min_size(full.min, vec2(full.width(), 26.0));
        ui.painter().text(
            pos2(header.left() + 6.0, header.center().y),
            Align2::LEFT_CENTER,
            "Steps",
            FontId::new(theme::SMALL, theme::semibold()),
            theme::TEXT,
        );
        let favorite = self.selected.is_some_and(|id| self.favorites.contains(&id));
        let star_rect = Rect::from_center_size(
            pos2(header.right() - 42.0, header.center().y),
            vec2(18.0, 18.0),
        );
        let can_favor = self.selected.is_some() && !self.alphabetical;
        let star = ui.interact(star_rect, ui.scope_id().with("favorite"), Sense::click());
        glyphs::paint(
            ui.painter(),
            star_rect,
            if favorite {
                Glyph::StarFilled
            } else {
                Glyph::Star
            },
            if can_favor {
                theme::ICON
            } else {
                theme::DISABLED
            },
        );
        let label = if favorite {
            "Remove from Favorites"
        } else {
            "Add to Favorites"
        };
        star.widget_info(|| WidgetInfo::labeled(Role::Button, can_favor, label));
        if star.clicked() && can_favor {
            let id = self.selected.expect("selected");
            if favorite {
                self.favorites.retain(|f| *f != id);
            } else {
                self.favorites.push(id);
            }
        }
        star.on_hover_text(label);
        let alpha_rect = Rect::from_center_size(
            pos2(header.right() - 18.0, header.center().y),
            vec2(18.0, 18.0),
        );
        let alpha = ui.interact(
            alpha_rect,
            ui.scope_id().with("alphabetical"),
            Sense::click(),
        );
        glyphs::paint(
            ui.painter(),
            alpha_rect,
            Glyph::Alphabetical,
            if self.alphabetical {
                theme::SELECTED
            } else {
                theme::ICON
            },
        );
        alpha.widget_info(|| {
            WidgetInfo::selected(
                Role::CheckBox,
                true,
                self.alphabetical,
                "Sort Alphabetically",
            )
        });
        if alpha.clicked() {
            self.alphabetical = !self.alphabetical;
        }
        alpha.on_hover_text("View steps alphabetically or by category");
        ui.painter().hline(
            full.x_range(),
            header.bottom(),
            egui::Stroke::new(1.0, theme::HAIRLINE),
        );
        ui.advance_cursor_after_rect(header);
        ui.add_space(6.0);
        super::search_field(ui, &mut self.search, "Search Steps", false);
        ui.add_space(6.0);
        let list_height = (full.bottom() - ui.cursor().top() - DESCRIPTION).max(40.0);
        egui::ScrollArea::vertical()
            .id_salt("steps_pane")
            .max_height(list_height)
            .min_scrolled_height(list_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let query = self.search.trim().to_lowercase();
                if !query.is_empty() || self.alphabetical {
                    let mut all: Vec<u16> = offered_steps()
                        .filter(|id| {
                            self.lists(*id)
                                && scripts::step_name(*id)
                                    .is_some_and(|n| n.to_lowercase().contains(&query))
                        })
                        .collect();
                    all.sort_by_key(|id| {
                        scripts::step_name(*id).unwrap_or_default().to_lowercase()
                    });
                    for id in all {
                        self.step_row(ui, id, 12.0, compatibility, &mut insert);
                    }
                    return;
                }
                for category in CATEGORIES {
                    let open = self.open.contains(category);
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                    glyphs::paint(
                        ui.painter(),
                        Rect::from_center_size(
                            pos2(rect.left() + 14.0, rect.center().y),
                            vec2(12.0, 12.0),
                        ),
                        if open {
                            Glyph::DisclosureDown
                        } else {
                            Glyph::DisclosureRight
                        },
                        theme::CAPTION,
                    );
                    ui.painter().text(
                        pos2(rect.left() + 26.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        category,
                        theme::small(),
                        theme::TEXT,
                    );
                    response
                        .widget_info(|| WidgetInfo::selected(Role::Button, true, open, category));
                    if response.clicked() {
                        if open {
                            self.open.remove(category);
                        } else {
                            self.open.insert(category);
                        }
                    }
                    if open {
                        let steps = if category == "Favorites" {
                            self.favorites.clone()
                        } else {
                            category_steps(category)
                        };
                        let steps: Vec<u16> =
                            steps.into_iter().filter(|id| self.lists(*id)).collect();
                        for id in steps {
                            self.step_row(ui, id, 38.0, compatibility, &mut insert);
                        }
                    }
                }
            });
        if focused
            && ui.input(|i| i.key_pressed(Key::Enter))
            && !ui.ctx().egui_wants_keyboard_input()
        {
            insert = insert.or(self.selected);
        }
        self.description(ui, full);
        insert
    }

    /// The Description panel at the pane's bottom.
    fn description(&self, ui: &mut Ui, full: Rect) {
        let rect = Rect::from_min_max(pos2(full.left(), full.bottom() - DESCRIPTION), full.max);
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, theme::CONTENT_BG);
        painter.hline(
            full.x_range(),
            rect.top(),
            egui::Stroke::new(1.0, theme::HAIRLINE),
        );
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), 22.0));
        painter.rect_filled(header, 0.0, theme::TOOLBAR_BG);
        painter.hline(
            full.x_range(),
            header.bottom(),
            egui::Stroke::new(1.0, theme::HAIRLINE),
        );
        painter.text(
            pos2(header.left() + 6.0, header.center().y),
            Align2::LEFT_CENTER,
            "Description",
            FontId::new(theme::SMALL, theme::semibold()),
            theme::TEXT,
        );
        glyphs::paint(
            painter,
            Rect::from_center_size(
                pos2(header.right() - 16.0, header.center().y),
                vec2(14.0, 14.0),
            ),
            Glyph::Help,
            theme::WEAK_TEXT,
        );
        let body = Rect::from_min_max(
            pos2(rect.left() + 8.0, header.bottom() + 6.0),
            rect.max - vec2(8.0, 4.0),
        );
        let description = self
            .selected
            .and_then(scripts::step_info)
            .map(|i| i.description.as_str())
            .filter(|d| !d.is_empty());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
        match description {
            Some(text) => {
                child.add(egui::Label::new(
                    egui::RichText::new(text)
                        .size(theme::SMALL)
                        .color(theme::TEXT),
                ));
            }
            None => {
                child.vertical_centered(|ui| {
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("No description")
                            .size(theme::SMALL)
                            .strong()
                            .color(theme::DISABLED),
                    );
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_step_is_in_one_of_filemakers_categories() {
        let listed: usize = CATEGORIES[1..]
            .iter()
            .map(|c| category_steps(c).len())
            .sum();
        assert_eq!(listed, offered_steps().count());
        let control = category_steps("Control");
        assert!(control.contains(&68) && control.contains(&141));
        let names: Vec<_> = control
            .iter()
            .map(|id| scripts::step_name(*id).unwrap())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|n| n.to_lowercase());
        assert_eq!(names, sorted);
    }
}
