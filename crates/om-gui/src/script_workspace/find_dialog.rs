//! The find steps' requests (script-steps.md 11.2): FileMaker's Specify
//! Find Requests dialog (each request's action and criteria; New, Edit,
//! Duplicate, Delete) and its Edit Find Request dialog (Find Records or
//! Omit Records, the criteria with Add, Change and Remove, the table's
//! fields, the repetition and Insert Operator), as the Claris Perform Find
//! page describes them.
//!
//! Criteria are text exactly as typed, operators and variables included
//! (11.2.1); the dialogs edit decoded [`StoredRequest`]s.

use std::sync::Arc;

use egui::{pos2, vec2, FontId, Rect, Sense};
use om_data::host::CalcSchema;
use om_data::scripts::specs::{SpecField, StoredRequest};

use super::pickers::{
    self, dialog, left_label, list_box, popup_menu, push_button, text_field, ListRow, Picked,
    BUTTON_WIDTH, CONTROL_HEIGHT,
};
use crate::theme;

/// Insert Operator's menu: the find operators (Claris "Finding text and
/// characters", 11.2.1's list) and what each inserts.
const OPERATORS: [(&str, &str); 16] = [
    ("< less than", "<"),
    ("≤ less than or equal", "≤"),
    ("> greater than", ">"),
    ("≥ greater than or equal", "≥"),
    ("= match whole word (or match empty)", "="),
    ("== match entire field", "=="),
    ("! duplicate values", "!"),
    ("... range", "..."),
    ("// today's date", "//"),
    ("? invalid date or time", "?"),
    ("@ one character", "@"),
    ("# one digit", "#"),
    ("* zero or more characters", "*"),
    ("\\ literal text", "\\"),
    ("\"\" match phrase (from word start)", "\"\""),
    ("~ relaxed search (Japanese only)", "~"),
];

/// The second column of the request lists.
const SECOND_COLUMN: f32 = 110.0;

/// `Table::Field` of a criterion's field.
fn field_name(names: Option<&CalcSchema>, field: &SpecField) -> String {
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

/// A request's criteria as the request list shows them:
/// `Contacts::Status: [In Progress]`, joined by `; `.
fn criteria_text(names: Option<&CalcSchema>, request: &StoredRequest) -> String {
    request
        .criteria
        .iter()
        .map(|(field, text)| format!("{}: [{text}]", field_name(names, field)))
        .collect::<Vec<_>>()
        .join("; ")
}

fn action(request: &StoredRequest) -> &'static str {
    if request.omit {
        "Omit Records"
    } else {
        "Find Records"
    }
}

/// Specify Find Requests.
pub(super) struct FindRequests {
    names: Option<Arc<CalcSchema>>,
    requests: Vec<StoredRequest>,
    selected: Option<usize>,
    /// Edit Find Request, open above, and the request it edits (`None`
    /// for a new one).
    editing: Option<(Option<usize>, Box<EditRequest>)>,
}

impl FindRequests {
    pub fn new(requests: Vec<StoredRequest>, names: Option<Arc<CalcSchema>>) -> Self {
        Self {
            names,
            selected: (!requests.is_empty()).then_some(0),
            requests,
            editing: None,
        }
    }

