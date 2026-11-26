//! Layouts > Part Setup...: FileMaker Pro's Part Setup dialog and its Part
//! Definition (re/client-ui.md, "Part Setup") for the layout the window
//! shows in Layout mode, their controls where Pro's `IDD_PART_SETUP.nib` and
//! `IDD_PART_DEFINITION.nib` put them. Change sets a part's page options
//! with the shared operation (`DataProvider::set_part_options`, authoring
//! operation 19), offering only what Pro's Part Definition offers for the
//! part's type (layout-model.md 10.15). Create adds a part of a type the
//! layout does not have, and Delete removes an empty part, with the shared
//! operation (`DataProvider::set_layout_parts`, authoring operation 22,
//! layout-model.md 10.18). A part's type and its row states are shown, not
//! offered.

use egui::{vec2, Align2, CornerRadius, Id, Rect, Stroke, UiBuilder};
use om_data::layout_placement::{
    part_options, part_rank, PartChange, PartOptionsChange, PartsChange, Revision, ADDABLE_PARTS,
};

use crate::{theme, widgets};

/// A part as Part Setup lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PartRow {
    pub id: u64,
    pub part_type: i32,
    pub flags: u64,
    pub page_break_after: u32,
    /// Its height, in points.
    pub height: f64,
    /// A sub-summary's sort field: its field id and name.
    pub sorted_by: Option<(u64, String)>,
    /// How many of the layout's objects it holds.
    pub objects: usize,
}

impl PartRow {
    /// The part's name, as FileMaker Pro's Save as XML names parts
    /// (layout-model.md 10.15).
    fn name(&self) -> String {
        let by = |end: &str| {
            let field = self.sorted_by.as_ref().map_or("?", |(_, name)| name);
            format!("Sub-summary by {field} ({end})")
        };
        match self.part_type {
            0 => "Title Header".into(),
            1 => "Header".into(),
            2 => "Leading Grand Summary".into(),
            3 => by("Leading"),
            4 => "Body".into(),
            5 => by("Trailing"),
            6 => "Trailing Grand Summary".into(),
            7 => "Footer".into(),
            8 => "Title Footer".into(),
            12 => "Top Navigation".into(),
            13 => "Bottom Navigation".into(),
            other => format!("Part of type {other}"),
        }
    }
}

/// What the dialog's user did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PartSetupAnswer {
    Open,
    Done,
    /// Part Definition's OK: the part's new options, to send.
    Change(PartOptionsChange),
    /// Create's Part Definition OK, or Delete: a part to add or delete.
    Parts(PartsChange),
}

/// The height Create gives a new part: what FileMaker Pro 26.0.3's Create
/// gave a Header (110 points) and a Footer, Title Header or Leading Grand
/// Summary (22) in `227-parts-objects/` [O]; 22 for the other kinds
/// [OpenMaker's choice].
fn new_part_height(part_type: i32) -> f64 {
    if part_type == 1 {
        110.0
    } else {
        22.0
    }
}

/// Part Definition's state for the part being changed.
struct Definition {
    index: usize,
    flags: u64,
    every: String,
    sorted_by: Option<u64>,
}

/// Part Setup's state, made when it opens over the layout it sets up.
pub(crate) struct PartSetup {
    /// The window that opened it, and the layout it sets up.
    pub(crate) window: u32,
    pub(crate) layout: u32,
    expected: Revision,
    /// The layout's occurrence and its base table, and that table's fields
    /// (id, name), which a sub-summary may be sorted by.
    occurrence: (u64, u64),
    fields: Vec<(u64, String)>,
    parts: Vec<PartRow>,
    selected: Option<usize>,
    definition: Option<Definition>,
    /// A change is with the host, and the row as it will be: the dialog
    /// takes no input until the host answers that request.
    waiting: Option<(PartOptionsChange, PartRow)>,
    /// Create's Part Definition is open, with the type chosen, if any.
    creating: Option<Option<i32>>,
    /// A part added or deleted is with the host.
    waiting_parts: Option<PartsChange>,
    /// Opened by the Part tool or Insert > Part: Part Definition alone,
    /// without Part Setup's list. It stays, taking no input, while its OK is
    /// with the host, and closes with Cancel or once the host has answered.
    alone: bool,
    /// Opened by the Part tool dropped below the layout's parts: the new
    /// part's height, from the layout's bottom to the drop point, and the
    /// rank (`part_rank`) of the last part, which the kinds offered follow.
    dropped: Option<(f64, Option<u8>)>,
}

