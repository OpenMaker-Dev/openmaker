//! File > Manage > Database: the provider's schema, edited as FileMaker Pro
//! edits it. Opening the dialog begins an edit on the provider (which, for
//! a host, takes FileMaker's schema lock); Create, Change and Delete stage
//! each change on the provider's copy and show the result; OK saves them
//! all and Cancel drops them (`DataProvider::begin_schema_edit`,
//! `apply_schema_change`, `commit_schema_changes`, `cancel_schema_changes`).
//! The Tables and Fields tabs follow FileMaker Pro 26's layout: the
//! column lists, "View by", the name and comment editors and the buttons,
//! and Options for Field (`database_options`); what OpenMaker cannot do yet
//! (calculation and summary fields, copy and paste, import, print, custom
//! order) is shown disabled.

use egui::{
    pos2, vec2, Align, Align2, CornerRadius, Id, Layout, RichText, Role, Sense, Stroke, StrokeKind,
    WidgetInfo,
};
use std::sync::Arc;

use om_data::calc::ValueType;
use om_data::calc_field::{CalcStorage, CalculationField};
use om_data::host::CalcSchema;
use om_data::{DataError, DatabaseSchema, FieldKind, SchemaChange, SchemaFieldKind};

use crate::database_options::{Answer, OptionsDialog};
use crate::script_workspace::calculation::{CalcOutcome, FieldOptions, SpecifyCalculation};
use crate::{database_graph, theme, widgets};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DatabaseAction {
    /// Stage a change on the provider's draft.
    Apply(SchemaChange),
    /// OK: save the staged changes.
    Save,
    /// Cancel: drop them.
    Cancel,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    Tables,
    #[default]
    Fields,
    Relationships,
}

/// The "View by" orders FileMaker Pro offers that OpenMaker has: creation
/// order (by id) and by name; for fields also by type.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Order {
    #[default]
    Creation,
    Name,
    Type,
}

impl Order {
    fn label(self) -> &'static str {
        match self {
            Order::Creation => "creation order",
            Order::Name => "name",
            Order::Type => "field type",
        }
    }
}

/// One cell of a list row and whether it is drawn dimmed.
struct Cell {
    text: String,
    weak: bool,
}

impl Cell {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            weak: false,
        }
    }
}

pub(crate) struct ManageDatabase {
    file: String,
    schema: Option<DatabaseSchema>,
    error: Option<String>,
    busy: bool,
    /// OK was pressed and the save is outstanding.
    saving: bool,
    tab: Tab,
    table: Option<u64>,
    field: Option<u64>,
    occurrence: Option<u64>,
    /// The other occurrences selected with `occurrence` (Shift-click).
    also: std::collections::BTreeSet<u64>,
    name: String,
    comment: String,
    data_type: FieldKind,
    table_order: Order,
    field_order: Order,
    /// Whether the Tables tab's third column shows comments instead of
    /// details (FileMaker's "Click to toggle").
    table_comments: bool,
    /// What the Fields tab's third column shows; its header steps it.
    field_column: om_data::FieldColumn,
    confirm_delete: Option<SchemaChange>,
    /// "Also remove occurrences of these tables in the graph".
    remove_occurrences: bool,
    /// A failed save's reason, kept over the schema read after it.
    save_error: Option<String>,
    /// Options for Field, while it is open.
    options: Option<OptionsDialog>,
    /// The selected relationship, and the open Edit Relationship and
    /// Specify Table dialogs.
    relationship: Option<u64>,
    edit_relationship: Option<crate::database_relationship::EditRelationship>,
    specify_table: Option<crate::database_relationship::SpecifyTable>,
    /// The kind the Type menu chose: normal (with `data_type` its type),
    /// calculation or summary.
    field_kind: SchemaFieldKind,
    /// Options for Summary Field for a new or a selected summary field.
    summary: Option<crate::database_summary::SummaryDialog>,
    /// Specify Calculation for a new or a selected calculation field.
    calculation: Option<CalculationEdit>,
    /// The file's calculation schema: its functions and locale for
    /// Specify Calculation.
    names: Option<Arc<CalcSchema>>,
}

/// Specify Calculation, open from Manage Database.
struct CalculationEdit {
    dialog: SpecifyCalculation,
    /// The new field's name and comment, or the field being changed.
    target: CalculationTarget,
    /// OK was pressed and the change is staged on the provider: the
    /// dialog closes when it is accepted and stays, with FileMaker's
    /// alert, when it is refused.
    waiting: bool,
}

enum CalculationTarget {
    New {
        table_id: u64,
        name: String,
        comment: String,
    },
    Existing {
        table_id: u64,
        field_id: u64,
    },
}

impl ManageDatabase {
    pub(crate) fn new(file: String) -> Self {
        Self {
            file,
            schema: None,
            error: None,
            busy: true,
            saving: false,
            tab: Tab::Fields,
            table: None,
            field: None,
            occurrence: None,
            also: Default::default(),
            name: String::new(),
            comment: String::new(),
            data_type: FieldKind::Text,
            table_order: Order::Creation,
            field_order: Order::Creation,
            table_comments: false,
            field_column: om_data::FieldColumn::Options,
            confirm_delete: None,
            remove_occurrences: false,
            save_error: None,
            options: None,
            relationship: None,
            edit_relationship: None,
            specify_table: None,
            field_kind: SchemaFieldKind::Normal,
            summary: None,
            calculation: None,
            names: None,
        }
    }

    /// The file's calculation schema, for Specify Calculation's names.
    pub(crate) fn set_names(&mut self, names: Option<Arc<CalcSchema>>) {
        self.names = names;
    }