    fn edit(&mut self, index: Option<usize>) {
        let request = index
            .and_then(|i| self.requests.get(i).cloned())
            .unwrap_or(StoredRequest {
                omit: false,
                criteria: Vec::new(),
            });
        self.editing = Some((
            index,
            Box::new(EditRequest::new(request, self.names.clone())),
        ));
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        on_top: bool,
    ) -> Option<Picked<Vec<StoredRequest>>> {
        let mut outcome = None;
        let mut open = None;
        let names = self.names.clone();
        let shown = dialog(
            ctx,
            "specify_find_requests",
            "Specify Find Requests",
            560.0,
            on_top && self.editing.is_none(),
            |ui| {
                let full = ui.available_width();
                ui.add_space(14.0);
                let (head, _) = ui.allocate_exact_size(vec2(full, 18.0), Sense::hover());
                let bold = FontId::new(theme::SMALL, theme::semibold());
                left_label(
                    ui,
                    Rect::from_min_size(head.min + vec2(8.0, 0.0), vec2(100.0, 18.0)),
                    "Action",
                    bold.clone(),
                    theme::CAPTION,
                );
                left_label(
                    ui,
                    Rect::from_min_max(head.min + vec2(SECOND_COLUMN + 1.0, 0.0), head.max),
                    "Criteria",
                    bold,
                    theme::CAPTION,
                );
                let rows: Vec<ListRow> = self
                    .requests
                    .iter()
                    .map(|r| ListRow {
                        second: Some((SECOND_COLUMN, criteria_text(names.as_deref(), r))),
                        ..ListRow::item(action(r))
                    })
                    .collect();
                let (list, _) = ui.allocate_exact_size(vec2(full, 200.0), Sense::hover());
                let click = list_box(ui, list, "find_requests", &rows, self.selected, true);
                if let Some(i) = click.clicked {
                    self.selected = Some(i);
                }
                if let Some(i) = click.double_clicked {
                    self.selected = Some(i);
                    open = Some(Some(i));
                }
                ui.add_space(10.0);
                let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
                let button = |n: usize| {
                    Rect::from_min_size(
                        pos2(row.left() + n as f32 * (BUTTON_WIDTH + 8.0), row.top()),
                        vec2(BUTTON_WIDTH, CONTROL_HEIGHT),
                    )
                };
                let any = self.selected.is_some_and(|i| i < self.requests.len());
                if push_button(ui, button(0), "New", "New", false, true).clicked() {
                    open = Some(None);
                }
                if push_button(ui, button(1), "Edit", "Edit", false, any).clicked() {
                    open = Some(self.selected);
                }
                if push_button(ui, button(2), "Duplicate", "Duplicate", false, any).clicked() {
                    if let Some(i) = self.selected {
                        let copy = self.requests[i].clone();
                        self.requests.insert(i + 1, copy);
                        self.selected = Some(i + 1);
                    }
                }
                if push_button(ui, button(3), "Delete", "Delete", false, any).clicked() {
                    if let Some(i) = self.selected {
                        self.requests.remove(i);
                        self.selected = if self.requests.is_empty() {
                            None
                        } else {
                            Some(i.min(self.requests.len() - 1))
                        };
                    }
                }
                ui.add_space(20.0);
                let (cancel, ok) = pickers::cancel_ok(ui, true);
                if cancel {
                    outcome = Some(Picked::Cancel);
                } else if ok {
                    outcome = Some(Picked::Ok(self.requests.clone()));
                }
            },
        );
        if outcome.is_none() {
            if shown.escape {
                outcome = Some(Picked::Cancel);
            } else if shown.enter {
                outcome = Some(Picked::Ok(self.requests.clone()));
            }
        }
        if let Some(index) = open {
            self.edit(index);
        }
        if let Some((index, editor)) = &mut self.editing {
            match editor.show(ctx, true) {
                Some(Picked::Ok(request)) => {
                    match *index {
                        Some(i) if i < self.requests.len() => self.requests[i] = request,
                        _ => {
                            self.requests.push(request);
                            self.selected = Some(self.requests.len() - 1);
                        }
                    }
                    self.editing = None;
                }
                Some(Picked::Cancel) => self.editing = None,
                None => {}
            }
        }
        outcome
    }
}

/// Edit Find Request: one request's action and criteria.
pub(super) struct EditRequest {
    names: Option<Arc<CalcSchema>>,
    request: StoredRequest,
    /// The criterion selected in the list.
    selected: Option<usize>,
    occurrence: Option<u64>,
    field: Option<u64>,
    repetition: String,
    criteria: String,
}

impl EditRequest {
    pub fn new(request: StoredRequest, names: Option<Arc<CalcSchema>>) -> Self {
        let occurrence = request
            .criteria
            .first()
            .and_then(|(f, _)| f.occurrence)
            .or_else(|| {
                names
                    .as_ref()
                    .and_then(|n| n.occurrences_in_order().first().map(|o| o.id))
            });
        Self {
            names,
            request,
            selected: None,
            occurrence,
            field: None,
            repetition: "1".into(),
            criteria: String::new(),
        }
    }

    fn fields(&self) -> Vec<(u64, String)> {
        let (Some(names), Some(occurrence)) = (&self.names, self.occurrence) else {
            return Vec::new();
        };
        names
            .table_of(occurrence)
            .map(|t| t.fields.values().map(|f| (f.id, f.name.clone())).collect())
            .unwrap_or_default()
    }