/// FileMaker Pro 26.1.1's alert for the Part tool dropped where it adds no
/// part (re/client-ui.md, "The Part tool").
const CANNOT_POSITION: &str = "This part cannot be positioned here.";

/// The farthest below the layout's last part the Part tool adds a part:
/// the farthest FileMaker Pro 26.1.1 was seen to add one (25 points below a
/// Form's Body). It added none 50 points below that Body, nor about 22
/// below a List's Footer, so its own limit is not known.
const MAX_DROP_HEIGHT: f64 = 25.0;

/// Why a control is shown but not offered.
const NOT_YET: &str = "OpenMaker does not change this yet.";

/// The radio buttons of the part types, top to bottom, at the nib's
/// tops, and the types each stands for (a sub-summary's end is its place).
const TYPES: [(&str, &[i32], f32); 10] = [
    ("Top Navigation", &[12], 20.0),
    ("Title Header", &[0], 42.0),
    ("Header", &[1], 64.0),
    ("Leading Grand Summary", &[2], 86.0),
    ("Body", &[4], 108.0),
    ("Sub-summary when sorted by", &[3, 5], 130.0),
    ("Trailing Grand Summary", &[6], 152.0),
    ("Footer", &[7], 174.0),
    ("Title Footer", &[8], 196.0),
    ("Bottom Navigation", &[13], 218.0),
];

impl PartSetup {
    pub(crate) fn new(
        window: u32,
        (layout, expected): (u32, Revision),
        occurrence: (u64, u64),
        fields: Vec<(u64, String)>,
        parts: Vec<PartRow>,
    ) -> Self {
        Self {
            window,
            layout,
            expected,
            occurrence,
            fields,
            parts,
            selected: None,
            definition: None,
            waiting: None,
            creating: None,
            waiting_parts: None,
            alone: false,
            dropped: None,
        }
    }

