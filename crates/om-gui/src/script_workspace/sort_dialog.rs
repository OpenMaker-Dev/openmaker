//! Sort Records' sort order (script-steps.md 11.1): FileMaker's Sort
//! Records dialog for a stored sort. The table pop-up and its fields at the
//! left, the sort order at the right (dragged into a new order), Clear All,
//! Move and Clear between them, the selected field's direction (ascending,
//! descending, a value list's order), its summary field and language below,
//! and the spec's "Keep records in sorted order" and blanks-last flags.
//!
//! The dialog edits a decoded [`SortSpec`]; what an entry stores and the
//! dialog does not show (its repetition) is kept.
//!
//! Browse's Sort Records (the toolbar's Sort, Records > Sort Records...,
//! Sort Records with a dialog) is the same dialog with the layout's fields
//! first and Unsort beside Cancel and Sort ([`SortOrder::browse`]).

use std::sync::Arc;

use egui::{pos2, vec2, Color32, FontId, Painter, Rect, Sense, Stroke};
use om_data::host::CalcSchema;
use om_data::scripts::specs::{SortEntry, SortSpec, SpecField};

use super::pickers::{
    self, checkbox, dialog, left_label, list_box, list_box_with, popup_menu, push_button, radio,
    FieldTarget, ListRow, Picked, SpecifyField, BUTTON_WIDTH, CONTROL_HEIGHT,
};
use crate::theme;

/// `Locale::Type` names for "Override field's language for sort"
/// (script-steps.md 11.1.2), in that list's order. Options for Field's
/// "Default language" lists them too (`database_options`).
pub(crate) const LANGUAGES: [(u32, &str); 63] = [
    (2, "Unicode"),
    (16, "Catalan"),
    (17, "Croatian"),
    (18, "Czech"),
    (19, "Danish"),
    (20, "Dutch"),
    (21, "English"),
    (22, "Finnish"),
    (23, "Finnish (FileMaker)"),
    (24, "French"),
    (25, "German"),
    (26, "German (dictionary)"),
    (27, "Greek"),
    (28, "Hungarian"),
    (29, "Icelandic"),
    (30, "Italian"),
    (31, "Japanese"),
    (32, "Norwegian"),
    (33, "Polish"),
    (34, "Portuguese"),
    (35, "Romanian"),
    (36, "Russian"),
    (37, "Slovak"),
    (38, "Slovenian"),
    (39, "Spanish"),
    (40, "Spanish (traditional)"),
    (41, "Swedish"),
    (42, "Swedish (FileMaker)"),
    (43, "Turkish"),
    (44, "Ukrainian"),
    (45, "Chinese"),
    (46, "Chinese (stroke)"),
    (47, "Hebrew"),
    (48, "Hindi"),
    (49, "Arabic"),
    (50, "Estonian"),
    (51, "Lithuanian"),
    (52, "Latvian"),
    (53, "Serbian"),
    (54, "Persian"),
    (55, "Bulgarian"),
    (56, "Vietnamese"),
    (57, "Thai"),
    (58, "Greek (mixed)"),
    (59, "Bengali"),
    (60, "Telugu"),
    (61, "Marathi"),
    (62, "Tamil"),
    (63, "Gujarati"),
    (64, "Kannada"),
    (65, "Malayalam"),
    (66, "Oriya"),
    (67, "Punjabi"),
    (68, "Sinhala"),
    (69, "Urdu"),
    (70, "Divehi"),
    (71, "Burmese"),
    (72, "Sanskrit"),
    (73, "Lao"),
    (74, "Khmer"),
    (75, "Tibetan"),
    (76, "Korean"),
    (77, "English (international)"),
];

/// The language a new override starts with.
const ENGLISH: u32 = 21;

/// How the selected entry, or the next field moved, is sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Ascending,
    Descending,
    /// By a value list's order.
    Custom,
}