    /// The field and repetition chosen below the criteria.
    fn chosen(&self) -> Option<SpecField> {
        let repetition = self
            .repetition
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|r| *r >= 1)?;
        Some(SpecField {
            occurrence: Some(self.occurrence?),
            field: self.field?,
            repetition,
        })
    }

    /// Select criterion `i`: its field, repetition and text below.
    fn select(&mut self, i: usize) {
        if let Some((field, text)) = self.request.criteria.get(i).cloned() {
            self.selected = Some(i);
            self.occurrence = field.occurrence.or(self.occurrence);
            self.field = Some(field.field);
            self.repetition = field.repetition.to_string();
            self.criteria = text;
        }
    }

    /// Add: a field's criterion, replacing the field's earlier one (a
    /// request holds one criterion per field and repetition).
    fn add(&mut self) {
        let Some(field) = self.chosen() else {
            return;
        };
        match self.request.criteria.iter().position(|(f, _)| *f == field) {
            Some(i) => {
                self.request.criteria[i].1 = self.criteria.clone();
                self.selected = Some(i);
            }
            None => {
                self.request.criteria.push((field, self.criteria.clone()));
                self.selected = Some(self.request.criteria.len() - 1);
            }
        }
    }

    /// Change: the selected criterion becomes the field and text below.
    fn change(&mut self) {
        let (Some(i), Some(field)) = (self.selected, self.chosen()) else {
            return;
        };
        // Another line for the same field gives way.
        if let Some(j) = self
            .request
            .criteria
            .iter()
            .position(|(f, _)| *f == field)
            .filter(|j| *j != i)
        {
            self.request.criteria.remove(j);
            let i = if j < i { i - 1 } else { i };
            self.request.criteria[i] = (field, self.criteria.clone());
            self.selected = Some(i);
            return;
        }
        self.request.criteria[i] = (field, self.criteria.clone());
    }

    fn remove(&mut self) {
        if let Some(i) = self.selected.take() {
            if i < self.request.criteria.len() {
                self.request.criteria.remove(i);
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, on_top: bool) -> Option<Picked<StoredRequest>> {
        let mut outcome = None;
        let names = self.names.clone();
        let shown = dialog(
            ctx,
            "edit_find_request",
            "Edit Find Request",
            600.0,
            on_top,
            |ui| {
                let full = ui.available_width();
                let left = ui.cursor().left();
                ui.add_space(14.0);
                let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
                pickers::row_label(
                    ui,
                    Rect::from_min_size(row.min, vec2(50.0, CONTROL_HEIGHT)),
                    "Action:",
                    true,
                );
                let items = ["Find Records".to_owned(), "Omit Records".to_owned()];
                let popup = Rect::from_min_size(
                    pos2(row.left() + 58.0, row.top()),
                    vec2(180.0, CONTROL_HEIGHT),
                );
                let current = action(&self.request);
                if let Some(n) = popup_menu(ui, popup, "Action", current, &items, true) {
                    self.request.omit = n == 1;
                }
                ui.add_space(12.0);
                let (head, _) = ui.allocate_exact_size(vec2(full, 18.0), Sense::hover());
                let bold = FontId::new(theme::SMALL, theme::semibold());
                left_label(
                    ui,
                    Rect::from_min_size(head.min + vec2(8.0, 0.0), vec2(200.0, 18.0)),
                    "Field",
                    bold.clone(),
                    theme::CAPTION,
                );
                let second = 230.0;
                left_label(
                    ui,
                    Rect::from_min_max(head.min + vec2(second + 1.0, 0.0), head.max),
                    "Criteria",
                    bold,
                    theme::CAPTION,
                );
                let rows: Vec<ListRow> = self
                    .request
                    .criteria
                    .iter()
                    .map(|(field, text)| ListRow {
                        second: Some((second, text.clone())),
                        ..ListRow::item(field_name(names.as_deref(), field))
                    })
                    .collect();
                let (list, _) = ui.allocate_exact_size(vec2(full, 110.0), Sense::hover());
                let click = list_box(ui, list, "criteria", &rows, self.selected, true);
                if let Some(i) = click.clicked.or(click.double_clicked) {
                    self.select(i);
                }
                ui.add_space(12.0);
                // The field to find, at the left; its criteria at the right.
                let column = 250.0;
                let right_x = left + column + 24.0;
                let (row, _) = ui.allocate_exact_size(vec2(full, 18.0), Sense::hover());
                let when = if self.request.omit {
                    "Omit records when:"
                } else {
                    "Find records when:"
                };
                left_label(
                    ui,
                    Rect::from_min_size(row.min, vec2(column, 18.0)),
                    when,
                    pickers::body(),
                    theme::TEXT,
                );
                left_label(
                    ui,
                    Rect::from_min_max(pos2(right_x, row.top()), row.max),
                    "Criteria:",
                    pickers::body(),
                    theme::TEXT,
                );
                ui.add_space(4.0);
                let (area, _) = ui.allocate_exact_size(vec2(full, 180.0), Sense::hover());
                let occurrences: Vec<(u64, String)> = names
                    .as_ref()
                    .map(|n| {
                        n.occurrences_in_order()
                            .into_iter()
                            .map(|o| (o.id, o.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let current = occurrences
                    .iter()
                    .find(|(id, _)| Some(*id) == self.occurrence)
                    .map_or_else(String::new, |(_, n)| n.clone());
                let items: Vec<String> = occurrences.iter().map(|(_, n)| n.clone()).collect();
                let popup = Rect::from_min_size(area.min, vec2(column, CONTROL_HEIGHT));
                if let Some(i) = popup_menu(ui, popup, "Table", &current, &items, true) {
                    if self.occurrence != Some(occurrences[i].0) {
                        self.occurrence = Some(occurrences[i].0);
                        self.field = None;
                    }
                }
                let fields = self.fields();
                let rows: Vec<ListRow> = fields.iter().map(|(_, n)| ListRow::item(n)).collect();
                let selected = fields.iter().position(|(id, _)| Some(*id) == self.field);
                let field_list = Rect::from_min_max(
                    pos2(area.left(), area.top() + CONTROL_HEIGHT + 6.0),
                    pos2(area.left() + column, area.bottom()),
                );
                let click = list_box(ui, field_list, "find_fields", &rows, selected, true);
                if let Some(i) = click.clicked.or(click.double_clicked) {
                    self.field = Some(fields[i].0);
                }
                // Criteria, Insert Operator, Add / Change / Remove.
                let criteria_rect = Rect::from_min_max(
                    pos2(right_x, area.top()),
                    pos2(area.right(), area.top() + CONTROL_HEIGHT),
                );
                text_field(ui, criteria_rect, &mut self.criteria, "Criteria", true);
                let operators: Vec<String> =
                    OPERATORS.iter().map(|(l, _)| (*l).to_owned()).collect();
                let popup = Rect::from_min_size(
                    pos2(right_x, criteria_rect.bottom() + 10.0),
                    vec2(180.0, CONTROL_HEIGHT),
                );
                if let Some(n) = popup_menu(
                    ui,
                    popup,
                    "Insert Operator",
                    "Insert Operator",
                    &operators,
                    true,
                ) {
                    self.criteria.push_str(OPERATORS[n].1);
                }
                let button = |n: usize| {
                    Rect::from_min_size(
                        pos2(
                            right_x + n as f32 * (BUTTON_WIDTH + 8.0),
                            area.bottom() - CONTROL_HEIGHT,
                        ),
                        vec2(BUTTON_WIDTH, CONTROL_HEIGHT),
                    )
                };
                let chosen = self.chosen().is_some();
                if push_button(ui, button(0), "Add", "Add", false, chosen).clicked() {
                    self.add();
                }
                let selected = self.selected.is_some();
                if push_button(ui, button(1), "Change", "Change", false, chosen && selected)
                    .clicked()
                {
                    self.change();
                }
                if push_button(ui, button(2), "Remove", "Remove", false, selected).clicked() {
                    self.remove();
                }
                ui.add_space(10.0);
                let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
                pickers::row_label(
                    ui,
                    Rect::from_min_size(row.min, vec2(78.0, CONTROL_HEIGHT)),
                    "Repetition:",
                    true,
                );
                text_field(
                    ui,
                    Rect::from_min_size(
                        pos2(row.left() + 86.0, row.top()),
                        vec2(60.0, CONTROL_HEIGHT),
                    ),
                    &mut self.repetition,
                    "Criterion Repetition",
                    true,
                );
                ui.add_space(20.0);
                let (cancel, ok) = pickers::cancel_ok(ui, true);
                if cancel {
                    outcome = Some(Picked::Cancel);
                } else if ok {
                    outcome = Some(Picked::Ok(self.request.clone()));
                }
            },
        );
        if outcome.is_none() {
            if shown.escape {
                outcome = Some(Picked::Cancel);
            } else if shown.enter {
                outcome = Some(Picked::Ok(self.request.clone()));
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(n: u64) -> SpecField {
        SpecField {
            occurrence: Some(0xd00002),
            field: n,
            repetition: 1,
        }
    }

    #[test]
    fn criteria_are_added_changed_and_removed_one_per_field() {
        let mut edit = EditRequest::new(
            StoredRequest {
                omit: false,
                criteria: vec![(field(4), "In Progress".into())],
            },
            None,
        );
        edit.occurrence = Some(0xd00002);
        edit.field = Some(1);
        edit.criteria = "alpha".into();
        edit.add();
        edit.criteria = "beta".into();
        edit.add();
        assert_eq!(
            edit.request.criteria,
            [(field(4), "In Progress".into()), (field(1), "beta".into())]
        );
        edit.select(0);
        assert_eq!(edit.criteria, "In Progress");
        edit.criteria = "==Done".into();
        edit.change();
        assert_eq!(edit.request.criteria[0].1, "==Done");
        // Changing line 1 to field 4's field drops the older line for it.
        edit.select(1);
        edit.field = Some(4);
        edit.change();
        assert_eq!(edit.request.criteria, [(field(4), "beta".into())]);
        edit.remove();
        assert!(edit.request.criteria.is_empty());
        assert_eq!(
            criteria_text(
                None,
                &StoredRequest {
                    omit: true,
                    criteria: vec![(field(4), "x".into())]
                }
            ),
            format!(
                "{}::{}: [x]",
                om_data::formula::TABLE_MISSING,
                om_data::formula::FIELD_MISSING
            )
        );
    }
}