    /// This dialog as Insert > Part (`dropped_at` None) or the Part tool
    /// opens it: Part Definition alone. Insert > Part offers the kinds of
    /// part Create adds that the layout lacks. The Part tool, dropped at
    /// layout point `dropped_at` (its y), adds a part below the layout's
    /// last part reaching down to that point, and offers the kinds that go
    /// after the last part, as FileMaker Pro 26.1.1's did; a drop within
    /// the parts is refused, as Pro added a part at none it was tried at
    /// (re/client-ui.md, "The Part tool"). Refused, with the reason, when no
    /// kind is offered (OpenMaker adds no sub-summaries, which FileMaker Pro
    /// adds any number of).
    pub(crate) fn creating_alone(mut self, dropped_at: Option<f64>) -> Result<Self, &'static str> {
        if let Some(y) = dropped_at {
            let bottom: f64 = self.parts.iter().map(|p| p.height).sum();
            // Not a number when a stored height is not one: refused.
            let height = (y - bottom).round();
            // The host puts a new part before the first part ranked after
            // it, which is after the last part only when the parts are in
            // FileMaker's order.
            let ranks: Option<Vec<u8>> =
                self.parts.iter().map(|p| part_rank(p.part_type)).collect();
            let Some(ranks) = ranks.filter(|ranks| ranks.is_sorted()) else {
                return Err(CANNOT_POSITION);
            };
            let last = ranks.last().copied();
            // Nothing goes after a Bottom Navigation part.
            if !(1.0..=MAX_DROP_HEIGHT).contains(&height) || last == part_rank(13) {
                return Err(CANNOT_POSITION);
            }
            self.dropped = Some((height, last));
        }
        // The first kind offered is chosen, as FileMaker Pro 26.1.1's chose
        // Title Header for Insert > Part, and the first kind that follows
        // the last part for the Part tool (re/client-ui.md, "The Part
        // tool").
        let Some(first) = TYPES
            .iter()
            .find(|(_, types, _)| types.len() == 1 && self.offered(types[0]))
            .map(|(_, types, _)| types[0])
        else {
            return Err(if self.dropped.is_some() {
                "The layout has every kind of part OpenMaker adds below its last part."
            } else {
                "The layout has every kind of part OpenMaker adds."
            });
        };
        self.creating = Some(Some(first));
        self.alone = true;
        Ok(self)
    }

    /// The host's answer to `sent`, a part added or deleted: saved, the
    /// list shows the parts keyed again from the top and later changes go
    /// at the layout's new revision; refused, it is as it was. False, and
    /// nothing changes, when `sent` is not the request the dialog waits for
    /// (a boundary dragged, or a dialog closed and opened again since).
    pub(crate) fn parts_answered(
        &mut self,
        sent: &PartsChange,
        revision: Option<Revision>,
    ) -> bool {
        if self.waiting_parts.as_ref() != Some(sent) {
            return false;
        }
        self.waiting_parts = None;
        if self.alone {
            // Part Definition alone closes with the answer.
            self.creating = None;
        }
        let Some(revision) = revision else {
            return true;
        };
        self.expected = revision;
        self.selected = None;
        match sent.change {
            PartChange::Add { part_type, size } => {
                let at = self
                    .parts
                    .iter()
                    .position(|p| part_rank(p.part_type) > part_rank(part_type))
                    .unwrap_or(self.parts.len());
                self.parts.insert(
                    at,
                    PartRow {
                        id: 0,
                        part_type,
                        flags: 0,
                        page_break_after: 0,
                        height: size,
                        sorted_by: None,
                        objects: 0,
                    },
                );
            }
            PartChange::Delete { part, .. } => self.parts.retain(|p| p.id != part),
            PartChange::Resize { .. } => {}
        }
        for (i, part) in self.parts.iter_mut().enumerate() {
            part.id = i as u64 + 1;
        }
        true
    }

    /// The part types Create adds: those it adds that the layout lacks.
    fn creatable(&self, part_type: i32) -> bool {
        ADDABLE_PARTS.contains(&part_type) && !self.parts.iter().any(|p| p.part_type == part_type)
    }

    /// The part types Part Definition offers: those Create adds, and for
    /// the Part tool only those that go after the layout's last part.
    fn offered(&self, part_type: i32) -> bool {
        self.creatable(part_type)
            && self.dropped.is_none_or(|(_, last)| {
                last.is_none_or(|last| part_rank(part_type).is_some_and(|rank| rank > last))
            })
    }

    /// Delete's part: the one chosen, if it is empty, of a type deleted, and
    /// not the only part.
    fn deletable(&self) -> Option<&PartRow> {
        let part = self.parts.get(self.selected?)?;
        (part.objects == 0 && ADDABLE_PARTS.contains(&part.part_type) && self.parts.len() > 1)
            .then_some(part)
    }

    /// The host's answer to `sent`, a part's options: saved, the part shows
    /// its new options and later changes go at the layout's new revision;
    /// refused, it keeps the options it had. False, and nothing changes,
    /// when `sent` is not the request the dialog waits for.
    pub(crate) fn answered(
        &mut self,
        sent: &PartOptionsChange,
        revision: Option<Revision>,
    ) -> bool {
        let Some((_, row)) = self.waiting.take_if(|(waits, _)| waits == sent) else {
            return false;
        };
        if let Some(revision) = revision {
            self.expected = revision;
            if let Some(part) = self.parts.iter_mut().find(|p| p.id == row.id) {
                *part = row;
            }
        }
        true
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> PartSetupAnswer {
        if self.creating.is_some() {
            return self.show_create(ctx);
        }
        if self.alone {
            // Part Definition was the whole dialog, and has been answered.
            return PartSetupAnswer::Done;
        }
        if self.definition.is_some() {
            return self.show_definition(ctx);
        }
        let can_create = ADDABLE_PARTS.iter().any(|&t| self.creatable(t));
        let can_delete = self.deletable().is_some();
        let mut create = false;
        let mut delete = false;
        let mut answer = PartSetupAnswer::Open;
        let mut change = false;
        let response = dialog(
            ctx,
            "part_setup",
            "Part Setup",
            vec2(444.0, 243.0),
            |ui, at| {
                if self.waiting.is_some() || self.waiting_parts.is_some() {
                    ui.disable();
                }
                let place = |ui: &mut egui::Ui, rect: Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.scope_builder(UiBuilder::new().max_rect(rect), add);
                };
                // The list of parts, top to bottom as the layout stacks them
                // (its place is OpenMaker's: the nib gives the table's size).
                let list = at(16.0, 16.0, 300.0, 210.0);
                ui.painter()
                    .rect_filled(list, CornerRadius::ZERO, theme::CONTROL_BG);
                ui.painter().rect_stroke(
                    list,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, theme::BORDER),
                    egui::StrokeKind::Inside,
                );
                place(ui, list.shrink(4.0), &mut |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    // The chosen part's name in the text color, readable on
                    // the selection fill (the theme's selection color is the
                    // focus ring's).
                    ui.visuals_mut().selection.stroke.color = theme::TEXT;
                    ui.set_width(ui.available_width());
                    ui.set_max_width(ui.available_width());
                    let rows = egui::Layout::top_down_justified(egui::Align::LEFT);
                    ui.with_layout(rows, |ui| {
                        for (index, part) in self.parts.iter().enumerate() {
                            let on = self.selected == Some(index);
                            let row = ui.selectable_label(on, part.name());
                            if row.clicked() {
                                self.selected = Some(index);
                            }
                            if row.double_clicked() {
                                self.selected = Some(index);
                                change = true;
                            }
                        }
                    });
                });
                let changeable = self.selected.is_some_and(|index| {
                    part_options::offered(self.parts[index].part_type).is_some()
                });
                for (label, top, enabled) in [
                    ("Create", 16.0, can_create),
                    ("Change", 48.0, changeable),
                    ("Delete", 80.0, can_delete),
                ] {
                    // Pro's push buttons: 101 x 32 frames, bezels included.
                    place(ui, at(336.0, top, 89.0, 22.0), &mut |ui| {
                        let button = ui
                            .add_enabled_ui(enabled, |ui| {
                                ui.add_sized(vec2(89.0, 22.0), egui::Button::new(label))
                            })
                            .inner;
                        if button.clicked() {
                            match label {
                                "Create" => create = true,
                                "Delete" => delete = true,
                                _ => change = true,
                            }
                        }
                        if !enabled {
                            button.on_disabled_hover_text(match label {
                                "Create" => "The layout has every kind of part Create adds.",
                                "Delete" => {
                                    "Choose an empty part that is not a sub-summary or the \
                                     layout's only part."
                                }
                                _ => "Choose a part whose options OpenMaker sets.",
                            });
                        }
                    });
                }
                place(ui, at(330.0, 199.0, 101.0, 22.0), &mut |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::dialog_button(ui, "Done", true, true).clicked() {
                            answer = PartSetupAnswer::Done;
                        }
                    });
                });
            },
        );
        if change {
            self.change();
        }
        if create {
            self.creating = Some(None);
        }
        if delete {
            if let Some(part) = self.deletable() {
                let change = PartsChange {
                    layout: self.layout,
                    expected: self.expected,
                    change: PartChange::Delete {
                        part: part.id,
                        part_type: part.part_type,
                    },
                };
                self.waiting_parts = Some(change.clone());
                answer = PartSetupAnswer::Parts(change);
            }
        }
        // Not closed while a request is with the host: its answer is the
        // dialog's to take.
        if response
            && answer == PartSetupAnswer::Open
            && self.waiting.is_none()
            && self.waiting_parts.is_none()
        {
            answer = PartSetupAnswer::Done;
        }
        answer
    }

    /// Open Part Definition on the selected part.
    fn change(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        let part = &self.parts[index];
        if part_options::offered(part.part_type).is_none() {
            return;
        }
        self.definition = Some(Definition {
            index,
            flags: part.flags,
            every: match part.page_break_after {
                0 => String::new(),
                n => n.to_string(),
            },
            sorted_by: part.sorted_by.as_ref().map(|(field, _)| *field),
        });
    }

    /// Create's Part Definition: the kinds of part Create adds that the
    /// layout lacks, offered as radio buttons where Pro's nib puts them;
    /// OK adds the one chosen at its place in the order of parts.
    fn show_create(&mut self, ctx: &egui::Context) -> PartSetupAnswer {
        let mut answer = PartSetupAnswer::Open;
        let mut close = false;
        let chosen = self.creating.flatten();
        let mut pick = chosen;
        let offered: Vec<bool> = TYPES
            .iter()
            .map(|(_, types, _)| types.len() == 1 && self.offered(types[0]))
            .collect();
        let waiting = self.waiting_parts.is_some();
        let escaped = dialog(
            ctx,
            "part_create",
            "Part Definition",
            vec2(458.0, 449.0),
            |ui, at| {
                if waiting {
                    ui.disable();
                }
                let place = |ui: &mut egui::Ui, rect: Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.scope_builder(UiBuilder::new().max_rect(rect), add);
                };
                for ((label, types, top), &enabled) in TYPES.iter().zip(&offered) {
                    place(ui, at(19.0, *top, 220.0, 18.0), &mut |ui| {
                        let on = pick.is_some_and(|t| types.contains(&t));
                        let radio = ui.add_enabled(enabled, egui::RadioButton::new(on, *label));
                        if radio.clicked() {
                            pick = Some(types[0]);
                        }
                    });
                }
                for (label, left, top) in [
                    ("Page break before each occurrence", 36.0, 249.0),
                    ("Page break after every", 36.0, 271.0),
                    ("Restart page numbers after each occurrence", 36.0, 293.0),
                    ("Allow part to break across page boundaries", 36.0, 315.0),
                    ("Discard remainder of part before new page", 53.0, 337.0),
                    ("Use alternate row state", 36.0, 361.0),
                    ("Use active row state", 36.0, 381.0),
                ] {
                    place(ui, at(left, top, 310.0, 18.0), &mut |ui| {
                        ui.add_enabled(false, egui::Checkbox::new(&mut false, label))
                            .on_disabled_hover_text(NOT_YET);
                    });
                }
                let right = egui::Layout::right_to_left(egui::Align::Center);
                place(ui, at(365.0, 409.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        let ok = widgets::dialog_button(ui, "OK", true, pick.is_some());
                        if ok.clicked() {
                            if let Some(part_type) = pick {
                                let change = PartsChange {
                                    layout: self.layout,
                                    expected: self.expected,
                                    change: PartChange::Add {
                                        part_type,
                                        size: self.dropped.map_or_else(
                                            || new_part_height(part_type),
                                            |(height, _)| height,
                                        ),
                                    },
                                };
                                answer = PartSetupAnswer::Parts(change);
                            }
                        }
                    });
                });
                place(ui, at(284.0, 409.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                            close = true;
                        }
                    });
                });
            },
        );
        if let PartSetupAnswer::Parts(change) = &answer {
            self.waiting_parts = Some(change.clone());
            // Alone, it waits for the answer (`parts_answered`); over Part
            // Setup, the list waits.
            close = !self.alone;
        }
        if waiting {
            // Not closed while its OK is with the host.
        } else if close || escaped {
            self.creating = None;
        } else {
            self.creating = Some(pick);
        }
        answer
    }

    fn show_definition(&mut self, ctx: &egui::Context) -> PartSetupAnswer {
        use part_options::{
            BREAK_ACROSS_PAGES as ACROSS, DISCARD_REMAINDER as DISCARD,
            PAGE_BREAK_BEFORE as BEFORE, PAGE_BREAK_EVERY as EVERY,
            RESTART_PAGE_NUMBERS as RESTART,
        };
        let mut answer = PartSetupAnswer::Open;
        let mut close = false;
        let Some(definition) = &mut self.definition else {
            return answer;
        };
        let part = self.parts[definition.index].clone();
        let offered = part_options::offered(part.part_type).unwrap_or(0);
        let every = definition.every.trim().parse::<u32>().ok();
        let n_ok = |n: Option<u32>| {
            n.is_some_and(|n| (1..=part_options::MAX_PAGE_BREAK_AFTER).contains(&n))
        };
        let ready = definition.flags & EVERY == 0 || n_ok(every);
        let fields = &self.fields;
        let escaped = dialog(
            ctx,
            "part_definition",
            "Part Definition",
            vec2(458.0, 449.0),
            |ui, at| {
                let place = |ui: &mut egui::Ui, rect: Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.scope_builder(UiBuilder::new().max_rect(rect), add);
                };
                // The part's type: shown, not offered.
                for (label, types, top) in TYPES {
                    place(ui, at(19.0, top, 220.0, 18.0), &mut |ui| {
                        let on = types.contains(&part.part_type);
                        ui.add_enabled(false, egui::RadioButton::new(on, label))
                            .on_disabled_hover_text(NOT_YET);
                    });
                }
                // The occurrence and its fields, for a leading sub-summary.
                let sortable = part.part_type == 3;
                place(ui, at(273.0, 20.0, 165.0, 22.0), &mut |ui| {
                    ui.add_enabled_ui(false, |ui| {
                        egui::ComboBox::from_id_salt("part_definition_table")
                            .width(159.0)
                            .selected_text("Current Table")
                            .show_ui(ui, |_| {});
                    });
                });
                let list = at(275.0, 48.0, 162.0, 136.0);
                ui.painter().rect_stroke(
                    list,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, theme::BORDER),
                    egui::StrokeKind::Inside,
                );
                place(ui, list.shrink(2.0), &mut |ui| {
                    ui.add_enabled_ui(sortable, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("part_definition_fields")
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                let rows = egui::Layout::top_down_justified(egui::Align::LEFT);
                                ui.with_layout(rows, |ui| {
                                    for (id, name) in fields {
                                        let on = definition.sorted_by == Some(*id);
                                        if ui.selectable_label(on, name.as_str()).clicked() {
                                            definition.sorted_by = Some(*id);
                                        }
                                    }
                                });
                            });
                    });
                });
                let line = at(0.0, 242.0, 458.0, 1.0);
                ui.painter().hline(
                    line.x_range(),
                    line.top(),
                    Stroke::new(1.0, theme::HAIRLINE),
                );
                // The options Pro's Part Definition offers for the part's type;
                // before each occurrence and after every N exclude each other,
                // and a discard needs breaking across pages.
                let mut flags = definition.flags;
                let check = |ui: &mut egui::Ui,
                             flags: &mut u64,
                             rect: Rect,
                             bit: u64,
                             label: &str,
                             enabled: bool| {
                    place(ui, rect, &mut |ui| {
                        let mut on = *flags & bit != 0;
                        let box_ = ui.add_enabled(enabled, egui::Checkbox::new(&mut on, label));
                        if box_.changed() {
                            *flags = if on { *flags | bit } else { *flags & !bit };
                            if on && bit == BEFORE {
                                *flags &= !EVERY;
                            }
                            if on && bit == EVERY {
                                *flags &= !BEFORE;
                            }
                            if !on && bit == ACROSS {
                                *flags &= !DISCARD;
                            }
                        }
                    });
                };
                check(
                    ui,
                    &mut flags,
                    at(36.0, 249.0, 243.0, 18.0),
                    BEFORE,
                    "Page break before each occurrence",
                    offered & BEFORE != 0,
                );
                check(
                    ui,
                    &mut flags,
                    at(36.0, 271.0, 167.0, 18.0),
                    EVERY,
                    "Page break after every",
                    offered & EVERY != 0,
                );
                check(
                    ui,
                    &mut flags,
                    at(36.0, 293.0, 301.0, 18.0),
                    RESTART,
                    "Restart page numbers after each occurrence",
                    offered & RESTART != 0,
                );
                check(
                    ui,
                    &mut flags,
                    at(36.0, 315.0, 299.0, 18.0),
                    ACROSS,
                    "Allow part to break across page boundaries",
                    offered & ACROSS != 0,
                );
                let discard = offered & DISCARD != 0 && flags & ACROSS != 0;
                check(
                    ui,
                    &mut flags,
                    at(53.0, 337.0, 298.0, 18.0),
                    DISCARD,
                    "Discard remainder of part before new page",
                    discard,
                );
                definition.flags = flags;
                place(ui, at(207.0, 270.0, 36.0, 22.0), &mut |ui| {
                    let field = ui.add_enabled(
                        flags & EVERY != 0,
                        egui::TextEdit::singleline(&mut definition.every).desired_width(36.0),
                    );
                    widgets::accessible_label(ui, field.id, "Occurrences between page breaks");
                });
                place(ui, at(248.0, 271.0, 74.0, 17.0), &mut |ui| {
                    ui.add(egui::Label::new("occurrences").wrap_mode(egui::TextWrapMode::Extend));
                });
                for (label, top) in [
                    ("Use alternate row state", 361.0),
                    ("Use active row state", 381.0),
                ] {
                    place(ui, at(36.0, top, 180.0, 18.0), &mut |ui| {
                        ui.add_enabled(false, egui::Checkbox::new(&mut false, label))
                            .on_disabled_hover_text(NOT_YET);
                    });
                }
                let right = egui::Layout::right_to_left(egui::Align::Center);
                place(ui, at(365.0, 409.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "OK", true, ready).clicked() {
                            let page_break_after = if flags & EVERY != 0 {
                                every.unwrap_or(0)
                            } else {
                                part.page_break_after
                            };
                            let sorted_by = definition.sorted_by.filter(|_| sortable);
                            answer = PartSetupAnswer::Change(PartOptionsChange {
                                layout: self.layout,
                                expected: self.expected,
                                part: part.id,
                                flags,
                                page_break_after,
                                break_field: sorted_by
                                    .filter(|field| {
                                        part.sorted_by.as_ref().map(|(f, _)| *f) != Some(*field)
                                    })
                                    .map(|field| (self.occurrence.0, self.occurrence.1, field)),
                            });
                        }
                    });
                });
                place(ui, at(284.0, 409.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                            close = true;
                        }
                    });
                });
            },
        );
        if let PartSetupAnswer::Change(change) = &answer {
            // The row as it will be once the host has it.
            let name = |field: u64| {
                self.fields
                    .iter()
                    .find(|(id, _)| *id == field)
                    .map(|(_, name)| (field, name.clone()))
            };
            let row = PartRow {
                flags: change.flags,
                page_break_after: change.page_break_after,
                sorted_by: change
                    .break_field
                    .and_then(|(.., field)| name(field))
                    .or(part.sorted_by.clone()),
                ..part
            };
            self.waiting = Some((*change, row));
            close = true;
        }
        if close || escaped {
            self.definition = None;
        }
        answer
    }
}