fn direction(entry: &SortEntry) -> Direction {
    match (entry.value_list, entry.descending) {
        (Some(_), _) => Direction::Custom,
        (None, false) => Direction::Ascending,
        (None, true) => Direction::Descending,
    }
}

/// The sort order's direction marks: rising bars, falling bars, a list.
fn ascending_mark(painter: &Painter, rect: Rect, color: Color32) {
    for (n, height) in [4.0, 7.0, 10.0].into_iter().enumerate() {
        let x = rect.left() + 2.0 + n as f32 * 4.0;
        painter.line_segment(
            [
                pos2(x, rect.bottom() - 1.0),
                pos2(x, rect.bottom() - 1.0 - height),
            ],
            Stroke::new(2.0, color),
        );
    }
}

fn descending_mark(painter: &Painter, rect: Rect, color: Color32) {
    for (n, height) in [10.0, 7.0, 4.0].into_iter().enumerate() {
        let x = rect.left() + 2.0 + n as f32 * 4.0;
        painter.line_segment(
            [
                pos2(x, rect.bottom() - 1.0),
                pos2(x, rect.bottom() - 1.0 - height),
            ],
            Stroke::new(2.0, color),
        );
    }
}

fn custom_mark(painter: &Painter, rect: Rect, color: Color32) {
    for n in 0..3 {
        let y = rect.top() + 3.0 + n as f32 * 4.0;
        painter.line_segment(
            [pos2(rect.left() + 1.0, y), pos2(rect.right() - 1.0, y)],
            Stroke::new(1.5, color),
        );
    }
}

/// What Browse's Sort Records dialog was answered with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BrowseSort {
    Sort(SortSpec),
    /// Unsort: the found set back in creation order.
    Unsort,
    Cancel,
}

/// Browse's dialog: the layout it sorts for.
struct Browse {
    /// "Current Layout": the layout's name and the fields it shows, with
    /// the names the layout gives them (a related one's `Table::Field`).
    layout: String,
    fields: Vec<(SpecField, String)>,
    /// "Current Table": the layout's occurrence.
    occurrence: Option<u64>,
}

/// The Sort Records dialog of a step's sort order.
pub(crate) struct SortOrder {
    names: Option<Arc<CalcSchema>>,
    spec: SortSpec,
    /// The table occurrence whose fields the left list shows.
    occurrence: Option<u64>,
    /// Browse's dialog, and whether its left list shows the layout's fields.
    browse: Option<Browse>,
    on_layout: bool,
    /// The field selected in the left list.
    field: Option<SpecField>,
    /// The entry selected in the sort order.
    entry: Option<usize>,
    /// The direction of the next field moved, with nothing selected.
    next: Direction,
    /// Specify Field for "Reorder based on summary field".
    summary: Option<Box<SpecifyField>>,
}

impl SortOrder {
    pub fn new(spec: SortSpec, names: Option<Arc<CalcSchema>>) -> Self {
        let occurrence = spec
            .fields
            .first()
            .and_then(|e| e.field.occurrence)
            .or_else(|| {
                names
                    .as_ref()
                    .and_then(|n| n.occurrences_in_order().first().map(|o| o.id))
            });
        Self {
            names,
            spec,
            occurrence,
            browse: None,
            on_layout: false,
            field: None,
            entry: None,
            next: Direction::Ascending,
            summary: None,
        }
    }

    /// Show `occurrence`'s fields first (a relationship side's records are
    /// sorted by them).
    pub fn for_occurrence(mut self, occurrence: u64) -> Self {
        if self.spec.fields.is_empty() {
            self.occurrence = Some(occurrence);
        }
        self
    }

    /// Browse's dialog for a layout: its fields listed first ("Current
    /// Layout"), then its occurrence's ("Current Table") and the other
    /// occurrences', and Unsort, Cancel and Sort for buttons.
    pub fn browse(
        mut self,
        layout: String,
        occurrence: Option<u64>,
        fields: Vec<(SpecField, String)>,
    ) -> Self {
        self.on_layout = true;
        self.occurrence = occurrence.or(self.occurrence);
        self.browse = Some(Browse {
            layout,
            fields,
            occurrence,
        });
        self
    }

