//! Layouts > Layout Setup... and the layout bar's Layout Setup: FileMaker
//! Pro's Layout Setup dialog (client-ui.md, "Layout Setup") for the layout
//! the window shows in Layout mode. Its controls sit where FileMaker Pro's
//! `IDD_LAYOUT_SETUP.nib` puts them, in a 485 x 428 pt window. OpenMaker
//! sets what the shared operation sets (`DataProvider::set_layout_setup`,
//! authoring operation 16): the Layout Name, "Include in layout menus", the
//! General tab's five options, the views available and the default view,
//! and the Printing tab's fixed page margins (with them, operation 23: one
//! commit).
//! The General tab's pop-ups and Reset Quick Find show the layout's values,
//! not offered, and so do the Printing tab's columns and Facing Pages; the
//! Script Triggers tab is not offered yet.

use egui::{vec2, Align2, CornerRadius, Id, Rect, Stroke, StrokeKind, UiBuilder};
use om_data::layout_catalog::{general, printing, views, LayoutSetupChange, MAX_NAME_CHARS};

use crate::icons::{self, Icon};
use crate::theme;
use crate::widgets;

/// What the dialog's user did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LayoutSetupAnswer {
    Open,
    Cancel,
    Ok(LayoutSetupChange),
}

/// The dialog's state, made when it opens over the layout it sets up.
pub(crate) struct LayoutSetup {
    /// The window that opened it, and the layout it sets up.
    pub(crate) window: u32,
    pub(crate) layout: u32,
    /// The layout's revision as shown, its flags `f1` as stored and the
    /// occurrence it shows records from.
    expected: om_data::layout_placement::Revision,
    stored: u64,
    occurrence: String,
    /// The layout's menu set, as Manage Layouts names the file's own.
    menu_set: String,
    name: String,
    in_menus: bool,
    /// The General tab's options, on or off, in [`general::OPTIONS`]' order.
    options: [bool; 5],
    /// Form, List, Table available, and the default view's index.
    views: [bool; 3],
    default: usize,
    /// The Printing tab: "Use fixed page margins", and the margins shown,
    /// top, bottom, left and right in points, and as stored
    /// (`LayoutDef.customMargins`), if they are.
    fixed: bool,
    margins: [f64; 4],
    stored_margins: Option<[f64; 4]>,
    /// "Print in N columns" as stored, shown, not offered.
    columns: (bool, u32),
    tab: usize,
    /// The Layout Name has had its first frame: it opens with its text
    /// selected and the keyboard, as FileMaker Pro's does.
    named: bool,
}

const VIEW_OFF: [u64; 3] = [views::FORM_OFF, views::LIST_OFF, views::TABLE_OFF];
const VIEW_NAMES: [&str; 3] = ["Form View", "List View", "Table View"];
/// Why an option is shown but not offered.
const NOT_YET: &str = "OpenMaker does not change this option yet.";

impl LayoutSetup {
    /// The dialog for `layout` of window `window`, named `name`, with the
    /// definition `def` (its flags `f1`, menu set, fixed page margins and
    /// columns), in the layout menus or not, at revision `expected`.
    pub(crate) fn new(
        window: u32,
        (layout, expected): (u32, om_data::layout_placement::Revision),
        (name, occurrence): (String, String),
        def: &om_layout::LayoutDef,
        in_menus: bool,
    ) -> Self {
        let (flags, menu_set) = (def.flags, def.menu_set_key);
        // A layout without stored margins shows 36 points, which every
        // starter layout and every new one stores [O, layout-model.md 4.1];
        // with the box checked they are stored, so it prints within them.
        let stored_margins = def
            .custom_margins
            .as_ref()
            .map(|m| [m.top, m.bottom, m.left, m.right]);
        let views = VIEW_OFF.map(|bit| flags & bit == 0);
        let default = if flags & views::DEFAULT_TABLE != 0 {
            2
        } else if flags & views::DEFAULT_LIST != 0 {
            1
        } else {
            0
        };
        Self {
            window,
            layout,
            expected,
            stored: flags,
            occurrence,
            menu_set: match menu_set {
                0 => "[File Default]".into(),
                key => format!("Custom menu set {key}"),
            },
            name,
            in_menus,
            options: general::OPTIONS.map(|(.., bit, set_on)| (flags & bit != 0) == set_on),
            views,
            default,
            fixed: flags & printing::FIXED_PAGE_MARGINS != 0,
            margins: stored_margins.unwrap_or([36.0; 4]),
            stored_margins,
            columns: (
                flags & printing::COLUMNS != 0,
                def.print_column_count.max(1),
            ),
            tab: 0,
            named: false,
        }
    }

