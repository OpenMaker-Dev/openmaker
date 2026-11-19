//! Manage Database's "Options for Field" dialog, as FileMaker Pro 26 draws
//! it (reference screenshots of the Contacts starter solution, 2026-11-11):
//! the Auto-Enter, Validation, Storage and Furigana tabs over a panel,
//! Cancel and OK. OK stages `SchemaChange::SetFieldOptions`; the provider's
//! draft decides what may be saved (`om_data::schema_draft`). A calculated
//! value and a validation calculation are written in Specify Calculation,
//! opened from the dialog as FileMaker Pro opens it, and a looked-up value
//! in Lookup for Field (`database_lookup`). The index language offers the
//! languages whose keys OpenMaker builds; the others are listed,
//! unavailable. Furigana takes a text field of the table and the
//! translations FileMaker Pro was seen to write (#1301); the others are
//! listed, unavailable. What OpenMaker does not edit yet is shown as the
//! field has it, unavailable: a lookup whose definition does not read.

use std::sync::Arc;

use egui::{Align, Color32, CornerRadius, Id, Layout, RichText, Stroke};
use om_data::field_options::{
    AutoCalculation, AutoEnterValue, EntryValue, FieldOptions, Indexing, Lookup, Message,
    OptionFormula, Serial, StrictType, ValidationCalculation,
};
use om_data::host::CalcSchema;
use om_data::{DatabaseSchema, FieldKind, SchemaChange};

use crate::database_lookup::{self, LookupDialog};

use crate::script_workspace::calculation::{
    CalcOutcome, FieldOptions as CalcOptions, OptionKind, SpecifyCalculation,
};
use crate::{database, theme, widgets};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    AutoEnter,
    Validation,
    Storage,
    Furigana,
}

/// The dialog's answer once closed.
pub(crate) enum Answer {
    Cancel,
    Ok(SchemaChange),
}

/// Which auto-enter value is checked; the dialog keeps each value's
/// settings while another is chosen, as FileMaker Pro's does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    None,
    Creation,
    Modification,
    Serial,
    LastVisited,
    Data,
    Calculated,
    LookedUp,
    Kept,
}

/// What the dialog's formulas compile against: the file's names with the
/// schema as Manage Database shows it, the occurrence a new formula is
/// evaluated from (the table's first), and the file's value lists, each
/// with why OpenMaker's record rules cannot check values against it.
pub(crate) struct OptionNames {
    pub names: Option<Arc<CalcSchema>>,
    pub context: u64,
    pub value_lists: Vec<(u64, String, Option<String>)>,
    /// The schema as Manage Database shows it, for Lookup for Field.
    pub schema: Option<DatabaseSchema>,
}

pub(crate) struct OptionsDialog {
    table_id: u64,
    field_id: u64,
    field: String,
    data_type: FieldKind,
    tab: Tab,
    original: FieldOptions,
    source: Source,
    creation: EntryValue,
    modification: EntryValue,
    serial: Serial,
    next: String,
    increment: String,
    data: String,
    /// The calculated value's formula and options, kept while another
    /// value is chosen.
    calculated: AutoCalculation,
    names: OptionNames,
    /// Specify Calculation, while open, and the option it is for.
    specify: Option<(SpecifyCalculation, OptionKind)>,
    /// The looked-up value, kept while another value is chosen, and
    /// Lookup for Field while open.
    looked_up: Option<Lookup>,
    lookup: Option<LookupDialog>,
    prohibit: bool,
    always: bool,
    allow_override: bool,
    strict: Option<StrictType>,
    strict_choice: StrictType,
    not_empty: bool,
    unique: bool,
    existing: bool,
    maximum: bool,
    maximum_text: String,
    value_list: bool,
    value_list_choice: u64,
    range: bool,
    range_from: String,
    range_to: String,
    /// The validation calculation, kept while unchecked.
    validated: bool,
    validation: ValidationCalculation,
    message: bool,
    /// A message typed as text; empty when the message is a calculation.
    message_text: String,
    /// The message calculation, written in Specify Calculation: the
    /// message when no text is typed.
    message_formula: OptionFormula,
    global: bool,
    repetitions: String,
    indexing: Indexing,
    automatic_indexes: bool,
    /// The index language, `Locale::Type` (definition bytes 4-7).
    language: u32,
    /// "Use Furigana Field", its field and its translation.
    furigana: om_data::field_options::Furigana,
    error: Option<String>,
}