    /// Open Specify Calculation for a new calculation field of
    /// `table_id`, or for field `field_id`'s calculation.
    /// What Options for Field's formulas compile against, as Specify
    /// Calculation has it for a calculation field, and the file's value
    /// lists.
    fn option_names(&self, table_id: u64) -> crate::database_options::OptionNames {
        let names = self.schema.as_ref().map(|schema| {
            Arc::new(match &self.names {
                Some(file) => file.with_database_schema(schema),
                None => CalcSchema::new(&self.file, om_data::calc::Locale::en_us())
                    .with_database_schema(schema),
            })
        });
        // The table's first occurrence, as for a new calculation field.
        let context = self
            .schema
            .iter()
            .flat_map(|s| &s.occurrences)
            .find(|o| o.table_id == Some(table_id))
            .map_or(0, |o| o.id);
        let value_lists = self
            .names
            .iter()
            .flat_map(|n| n.value_lists())
            .map(|list| {
                let problem = om_data::rules::value_list_problem(list);
                (list.id, list.name.clone(), problem)
            })
            .collect();
        crate::database_options::OptionNames {
            names,
            context,
            value_lists,
            schema: self.schema.clone(),
        }
    }

    fn open_calculation(&mut self, table_id: u64, field_id: Option<u64>) {
        let Some(schema) = &self.schema else {
            return;
        };
        let names = Arc::new(match &self.names {
            Some(file) => file.with_database_schema(schema),
            None => CalcSchema::new(&self.file, om_data::calc::Locale::en_us())
                .with_database_schema(schema),
        });
        let (title, calculation, target) = if let Some(field_id) = field_id {
            let Some((name, calculation)) = schema
                .table(table_id)
                .and_then(|t| t.fields.iter().find(|f| f.id == field_id))
                .and_then(|f| Some((f.name.clone(), f.calculation.clone()?)))
            else {
                return;
            };
            (
                name,
                calculation,
                CalculationTarget::Existing { table_id, field_id },
            )
        } else {
            // The table's first occurrence, which the context popup opens
            // on (FileMaker Pro's choice is not established).
            let Some(context) = schema
                .occurrences
                .iter()
                .find(|o| o.table_id == Some(table_id))
                .map(|o| o.id)
            else {
                self.error = Some(
                    "The table has no occurrence in the graph to evaluate a calculation from."
                        .into(),
                );
                return;
            };
            let mut calculation = CalculationField::new("", context);
            calculation.result = self.data_type;
            (
                self.name.trim().to_owned(),
                calculation,
                CalculationTarget::New {
                    table_id,
                    name: self.name.trim().to_owned(),
                    comment: self.comment.clone(),
                },
            )
        };
        let dialog = SpecifyCalculation::with_text(
            format!("Specify Calculation “{title}”"),
            &calculation.formula,
            Some(names),
        )
        .for_field(
            calculation.context,
            FieldOptions {
                result: value_type(calculation.result),
                repetitions: u32::from(calculation.repetitions),
                skip_when_empty: calculation.skip_when_empty,
                storage: Some(calculation.storage.clone()),
                option: None,
            },
        );
        self.calculation = Some(CalculationEdit {
            dialog,
            target,
            waiting: false,
        });
    }

    /// Open Options for Summary Field for a new summary field of
    /// `table_id`, or for field `field_id`'s summary.
    fn open_summary(&mut self, table_id: u64, field_id: Option<u64>) {
        use crate::database_summary::{SummaryDialog, Target};
        let Some(schema) = &self.schema else {
            return;
        };
        let Some(table) = schema.table(table_id) else {
            return;
        };
        let Some(occurrence) = schema
            .occurrences
            .iter()
            .find(|o| o.table_id == Some(table_id))
            .map(|o| o.id)
        else {
            self.error = Some("The table has no occurrence in the graph to summarize from.".into());
            return;
        };
        let existing = field_id.and_then(|id| table.fields.iter().find(|f| f.id == id));
        let (name, target, current) = match (field_id, existing) {
            (Some(field_id), Some(field)) => (
                field.name.clone(),
                Target::Existing { table_id, field_id },
                field.summary,
            ),
            (Some(_), None) => return,
            (None, _) => (
                self.name.trim().to_owned(),
                Target::New {
                    table_id,
                    name: self.name.trim().to_owned(),
                    comment: self.comment.clone(),
                },
                None,
            ),
        };
        self.summary = Some(SummaryDialog::new(
            &name,
            target,
            table.fields.clone(),
            occurrence,
            current.as_ref(),
        ));
    }

    /// Specify Calculation's OK: the change to stage.
    fn calculation_change(edit: &CalculationEdit) -> Option<SchemaChange> {
        let (formula, context) = edit.dialog.field_formula()?;
        let options = edit.dialog.field_options()?;
        let calculation = CalculationField {
            formula,
            context,
            result: field_kind(options.result),
            repetitions: u16::try_from(options.repetitions).unwrap_or(u16::MAX),
            skip_when_empty: options.skip_when_empty,
            storage: options.storage.unwrap_or_else(CalcStorage::default),
        };
        Some(match &edit.target {
            CalculationTarget::New {
                table_id,
                name,
                comment,
            } => SchemaChange::CreateCalculationField {
                table_id: *table_id,
                name: name.clone(),
                comment: comment.clone(),
                calculation,
            },
            CalculationTarget::Existing { table_id, field_id } => SchemaChange::ChangeCalculation {
                table_id: *table_id,
                field_id: *field_id,
                calculation,
            },
        })
    }