/// A modal dialog titled `title` over `ctx`, its content `size` points,
/// drawn by `add` with a function placing a rect at content coordinates.
/// Whether it was dismissed (Escape, or a click outside).
pub(crate) fn dialog(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    size: egui::Vec2,
    add: impl FnOnce(&mut egui::Ui, &dyn Fn(f32, f32, f32, f32) -> Rect),
) -> bool {
    let frame = egui::Frame::new()
        .fill(theme::DIALOG_BG)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .shadow(ctx.global_style().visuals.window_shadow);
    egui::Modal::new(Id::unique(id))
        .frame(frame)
        .backdrop_color(theme::SCRIM)
        .show(ctx, |ui| {
            let (bar, _) = ui.allocate_exact_size(vec2(size.x, 28.0), egui::Sense::hover());
            ui.painter().text(
                bar.center(),
                Align2::CENTER_CENTER,
                title,
                egui::FontId::new(13.0, theme::semibold()),
                theme::TEXT,
            );
            let (window, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            let at = |x: f32, y: f32, w: f32, h: f32| {
                Rect::from_min_size(window.min + vec2(x, y), vec2(w, h))
            };
            add(ui, &at);
        })
        .should_close()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(parts: &[(i32, f64)]) -> PartSetup {
        let rows = parts
            .iter()
            .enumerate()
            .map(|(i, &(part_type, height))| PartRow {
                id: i as u64 + 1,
                part_type,
                flags: 0,
                page_break_after: 0,
                height,
                sorted_by: None,
                objects: 0,
            })
            .collect();
        PartSetup::new(1, (1, [0; 32]), (1, 1), Vec::new(), rows)
    }

    /// The Part tool adds a part only below the last part, at most
    /// `MAX_DROP_HEIGHT` below, and only where the host appends it: not on
    /// a stored height that is not a number, which no answer would match,
    /// nor on parts out of FileMaker's order, where the host would put it
    /// above others.
    #[test]
    fn the_part_tool_adds_a_part_only_after_the_last_one() {
        let footer = [(4, 240.0), (7, 22.0)];
        let opened = dialog(&footer).creating_alone(Some(279.0)).ok().unwrap();
        assert_eq!(opened.dropped, Some((17.0, part_rank(7))));
        assert_eq!(opened.creating, Some(Some(8)), "Title Footer chosen");
        for at in [100.0, 262.0, 262.0 + 26.0] {
            assert_eq!(
                dialog(&footer).creating_alone(Some(at)).err(),
                Some(CANNOT_POSITION),
                "{at}"
            );
        }
        for parts in [[(4, f64::NAN), (7, 22.0)], [(7, 22.0), (4, 240.0)]] {
            assert_eq!(
                dialog(&parts).creating_alone(Some(279.0)).err(),
                Some(CANNOT_POSITION),
                "{parts:?}"
            );
        }
    }
}