    /// The flags the dialog's choices give: the view and option bits set as
    /// chosen.
    fn flags(&self) -> u64 {
        let mut flags = self.stored & !(views::ALL | general::ALL | printing::ALL);
        if self.fixed {
            flags |= printing::FIXED_PAGE_MARGINS;
        }
        for (on, bit) in self.views.iter().zip(VIEW_OFF) {
            if !on {
                flags |= bit;
            }
        }
        for (on, (.., bit, set_on)) in self.options.iter().zip(general::OPTIONS) {
            if *on == set_on {
                flags |= bit;
            }
        }
        flags | [0, views::DEFAULT_LIST, views::DEFAULT_TABLE][self.default]
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> LayoutSetupAnswer {
        let mut answer = LayoutSetupAnswer::Open;
        let frame = egui::Frame::new()
            .fill(theme::DIALOG_BG)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(10))
            .shadow(ctx.global_style().visuals.window_shadow);
        let response = egui::Modal::new(Id::unique("layout_setup"))
            .frame(frame)
            .backdrop_color(theme::SCRIM)
            .show(ctx, |ui| {
                // The title bar, then the nib's 485 x 428 content.
                let (title, _) = ui.allocate_exact_size(vec2(485.0, 28.0), egui::Sense::hover());
                ui.painter().text(
                    title.center(),
                    Align2::CENTER_CENTER,
                    "Layout Setup",
                    egui::FontId::new(13.0, theme::semibold()),
                    theme::TEXT,
                );
                let (window, _) = ui.allocate_exact_size(vec2(485.0, 428.0), egui::Sense::hover());
                let at = |x: f32, y: f32, w: f32, h: f32| {
                    Rect::from_min_size(window.min + vec2(x, y), vec2(w, h))
                };
                // The tab view's box (its frame less AppKit's margins), its
                // tabs centered on its top edge.
                ui.painter().rect(
                    at(20.0, 24.0, 453.0, 354.0),
                    CornerRadius::same(5),
                    theme::BAR_BG,
                    Stroke::new(1.0, theme::BORDER),
                    StrokeKind::Inside,
                );
                let tabs = ["General", "Views", "Printing", "Script Triggers"];
                let width: f32 = tabs
                    .iter()
                    .map(|t| widgets::text_width(ui, t, theme::small()) + 14.0)
                    .sum();
                let strip = at(13.0 + (467.0 - width) / 2.0, 16.0, width, 16.0);
                ui.scope_builder(UiBuilder::new().max_rect(strip), |ui| {
                    if let Some(tab) = widgets::segmented(ui, &tabs, self.tab) {
                        self.tab = tab;
                    }
                });
                // The tab's content, at the nib's {23, 25} 447 x 330.
                let content = |x: f32, y: f32, w: f32, h: f32| at(23.0 + x, 25.0 + y, w, h);
                let place = |ui: &mut egui::Ui, rect: Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.scope_builder(UiBuilder::new().max_rect(rect), add);
                };
                match self.tab {
                    0 => {
                        place(ui, content(49.0, 39.0, 88.0, 17.0), &mut |ui| {
                            ui.label("Layout Name");
                        });
                        place(ui, content(49.0, 61.0, 356.0, 22.0), &mut |ui| {
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut self.name)
                                    .char_limit(MAX_NAME_CHARS)
                                    .desired_width(356.0),
                            );
                            widgets::accessible_label(ui, edit.id, "Layout Name");
                            if !self.named {
                                self.named = true;
                                edit.request_focus();
                                let mut state = egui::TextEdit::load_state(ui.ctx(), edit.id)
                                    .unwrap_or_default();
                                state.cursor.set_char_range(Some(
                                    egui::text::CCursorRange::select_all_str(&self.name),
                                ));
                                state.store(ui.ctx(), edit.id);
                            }
                        });
                        place(ui, content(49.0, 88.0, 176.0, 18.0), &mut |ui| {
                            ui.checkbox(&mut self.in_menus, "Include in layout menus");
                        });
                        place(ui, content(49.0, 123.0, 131.0, 17.0), &mut |ui| {
                            ui.label("Show records from:");
                        });
                        place(ui, content(177.0, 120.0, 226.0, 22.0), &mut |ui| {
                            shown_popup(ui, "Show records from", &self.occurrence, 220.0);
                        });
                        place(ui, content(49.0, 246.0, 71.0, 17.0), &mut |ui| {
                            ui.label("Menu Set:");
                        });
                        place(ui, content(117.0, 244.0, 286.0, 22.0), &mut |ui| {
                            shown_popup(ui, "Menu Set", &self.menu_set, 280.0);
                        });
                        place(ui, content(219.0, 280.0, 180.0, 22.0), &mut |ui| {
                            ui.add_enabled(false, egui::Button::new("Reset Quick Find"))
                                .on_disabled_hover_text(NOT_YET);
                        });
                        // The nib's places, in `general::OPTIONS`' order.
                        let tops = [149.0, 173.0, 197.0, 221.0, 284.0];
                        for ((on, (_, label, ..)), top) in
                            self.options.iter_mut().zip(general::OPTIONS).zip(tops)
                        {
                            place(ui, content(48.0, top, 286.0, 18.0), &mut |ui| {
                                ui.checkbox(on, label);
                            });
                        }
                    }
                    1 => {
                        place(ui, content(72.0, 50.0, 320.0, 17.0), &mut |ui| {
                            ui.label("Views available in Browse mode and Find mode:");
                        });
                        for (index, (icon, top)) in [
                            (Icon::ViewForm, 78.0),
                            (Icon::ViewList, 125.0),
                            (Icon::ViewTable, 172.0),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let picture = content(100.0, top, 48.0, 48.0);
                            icons::paint(
                                ui.painter(),
                                picture.shrink(10.0),
                                icon,
                                theme::ICON,
                                theme::BAR_BG,
                            );
                            place(ui, content(154.0, top + 15.0, 125.0, 18.0), &mut |ui| {
                                ui.checkbox(&mut self.views[index], VIEW_NAMES[index]);
                            });
                        }
                        place(ui, content(252.0, 184.0, 117.0, 22.0), &mut |ui| {
                            ui.add_enabled(false, egui::Button::new("Properties…"))
                                .on_disabled_hover_text(NOT_YET);
                        });
                        place(ui, content(107.0, 230.0, 251.0, 17.0), &mut |ui| {
                            ui.label(
                                egui::RichText::new("(At least one view must be selected)")
                                    .color(theme::CAPTION),
                            );
                        });
                        place(ui, content(107.0, 267.0, 86.0, 17.0), &mut |ui| {
                            ui.label("Default view:");
                        });
                        if !self.views[self.default] {
                            self.default = self.views.iter().position(|on| *on).unwrap_or(0);
                        }
                        let views = self.views;
                        let default = &mut self.default;
                        place(ui, content(190.0, 265.0, 139.0, 22.0), &mut |ui| {
                            let combo = egui::ComboBox::from_id_salt("layout_setup_default")
                                .width(133.0)
                                .selected_text(VIEW_NAMES[*default])
                                .show_ui(ui, |ui| {
                                    for (index, name) in VIEW_NAMES.iter().enumerate() {
                                        if views[index] {
                                            ui.selectable_value(default, index, *name);
                                        }
                                    }
                                });
                            widgets::accessible_label(ui, combo.response.id, "Default view");
                        });
                    }
                    2 => self.printing(ui, &content),
                    _ => {
                        place(ui, content(24.0, 40.0, 400.0, 17.0), &mut |ui| {
                            ui.label(
                                egui::RichText::new("OpenMaker does not change these options yet.")
                                    .color(theme::CAPTION),
                            );
                        });
                    }
                }
                // Cancel and OK, at the nib's places.
                let ready = !self.name.trim().is_empty() && self.views.iter().any(|on| *on);
                // Each hugs the right of its frame: OpenMaker's dialog
                // buttons are narrower than FileMaker Pro's.
                let right = egui::Layout::right_to_left(egui::Align::Center);
                place(ui, at(389.0, 390.0, 84.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "OK", true, ready).clicked() {
                            answer = LayoutSetupAnswer::Ok(LayoutSetupChange {
                                layout: self.layout,
                                expected: self.expected,
                                name: self.name.clone(),
                                flags: self.flags(),
                                hidden: !self.in_menus,
                                margins: self.changed_margins(),
                            });
                        }
                    });
                });
                place(ui, at(293.0, 390.0, 84.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                            answer = LayoutSetupAnswer::Cancel;
                        }
                    });
                });
            });
        if response.should_close() && answer == LayoutSetupAnswer::Open {
            answer = LayoutSetupAnswer::Cancel;
        }
        answer
    }
}