    /// The schema the provider shows: the edit as it begins, or as a
    /// staged change left it. A refused change keeps the schema shown.
    pub(crate) fn accept_schema(&mut self, result: Result<DatabaseSchema, DataError>) {
        self.busy = false;
        self.saving = false;
        // Specify Calculation's change: accepted, it closes; refused, it
        // stays open with the reason, as FileMaker Pro's does.
        if let Some(dialog) = self.summary.as_mut().filter(|d| d.waiting) {
            dialog.waiting = false;
            match &result {
                Ok(_) => self.summary = None,
                Err(error) => {
                    dialog.error = Some(error.message.clone());
                    return;
                }
            }
        }
        if let Some(edit) = self.calculation.as_mut().filter(|e| e.waiting) {
            edit.waiting = false;
            match &result {
                Ok(_) => self.calculation = None,
                Err(error) => {
                    edit.dialog.show_alert(&error.message);
                    return;
                }
            }
        }
        match result {
            Ok(schema) => {
                // A table or field the change made is selected, as
                // FileMaker Pro selects what Create made.
                if let Some(before) = &self.schema {
                    let new_table = schema
                        .tables
                        .iter()
                        .find(|t| before.table(t.id).is_none())
                        .map(|t| t.id);
                    if let Some(id) = new_table {
                        self.table = Some(id);
                        self.field = None;
                    } else if let (Some(now), Some(then)) = (
                        self.table.and_then(|id| schema.table(id)),
                        self.table.and_then(|id| before.table(id)),
                    ) {
                        if let Some(field) = now
                            .fields
                            .iter()
                            .find(|f| !then.fields.iter().any(|g| g.id == f.id))
                        {
                            self.field = Some(field.id);
                        }
                    }
                }
                if !schema
                    .tables
                    .iter()
                    .any(|table| Some(table.id) == self.table)
                {
                    self.table = schema.tables.first().map(|table| table.id);
                    self.field = None;
                }
                if !schema
                    .occurrences
                    .iter()
                    .any(|item| Some(item.id) == self.occurrence)
                {
                    self.occurrence = None;
                }
                self.also
                    .retain(|id| schema.occurrences.iter().any(|item| item.id == *id));
                if self.occurrence.is_none() {
                    self.occurrence = self.also.pop_first();
                }
                if !schema
                    .relationships
                    .iter()
                    .any(|item| Some(item.id) == self.relationship)
                {
                    self.relationship = None;
                }
                if !self
                    .table
                    .and_then(|id| schema.table(id))
                    .is_some_and(|table| {
                        table
                            .fields
                            .iter()
                            .any(|field| Some(field.id) == self.field)
                    })
                {
                    self.field = None;
                }
                self.schema = Some(schema);
                self.error = self.save_error.take();
                self.sync_editor();
            }
            Err(error) => self.error = Some(error.message),
        }
    }

    /// Whether the dialog has shown a schema yet: an edit that could not
    /// begin closes it instead.
    pub(crate) fn has_schema(&self) -> bool {
        self.schema.is_some()
    }

    /// The save failed; the dialog stays open with the reason, and its
    /// schema is read again.
    pub(crate) fn save_failed(&mut self, error: DataError) {
        self.saving = false;
        self.busy = true;
        self.error = Some(error.message.clone());
        self.save_error = Some(error.message);
    }