impl OptionsDialog {
    pub(crate) fn new(
        table_id: u64,
        field_id: u64,
        field: &str,
        data_type: FieldKind,
        options: FieldOptions,
        names: OptionNames,
    ) -> Self {
        let value = &options.auto_enter.value;
        let source = match value {
            AutoEnterValue::None => Source::None,
            AutoEnterValue::Creation(_) => Source::Creation,
            AutoEnterValue::Modification(_) => Source::Modification,
            AutoEnterValue::Serial(_) => Source::Serial,
            AutoEnterValue::LastVisited => Source::LastVisited,
            AutoEnterValue::Data(_) => Source::Data,
            AutoEnterValue::Calculated(_) => Source::Calculated,
            AutoEnterValue::LookedUp(Some(_)) => Source::LookedUp,
            AutoEnterValue::LookedUp(None) => Source::Kept,
        };
        let looked_up = match value {
            AutoEnterValue::LookedUp(lookup) => lookup.clone(),
            _ => None,
        };
        let calculated = match value {
            AutoEnterValue::Calculated(calculation) => calculation.clone(),
            _ => AutoCalculation::new(OptionFormula::new("", names.context)),
        };
        let serial = match value {
            AutoEnterValue::Serial(serial) => serial.clone(),
            _ => Serial::default(),
        };
        let rules = &options.validation;
        let storage = &options.storage;
        Self {
            table_id,
            field_id,
            field: field.to_owned(),
            data_type,
            tab: Tab::AutoEnter,
            source,
            creation: match value {
                AutoEnterValue::Creation(entry) => *entry,
                _ => EntryValue::Date,
            },
            modification: match value {
                AutoEnterValue::Modification(entry) => *entry,
                _ => EntryValue::Date,
            },
            next: serial.next.clone(),
            increment: serial.increment.to_string(),
            serial,
            data: match value {
                AutoEnterValue::Data(data) => data.clone(),
                _ => String::new(),
            },
            calculated,
            specify: None,
            looked_up,
            lookup: None,
            prohibit: options.auto_enter.prohibit_modification,
            always: rules.always,
            allow_override: rules.allow_override,
            strict: rules.strict_type,
            strict_choice: rules.strict_type.unwrap_or(StrictType::NumericOnly),
            not_empty: rules.not_empty,
            unique: rules.unique,
            existing: rules.existing,
            maximum: rules.maximum_characters.is_some(),
            maximum_text: rules.maximum_characters.unwrap_or(1).to_string(),
            value_list: rules.value_list.is_some(),
            value_list_choice: rules
                .value_list
                .filter(|id| *id != 0)
                .or_else(|| {
                    names
                        .value_lists
                        .iter()
                        .find(|(_, _, problem)| problem.is_none())
                        .map(|(id, ..)| *id)
                })
                .unwrap_or(0),
            range: rules.range.is_some(),
            range_from: rules.range.clone().unwrap_or_default().0,
            range_to: rules.range.clone().unwrap_or_default().1,
            validated: rules.calculation.is_some(),
            validation: rules.calculation.clone().unwrap_or(ValidationCalculation {
                formula: OptionFormula::new("", names.context),
                only_if_modified: true,
            }),
            message: rules.message.is_some(),
            message_text: match &rules.message {
                Some(Message::Text(text)) => text.clone(),
                _ => String::new(),
            },
            message_formula: match &rules.message {
                Some(Message::Calculation(formula)) => formula.clone(),
                _ => OptionFormula::new("", names.context),
            },
            names,
            global: storage.global,
            repetitions: storage.repetitions.to_string(),
            indexing: storage.indexing,
            automatic_indexes: storage.automatic_indexes,
            language: storage.language,
            furigana: options.furigana,
            original: options,
            error: None,
        }
    }