    /// `Table::Field` of a spec field (a field of Browse's layout as the
    /// layout names it when the file's names are not read).
    fn field_name(&self, field: &SpecField) -> String {
        let names = self.names.as_deref();
        let known = field
            .occurrence
            .and_then(|o| names.and_then(|n| n.field(o, field.field)))
            .is_some();
        if !known {
            if let Some((_, name)) = self
                .browse
                .as_ref()
                .and_then(|b| b.fields.iter().find(|(f, _)| f == field))
            {
                return name.clone();
            }
        }
        let table = field
            .occurrence
            .and_then(|o| names.and_then(|n| n.occurrence(o)))
            .map_or(om_data::formula::TABLE_MISSING, |o| o.name.as_str());
        let name = field
            .occurrence
            .and_then(|o| names.and_then(|n| n.field(o, field.field)))
            .map_or(om_data::formula::FIELD_MISSING, |f| f.name.as_str());
        let mut text = format!("{table}::{name}");
        if field.repetition > 1 {
            text.push_str(&format!("[{}]", field.repetition));
        }
        text
    }

    /// The fields the left list shows: the layout's (a related one as
    /// `Table::Field`), or those of the chosen occurrence's table.
    fn fields(&self) -> Vec<(SpecField, String)> {
        if let Some(browse) = self.browse.as_ref().filter(|_| self.on_layout) {
            return browse.fields.clone();
        }
        let (Some(names), Some(occurrence)) = (&self.names, self.occurrence) else {
            return Vec::new();
        };
        names
            .table_of(occurrence)
            .map(|t| {
                t.fields
                    .values()
                    .map(|f| {
                        let field = SpecField {
                            occurrence: Some(occurrence),
                            field: f.id,
                            repetition: 1,
                        };
                        (field, f.name.clone())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The table pop-up: its entries, the one shown, and what each selects
    /// (`None`: the layout's fields).
    fn tables(&self) -> (Vec<String>, String, Vec<Option<u64>>) {
        let names = self.names.as_deref();
        let mut items = Vec::new();
        let mut targets = Vec::new();
        let mut shown = None;
        if let Some(browse) = &self.browse {
            // Claris's help names them "Current Layout (LayoutName)" and
            // "Current Table (TableName)"; the quotes are Specify
            // Calculation's (unverified here).
            items.push(format!("Current Layout (\"{}\")", browse.layout));
            targets.push(None);
            if self.on_layout {
                shown = Some(items[0].clone());
            }
            if let Some(o) = browse.occurrence {
                let name = names
                    .and_then(|n| n.occurrence(o))
                    .map_or("", |o| o.name.as_str());
                items.push(format!("Current Table (\"{name}\")"));
                targets.push(Some(o));
                if !self.on_layout && self.occurrence == Some(o) {
                    shown = Some(items[items.len() - 1].clone());
                }
            }
        }
        for o in names.map(|n| n.occurrences_in_order()).unwrap_or_default() {
            if shown.is_none() && !self.on_layout && self.occurrence == Some(o.id) {
                shown = Some(o.name.clone());
            }
            items.push(o.name.clone());
            targets.push(Some(o.id));
        }
        (items, shown.unwrap_or_default(), targets)
    }

    fn value_lists(&self) -> Vec<(u64, String)> {
        self.names
            .as_ref()
            .map(|n| {
                n.value_lists()
                    .iter()
                    .map(|v| (v.id, v.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Add the selected field to the sort order (Move).
    fn add(&mut self) {
        let Some(spec_field) = self.field else {
            return;
        };
        if let Some(i) = self.spec.fields.iter().position(|e| e.field == spec_field) {
            self.entry = Some(i);
            return;
        }
        let value_list = match self.next {
            Direction::Custom => self.value_lists().first().map(|(id, _)| *id),
            _ => None,
        };
        self.spec.fields.push(SortEntry {
            field: spec_field,
            summary: None,
            value_list,
            descending: self.next == Direction::Descending,
            locale: 0,
        });
        self.entry = Some(self.spec.fields.len() - 1);
    }

    /// Set the direction of the selected entry, or of the next field.
    fn set_direction(&mut self, direction: Direction) {
        let first_list = self.value_lists().first().map(|(id, _)| *id);
        match self.entry.and_then(|i| self.spec.fields.get_mut(i)) {
            Some(entry) => match direction {
                Direction::Ascending | Direction::Descending => {
                    entry.value_list = None;
                    entry.descending = direction == Direction::Descending;
                }
                Direction::Custom => {
                    if entry.value_list.is_none() {
                        entry.value_list = first_list;
                    }
                }
            },
            None => self.next = direction,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, on_top: bool) -> Option<Picked<SortSpec>> {
        self.show_dialog(ctx, on_top).map(|answer| match answer {
            BrowseSort::Sort(spec) => Picked::Ok(spec),
            BrowseSort::Unsort | BrowseSort::Cancel => Picked::Cancel,
        })
    }

    /// Browse's dialog ([`Self::browse`]).
    pub fn show_browse(&mut self, ctx: &egui::Context) -> Option<BrowseSort> {
        self.show_dialog(ctx, true)
    }

    fn show_dialog(&mut self, ctx: &egui::Context, on_top: bool) -> Option<BrowseSort> {
        let mut outcome = None;
        let mut double_field = false;
        let shown = dialog(
            ctx,
            "sort_records",
            "Sort Records",
            600.0,
            on_top && self.summary.is_none(),
            |ui| {
                let full = ui.available_width();
                let left = ui.cursor().left();
                let list_width = 220.0;
                let middle = 112.0;
                let right_x = left + list_width + middle;
                ui.add_space(14.0);
                // The table pop-up over the fields, the sort order's title.
                let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
                let (items, current, targets) = self.tables();
                let popup = Rect::from_min_size(row.min, vec2(list_width, CONTROL_HEIGHT));
                if let Some(i) = popup_menu(ui, popup, "Table", &current, &items, true) {
                    let on_layout = targets[i].is_none();
                    let occurrence = targets[i].or(self.occurrence);
                    if on_layout != self.on_layout || occurrence != self.occurrence {
                        self.on_layout = on_layout;
                        self.occurrence = occurrence;
                        self.field = None;
                    }
                }
                left_label(
                    ui,
                    Rect::from_min_max(pos2(right_x, row.top()), row.max),
                    "Sort Order",
                    FontId::new(theme::BODY, theme::semibold()),
                    theme::TEXT,
                );
                ui.add_space(6.0);
                let (lists, _) = ui.allocate_exact_size(vec2(full, 210.0), Sense::hover());
                // The table's fields.
                let fields = self.fields();
                let rows: Vec<ListRow> = fields.iter().map(|(_, n)| ListRow::item(n)).collect();
                let selected = fields.iter().position(|(f, _)| Some(*f) == self.field);
                let field_list = Rect::from_min_size(lists.min, vec2(list_width, lists.height()));
                let click = list_box(ui, field_list, "sort_fields", &rows, selected, true);
                if let Some(i) = click.clicked.or(click.double_clicked) {
                    self.field = Some(fields[i].0);
                    self.entry = None;
                }
                if click.double_clicked.is_some() {
                    double_field = true;
                }
                // Clear All, Move, Clear.
                let button_x = lists.left() + list_width + (middle - BUTTON_WIDTH) / 2.0;
                let button = |n: usize| {
                    Rect::from_min_size(
                        pos2(button_x, lists.top() + 40.0 + n as f32 * 32.0),
                        vec2(BUTTON_WIDTH, CONTROL_HEIGHT),
                    )
                };
                let has_entries = !self.spec.fields.is_empty();
                if push_button(ui, button(0), "Clear All", "Clear All", false, has_entries)
                    .clicked()
                {
                    self.spec.fields.clear();
                    self.entry = None;
                }
                let can_move = self.field.is_some();
                if push_button(ui, button(1), "Move", "Move", false, can_move).clicked()
                    || double_field
                {
                    self.add();
                }
                let can_clear = self.entry.is_some();
                if push_button(ui, button(2), "Clear", "Clear", false, can_clear).clicked() {
                    if let Some(i) = self.entry.take() {
                        self.spec.fields.remove(i);
                    }
                }
                // The sort order.
                let order: Vec<ListRow> = self
                    .spec
                    .fields
                    .iter()
                    .map(|e| ListRow {
                        mark: Some(match direction(e) {
                            Direction::Ascending => ascending_mark as pickers::Mark,
                            Direction::Descending => descending_mark,
                            Direction::Custom => custom_mark,
                        }),
                        ..ListRow::item(self.field_name(&e.field))
                    })
                    .collect();
                let order_list = Rect::from_min_max(pos2(right_x, lists.top()), lists.max);
                let click =
                    list_box_with(ui, order_list, "sort_order", &order, self.entry, true, true);
                if let Some(i) = click.clicked {
                    self.entry = Some(i);
                    self.field = None;
                }
                if let Some(i) = click.double_clicked {
                    // A double click takes a field out, as FileMaker does.
                    self.spec.fields.remove(i);
                    self.entry = None;
                }
                if let Some((from, to)) = click.moved {
                    if from < self.spec.fields.len() {
                        let entry = self.spec.fields.remove(from);
                        let to = if to > from { to - 1 } else { to }.min(self.spec.fields.len());
                        self.spec.fields.insert(to, entry);
                        self.entry = Some(to);
                    }
                }
                ui.add_space(12.0);
                // The direction of the selected entry.
                let entry = self.entry.and_then(|i| self.spec.fields.get(i)).cloned();
                let current = entry.as_ref().map_or(self.next, direction);
                let lists_available = !self.value_lists().is_empty();
                for (label, value, enabled) in [
                    ("Ascending order", Direction::Ascending, true),
                    ("Descending order", Direction::Descending, true),
                    (
                        "Custom order based on value list",
                        Direction::Custom,
                        lists_available,
                    ),
                ] {
                    let (row, _) = ui.allocate_exact_size(vec2(full, 22.0), Sense::hover());
                    let radio_rect = Rect::from_min_size(row.min, vec2(260.0, row.height()));
                    if radio(ui, radio_rect, current == value, label, enabled) {
                        self.set_direction(value);
                    }
                    if value == Direction::Custom {
                        let lists = self.value_lists();
                        let list_id = entry.as_ref().and_then(|e| e.value_list);
                        let shown = list_id.map_or_else(String::new, |id| {
                            lists
                                .iter()
                                .find(|(v, _)| *v == id)
                                .map_or_else(|| format!("Value list {id}"), |(_, n)| n.clone())
                        });
                        let items: Vec<String> = lists.iter().map(|(_, n)| n.clone()).collect();
                        let popup = Rect::from_min_size(
                            pos2(row.left() + 270.0, row.center().y - CONTROL_HEIGHT / 2.0),
                            vec2(200.0, CONTROL_HEIGHT),
                        );
                        let enabled = current == Direction::Custom && entry.is_some();
                        if let Some(n) =
                            popup_menu(ui, popup, "Value List", &shown, &items, enabled)
                        {
                            if let Some(e) = self.entry.and_then(|i| self.spec.fields.get_mut(i)) {
                                e.value_list = Some(lists[n].0);
                            }
                        }
                    }
                }
                ui.add_space(6.0);
                // Summary field and language of the selected entry.
                let (row, _) = ui.allocate_exact_size(vec2(full, 26.0), Sense::hover());
                let mut summary = entry.as_ref().is_some_and(|e| e.summary.is_some());
                let check = Rect::from_min_size(row.min, vec2(260.0, row.height()));
                let has_entry = entry.is_some();
                if checkbox(
                    ui,
                    check,
                    &mut summary,
                    "Reorder based on summary field",
                    has_entry,
                ) {
                    if summary {
                        self.summary =
                            Some(Box::new(SpecifyField::new(self.names.clone(), None, false)));
                    } else if let Some(e) = self.entry.and_then(|i| self.spec.fields.get_mut(i)) {
                        e.summary = None;
                    }
                }
                if let Some(field) = entry.as_ref().and_then(|e| e.summary) {
                    left_label(
                        ui,
                        Rect::from_min_max(pos2(row.left() + 270.0, row.top()), row.max),
                        &self.field_name(&field),
                        pickers::body(),
                        theme::CAPTION,
                    );
                }
                let (row, _) = ui.allocate_exact_size(vec2(full, 26.0), Sense::hover());
                let locale = entry.as_ref().map_or(0, |e| e.locale);
                let mut language = locale != 0;
                let check = Rect::from_min_size(row.min, vec2(260.0, row.height()));
                if checkbox(
                    ui,
                    check,
                    &mut language,
                    "Override field's language for sort",
                    has_entry,
                ) {
                    if let Some(e) = self.entry.and_then(|i| self.spec.fields.get_mut(i)) {
                        e.locale = if language { ENGLISH } else { 0 };
                    }
                }
                let shown = LANGUAGES.iter().find(|(id, _)| *id == locale).map_or_else(
                    || {
                        if locale == 0 {
                            String::new()
                        } else {
                            format!("Language {locale}")
                        }
                    },
                    |(_, n)| (*n).to_owned(),
                );
                let items: Vec<String> = LANGUAGES.iter().map(|(_, n)| (*n).to_owned()).collect();
                let popup = Rect::from_min_size(
                    pos2(row.left() + 270.0, row.center().y - CONTROL_HEIGHT / 2.0),
                    vec2(200.0, CONTROL_HEIGHT),
                );
                if let Some(n) = popup_menu(ui, popup, "Language", &shown, &items, locale != 0) {
                    if let Some(e) = self.entry.and_then(|i| self.spec.fields.get_mut(i)) {
                        e.locale = LANGUAGES[n].0;
                    }
                }
                ui.add_space(8.0);
                ui.painter().hline(
                    left..=left + full,
                    ui.cursor().top(),
                    Stroke::new(1.0, theme::HAIRLINE),
                );
                ui.add_space(8.0);
                let (row, _) = ui.allocate_exact_size(vec2(full, 24.0), Sense::hover());
                checkbox(
                    ui,
                    row,
                    &mut self.spec.keep_sorted,
                    "Keep records in sorted order",
                    true,
                );
                let (row, _) = ui.allocate_exact_size(vec2(full, 24.0), Sense::hover());
                checkbox(
                    ui,
                    row,
                    &mut self.spec.blanks_last,
                    "In ascending order, sort blank records last",
                    true,
                );
                ui.add_space(18.0);
                if self.browse.is_some() {
                    outcome = browse_buttons(ui, &self.spec);
                } else {
                    let (cancel, ok) = pickers::cancel_ok(ui, true);
                    if cancel {
                        outcome = Some(BrowseSort::Cancel);
                    } else if ok {
                        outcome = Some(BrowseSort::Sort(self.spec.clone()));
                    }
                }
            },
        );
        // Browse's Sort needs a field to sort by (OpenMaker's reading).
        let can_sort = self.browse.is_none() || !self.spec.fields.is_empty();
        if outcome.is_none() {
            if shown.escape {
                outcome = Some(BrowseSort::Cancel);
            } else if shown.enter && can_sort {
                outcome = Some(BrowseSort::Sort(self.spec.clone()));
            }
        }
        if let Some(picker) = &mut self.summary {
            match picker.show(ctx, true) {
                Some(Picked::Ok(FieldTarget::Field {
                    occurrence,
                    field,
                    repetition,
                })) => {
                    if let Some(e) = self.entry.and_then(|i| self.spec.fields.get_mut(i)) {
                        e.summary = Some(SpecField {
                            occurrence: Some(occurrence),
                            field,
                            repetition: repetition.max(1) as u16,
                        });
                    }
                    self.summary = None;
                }
                Some(_) => self.summary = None,
                None => {}
            }
        }
        outcome
    }
}

/// Browse's buttons, right-aligned: Unsort, Cancel and the default Sort
/// (Claris's help: "Click Unsort", "Click Sort"; the order is OpenMaker's,
/// unmeasured). Sort needs a field in the order.
fn browse_buttons(ui: &mut egui::Ui, spec: &SortSpec) -> Option<BrowseSort> {
    let (row, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), CONTROL_HEIGHT), Sense::hover());
    let sort = Rect::from_min_size(
        pos2(row.right() - BUTTON_WIDTH, row.top()),
        vec2(BUTTON_WIDTH, CONTROL_HEIGHT),
    );
    let step = vec2(-(BUTTON_WIDTH + pickers::BUTTON_GAP), 0.0);
    let cancel = sort.translate(step);
    let unsort = cancel.translate(step);
    let can_sort = !spec.fields.is_empty();
    if push_button(ui, unsort, "Unsort", "Unsort", false, true).clicked() {
        return Some(BrowseSort::Unsort);
    }
    if push_button(ui, cancel, "Cancel", "Cancel", false, true).clicked() {
        return Some(BrowseSort::Cancel);
    }
    if push_button(ui, sort, "Sort", "Sort", true, can_sort).clicked() && can_sort {
        return Some(BrowseSort::Sort(spec.clone()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SortSpec {
        SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: Vec::new(),
        }
    }

    #[test]
    fn moving_fields_builds_the_order_and_directions_follow_the_selection() {
        let mut dialog = SortOrder::new(spec(), None);
        dialog.occurrence = Some(0xd00002);
        dialog.field = Some(SpecField {
            occurrence: Some(0xd00002),
            field: 8,
            repetition: 1,
        });
        dialog.set_direction(Direction::Descending);
        dialog.add();
        // Moving it again selects it rather than adding it twice.
        dialog.add();
        assert_eq!(dialog.spec.fields.len(), 1);
        assert!(dialog.spec.fields[0].descending);
        dialog.set_direction(Direction::Ascending);
        assert!(!dialog.spec.fields[0].descending);
        // No value list in the file: Custom keeps the entry as it is.
        dialog.set_direction(Direction::Custom);
        assert_eq!(dialog.spec.fields[0].value_list, None);
        assert_eq!(
            dialog.spec.fields[0].field,
            SpecField {
                occurrence: Some(0xd00002),
                field: 8,
                repetition: 1
            }
        );
    }

    #[test]
    fn browse_lists_the_layouts_fields_first_and_moves_a_related_one() {
        let base = SpecField {
            occurrence: Some(1),
            field: 2,
            repetition: 1,
        };
        let related = SpecField {
            occurrence: Some(5),
            field: 3,
            repetition: 1,
        };
        let fields = vec![
            (base, "Name".to_owned()),
            (related, "Notes::Text".to_owned()),
        ];
        let mut dialog = SortOrder::new(spec(), None).browse("Details".into(), Some(1), fields);
        let (items, shown, targets) = dialog.tables();
        assert_eq!(items[0], "Current Layout (\"Details\")");
        assert_eq!(shown, items[0]);
        assert_eq!(targets[..2], [None, Some(1)]);
        let fields: Vec<SpecField> = dialog.fields().into_iter().map(|(f, _)| f).collect();
        assert_eq!(fields, [base, related]);
        dialog.field = Some(related);
        dialog.add();
        assert_eq!(dialog.spec.fields[0].field, related);
        // Without the file's names, the order shows the layout's name.
        assert_eq!(dialog.field_name(&related), "Notes::Text");
    }
}