    fn sync_editor(&mut self) {
        let Some(schema) = &self.schema else {
            return;
        };
        match self.tab {
            Tab::Tables => {
                let table = self.table.and_then(|id| schema.table(id));
                self.name = table.map_or_else(String::new, |t| t.name.clone());
                self.comment = table.map_or_else(String::new, |t| t.comment.clone());
            }
            Tab::Fields => {
                if let Some(field) = self
                    .table
                    .and_then(|id| schema.table(id))
                    .and_then(|table| {
                        table
                            .fields
                            .iter()
                            .find(|field| Some(field.id) == self.field)
                    })
                {
                    self.name.clone_from(&field.name);
                    self.comment.clone_from(&field.comment);
                    self.data_type = field.data_type;
                    self.field_kind = field.kind;
                } else {
                    self.name.clear();
                    self.comment.clear();
                    self.data_type = FieldKind::Text;
                    self.field_kind = SchemaFieldKind::Normal;
                }
            }
            Tab::Relationships => {
                self.name = schema
                    .occurrences
                    .iter()
                    .find(|item| Some(item.id) == self.occurrence)
                    .map_or_else(String::new, |item| item.name.clone())
            }
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<DatabaseAction> {
        let width = (ctx.content_rect().width() - 64.0).clamp(560.0, 900.0);
        let height = (ctx.content_rect().height() - 300.0).clamp(160.0, 420.0);
        let mut action = None;
        let response = egui::Modal::new(Id::unique("manage_database"))
            .frame(
                egui::Frame::new()
                    .fill(theme::DIALOG_BG)
                    .stroke(Stroke::new(1.0, theme::DIVIDER))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(18)
                    .shadow(ctx.global_style().visuals.window_shadow),
            )
            .backdrop_color(theme::SCRIM)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.label(
                    RichText::new(format!("Manage Database for “{}”", self.file))
                        .font(egui::FontId::new(15.0, theme::semibold())),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space((width - 280.0).max(0.0) / 2.0);
                    let tabs = ["Tables", "Fields", "Relationships"];
                    let selected = match self.tab {
                        Tab::Tables => 0,
                        Tab::Fields => 1,
                        Tab::Relationships => 2,
                    };
                    if let Some(index) = tab_bar(ui, &tabs, selected) {
                        self.tab = [Tab::Tables, Tab::Fields, Tab::Relationships][index];
                        self.sync_editor();
                    }
                });
                ui.add_space(10.0);
                ui.add_enabled_ui(!self.busy, |ui| {
                    if self.schema.is_some() {
                        match self.tab {
                            Tab::Tables => self.tables(ui, width, height, &mut action),
                            Tab::Fields => self.fields(ui, width, height, &mut action),
                            Tab::Relationships => self.relationships(ui, height, &mut action),
                        }
                    } else {
                        ui.set_min_height(height);
                        ui.centered_and_justified(|ui| {
                            if self.busy {
                                ui.spinner();
                                ui.label("Loading database schema…");
                            } else {
                                ui.label("The database schema could not be loaded.");
                            }
                        });
                    }
                });
                if self.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(if self.saving {
                            "Saving the schema…"
                        } else {
                            "Waiting for the server…"
                        });
                    });
                }
                if let Some(error) = &self.error {
                    ui.colored_label(theme::RED_INK, error);
                }
                if let Some(reason) = self
                    .schema
                    .as_ref()
                    .and_then(|schema| schema.capabilities.unavailable_reason.as_deref())
                {
                    ui.label(RichText::new(reason).small().color(theme::CAPTION));
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    widgets::dialog_button(ui, "Print…", false, false)
                        .on_disabled_hover_text("OpenMaker does not print the schema yet.");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::dialog_button(ui, "OK", true, !self.busy).clicked() {
                            action = Some(DatabaseAction::Save);
                        }
                        if widgets::dialog_button(ui, "Cancel", false, !self.saving).clicked() {
                            action = Some(DatabaseAction::Cancel);
                        }
                    });
                });
            });
        if response.should_close()
            && !self.busy
            && self.confirm_delete.is_none()
            && self.options.is_none()
            && self.edit_relationship.is_none()
            && self.specify_table.is_none()
            && self.calculation.is_none()
            && self.summary.is_none()
            && action.is_none()
        {
            action = Some(DatabaseAction::Cancel);
        }
        if let Some(schema) = &self.schema {
            use crate::database_relationship::Answer;
            if let Some(dialog) = &mut self.edit_relationship {
                // "Sort records" or "Specify…": Sort Records over the schema
                // as edited, on that side's occurrence.
                if let Some((right, occurrence, spec)) = dialog.take_sort_request() {
                    let file = self.names.clone();
                    let names = std::sync::Arc::new(match file {
                        Some(file) => file.with_database_schema(schema),
                        None => om_data::host::CalcSchema::new(
                            &self.file,
                            om_data::calc::Locale::en_us(),
                        )
                        .with_database_schema(schema),
                    });
                    dialog.open_sort(
                        right,
                        crate::script_workspace::SortOrder::new(spec, Some(names))
                            .for_occurrence(occurrence),
                    );
                }
                match dialog.show(ctx, schema) {
                    Answer::Open => {}
                    Answer::Cancel => self.edit_relationship = None,
                    Answer::Ok(change) => {
                        self.edit_relationship = None;
                        action = Some(DatabaseAction::Apply(change));
                    }
                }
            } else if let Some(dialog) = &mut self.specify_table {
                match dialog.show(ctx, schema) {
                    Answer::Open => {}
                    Answer::Cancel => self.specify_table = None,
                    Answer::Ok(change) => {
                        self.specify_table = None;
                        action = Some(DatabaseAction::Apply(change));
                    }
                }
            }
        }
        if let Some(change) = self.confirm_delete.clone() {
            if let Some(confirmed) = self.confirm(ctx, change) {
                action = confirmed.map(DatabaseAction::Apply);
                self.confirm_delete = None;
            }
        }
        if let Some(dialog) = self.options.as_mut() {
            match dialog.show(ctx) {
                Some(Answer::Ok(change)) => {
                    action = Some(DatabaseAction::Apply(change));
                    self.options = None;
                }
                Some(Answer::Cancel) => self.options = None,
                None => {}
            }
        }
        if let Some(edit) = self.calculation.as_mut() {
            match edit.dialog.show(ctx) {
                _ if edit.waiting => {}
                Some(CalcOutcome::Ok(_)) => {
                    if let Some(change) = Self::calculation_change(edit) {
                        edit.waiting = true;
                        action = Some(DatabaseAction::Apply(change));
                    }
                }
                Some(CalcOutcome::Cancel) => self.calculation = None,
                None => {}
            }
        }
        if let Some(dialog) = self.summary.as_mut() {
            match dialog.show(ctx) {
                _ if dialog.waiting => {}
                Some(crate::database_summary::Answer::Ok(change)) => {
                    dialog.waiting = true;
                    dialog.error = None;
                    action = Some(DatabaseAction::Apply(change));
                }
                Some(crate::database_summary::Answer::Cancel) => self.summary = None,
                None => {}
            }
        }
        match action {
            Some(DatabaseAction::Apply(_)) => {
                self.busy = true;
                self.error = None;
            }
            Some(DatabaseAction::Save) => {
                self.busy = true;
                self.saving = true;
                self.error = None;
            }
            _ => {}
        }
        action
    }

    /// FileMaker Pro's confirmation of a deletion: `Some(Some(change))`
    /// to delete, `Some(None)` to cancel, `None` while it is open.
    fn confirm(
        &mut self,
        ctx: &egui::Context,
        change: SchemaChange,
    ) -> Option<Option<SchemaChange>> {
        let mut answer = None;
        egui::Modal::new(Id::unique("database_delete_confirmation")).show(ctx, |ui| {
            ui.set_width(420.0);
            let (title, text) = match &change {
                SchemaChange::DeleteTable { .. } => (
                    Some("Delete Tables"),
                    "Permanently delete the selected 1 FileMaker table(s) and all of their \
                     fields and record data?",
                ),
                SchemaChange::DeleteField { .. } => (
                    None,
                    "Permanently delete the selected 1 field(s) and ALL of their field data?",
                ),
                SchemaChange::DeleteRelationship { .. } => {
                    (None, "Delete the selected relationship?")
                }
                _ => (
                    None,
                    "Delete the selected table occurrence and the relationships that use it?",
                ),
            };
            if let Some(title) = title {
                ui.label(RichText::new(title).strong());
                ui.add_space(6.0);
            }
            ui.label(text);
            if matches!(change, SchemaChange::DeleteTable { .. }) {
                ui.checkbox(
                    &mut self.remove_occurrences,
                    "Also remove occurrences of these tables in the graph",
                );
            }
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let table = matches!(change, SchemaChange::DeleteTable { .. });
                // FileMaker Pro makes Cancel the default for a table.
                if widgets::dialog_button(ui, "Cancel", table, true).clicked() {
                    answer = Some(None);
                }
                if widgets::dialog_button(ui, "Delete", !table, true).clicked() {
                    let change = match change.clone() {
                        SchemaChange::DeleteTable { table_id, .. } => SchemaChange::DeleteTable {
                            table_id,
                            remove_occurrences: self.remove_occurrences,
                        },
                        other => other,
                    };
                    answer = Some(Some(change));
                }
            });
        });
        answer
    }

    fn tables(
        &mut self,
        ui: &mut egui::Ui,
        width: f32,
        height: f32,
        action: &mut Option<DatabaseAction>,
    ) {
        let schema = self.schema.as_ref().expect("schema loaded");
        let writable = schema.capabilities.tables;
        ui.label("A table is a unique set of records and fields. A file can contain more than one table.");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} defined in this file",
                count_label(schema.tables.len(), "table")
            ));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                order_menu(
                    ui,
                    "Tables View By",
                    &mut self.table_order,
                    &[Order::Creation, Order::Name],
                );
                ui.label("View by:");
            });
        });
        let mut tables: Vec<_> = schema.tables.iter().collect();
        if self.table_order == Order::Name {
            tables.sort_by_key(|t| t.name.to_lowercase());
        }
        // FileMaker Pro's title stays as it is whatever the column shows
        // (`FMDefineTablesPage::OnInitDialog`, resource 329 `0x66e`).
        let third = "Details / Comments (Click to toggle)";
        let rows: Vec<(u64, Vec<Cell>)> = tables
            .iter()
            .map(|table| {
                let occurrences: Vec<&str> = schema
                    .occurrences
                    .iter()
                    .filter(|o| o.table_id == Some(table.id))
                    .map(|o| o.name.as_str())
                    .collect();
                let details = if self.table_comments {
                    table.comment.clone()
                } else {
                    count_label(table.fields.len(), "field")
                };
                (
                    table.id,
                    vec![
                        Cell::new(table.name.clone()),
                        Cell::new("FileMaker"),
                        Cell::new(details),
                        Cell::new(occurrences.join(", ")),
                    ],
                )
            })
            .collect();
        let ListInput {
            clicked, toggled, ..
        } = column_list(
            ui,
            "database_tables",
            width,
            height,
            &[
                ("Table Name", 0.22),
                ("Source", 0.12),
                (third, 0.32),
                ("Occurrences in Graph", 0.34),
            ],
            Some(2),
            &rows,
            self.table,
        );
        if toggled {
            self.table_comments = !self.table_comments;
        }
        if let Some(id) = clicked {
            self.table = Some(id);
            self.field = None;
            self.sync_editor();
        }
        ui.add_space(8.0);
        let named = valid_name(&self.name);
        let selected = self.table;
        ui.horizontal(|ui| {
            labeled_input(ui, "Table Name", &mut self.name, width - 330.0);
            if widgets::dialog_button(ui, "Create", false, writable && named).clicked() {
                *action = Some(DatabaseAction::Apply(SchemaChange::CreateTable {
                    name: self.name.trim().to_owned(),
                    comment: self.comment.clone(),
                }));
            }
            if widgets::dialog_button(ui, "Change", false, writable && named && selected.is_some())
                .clicked()
            {
                *action = Some(DatabaseAction::Apply(SchemaChange::ChangeTable {
                    table_id: selected.expect("enabled with a table"),
                    name: self.name.trim().to_owned(),
                    comment: self.comment.clone(),
                }));
            }
            if widgets::dialog_button(ui, "Delete", false, writable && selected.is_some()).clicked()
            {
                self.remove_occurrences = false;
                self.confirm_delete = Some(SchemaChange::DeleteTable {
                    table_id: selected.expect("enabled with a table"),
                    remove_occurrences: false,
                });
            }
        });
        ui.horizontal(|ui| {
            labeled_input(ui, "Comment", &mut self.comment, width - 330.0);
            for label in ["Copy", "Paste", "Import…"] {
                widgets::dialog_button(ui, label, false, false)
                    .on_disabled_hover_text("OpenMaker cannot copy, paste or import tables yet.");
            }
        });
    }

    fn fields(
        &mut self,
        ui: &mut egui::Ui,
        width: f32,
        height: f32,
        action: &mut Option<DatabaseAction>,
    ) {
        let schema = self.schema.as_ref().expect("schema loaded");
        let mut table_id = self.table;
        ui.horizontal(|ui| {
            ui.label("Table:");
            egui::ComboBox::from_id_salt("database_table")
                .width(180.0)
                .selected_text(
                    table_id
                        .and_then(|id| schema.table(id))
                        .map_or("", |t| t.name.as_str()),
                )
                .show_ui(ui, |ui| {
                    for table in &schema.tables {
                        ui.selectable_value(&mut table_id, Some(table.id), &table.name);
                    }
                })
                .response
                .widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Database Table")
                });
            if let Some(table) = table_id.and_then(|id| schema.table(id)) {
                ui.add_space(12.0);
                ui.label(count_label(table.fields.len(), "field"));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                order_menu(
                    ui,
                    "Fields View By",
                    &mut self.field_order,
                    &[Order::Creation, Order::Name, Order::Type],
                );
                ui.label("View by:");
            });
        });
        if table_id != self.table {
            self.table = table_id;
            self.field = None;
            self.sync_editor();
        }
        let schema = self.schema.as_ref().expect("schema loaded");
        let writable = schema.capabilities.fields;
        let Some(table) = self.table.and_then(|id| schema.table(id)) else {
            ui.label("Create a table to define fields.");
            return;
        };
        let table_id = table.id;
        let mut fields: Vec<_> = table.fields.iter().collect();
        match self.field_order {
            Order::Name => fields.sort_by_key(|field| field.name.to_lowercase()),
            Order::Type => {
                fields.sort_by_key(|field| (type_rank(field.kind, field.data_type), field.id))
            }
            Order::Creation => {}
        }
        // FileMaker Pro's title stays as it is whatever the column shows.
        let third = om_data::FieldColumn::TITLE;
        let rows: Vec<(u64, Vec<Cell>)> = fields
            .iter()
            .map(|field| {
                (
                    field.id,
                    vec![
                        Cell::new(field.name.clone()),
                        Cell::new(field_type(field.kind, field.data_type)),
                        Cell::new(schema.field_column_text(table, field, self.field_column)),
                    ],
                )
            })
            .collect();
        let ListInput {
            clicked,
            opened,
            toggled,
        } = column_list(
            ui,
            "database_fields",
            width,
            height,
            &[("Field Name", 0.27), ("Type", 0.2), (third, 0.53)],
            Some(2),
            &rows,
            self.field,
        );
        if toggled {
            self.field_column = self.field_column.next();
        }
        if let Some(id) = opened.or(clicked) {
            self.field = Some(id);
            self.sync_editor();
        }
        let selected = self.field;
        let selected_field = self
            .schema
            .as_ref()
            .and_then(|schema| schema.table(table_id))
            .and_then(|table| table.fields.iter().find(|field| Some(field.id) == selected));
        // Change renames and comments a field of the kind the Type menu
        // shows; a normal field's type changes too. Turning a field into a
        // calculation or back is not built.
        let editable_field = selected_field.is_some_and(|field| match field.kind {
            SchemaFieldKind::Normal => self.field_kind == SchemaFieldKind::Normal,
            kind => kind == self.field_kind,
        });
        // Options for Field, for a normal field whose definition reads.
        let options_of = selected_field
            .filter(|field| field.kind == SchemaFieldKind::Normal)
            .and_then(|field| {
                Some((
                    field.id,
                    field.name.clone(),
                    field.data_type,
                    field.options.clone()?,
                ))
            });
        // Specify Calculation, for a calculation field whose formula reads.
        let calculation_of = selected_field
            .filter(|field| field.calculation.is_some())
            .map(|field| field.id);
        // Options for Summary Field, for a summary field.
        let summary_of = selected_field
            .filter(|field| field.summary.is_some())
            .map(|field| field.id);
        ui.add_space(8.0);
        let named = valid_name(&self.name);
        let mut open_calculation = None;
        let mut open_summary = None;
        ui.horizontal(|ui| {
            labeled_input(ui, "Field Name", &mut self.name, width - 380.0);
            ui.label("Type:");
            let shown = match self.field_kind {
                SchemaFieldKind::Calculation => "Calculation",
                SchemaFieldKind::Summary => "Summary",
                _ => type_name(self.data_type),
            };
            egui::ComboBox::from_id_salt("database_field_type")
                .width(110.0)
                // Every type and the two kinds, without a scroll bar.
                .height(400.0)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for kind in field_types() {
                        let on =
                            self.field_kind == SchemaFieldKind::Normal && self.data_type == kind;
                        if ui.selectable_label(on, type_name(kind)).clicked() {
                            self.data_type = kind;
                            self.field_kind = SchemaFieldKind::Normal;
                        }
                    }
                    ui.separator();
                    for (kind, label) in [
                        (SchemaFieldKind::Calculation, "Calculation"),
                        (SchemaFieldKind::Summary, "Summary"),
                    ] {
                        if ui
                            .selectable_label(self.field_kind == kind, label)
                            .clicked()
                        {
                            self.field_kind = kind;
                        }
                    }
                })
                .response
                .widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Database Field Type")
                });
            let enabled = writable
                && (options_of.is_some() || calculation_of.is_some() || summary_of.is_some());
            let options = widgets::dialog_button(ui, "Options…", false, enabled);
            // The double-click selected its row before resolving the options.
            if options.clicked() || (enabled && opened.is_some()) {
                if let Some((field_id, name, data_type, options)) = options_of.clone() {
                    let names = self.option_names(table_id);
                    self.options = Some(OptionsDialog::new(
                        table_id, field_id, &name, data_type, options, names,
                    ));
                } else if let Some(field_id) = calculation_of {
                    open_calculation = Some(Some(field_id));
                } else if let Some(field_id) = summary_of {
                    open_summary = Some(Some(field_id));
                }
            }
            options.on_disabled_hover_text("Select a field with options.");
        });
        ui.horizontal(|ui| {
            labeled_input(ui, "Comment", &mut self.comment, width - 250.0);
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if widgets::dialog_button(ui, "Create", writable && named, writable && named).clicked()
            {
                if self.field_kind == SchemaFieldKind::Calculation {
                    // A calculation field is made by Specify Calculation's OK.
                    open_calculation = Some(None);
                } else if self.field_kind == SchemaFieldKind::Summary {
                    // A summary field is made by Options for Summary Field's OK.
                    open_summary = Some(None);
                } else {
                    *action = Some(DatabaseAction::Apply(SchemaChange::CreateField {
                        table_id,
                        name: self.name.trim().to_owned(),
                        data_type: self.data_type,
                        comment: self.comment.clone(),
                    }));
                }
            }
            if widgets::dialog_button(
                ui,
                "Change",
                false,
                writable && editable_field && named && selected.is_some(),
            )
            .clicked()
            {
                *action = Some(DatabaseAction::Apply(SchemaChange::ChangeField {
                    table_id,
                    field_id: selected.expect("enabled with a field"),
                    name: self.name.trim().to_owned(),
                    data_type: self.data_type,
                    comment: self.comment.clone(),
                }));
            }
            if widgets::dialog_button(ui, "Duplicate", false, writable && selected.is_some())
                .clicked()
            {
                *action = Some(DatabaseAction::Apply(SchemaChange::DuplicateField {
                    table_id,
                    field_id: selected.expect("enabled with a field"),
                }));
            }
            if widgets::dialog_button(ui, "Delete", false, writable && selected.is_some()).clicked()
            {
                self.confirm_delete = Some(SchemaChange::DeleteField {
                    table_id,
                    field_id: selected.expect("enabled with a field"),
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for label in ["Paste", "Copy"] {
                    widgets::dialog_button(ui, label, false, false)
                        .on_disabled_hover_text("OpenMaker cannot copy or paste fields yet.");
                }
            });
        });
        if let Some(field) = open_calculation {
            self.open_calculation(table_id, field);
        }
        if let Some(field) = open_summary {
            self.open_summary(table_id, field);
        }
    }

    fn relationships(
        &mut self,
        ui: &mut egui::Ui,
        height: f32,
        action: &mut Option<DatabaseAction>,
    ) {
        use crate::database_graph::{GraphEvent, Selection};
        use crate::database_relationship::{EditRelationship, SpecifyTable};
        let schema = self.schema.as_ref().expect("schema loaded");
        let editable = schema.capabilities.occurrences;
        let selection = Selection {
            occurrence: self.occurrence,
            relationship: self.relationship,
            also: self.also.clone(),
        };
        let events = database_graph::show(ui, schema, &selection, editable, height);
        let occurrence =
            |id: Option<u64>| id.and_then(|id| schema.occurrences.iter().find(|o| o.id == id));
        let mut edit_relationship = None;
        let mut specify_table = None;
        let mut confirm = None;
        let mut selected = (self.occurrence, self.relationship);
        let mut also = self.also.clone();
        for event in events {
            match event {
                GraphEvent::SelectOccurrence(id) => {
                    selected = (Some(id), None);
                    also.clear();
                }
                GraphEvent::ToggleOccurrence(id) => {
                    if selected.0 == Some(id) {
                        selected.0 = also.pop_first();
                    } else if !also.remove(&id) {
                        also.extend(selected.0);
                        selected.0 = Some(id);
                    }
                    selected.1 = None;
                }
                GraphEvent::ArrangeAll(Ok(change)) => {
                    *action = Some(DatabaseAction::Apply(change));
                }
                GraphEvent::ArrangeAll(Err(error)) => self.error = Some(error),
                // Planned by the graph before it returns its events.
                GraphEvent::ArrangeSelected(_) => {}
                GraphEvent::Duplicate => {
                    let occurrence_ids: Vec<u64> =
                        selected.0.into_iter().chain(also.iter().copied()).collect();
                    if !occurrence_ids.is_empty() {
                        *action = Some(DatabaseAction::Apply(SchemaChange::DuplicateOccurrences {
                            occurrence_ids,
                        }));
                    }
                }
                GraphEvent::SelectRelationship(id) => {
                    selected = (None, Some(id));
                    also.clear();
                }
                GraphEvent::EditOccurrence(id) => {
                    specify_table =
                        occurrence(Some(id)).map(|o| SpecifyTable::edit(self.file.clone(), o));
                }
                GraphEvent::EditRelationship(id) => {
                    edit_relationship = EditRelationship::edit(schema, id);
                }
                GraphEvent::Arrange {
                    occurrence,
                    left,
                    top,
                    width,
                    height,
                } => {
                    *action = Some(DatabaseAction::Apply(SchemaChange::ArrangeOccurrence {
                        occurrence_id: occurrence,
                        left,
                        top,
                        width,
                        height,
                    }));
                }
                GraphEvent::StaleFieldLink => {
                    self.error = Some(
                        "The schema changed during the field drag. Start the drag again.".into(),
                    );
                }
                GraphEvent::LinkFields { from, to } => match schema.link_fields(from, to) {
                    Ok(Some(change)) => *action = Some(DatabaseAction::Apply(change)),
                    Ok(None) => {}
                    Err(error) => self.error = Some(error.message),
                },
                GraphEvent::AddOccurrence => {
                    specify_table = Some(SpecifyTable::new(self.file.clone()));
                }
                GraphEvent::AddRelationship => {
                    edit_relationship = Some(EditRelationship::new(selected.0, None));
                }
                GraphEvent::EditSelected => match selected {
                    (Some(id), _) => {
                        specify_table =
                            occurrence(Some(id)).map(|o| SpecifyTable::edit(self.file.clone(), o));
                    }
                    (None, Some(id)) => edit_relationship = EditRelationship::edit(schema, id),
                    _ => {}
                },
                GraphEvent::DeleteSelected => {
                    confirm = match selected {
                        (Some(occurrence_id), _) => {
                            Some(SchemaChange::DeleteOccurrence { occurrence_id })
                        }
                        (None, Some(relationship_id)) => {
                            Some(SchemaChange::DeleteRelationship { relationship_id })
                        }
                        _ => None,
                    };
                }
                GraphEvent::Color(color) => {
                    if let Some(occurrence_id) = selected.0 {
                        *action = Some(DatabaseAction::Apply(SchemaChange::ColorOccurrence {
                            occurrence_id,
                            color,
                        }));
                    }
                }
            }
        }
        (self.occurrence, self.relationship) = selected;
        self.also = also;
        if edit_relationship.is_some() {
            self.edit_relationship = edit_relationship;
        }
        if specify_table.is_some() {
            self.specify_table = specify_table;
        }
        if confirm.is_some() {
            self.confirm_delete = confirm;
        }
    }
}