impl LayoutSetup {
    /// The fixed page margins to set with the setup: the ones shown, when
    /// they or "Use fixed page margins" changed, or when none are stored
    /// and the layout is to print within them; `None` leaves the layout's
    /// as they are (operation 16, which a host without operation 23 still
    /// takes).
    fn changed_margins(&self) -> Option<om_layout::Rect> {
        let [top, bottom, left, right] = self.margins;
        let box_changed = self.fixed != (self.stored & printing::FIXED_PAGE_MARGINS != 0);
        let edited = self.margins != self.stored_margins.unwrap_or([36.0; 4]);
        // With the box changed, operation 23 even for the same margins, so
        // a host without it says so; turned on over no stored margins, the
        // shown ones are stored. Otherwise only an edit sends them.
        let changed = edited || (box_changed && (self.stored_margins.is_some() || self.fixed));
        changed.then_some(om_layout::Rect {
            top,
            left,
            bottom,
            right,
            ..Default::default()
        })
    }

    /// The Printing tab, its controls at the nib's places (FileMaker Pro
    /// 26.1.1's `IDD_LAYOUT_SETUP.nib`, its tab view 339 points high). The
    /// margins are edited in points; with "Use fixed page margins" off they
    /// are not offered and show the printer's, as FileMaker Pro's fields do
    /// [O: layout-model.md 4.2], and the stored ones are kept. On a layout
    /// printing in columns they are shown, not offered
    /// (`layout_catalog::plan_setup_with_margins`).
    fn printing(&mut self, ui: &mut egui::Ui, content: &dyn Fn(f32, f32, f32, f32) -> Rect) {
        let place = |ui: &mut egui::Ui, rect: Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.scope_builder(UiBuilder::new().max_rect(rect), add);
        };
        // Shown as stored, not offered.
        let (mut columns_on, mut count) = self.columns;
        place(ui, content(61.0, 56.0, 78.0, 18.0), &mut |ui| {
            ui.add_enabled(false, egui::Checkbox::new(&mut columns_on, "Print in"))
                .on_disabled_hover_text(NOT_YET);
        });
        place(ui, content(138.0, 53.0, 42.0, 22.0), &mut |ui| {
            let field = ui
                .add_enabled(false, egui::DragValue::new(&mut count))
                .on_disabled_hover_text(NOT_YET);
            widgets::accessible_label(ui, field.id, "Columns");
        });
        place(ui, content(187.0, 56.0, 62.0, 17.0), &mut |ui| {
            ui.label("columns");
        });
        for (label, x, width) in [("Across first", 121.0, 113.0), ("Down first", 279.0, 101.0)] {
            place(ui, content(x, 96.0, width, 18.0), &mut |ui| {
                ui.add_enabled(false, egui::RadioButton::new(false, label))
                    .on_disabled_hover_text(NOT_YET);
            });
        }
        let columns = self.columns.0;
        place(ui, content(61.0, 156.0, 180.0, 18.0), &mut |ui| {
            ui.add_enabled(
                !columns,
                egui::Checkbox::new(&mut self.fixed, "Use fixed page margins"),
            )
            .on_disabled_hover_text(
                "OpenMaker keeps the page margins of a layout that prints in columns.",
            );
        });
        let printer = om_layout::paper::printer_margins();
        let shown = if self.fixed {
            self.margins
        } else {
            [printer.top, printer.bottom, printer.left, printer.right]
        };
        // Top and Bottom at the left, Left and Right at the right.
        let sides = [
            ("Top:", 0, 78.0, 55.0, 136.0, 184.0),
            ("Bottom:", 1, 74.0, 60.0, 136.0, 210.0),
            ("Left:", 2, 227.0, 63.0, 292.0, 184.0),
            ("Right:", 3, 227.0, 63.0, 292.0, 210.0),
        ];
        for (label, side, label_x, label_width, field_x, top) in sides {
            place(ui, content(label_x, top, label_width, 17.0), &mut |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(label);
                });
            });
            let mut value = shown[side];
            // Leaving a field reparses the two decimals it shows (28.35
            // for a centimeter's 28.3465), which is no edit.
            let displayed: f64 = format!("{value:.2}").parse().unwrap_or(value);
            let editable = self.fixed && !self.columns.0;
            place(ui, content(field_x, top - 3.0, 60.0, 22.0), &mut |ui| {
                let field = ui.add_enabled(
                    editable,
                    egui::DragValue::new(&mut value)
                        .speed(1.0)
                        .range(0.0..=om_data::layout_placement::MAX_MARGIN)
                        .max_decimals(2),
                );
                widgets::accessible_label(ui, field.id, label.trim_end_matches(':'));
                let field = if self.fixed && self.columns.0 {
                    field.on_disabled_hover_text(
                        "OpenMaker keeps the margins of a layout that prints in columns.",
                    )
                } else {
                    field
                };
                if editable && field.changed() && value != displayed && value != shown[side] {
                    self.margins[side] = value;
                }
            });
            place(ui, content(field_x + 65.0, top, 29.0, 17.0), &mut |ui| {
                ui.label("pt");
            });
        }
        let mut facing = false;
        place(ui, content(292.0, 237.0, 106.0, 18.0), &mut |ui| {
            ui.add_enabled(false, egui::Checkbox::new(&mut facing, "Facing Pages"))
                .on_disabled_hover_text(NOT_YET);
        });
    }
}

/// A pop-up menu showing `text`, not offered (the option is not changed
/// yet).
fn shown_popup(ui: &mut egui::Ui, label: &str, text: &str, width: f32) {
    let combo = ui
        .add_enabled_ui(false, |ui| {
            egui::ComboBox::from_id_salt(("layout_setup", label))
                .width(width)
                .selected_text(text)
                .show_ui(ui, |_| {})
                .response
        })
        .inner;
    widgets::accessible_label(ui, combo.id, label);
    combo.on_disabled_hover_text(NOT_YET);
}