    /// The options as the dialog shows them, or why they cannot be read.
    fn options(&self) -> Result<FieldOptions, String> {
        let mut options = self.original.clone();
        let number = |text: &str, what: &str| {
            text.trim()
                .parse::<u32>()
                .map_err(|_| format!("Enter a whole number for {what}."))
        };
        options.auto_enter.value = match self.source {
            Source::None => AutoEnterValue::None,
            Source::Creation => AutoEnterValue::Creation(self.creation),
            Source::Modification => AutoEnterValue::Modification(self.modification),
            Source::Serial => AutoEnterValue::Serial(Serial {
                on_commit: self.serial.on_commit,
                next: self.next.trim().to_owned(),
                increment: number(&self.increment, "the increment")?,
            }),
            Source::LastVisited => AutoEnterValue::LastVisited,
            Source::Data => AutoEnterValue::Data(self.data.clone()),
            Source::Calculated => AutoEnterValue::Calculated(self.calculated.clone()),
            Source::LookedUp => AutoEnterValue::LookedUp(self.looked_up.clone()),
            Source::Kept => self.original.auto_enter.value.clone(),
        };
        options.auto_enter.prohibit_modification = self.prohibit;
        let rules = &mut options.validation;
        rules.always = self.always;
        rules.allow_override = self.allow_override;
        rules.strict_type = self.strict;
        rules.not_empty = self.not_empty;
        rules.unique = self.unique;
        rules.existing = self.existing;
        rules.maximum_characters = if self.maximum {
            Some(number(
                &self.maximum_text,
                "the maximum number of characters",
            )?)
        } else {
            None
        };
        rules.value_list = self.value_list.then_some(self.value_list_choice);
        rules.range = self
            .range
            .then(|| (self.range_from.clone(), self.range_to.clone()));
        rules.calculation = self.validated.then(|| self.validation.clone());
        rules.message = match self.message {
            false => None,
            // Text typed replaces a calculation; without text the message
            // is the calculation, as FileMaker Pro 26 writes every one.
            true if !self.message_text.is_empty() => Some(Message::Text(self.message_text.clone())),
            true if self.message_formula.text.as_deref() != Some("")
                || matches!(
                    self.original.validation.message,
                    Some(Message::Calculation(_))
                ) =>
            {
                Some(Message::Calculation(self.message_formula.clone()))
            }
            true => Some(Message::Text(String::new())),
        };
        let storage = &mut options.storage;
        storage.global = self.global;
        storage.repetitions = self
            .repetitions
            .trim()
            .parse()
            .map_err(|_| "Enter a number of repetitions from 1 to 32000.".to_owned())?;
        storage.indexing = self.indexing;
        storage.automatic_indexes = self.automatic_indexes;
        storage.language = self.language;
        options.furigana = self.furigana;
        Ok(options)
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<Answer> {
        let mut answer = None;
        egui::Modal::new(Id::unique("database_field_options"))
            .frame(
                egui::Frame::new()
                    .fill(theme::DIALOG_BG)
                    .stroke(Stroke::new(1.0, theme::DIVIDER))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(18)
                    .shadow(ctx.global_style().visuals.window_shadow),
            )
            .show(ctx, |ui| {
                let width = 480.0;
                ui.set_width(width);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(format!("Options for Field “{}”", self.field))
                            .font(egui::FontId::new(13.0, theme::semibold())),
                    );
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space((width - 300.0).max(0.0) / 2.0);
                    let tabs = ["Auto-Enter", "Validation", "Storage", "Furigana"];
                    let all = [Tab::AutoEnter, Tab::Validation, Tab::Storage, Tab::Furigana];
                    let selected = all.iter().position(|t| *t == self.tab).unwrap_or(0);
                    if let Some(index) = database::tab_bar(ui, &tabs, selected) {
                        self.tab = all[index];
                    }
                });
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(Color32::from_gray(0xEC))
                    .stroke(Stroke::new(1.0, theme::DIVIDER))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(14)
                    .show(ui, |ui| {
                        ui.set_width(width - 28.0);
                        ui.set_min_height(380.0);
                        match self.tab {
                            Tab::AutoEnter => self.auto_enter(ui),
                            Tab::Validation => self.validation(ui),
                            Tab::Storage => self.storage(ui),
                            Tab::Furigana => self.furigana(ui),
                        }
                    });
                if let Some(error) = &self.error {
                    ui.add_space(6.0);
                    ui.colored_label(Color32::from_rgb(175, 35, 35), error);
                }
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::dialog_button(ui, "OK", true, true).clicked() {
                        match self.options() {
                            Ok(options) => {
                                answer = Some(Answer::Ok(SchemaChange::SetFieldOptions {
                                    table_id: self.table_id,
                                    field_id: self.field_id,
                                    options: Box::new(options),
                                }));
                            }
                            Err(error) => self.error = Some(error),
                        }
                    }
                    if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
            });
        self.show_specify(ctx);
        if let Some(dialog) = self.lookup.as_mut() {
            match dialog.show(ctx) {
                Some(database_lookup::Answer::Ok(lookup)) => {
                    self.looked_up = Some(lookup);
                    self.source = Source::LookedUp;
                    self.lookup = None;
                }
                Some(database_lookup::Answer::Cancel) => self.lookup = None,
                None => {}
            }
        }
        answer
    }

    /// Open Lookup for Field on the field's lookup, or a new one starting
    /// with the table's first occurrence.
    fn open_lookup(&mut self) {
        let Some(schema) = self.names.schema.clone() else {
            return;
        };
        self.lookup = Some(LookupDialog::new(
            &self.field,
            schema,
            self.table_id,
            self.names.context,
            self.looked_up.as_ref(),
        ));
    }

    /// Open Specify Calculation on the calculated value's or the
    /// validation calculation's formula.
    fn open_specify(&mut self, kind: OptionKind) {
        let (formula, check) = match kind {
            OptionKind::AutoEnter => (&self.calculated.formula, self.calculated.skip_when_empty),
            OptionKind::Validation => (&self.validation.formula, self.validation.only_if_modified),
            OptionKind::Message => (&self.message_formula, false),
        };
        let dialog = SpecifyCalculation::with_text(
            format!("Specify Calculation “{}”", self.field),
            formula.text.as_deref().unwrap_or_default(),
            self.names.names.clone(),
        )
        .for_field(
            formula.context,
            CalcOptions {
                // A message is text, whatever the field's type.
                result: if kind == OptionKind::Message {
                    om_data::calc::ValueType::Text
                } else {
                    database::value_type(self.data_type)
                },
                repetitions: 1,
                skip_when_empty: check,
                storage: None,
                option: Some(kind),
            },
        );
        self.specify = Some((dialog, kind));
    }

    /// Specify Calculation over the dialog: OK takes its formula (and
    /// checks the option), Cancel leaves the option as it was.
    fn show_specify(&mut self, ctx: &egui::Context) {
        let Some((dialog, kind)) = self.specify.as_mut() else {
            return;
        };
        let accepted = match dialog.show(ctx) {
            Some(CalcOutcome::Ok(_)) => dialog
                .field_formula()
                .map(|f| (f, dialog.field_options().map(|o| o.skip_when_empty))),
            Some(CalcOutcome::Cancel) => None,
            None => return,
        };
        let kind = *kind;
        self.specify = None;
        let Some(((text, context), check)) = accepted else {
            return;
        };
        let formula = OptionFormula::new(text, context);
        match kind {
            OptionKind::AutoEnter => {
                self.calculated.formula = formula;
                if let Some(check) = check {
                    self.calculated.skip_when_empty = check;
                }
                self.source = Source::Calculated;
            }
            OptionKind::Validation => {
                self.validation.formula = formula;
                if let Some(check) = check {
                    self.validation.only_if_modified = check;
                }
                self.validated = true;
            }
            OptionKind::Message => {
                self.message_formula = formula;
                self.message_text.clear();
                self.message = true;
            }
        }
    }

    /// A checkbox for one auto-enter value: checking it clears the others
    /// (E40); a kept calculated or looked-up value cannot be cleared.
    fn source_box(&mut self, ui: &mut egui::Ui, source: Source, label: &str) -> egui::Response {
        let mut on = self.source == source;
        let enabled = self.source != Source::Kept;
        let response = ui
            .add_enabled(enabled, egui::Checkbox::new(&mut on, label))
            .on_disabled_hover_text(
                "The field's lookup cannot be read, so OpenMaker does not change it.",
            );
        if response.changed() {
            self.source = if on { source } else { Source::None };
        }
        response
    }

    fn entry_menu(ui: &mut egui::Ui, id: &str, value: &mut EntryValue, enabled: bool) {
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt(id)
                .width(220.0)
                .selected_text(value.label())
                .show_ui(ui, |ui| {
                    for entry in EntryValue::ALL {
                        ui.selectable_value(value, entry, entry.label());
                    }
                })
                .response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, enabled, id));
        });
    }

    fn auto_enter(&mut self, ui: &mut egui::Ui) {
        ui.label("Automatically enter the following data into this field:");
        ui.add_space(4.0);
        let column = 120.0;
        for (source, label, id) in [
            (Source::Creation, "Creation", "Creation Value"),
            (Source::Modification, "Modification", "Modification Value"),
        ] {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let checkbox = self.source_box(ui, source, label);
                ui.add_space((column - checkbox.rect.width()).max(4.0));
                let mut value = if source == Source::Creation {
                    self.creation
                } else {
                    self.modification
                };
                Self::entry_menu(ui, id, &mut value, self.source != Source::Kept);
                if source == Source::Creation {
                    self.creation = value;
                } else {
                    self.modification = value;
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            self.source_box(ui, Source::Serial, "Serial number");
        });
        let serial = self.source == Source::Serial;
        ui.add_enabled_ui(serial, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(36.0);
                ui.label("Generate:");
                ui.radio_value(&mut self.serial.on_commit, false, "On creation");
                ui.radio_value(&mut self.serial.on_commit, true, "On commit");
            });
            ui.horizontal(|ui| {
                ui.add_space(36.0);
                ui.label("next value");
                let next = ui.add(egui::TextEdit::singleline(&mut self.next).desired_width(110.0));
                widgets::accessible_label(ui, next.id, "Next Serial Value");
                ui.add_space(12.0);
                ui.label("increment by");
                let by =
                    ui.add(egui::TextEdit::singleline(&mut self.increment).desired_width(70.0));
                widgets::accessible_label(ui, by.id, "Serial Increment");
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            self.source_box(ui, Source::LastVisited, "Value from last visited record");
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let checkbox = self.source_box(ui, Source::Data, "Data:");
            ui.add_space((column - 50.0 - checkbox.rect.width()).max(4.0));
            let data = ui.add_enabled(
                self.source != Source::Kept,
                egui::TextEdit::multiline(&mut self.data)
                    .desired_width(300.0)
                    .desired_rows(3),
            );
            widgets::accessible_label(ui, data.id, "Auto-Enter Data");
        });
        let open = self.source != Source::Kept;
        let readable = self.calculated.formula.text.is_some();
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            // Checking the box opens Specify Calculation; its OK checks it.
            let mut on = self.source == Source::Calculated;
            let response = ui.add_enabled(
                open && readable,
                egui::Checkbox::new(&mut on, "Calculated value"),
            );
            if response.changed() {
                if on {
                    self.open_specify(OptionKind::AutoEnter);
                } else {
                    self.source = Source::None;
                }
            }
            let specify = widgets::dialog_button(
                ui,
                "Specify…",
                false,
                self.source == Source::Calculated && readable,
            );
            widgets::accessible_label(ui, specify.id, "Specify Calculated Value");
            if specify.clicked() {
                self.open_specify(OptionKind::AutoEnter);
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(36.0);
            let mut keep = !self.calculated.replace_existing;
            if ui
                .add_enabled(
                    self.source == Source::Calculated,
                    egui::Checkbox::new(
                        &mut keep,
                        "Do not replace existing value of field (if any)",
                    ),
                )
                .changed()
            {
                self.calculated.replace_existing = !keep;
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            // Checking the box opens Lookup for Field; its OK checks it.
            // Kept is a lookup whose definition does not read.
            let mut on = matches!(self.source, Source::LookedUp | Source::Kept);
            let available = open && self.names.schema.is_some();
            let response = ui
                .add_enabled(available, egui::Checkbox::new(&mut on, "Looked-up value"))
                .on_disabled_hover_text(
                    "The field's lookup cannot be read, so OpenMaker does not change it.",
                );
            if response.changed() {
                if on {
                    self.open_lookup();
                } else {
                    self.source = Source::None;
                }
            }
            let specify = widgets::dialog_button(
                ui,
                "Specify…",
                false,
                self.source == Source::LookedUp && available,
            );
            widgets::accessible_label(ui, specify.id, "Specify Looked-up Value");
            if specify.clicked() {
                self.open_lookup();
            }
        });
        ui.separator();
        // FileMaker Pro offers this with a value the field enters (E16).
        let can_prohibit = self.source != Source::None || self.prohibit;
        ui.add_enabled(
            can_prohibit,
            egui::Checkbox::new(
                &mut self.prohibit,
                "Prohibit modification of value during data entry",
            ),
        );
    }

    fn validation(&mut self, ui: &mut egui::Ui) {
        ui.label("Validate data in this field:");
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.radio_value(&mut self.always, true, "Always");
            ui.add_space(40.0);
            ui.radio_value(&mut self.always, false, "Only during data entry");
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.checkbox(
                &mut self.allow_override,
                "Allow user to override during data entry",
            );
        });
        ui.separator();
        ui.label("Require:");
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let mut strict = self.strict.is_some();
            if ui.checkbox(&mut strict, "Strict data type:").changed() {
                self.strict = strict.then_some(self.strict_choice);
            }
            egui::ComboBox::from_id_salt("Strict Data Type")
                .width(160.0)
                .selected_text(self.strict_choice.label())
                .show_ui(ui, |ui| {
                    for choice in StrictType::ALL {
                        ui.selectable_value(&mut self.strict_choice, choice, choice.label());
                    }
                })
                .response
                .widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Strict Data Type")
                });
            if self.strict.is_some() {
                self.strict = Some(self.strict_choice);
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.checkbox(&mut self.not_empty, "Not empty");
            ui.add_space(24.0);
            ui.checkbox(&mut self.unique, "Unique value");
            ui.add_space(24.0);
            ui.checkbox(&mut self.existing, "Existing value");
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let lists = !self.names.value_lists.is_empty();
            ui.add_enabled(
                lists || self.value_list,
                egui::Checkbox::new(&mut self.value_list, "Member of value list:"),
            )
            .on_disabled_hover_text("The file has no value lists.");
            let chosen = self
                .names
                .value_lists
                .iter()
                .find(|(id, ..)| *id == self.value_list_choice)
                .map_or("<unknown>", |(_, name, _)| name.as_str())
                .to_owned();
            let enabled = self.value_list && lists;
            ui.add_enabled_ui(enabled, |ui| {
                egui::ComboBox::from_id_salt("Validation Value List")
                    .width(200.0)
                    .selected_text(chosen)
                    .show_ui(ui, |ui| {
                        for (id, name, problem) in &self.names.value_lists {
                            let on = self.value_list_choice == *id;
                            let item = ui.add_enabled(
                                problem.is_none(),
                                egui::Button::selectable(on, name.as_str()),
                            );
                            if item.clicked() {
                                self.value_list_choice = *id;
                            }
                            if let Some(problem) = problem {
                                item.on_disabled_hover_text(format!(
                                    "OpenMaker cannot yet validate by this value list: {problem}"
                                ));
                            }
                        }
                    })
                    .response
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::ComboBox,
                            enabled,
                            "Validation Value List",
                        )
                    });
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.checkbox(&mut self.range, "In range:");
            let from = ui.add_enabled(
                self.range,
                egui::TextEdit::singleline(&mut self.range_from).desired_width(110.0),
            );
            widgets::accessible_label(ui, from.id, "Range From");
            ui.label("to");
            let to = ui.add_enabled(
                self.range,
                egui::TextEdit::singleline(&mut self.range_to).desired_width(110.0),
            );
            widgets::accessible_label(ui, to.id, "Range To");
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let readable = self.validation.formula.text.is_some();
            // Checking the box opens Specify Calculation; its OK checks it.
            let mut on = self.validated;
            let response = ui.add_enabled(
                readable,
                egui::Checkbox::new(&mut on, "Validated by calculation"),
            );
            if response.changed() {
                if on {
                    self.open_specify(OptionKind::Validation);
                } else {
                    self.validated = false;
                }
            }
            let specify = widgets::dialog_button(ui, "Specify…", false, self.validated && readable);
            widgets::accessible_label(ui, specify.id, "Specify Validation Calculation");
            if specify.clicked() {
                self.open_specify(OptionKind::Validation);
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.checkbox(&mut self.maximum, "Maximum number of characters:");
            let maximum = ui.add_enabled(
                self.maximum,
                egui::TextEdit::singleline(&mut self.maximum_text).desired_width(70.0),
            );
            widgets::accessible_label(ui, maximum.id, "Maximum Number of Characters");
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut self.message,
                "Display custom message if validation fails",
            );
            let specify = widgets::dialog_button(ui, "Specify…", false, true);
            widgets::accessible_label(ui, specify.id, "Specify Message Calculation");
            if specify.clicked() {
                self.open_specify(OptionKind::Message);
            }
        });
        // The message calculation is shown as its formula until text
        // replaces it.
        let hint = self.message_formula.text.clone().unwrap_or_default();
        let message = ui.add_enabled(
            self.message,
            egui::TextEdit::multiline(&mut self.message_text)
                .hint_text(hint)
                .desired_width(ui.available_width())
                .desired_rows(4),
        );
        widgets::accessible_label(ui, message.id, "Custom Message");
    }

    fn storage(&mut self, ui: &mut egui::Ui) {
        // A section title followed by a rule to the panel's edge.
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).size(13.5));
                let rest = ui.available_rect_before_wrap();
                ui.painter().hline(
                    (rest.left() + 8.0)..=rest.right(),
                    rest.center().y,
                    Stroke::new(1.0, theme::DIVIDER),
                );
            });
        };
        let note = |ui: &mut egui::Ui, text: &str| {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(text).size(11.5)).wrap());
                });
            });
        };
        heading(ui, "Global Storage");
        note(
            ui,
            "A field that uses global storage contains only one value that is shared across \
             all records.  A global field can also be used as a temporary storage location \
             (for example, in scripts).",
        );
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            // A global field has no index (FileMaker Pro greys indexing out).
            if ui
                .checkbox(
                    &mut self.global,
                    "Use global storage (one value for all records)",
                )
                .changed()
                && self.global
            {
                self.indexing = Indexing::None;
            }
        });
        heading(ui, "Repeating");
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label("Maximum number of repetitions:");
            let repetitions =
                ui.add(egui::TextEdit::singleline(&mut self.repetitions).desired_width(70.0));
            widgets::accessible_label(ui, repetitions.id, "Maximum Number of Repetitions");
        });
        heading(ui, "Indexing");
        note(
            ui,
            "Indexing improves performance for some operations like finds and supports \
             functionality like relational joins and field value uniqueness at the cost of \
             increased file size and time spent indexing.",
        );
        let text = self.data_type == FieldKind::Text;
        let container = self.data_type == FieldKind::Container;
        ui.add_enabled_ui(!container && !self.global, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label("Indexing:");
                ui.add_space(8.0);
                ui.radio_value(&mut self.indexing, Indexing::None, "None");
                ui.add_space(60.0);
                // FileMaker Pro offers Minimal for text only (E43).
                ui.add_enabled_ui(text, |ui| {
                    ui.radio_value(&mut self.indexing, Indexing::Minimal, "Minimal");
                });
                ui.add_space(60.0);
                ui.radio_value(&mut self.indexing, Indexing::All, "All");
            });
            ui.horizontal(|ui| {
                ui.add_space(80.0);
                ui.checkbox(
                    &mut self.automatic_indexes,
                    "Automatically create indexes as needed",
                );
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label("Default language:");
            // Sort Records' list of languages (script-steps.md 11.1.2); a
            // field may have one it does not show (none, System, Unicode
            // standard, catalog), shown by its catalog name.
            let languages = &crate::script_workspace::sort_dialog::LANGUAGES;
            let shown = languages
                .iter()
                .find(|(id, _)| *id == self.language)
                .map_or_else(
                    || {
                        let id = u16::try_from(self.language).unwrap_or(u16::MAX);
                        match om_data::calc::Language::from_id(id).type_name() {
                            "" => format!("Language {}", self.language),
                            name => name.replace('_', " "),
                        }
                    },
                    |(_, name)| (*name).to_owned(),
                );
            egui::ComboBox::from_id_salt("Default Language")
                .width(200.0)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for (id, name) in languages {
                        // Only a language whose index keys OpenMaker
                        // builds as FileMaker does (#159).
                        let keyed = om_data::rules::written_text_keys(*id, 0).is_some();
                        ui.add_enabled_ui(keyed, |ui| {
                            ui.selectable_value(&mut self.language, *id, *name)
                        })
                        .response
                        .on_disabled_hover_text(
                            "OpenMaker does not build index keys in this language yet.",
                        );
                    }
                });
        });
        note(
            ui,
            "Applies to text indexing, sorting, and default display font.",
        );
    }

    fn furigana(&mut self, ui: &mut egui::Ui) {
        use om_data::field_options::Furigana;
        // The table's stored text fields other than this one, which the
        // draft takes as the field receiving the reading.
        let targets: Vec<(u64, String)> = self
            .names
            .schema
            .as_ref()
            .and_then(|schema| schema.tables.iter().find(|t| t.id == self.table_id))
            .map(|table| {
                table
                    .fields
                    .iter()
                    .filter(|f| {
                        f.id != self.field_id
                            && f.kind == om_data::SchemaFieldKind::Normal
                            && f.data_type == FieldKind::Text
                            && !f.options.as_ref().is_some_and(|o| o.storage.global)
                    })
                    .map(|f| (f.id, f.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let text = self.data_type == FieldKind::Text;
        let stored = self.original.furigana.translation;
        ui.add_enabled_ui(text, |ui| {
            ui.checkbox(&mut self.furigana.on, "Use Furigana Field");
            ui.add_enabled_ui(self.furigana.on, |ui| {
                ui.label("Field that receives translated text");
                let field = self.furigana.field;
                let shown = targets
                    .iter()
                    .find(|(id, _)| *id == field)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| match field {
                        0 => "Choose a field".to_owned(),
                        other => format!("Field {other}"),
                    });
                egui::ComboBox::from_id_salt("Furigana Field")
                    .width(200.0)
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for (id, name) in &targets {
                            ui.selectable_value(&mut self.furigana.field, *id, name);
                        }
                    });
                ui.label("Translate into");
                let translation = self.furigana.translation;
                let shown = Furigana::TRANSLATIONS
                    .iter()
                    .find(|(value, _)| *value == translation)
                    .map(|(_, name)| (*name).to_owned())
                    .unwrap_or_else(|| match translation {
                        0 => "Choose a translation".to_owned(),
                        other => format!("Translation {other}"),
                    });
                egui::ComboBox::from_id_salt("Furigana Translation")
                    .width(200.0)
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for (value, name) in Furigana::TRANSLATIONS {
                            // Those FileMaker Pro was seen to write, or the
                            // field's own, kept as stored.
                            let known = Furigana::CONFIRMED.contains(&value) || value == stored;
                            ui.add_enabled_ui(known, |ui| {
                                ui.selectable_value(&mut self.furigana.translation, value, name)
                            })
                            .response
                            .on_disabled_hover_text(
                                "OpenMaker does not write this translation yet: what FileMaker \
                                 Pro writes for it was not seen (#1334).",
                            );
                        }
                    });
            });
        })
        .response
        .on_disabled_hover_text("Furigana is set on text fields.");
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "The Furigana option allows you to specify a field that will receive a \
                 phonetic translation of Japanese text entered in the current field.",
            )
            .small(),
        );
    }
}