/// What a [`column_list`] was asked this pass.
struct ListInput {
    /// The row clicked.
    clicked: Option<u64>,
    /// The row double-clicked (its first click is `clicked` too).
    opened: Option<u64>,
    /// Whether the heading of the toggled column was clicked (FileMaker's
    /// "Click to toggle").
    toggled: bool,
}

/// A FileMaker-style list with column headings: white rows, the selected
/// one shaded.
#[allow(clippy::too_many_arguments)]
fn column_list(
    ui: &mut egui::Ui,
    id: &str,
    width: f32,
    height: f32,
    columns: &[(&str, f32)],
    toggle: Option<usize>,
    rows: &[(u64, Vec<Cell>)],
    selected: Option<u64>,
) -> ListInput {
    let mut clicked = None;
    let mut opened = None;
    let mut toggled = false;
    let edges: Vec<f32> = std::iter::once(0.0)
        .chain(columns.iter().scan(0.0, |sum, (_, w)| {
            *sum += w;
            Some(*sum)
        }))
        .collect();
    let cell = |rect: egui::Rect, column: usize| {
        egui::Rect::from_min_max(
            pos2(
                rect.left() + rect.width() * edges[column] + 10.0,
                rect.top(),
            ),
            pos2(
                rect.left() + rect.width() * edges[column + 1] - 4.0,
                rect.bottom(),
            ),
        )
    };
    egui::Frame::new()
        .fill(theme::CONTROL_BG)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let (header, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
            for (column, (title, _)) in columns.iter().enumerate() {
                let rect = cell(header, column);
                if Some(column) == toggle {
                    let response =
                        ui.interact(rect, ui.scope_id().with((id, "toggle")), Sense::click());
                    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, *title));
                    toggled |= response.clicked();
                }
                widgets::put_label(ui, rect, title, theme::body(), theme::TEXT, Align::Min);
                if column > 0 {
                    let x = rect.left() - 6.0;
                    ui.painter().vline(
                        x,
                        header.y_range().shrink(4.0),
                        Stroke::new(1.0, theme::HAIRLINE),
                    );
                }
            }
            ui.painter().hline(
                header.x_range(),
                header.bottom(),
                Stroke::new(1.0, theme::HAIRLINE),
            );
            egui::ScrollArea::vertical()
                .id_salt(id)
                .min_scrolled_height(height)
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(width);
                    for (row_id, cells) in rows {
                        let (rect, response) =
                            ui.allocate_exact_size(vec2(width, 20.0), Sense::click());
                        let on = selected == Some(*row_id);
                        if on {
                            ui.painter()
                                .rect_filled(rect, CornerRadius::ZERO, theme::HOVER_FILL);
                        }
                        for (column, value) in cells.iter().enumerate() {
                            let color = if value.weak {
                                theme::WEAK_TEXT
                            } else {
                                theme::TEXT
                            };
                            let area = cell(rect, column);
                            ui.painter().with_clip_rect(area).text(
                                area.left_center(),
                                Align2::LEFT_CENTER,
                                value.text.replace(['\r', '\n'], " "),
                                theme::body(),
                                color,
                            );
                        }
                        let name = cells.first().map_or("", |c| c.text.as_str());
                        response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, name));
                        if response.clicked() {
                            clicked = Some(*row_id);
                        }
                        if response.double_clicked() {
                            opened = Some(*row_id);
                        }
                    }
                });
        });
    ListInput {
        clicked,
        opened,
        toggled,
    }
}

/// The macOS segmented control at the top of Manage Database: a light
/// track with the selected tab raised in white.
pub(crate) fn tab_bar(ui: &mut egui::Ui, tabs: &[&str], selected: usize) -> Option<usize> {
    let widths: Vec<f32> = tabs
        .iter()
        .map(|tab| widgets::text_width(ui, tab, theme::body()) + 30.0)
        .collect();
    let (track, _) = ui.allocate_exact_size(vec2(widths.iter().sum(), 22.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(track, CornerRadius::same(6), theme::SIDEBAR_BG);
    let mut left = track.left();
    let mut clicked = None;
    for (index, (tab, width)) in tabs.iter().zip(&widths).enumerate() {
        let rect = egui::Rect::from_min_size(pos2(left, track.top()), vec2(*width, track.height()));
        left += width;
        let on = index == selected;
        if on {
            painter.rect(
                rect.shrink(2.0),
                CornerRadius::same(5),
                theme::CONTROL_BG,
                Stroke::new(0.5, theme::BUTTON_BORDER),
                StrokeKind::Inside,
            );
        } else if index > 0 && index != selected + 1 {
            painter.vline(
                rect.left(),
                rect.y_range().shrink(5.0),
                Stroke::new(1.0, theme::DIVIDER),
            );
        }
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            *tab,
            theme::body(),
            theme::TEXT,
        );
        let response = ui.interact(
            rect,
            ui.scope_id().with(("database_tab", index)),
            Sense::click(),
        );
        response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, *tab));
        if response.clicked() && !on {
            clicked = Some(index);
        }
    }
    clicked
}

/// The "View by" menu.
fn order_menu(ui: &mut egui::Ui, label: &str, order: &mut Order, choices: &[Order]) {
    egui::ComboBox::from_id_salt(label)
        .width(140.0)
        .selected_text(order.label())
        .show_ui(ui, |ui| {
            for choice in choices {
                ui.selectable_value(order, *choice, choice.label());
            }
            ui.add_enabled(false, egui::Button::selectable(false, "custom order"))
                .on_disabled_hover_text("OpenMaker cannot reorder the schema yet.");
        })
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, label));
}

/// A text box with its caption to the left, as Manage Database's editors.
fn labeled_input(ui: &mut egui::Ui, label: &str, name: &mut String, width: f32) {
    ui.horizontal(|ui| {
        ui.add_sized(vec2(90.0, 20.0), egui::Label::new(format!("{label}:")));
        let response = ui.add(egui::TextEdit::singleline(name).desired_width(width.max(160.0)));
        widgets::accessible_label(ui, response.id, label);
    });
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
}

fn field_types() -> [FieldKind; 6] {
    [
        FieldKind::Text,
        FieldKind::Number,
        FieldKind::Date,
        FieldKind::Time,
        FieldKind::Timestamp,
        FieldKind::Container,
    ]
}

fn type_name(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::Text => "Text",
        FieldKind::Number => "Number",
        FieldKind::Date => "Date",
        FieldKind::Time => "Time",
        FieldKind::Timestamp => "Timestamp",
        FieldKind::Container => "Container",
    }
}

/// The order of "View by field type": FileMaker's Type menu order.
/// A calculation's result type in Specify Calculation's terms.
pub(crate) fn value_type(kind: FieldKind) -> ValueType {
    match kind {
        FieldKind::Text => ValueType::Text,
        FieldKind::Number => ValueType::Number,
        FieldKind::Date => ValueType::Date,
        FieldKind::Time => ValueType::Time,
        FieldKind::Timestamp => ValueType::Timestamp,
        FieldKind::Container => ValueType::Container,
    }
}

/// Specify Calculation's result type as a field's.
fn field_kind(value_type: ValueType) -> FieldKind {
    match value_type {
        ValueType::Text => FieldKind::Text,
        ValueType::Number => FieldKind::Number,
        ValueType::Date => FieldKind::Date,
        ValueType::Time => FieldKind::Time,
        ValueType::Timestamp => FieldKind::Timestamp,
        ValueType::Container => FieldKind::Container,
    }
}

fn type_rank(kind: SchemaFieldKind, data_type: FieldKind) -> usize {
    let base = field_types()
        .iter()
        .position(|k| *k == data_type)
        .unwrap_or(0);
    match kind {
        SchemaFieldKind::Normal => base,
        SchemaFieldKind::Calculation => 10 + base,
        SchemaFieldKind::Summary => 20 + base,
        SchemaFieldKind::Unknown(_) => 30 + base,
    }
}

fn field_type(kind: SchemaFieldKind, data_type: FieldKind) -> String {
    match kind {
        SchemaFieldKind::Normal => type_name(data_type).into(),
        SchemaFieldKind::Calculation => format!("Calculation - {}", type_name(data_type)),
        SchemaFieldKind::Summary => format!("Summary - {}", type_name(data_type)),
        SchemaFieldKind::Unknown(value) => format!("Unknown ({value}) - {}", type_name(data_type)),
    }
}

fn count_label(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}
