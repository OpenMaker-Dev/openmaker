//! A step's options dialog (capture 35b): `"Go to Object" Options`, the
//! step's description, one row per option of its parameter schema, Cancel
//! and OK.
//!
//! Rows follow the schema (script-step-params.csv): Booleans are
//! checkboxes, enumerations pop-up menus, calculations a text field with
//! Specify… (Specify Calculation on top), text a text field, fields and
//! targets, layouts, scripts and tables a name with Specify… (the pickers
//! of `pickers.rs`), Sort Records' sort order and the find steps' requests
//! their own dialogs. What no row edits is listed as kept as stored.
//!
//! OK applies only the rows that changed, each through om-scripts' setters,
//! to the step as it was: OK with no change answers the step unchanged,
//! byte for byte.

use std::sync::Arc;

use egui::{pos2, vec2, Color32, FontId, Rect, Sense};
use om_data::host::CalcSchema;
use om_data::runtime::{OwnedNode, WindowOptions};
use om_data::scripts::display::{calculation_text, field_text, NoStepNames, StepNames, UNKNOWN};
use om_data::scripts::specs::{self, SortSpec, StoredRequest};
use om_data::scripts::{
    bytes_node, flag, step_info, FieldSpec, Location, ParamKind, ParamSchema, ParamValue, RefKind,
    SlotKind, StepData, StoredKey, VARIABLE_NAME_SLOT,
};
use om_data::{Calculation, ScriptStep};

use super::calculation::{self, CalcOutcome, SpecifyCalculation};
use super::export_dialog::{self, ExportOrder};
use super::file_dialog::SpecifyFile;
use super::find_dialog::FindRequests;
use super::import_dialog::{ImportMapping, ImportParts, Mapped};
use super::pickers::{
    self, checkbox, left_label, popup_menu, push_button, row_label, text_field, value_box,
    FieldTarget, ListPicker, Picked, SpecifyField, BUTTON_WIDTH, CONTROL_HEIGHT, MARGIN_LEFT,
    MARGIN_RIGHT,
};
use super::sort_dialog::SortOrder;
use crate::theme;
use om_transfer::{ExportOptions, FieldFlags, FileType, ImportOptions, Profile, ProfileEx};

pub(crate) enum OptionsOutcome {
    Cancel,
    Ok(ScriptStep),
}

/// 35b's description of Go to Object, which differs from the step
/// catalog's one-line summary.
const GO_TO_OBJECT: &str = "Specify an object on the current layout to make active. Object \
                            names are assigned using the Inspector command in Layout mode.";
/// The description's size and line height (35b: 14 pt lines).
const DESCRIPTION: f32 = theme::SMALL;
const DESCRIPTION_LINE: f32 = 14.0;
/// Rows with a text field, pop-up menu or name, and checkbox rows (35b:
/// 34 pt between the two rows).
const FIELD_ROW: f32 = 34.0;
const CHECK_ROW: f32 = 26.0;
/// The narrowest text field (35b: 208 pt).
const FIELD_WIDTH: f32 = 208.0;
/// The narrowest dialog (35b: 435 pt).
const MIN_WIDTH: f32 = 435.0;
/// Between a row's label and its control, and its control and Specify….
const LABEL_GAP: f32 = 8.0;
const SPECIFY_GAP: f32 = 11.0;

/// Go to Layout and Go to Related Record transitions (slot 6), in the
/// order the step text names them (script-steps.md, step text item 4).
const ANIMATIONS: [&str; 11] = [
    "None",
    "Slide in from Left",
    "Slide in from Right",
    "Slide in from Bottom",
    "Slide out to Left",
    "Slide out to Right",
    "Slide out to Bottom",
    "Flip from Left",
    "Flip from Right",
    "Zoom In",
    "Zoom Out",
];

/// Perform Script's "Specify by name" bits written with a calculated
/// name (corpus: `[00 1] = 1`, name in slot 2, `0x800` and `0x2000000`).
const BY_NAME_FLAGS: u32 = flag::STORE_REFERENCE | flag::RESTORE;

/// One option row.
struct Row {
    /// The row's label (without the colon); a checkbox's text.
    label: String,
    edit: Edit,
    /// Shown only while row `.0` (a choice) has one of the values `.1`.
    when: Option<(usize, &'static [u32])>,
    /// The schema parameter the row edits.
    param: String,
}

/// What a row edits, as it is in the dialog and as it was.
enum Edit {
    /// A Boolean in flag bits or numeric data.
    Flag {
        location: Location,
        on: bool,
        was: bool,
    },
    /// An enumeration in the step value or a slot's list value; `flags`
    /// are written with it (Loop's Flush reads its value only with
    /// `kOption`).
    Choice {
        name: String,
        location: Location,
        flags: u32,
        choices: Vec<(u32, String)>,
        value: Option<u32>,
        was: Option<u32>,
    },
    /// A calculation slot; `flags` mark it as present (Set Field By Name's
    /// `kFieldSpecByCalc`, Perform Script's by-name bits).
    Calculation {
        slot: u16,
        flags: u32,
        text: String,
        was: String,
        /// The answer of Specify Calculation, while the text shows it.
        specified: Option<Calculation>,
    },
    Text {
        slot: u16,
        text: String,
        was: String,
    },
    Number {
        slot: u16,
        text: String,
        was: String,
    },
    /// A field spec (slot 0 is `mReserved`), or a variable for targets.
    Field {
        slot: u16,
        target: bool,
        value: Option<FieldTarget>,
        was: Option<FieldTarget>,
    },
    /// The layout key in `mReserved`.
    Layout {
        value: Option<u64>,
        was: Option<u64>,
        /// Specify Layout answered: Go to Related Record's `<Current
        /// Layout>` is written even where the step already read as none.
        picked: bool,
    },
    /// The script key in `mReserved` (serial id).
    Script {
        value: Option<u64>,
        was: Option<u64>,
    },
    /// A table occurrence at `[00 n, 11]`, or a base table key in
    /// `mReserved` (Truncate Table).
    Table {
        aliases: Option<u16>,
        value: Option<u64>,
        was: Option<u64>,
    },
    /// Perform Script's "Specified: From list / By name" (`[00 1]`).
    ScriptBy { by_name: bool, was: bool },
    /// The window steps' window (script-steps.md, "Implementation notes",
    /// `WindowReference`): the step value, 0 the current window, 1 (or 2)
    /// a window named by the calculation in slot 0. Written without
    /// `kMenuSelection`, which no stored window step has.
    Window { by_name: bool, was: bool },
    /// Sort Records: "Specify sort order" (`kRestore`) and the sort order
    /// (`0a`). `stored` is `None` when no order is stored or it does not
    /// decode (`editable` tells which).
    Sort {
        restore: bool,
        was: bool,
        stored: Option<SortSpec>,
        edited: Option<SortSpec>,
        editable: bool,
    },
    /// The find steps: "Specify find requests" (`kRestore`) and the
    /// requests (`06`).
    Find {
        restore: bool,
        was: bool,
        stored: Option<Vec<StoredRequest>>,
        edited: Option<Vec<StoredRequest>>,
        editable: bool,
    },
    /// Export Records: "Specify export order" (`kRestore`) and the export
    /// order (`85`, script-steps.md 11.4). `stored` is `None` when no order
    /// is stored or it does not decode (`editable` tells which).
    Export {
        restore: bool,
        was: bool,
        stored: Option<ExportOptions>,
        edited: Option<ExportOptions>,
        editable: bool,
    },
    /// Import Records: "Specify import order" (`kRestore`) and the import
    /// order (`84`) with the profile extension (`87`), script-steps.md 11.3
    /// and 11.5. `edited` holds what Import Field Mapping answered;
    /// `editable` is false when the stored order does not decode.
    Import {
        restore: bool,
        was: bool,
        edited: Option<Mapped>,
        editable: bool,
    },
    /// Export Records' output file type (`86`, script-steps.md 11.5): one of
    /// [`OUTPUT_TYPES`], whose profiles have no options. `was` is the type
    /// shown when the options opened (Tab-Separated Values when no profile
    /// is stored, as the step then writes); only a different choice is
    /// written. `field_names` is the stored profile's field names word.
    FileType {
        value: FileType,
        was: FileType,
        field_names: u16,
    },
    /// A step's file path list (`[00 0, 82]`, script-steps.md 11.0), one
    /// path per line, edited in Specify Output File or Specify File.
    Paths { text: String, was: String },
    /// New Window's style word at `[00 0, 03]` (`set_number`, as the step
    /// text's parser writes it): one of [`WINDOW_STYLES`]; `was` is the word
    /// shown when the options opened (Document when none is stored).
    WindowStyle { value: u32, was: u32 },
    /// Shown, not edited: the stored value stays as it is.
    Kept,
}

impl Edit {
    /// The current value of a row other rows depend on.
    fn choice(&self) -> Option<u32> {
        match self {
            Edit::Choice { value, .. } => Some(value.unwrap_or(0)),
            Edit::ScriptBy { by_name, .. } | Edit::Window { by_name, .. } => {
                Some(u32::from(*by_name))
            }
            // A checkbox other rows show with: 1 while it is checked.
            Edit::Flag { on, .. } => Some(u32::from(*on)),
            _ => None,
        }
    }

    /// Whether the row is drawn as a checkbox line.
    fn is_check(&self) -> bool {
        matches!(
            self,
            Edit::Flag { .. }
                | Edit::Sort { .. }
                | Edit::Find { .. }
                | Edit::Export { .. }
                | Edit::Import { .. }
        )
    }
}

/// A dialog open above the options.
enum Nested {
    Calculation {
        row: usize,
        dialog: Box<SpecifyCalculation>,
    },
    Field {
        row: usize,
        dialog: Box<SpecifyField>,
    },
    List {
        row: usize,
        dialog: Box<ListPicker>,
    },
    Sort {
        row: usize,
        dialog: Box<SortOrder>,
    },
    Find {
        row: usize,
        dialog: Box<FindRequests>,
    },
    Export {
        row: usize,
        dialog: Box<ExportOrder>,
    },
    Import {
        row: usize,
        dialog: Box<ImportMapping>,
    },
    Paths {
        row: usize,
        dialog: Box<SpecifyFile>,
    },
}

pub(crate) struct StepOptions {
    original: ScriptStep,
    names: Option<Arc<CalcSchema>>,
    title: String,
    description: String,
    rows: Vec<Row>,
    nested: Option<Nested>,
    /// A calculation that did not compile: FileMaker's message.
    alert: Option<String>,
    /// Frames shown, to give the first text field the keyboard once the
    /// modal's sizing pass is over.
    frames: u32,
}

fn step_names(names: &Option<Arc<CalcSchema>>) -> &dyn StepNames {
    match names {
        Some(schema) => schema.as_ref(),
        None => &NoStepNames,
    }
}

/// A schema name as a row label: words capitalized as FileMaker's
/// dialogs label them (`Object name` -> `Object Name`); words with a
/// capital already (`cURL`) and short joining words stay.
fn title_case(name: &str) -> String {
    const SMALL: [&str; 14] = [
        "a", "an", "and", "as", "at", "by", "for", "from", "in", "of", "on", "or", "the", "to",
    ];
    name.split(' ')
        .enumerate()
        .map(|(n, word)| {
            let keep = word.chars().any(char::is_uppercase)
                || (n > 0 && SMALL.contains(&word))
                || !word.starts_with(char::is_alphabetic);
            if keep {
                word.to_owned()
            } else {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first slot of `kind` among a parameter's locations.
fn slot_of(p: &ParamSchema, kind: SlotKind) -> Option<u16> {
    p.storage.iter().find_map(|l| match *l {
        Location::Slot { slot, kind: k } if k == kind => Some(slot),
        _ => None,
    })
}

/// The number of slots of `kind` among a parameter's locations.
fn slots_of(p: &ParamSchema, kind: SlotKind) -> usize {
    p.storage
        .iter()
        .filter(|l| matches!(l, Location::Slot { kind: k, .. } if *k == kind))
        .count()
}

/// The flag bits set together with a value (non-inverted flag locations).
pub(super) fn companion_flags(p: &ParamSchema) -> u32 {
    p.storage.iter().fold(0, |acc, l| match *l {
        Location::Flag {
            bits,
            inverted: false,
        } => acc | bits,
        _ => acc,
    })
}

/// A stored field spec as the pickers name it.
fn field_target(step: &ScriptStep, slot: u16) -> Option<FieldTarget> {
    let spec = step.slot_field_spec(slot)?;
    if spec.is_variable() {
        return Some(FieldTarget::Variable(
            step.text(VARIABLE_NAME_SLOT + slot).unwrap_or_default(),
        ));
    }
    Some(FieldTarget::Field {
        occurrence: spec.table().id()?,
        field: spec.field().id()?,
        repetition: spec.repetition(),
    })
}

/// The occurrence at `[00 n, 11]` (`[u8 len][segment]`), when it is one
/// plain key; `Err` when something else is stored there.
fn aliases_occurrence(step: &ScriptStep, slot: u16) -> Result<Option<u64>, ()> {
    let Some(node) = step.data.as_ref().and_then(|d| d.slot(slot)?.child(0x11)) else {
        return Ok(None);
    };
    if !node.children.is_empty() {
        return Err(());
    }
    let bytes = node.value.as_deref().ok_or(())?;
    let (&len, segment) = bytes.split_first().ok_or(())?;
    if segment.len() != usize::from(len) {
        return Err(());
    }
    let key = StoredKey::from_segment(segment).ok_or(())?;
    key.id().map(Some).ok_or(())
}

/// Import Records' profile extension and per-field flags (script-steps.md
/// 11.3: `KImportProfileEx`, `KFlags`).
const IMPORT_PROFILE_EX: u8 = 0x87;
const IMPORT_FIELD_FLAGS: u8 = 0xf0;

/// The output file types Export Records' Type row offers: those whose
/// profile has no options (script-steps.md 11.5). XML, whose grammar and
/// style sheet FileMaker sets in a dialog of its own, stays as stored.
const OUTPUT_TYPES: [FileType; 6] = [
    FileType::TabSeparated,
    FileType::CommaSeparated,
    FileType::Merge,
    FileType::HtmlTable,
    FileType::ExcelExport,
    FileType::DBase,
];

/// New Window's style words FileMaker is observed to write
/// (script-steps.md, New Window style word): its default Document with its
/// seven options, Dialog, and the starter files' Card.
const WINDOW_STYLES: [(u32, &str); 3] = [
    (WindowOptions::DOCUMENT_WORD, "Document"),
    (WindowOptions::DIALOG_WORD, "Dialog"),
    (WindowOptions::CARD_WORD, "Card"),
];

/// Replace Field Contents (91) and Replace Field Contents by Name (249),
/// which store its options alike (script-steps.md 12.4).
fn replaces(id: u16) -> bool {
    matches!(id, 91 | 249)
}

/// The label of the step's Restore option where Claris names it.
fn restore_label(id: u16) -> &'static str {
    match id {
        39 => "Specify sort order",
        22 | 28 | 126 | 127 => "Specify find requests",
        35 => "Specify import order",
        36 => "Specify export order",
        43 => "Specify print options",
        _ => "Restore",
    }
}

/// Step-specific enumerations (script-steps.md 3.2 and the step text)
/// where the schema's values are not the menu's.
fn choices_for(id: u16, p: &ParamSchema) -> Vec<(u32, String)> {
    let named = |items: &[(u32, &str)]| items.iter().map(|(v, l)| (*v, (*l).to_owned())).collect();
    match (id, p.name.as_str(), p.kind) {
        // Pause/Resume Script: a duration is choice 1 (script-steps.md,
        // "Defaults the reader writes").
        (62, "Pause", ParamKind::Choice) => named(&[(0, "Indefinitely"), (1, "For duration")]),
        (6, "Layout", ParamKind::Choice) => named(&[
            (1, "original layout"),
            (5, "Layout"),
            (3, "Layout Name by calculation"),
            (4, "Layout Number by calculation"),
        ]),
        (_, "Animation", _) => (0u32..)
            .zip(ANIMATIONS.iter().map(|a| (*a).to_owned()))
            .collect(),
        _ => p.choices(),
    }
}

impl StepOptions {
    pub(crate) fn new(step: ScriptStep, names: Option<Arc<CalcSchema>>) -> Self {
        let id = step.id();
        let title = format!("\"{}\" Options", step.name());
        let description = match id {
            145 => GO_TO_OBJECT.to_owned(),
            _ => step_info(id)
                .map(|info| info.description.clone())
                .unwrap_or_default(),
        };
        let rows = rows(&step, &names);
        Self {
            original: step,
            names,
            title,
            description,
            rows,
            nested: None,
            alert: None,
            frames: 0,
        }
    }

    fn calc_text(&self, calc: &Calculation) -> String {
        calculation_text(calc, step_names(&self.names))
    }

    /// The step with every changed row applied, or FileMaker's message
    /// for a calculation that does not compile.
    fn result(&self) -> Result<ScriptStep, String> {
        let mut step = self.original.clone();
        for (i, row) in self.rows.iter().enumerate() {
            if self.visible(i) {
                self.apply(&mut step, row)?;
            }
        }
        // A window by name needs its name: without one the runtime acts
        // on the current window, which is not what was chosen.
        let by_name = self
            .rows
            .iter()
            .any(|r| matches!(r.edit, Edit::Window { by_name: true, .. }));
        if by_name && step.calculation(0).is_none() {
            return Err(
                "Window Name needs the window's name: type it in quotes, or a \
                        calculation that gives it."
                    .into(),
            );
        }
        // Pause/Resume Script: back to Indefinitely drops the duration,
        // which the runtime and the step text read as the choice. For
        // duration chosen here, or a duration cleared, needs one; a stored
        // step left as it was OKs unchanged.
        if step.id() == 62 {
            let pause = self.rows.iter().find_map(|r| match r.edit {
                Edit::Choice {
                    location: Location::Value,
                    value,
                    was,
                    ..
                } => Some((value, was)),
                _ => None,
            });
            match pause {
                Some((Some(0), Some(1))) => set_calculation(&mut step, 0, 0, None),
                Some((Some(1), was))
                    if step.calculation(0).is_none()
                        && (was != Some(1) || self.original.calculation(0).is_some()) =>
                {
                    return Err("For duration needs the number of seconds.".into());
                }
                _ => {}
            }
        }
        // A calculated result needs its calculation: without one the
        // runtime refuses the step (error 5). Whether FileMaker's dialog
        // lets it be left empty is not measured.
        if replaces(step.id()) && step.number(1) == Some(3) && step.calculation(0).is_none() {
            return Err("Calculated result needs its calculation.".into());
        }
        // A Show Custom Dialog button labelled here gets its "Commit data"
        // byte with it, as FileMaker Pro stores one beside every labelled
        // button: without it, the step text reads the default button as
        // not committing where the runtime commits.
        if step.id() == 87 {
            for slot in 0..3u16 {
                let labelled = self.rows.iter().any(|r| {
                    matches!(&r.edit, Edit::Text { slot: s, text, was }
                        if *s == slot && text != was && !text.is_empty())
                });
                let shown = self.rows.iter().find_map(|r| match r.edit {
                    Edit::Flag {
                        location:
                            Location::Slot {
                                slot: s,
                                kind: SlotKind::Number,
                            },
                        on,
                        ..
                    } if s == 3 + slot => Some(on),
                    _ => None,
                });
                if let (true, Some(on), None) = (labelled, shown, step.number(3 + slot)) {
                    step.set_number_bool(3 + slot, Some(on));
                }
            }
        }
        // FileMaker reads Export Records' file type only with its output
        // file (`86` with `kFilePath`, script-steps.md 11.5): a type chosen
        // without a file is not stored.
        let typed = self
            .rows
            .iter()
            .any(|r| matches!(r.edit, Edit::FileType { value, was, .. } if value != was));
        if typed && step.path_list().is_none() {
            return Err(
                "The output file's type is stored with the output file: specify the file \
                 (File Paths) first."
                    .into(),
            );
        }
        Ok(step)
    }

    fn apply(&self, step: &mut ScriptStep, row: &Row) -> Result<(), String> {
        match &row.edit {
            // Read-only (`enabled`): never written.
            edit if read_only_option(step.id(), edit) => {}
            Edit::Flag { location, on, was } if on != was => match *location {
                Location::Flag { bits, inverted } => step.set_flag(bits, *on != inverted),
                Location::Slot { slot, .. } => step.set_number_bool(slot, Some(*on)),
                _ => {}
            },
            Edit::Choice {
                name,
                location,
                flags,
                value: Some(value),
                was,
                ..
            } if Some(*value) != *was => {
                match *location {
                    // Save Records as PDF keeps its records in the step
                    // value under its compound PDF options (11.7), which the
                    // schema does not list as a value location; written as
                    // the step text's parser writes it.
                    Location::Value if step.id() == 144 => {
                        step.value = u16::try_from(*value).map_err(|_| "No such choice.")?;
                        step.flags |= flag::MENU_SELECTION;
                    }
                    // Print's records (11.6, #843): the step value and the
                    // stored settings' `printStyleSetting`, as FileMaker
                    // Pro 26 writes them, without kMenuSelection.
                    Location::Key(0x88) if step.id() == 43 => {
                        let choice = u8::try_from(*value).map_err(|_| "No such choice.")?;
                        let refused = || "The stored print settings cannot take this choice.";
                        let node = step.data_leaf(0x88).ok_or_else(refused)?;
                        let node =
                            crate::printing::with_print_style(node, choice).ok_or_else(refused)?;
                        // The record is replaced where it is stored: the
                        // step's flags stay as they are (kExtendedData too).
                        let flags = step.flags;
                        step.set_data_leaf(0x88, Some(node));
                        step.flags = flags;
                        step.value = u16::from(choice);
                    }
                    Location::Value => step
                        .set_parameter(name, ParamValue::Choice(*value))
                        .map_err(|e| e.0)?,
                    // Save Records as Excel's records are two bytes (11.7).
                    Location::Slot {
                        slot,
                        kind: SlotKind::Number,
                    } if step.id() == 143 => step.set_number_u16(slot, u16::try_from(*value).ok()),
                    // A choice kept as numeric data (Replace Field Contents'
                    // `[00 1, 03]`, written as the step text's parser does).
                    Location::Slot {
                        slot,
                        kind: SlotKind::Number,
                    } => step.set_number(slot, i32::try_from(*value).ok()),
                    Location::Slot { slot, .. } => step.set_list_value(slot, Some(*value)),
                    _ => {}
                }
                if *flags != 0 {
                    step.set_flag(*flags, true);
                }
            }
            Edit::Calculation {
                slot,
                flags,
                text,
                was,
                specified,
            } if text != was || specified.is_some() => {
                let calc = match specified {
                    Some(calc) if self.calc_text(calc) == *text => Some(calc.clone()),
                    _ if text.trim().is_empty() => None,
                    _ => Some(calculation::compile(text, self.names.as_deref())?),
                };
                if calc.is_none() && step.calculation(*slot).is_none() {
                    return Ok(());
                }
                set_calculation(step, *slot, *flags, calc.as_ref());
            }
            Edit::Text { slot, text, was } if text != was => step.set_text(*slot, Some(text)),
            Edit::Number { slot, text, was } if text != was => {
                let number = match text.trim() {
                    "" => None,
                    t => Some(
                        t.parse::<i32>()
                            .map_err(|_| format!("“{}” needs a whole number.", row.label))?,
                    ),
                };
                step.set_number(*slot, number);
            }
            Edit::Field {
                slot,
                value: Some(value),
                was,
                ..
            } if Some(value) != was.as_ref() => match value {
                FieldTarget::Field {
                    occurrence,
                    field,
                    repetition,
                } => {
                    let (Some(table), Some(field)) =
                        (StoredKey::from_id(*occurrence), StoredKey::from_id(*field))
                    else {
                        return Err("This field cannot be stored in a step.".into());
                    };
                    step.set_field_spec(*slot, Some(FieldSpec::new(table, field, *repetition)));
                    if matches!(was, Some(FieldTarget::Variable(_))) {
                        step.set_text(VARIABLE_NAME_SLOT + slot, None);
                    }
                }
                FieldTarget::Variable(name) => step.set_variable_target(*slot, name),
            },
            Edit::Layout {
                value: Some(id),
                was,
                ..
            } if Some(*id) != *was => {
                step.set_member_key(StoredKey::from_id(*id));
                // Go to Related Record shows a stored layout only with
                // step value 5 (1 is the current layout).
                match step.id() {
                    6 => step
                        .set_parameter("Layout", ParamValue::Choice(5))
                        .map_err(|e| e.0)?,
                    74 => step
                        .set_parameter("Options", ParamValue::Choice(5))
                        .map_err(|e| e.0)?,
                    // New Window's layout discriminator is 5 without
                    // kMenuSelection, as in the native stored steps.
                    122 => step.value = 5,
                    _ => {}
                }
            }
            // Go to Related Record's <Current Layout> chosen: step value 1
            // and no layout key, as the step text and XML store it. A step
            // that read as none (value 0, or 5 without a key) gets them too.
            Edit::Layout {
                value: None,
                picked: true,
                ..
            } if step.id() == 74 => {
                if step.member_key().is_some() {
                    step.set_member_key(None);
                }
                if step.value != 1 {
                    step.set_parameter("Options", ParamValue::Choice(1))
                        .map_err(|e| e.0)?;
                }
            }
            Edit::Script {
                value: Some(id),
                was,
            } if Some(*id) != *was => {
                step.set_script_key(StoredKey::from_serial_id(*id));
                // A script of this file: no file reference to another one.
                if step.data_leaf(StepData::FILE_REFERENCES).is_some() {
                    step.set_data_leaf(StepData::FILE_REFERENCES, None);
                }
            }
            Edit::Table {
                aliases,
                value: Some(id),
                was,
            } if Some(*id) != *was => {
                let key = StoredKey::from_id(*id)
                    .ok_or_else(|| "This table cannot be stored in a step.".to_owned())?;
                match aliases {
                    Some(slot) => {
                        let bytes = key.key().unwrap_or_default().to_vec();
                        step.set_slot_child(*slot, 0x11, Some(OwnedNode::leaf([0x11], bytes)));
                    }
                    None => {
                        step.set_member_key(Some(key));
                        if step.id() == 182 {
                            step.set_parameter("Table", ParamValue::Choice(1))
                                .map_err(|e| e.0)?;
                        }
                    }
                }
            }
            Edit::Paths { text, was } if text != was => {
                step.set_path_list(text)?;
                if step.id() == 35 {
                    // Import Records' data source kind: 1 a file, with
                    // kMenuSelection, as every stored file source has it
                    // (script-steps.md 11.3).
                    let file = !text.trim().is_empty();
                    step.value = u16::from(file);
                    step.set_flag(flag::MENU_SELECTION, file);
                }
            }
            Edit::Paths { text, was } if text != was => step.set_path_list(text)?,
            Edit::WindowStyle { value, was } if value != was => {
                step.set_number(0, Some(*value as i32));
            }
            Edit::Window { by_name, was } if by_name != was => {
                step.value = u16::from(*by_name);
                if !by_name {
                    // The name is read only by name: none is kept beside
                    // the current window, as FileMaker's XML importer
                    // stores none.
                    step.set_calculation(0, None);
                }
            }
            Edit::ScriptBy { by_name, was } if by_name != was => {
                step.set_list_value(1, Some(u32::from(*by_name)));
                step.set_flag(BY_NAME_FLAGS, *by_name);
                if !by_name && step.id() == 164 {
                    // Clear the legacy name so the runtime also uses the
                    // chosen script, not its slot-1 compatibility path.
                    step.set_calculation(1, None);
                }
            }
            Edit::Sort {
                restore,
                was,
                stored,
                edited,
                ..
            } => {
                if let Some(spec) = edited.as_ref().filter(|e| Some(*e) != stored.as_ref()) {
                    step.set_data_leaf(
                        StepData::SORT_ORDER,
                        Some(bytes_node(&[StepData::SORT_ORDER], &spec.encode())),
                    );
                }
                if restore != was {
                    step.set_flag(flag::RESTORE, *restore);
                }
            }
            Edit::Find {
                restore,
                was,
                stored,
                edited,
                ..
            } => {
                if let Some(requests) = edited.as_ref().filter(|e| Some(*e) != stored.as_ref()) {
                    step.set_data_leaf(StepData::REQUESTS, Some(specs::encode_requests(requests)));
                }
                if restore != was {
                    step.set_flag(flag::RESTORE, *restore);
                }
            }
            Edit::Import {
                restore,
                was,
                edited,
                ..
            } => {
                if let Some((order, ex, profile)) = edited {
                    let mut parts = vec![(StepData::IMPORT_ORDER, order.encode())];
                    if let Some(ex) = ex {
                        parts.push((IMPORT_PROFILE_EX, ex.encode()));
                    }
                    if let Some(profile) = profile {
                        parts.push((StepData::PROFILE, profile.encode()));
                    }
                    for (key, bytes) in parts {
                        let stored = step
                            .data_leaf(key)
                            .and_then(om_data::scripts::node_bytes)
                            .map(|b| b.into_owned());
                        // What decodes to the same value keeps its stored
                        // bytes (an accepted form the encoder writes
                        // otherwise).
                        let same = stored.as_deref().is_some_and(|stored| {
                            stored == bytes.as_slice()
                                || match key {
                                    StepData::IMPORT_ORDER => {
                                        ImportOptions::decode(stored).ok() == Some(order.clone())
                                    }
                                    IMPORT_PROFILE_EX => {
                                        ProfileEx::decode(stored).ok() == ex.clone()
                                    }
                                    _ => false,
                                }
                        });
                        if !same {
                            step.set_data_leaf(key, Some(bytes_node(&[key], &bytes)));
                        }
                    }
                }
                if restore != was {
                    step.set_flag(flag::RESTORE, *restore);
                }
            }
            Edit::FileType {
                value,
                was,
                field_names,
            } if value != was => {
                let profile = Profile {
                    field_names: *field_names,
                    ..Profile::new(*value)
                };
                step.set_data_leaf(
                    StepData::PROFILE,
                    Some(bytes_node(&[StepData::PROFILE], &profile.encode())),
                );
                // The output format's flag (`kExtendedData`, the step
                // parameter catalog).
                step.set_flag(flag::EXTENDED_DATA, true);
            }
            Edit::Export {
                restore,
                was,
                stored,
                edited,
                ..
            } => {
                if let Some(order) = edited.as_ref().filter(|e| Some(*e) != stored.as_ref()) {
                    step.set_data_leaf(
                        StepData::EXPORT_ORDER,
                        Some(bytes_node(&[StepData::EXPORT_ORDER], &order.encode())),
                    );
                }
                if restore != was {
                    step.set_flag(flag::RESTORE, *restore);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Whether row `i` is shown (its condition holds).
    fn visible(&self, i: usize) -> bool {
        match self.rows[i].when {
            Some((row, values)) => self.rows[row]
                .edit
                .choice()
                .is_some_and(|v| values.contains(&v)),
            None => true,
        }
    }

    /// Whether row `i` takes input. Go to Related Record's "Use external
    /// table's layouts" does only while the related table is another
    /// file's, as FileMaker Pro greys it out for this file's (#771's Pro
    /// check); a checked box, stored or just checked, stays clearable.
    fn enabled(&self, i: usize) -> bool {
        if read_only_option(self.original.id(), &self.rows[i].edit) {
            return false;
        }
        // Print (#843): its records take a choice only beside stored
        // settings that keep it (`printing::print_style`), and its options
        // cannot be specified where none are stored (FileMaker Pro asks for
        // them in its Print dialog, which OpenMaker does not have).
        if self.original.id() == 43 {
            let settings = self.original.data_leaf(0x88);
            match self.rows[i].edit {
                Edit::Choice {
                    location: Location::Key(0x88),
                    ..
                } => {
                    return settings
                        .and_then(|node| crate::printing::stored_record(node).ok())
                        .is_some_and(|bytes| crate::printing::print_style(&bytes).is_some());
                }
                Edit::Flag {
                    location:
                        Location::Flag {
                            bits: flag::RESTORE,
                            ..
                        },
                    was: false,
                    ..
                } => return settings.is_some(),
                _ => {}
            }
        }
        let Edit::Flag {
            location: Location::Flag {
                bits: flag::OPTION, ..
            },
            on,
            was,
        } = self.rows[i].edit
        else {
            return true;
        };
        if self.original.id() != 74 || on || was {
            return true;
        }
        let related = self.rows.iter().find_map(|r| match r.edit {
            Edit::Table { value, .. } => value,
            _ => None,
        });
        related.is_some_and(|o| {
            self.names
                .as_deref()
                .is_some_and(|n| n.external_occurrence(o).is_some())
        })
    }

    /// What a name row shows.
    fn shown_name(&self, edit: &Edit) -> String {
        let names = step_names(&self.names);
        match edit {
            Edit::Field { value, .. } => match value {
                Some(FieldTarget::Variable(name)) => name.clone(),
                Some(FieldTarget::Field {
                    occurrence,
                    field,
                    repetition,
                }) => match (StoredKey::from_id(*occurrence), StoredKey::from_id(*field)) {
                    (Some(table), Some(field)) => {
                        field_text(&FieldSpec::new(table, field, *repetition), names)
                    }
                    _ => UNKNOWN.into(),
                },
                None => String::new(),
            },
            Edit::Layout { value, .. } => match value {
                Some(id) => names.layout_name(*id).unwrap_or(UNKNOWN).to_owned(),
                None if self.original.id() == 74 => "<Current Layout>".into(),
                None => String::new(),
            },
            Edit::Script {
                value: Some(id), ..
            } => names.script_name(*id).unwrap_or(UNKNOWN).to_owned(),
            Edit::Table { aliases, value, .. } => match (value, aliases) {
                (Some(id), Some(_)) => names
                    .calc_names()
                    .occurrence_name(*id)
                    .unwrap_or(UNKNOWN)
                    .to_owned(),
                (Some(id), None) => names.table_name(*id).unwrap_or(UNKNOWN).to_owned(),
                (None, None) => "<Current Table>".into(),
                (None, Some(_)) => String::new(),
            },
            _ => String::new(),
        }
    }

    /// Open the dialog a row's Specify… opens.
    fn specify(&mut self, row: usize) {
        let names = self.names.clone();
        self.nested = match &self.rows[row].edit {
            Edit::Calculation {
                text, specified, ..
            } => {
                // The text as typed, when it compiles; else the stored one.
                let shown = specified
                    .clone()
                    .filter(|c| self.calc_text(c) == *text)
                    .or_else(|| calculation::compile(text, names.as_deref()).ok());
                Some(Nested::Calculation {
                    row,
                    dialog: Box::new(SpecifyCalculation::new(
                        "Specify Calculation".to_owned(),
                        shown.as_ref(),
                        names,
                    )),
                })
            }
            Edit::Field { target, value, .. } => Some(Nested::Field {
                row,
                dialog: Box::new(SpecifyField::new(names, value.clone(), *target)),
            }),
            Edit::Layout { value, .. } => Some(Nested::List {
                row,
                dialog: Box::new(if self.original.id() == 74 {
                    ListPicker::related_layouts(names.as_deref(), *value)
                } else {
                    ListPicker::layouts(names.as_deref(), *value)
                }),
            }),
            Edit::Script { value, .. } => Some(Nested::List {
                row,
                dialog: Box::new(ListPicker::scripts(names.as_deref(), *value)),
            }),
            Edit::Table { aliases, value, .. } => Some(Nested::List {
                row,
                dialog: Box::new(match aliases {
                    Some(_) => ListPicker::occurrences(names.as_deref(), *value),
                    None => ListPicker::base_tables(names.as_deref(), *value),
                }),
            }),
            Edit::Sort { stored, edited, .. } => {
                let spec = edited
                    .clone()
                    .or_else(|| stored.clone())
                    .unwrap_or(SortSpec {
                        keep_sorted: true,
                        blanks_last: false,
                        fields: Vec::new(),
                    });
                Some(Nested::Sort {
                    row,
                    dialog: Box::new(SortOrder::new(spec, names)),
                })
            }
            Edit::Find { stored, edited, .. } => {
                let requests = edited
                    .clone()
                    .or_else(|| stored.clone())
                    .unwrap_or_default();
                Some(Nested::Find {
                    row,
                    dialog: Box::new(FindRequests::new(requests, names)),
                })
            }
            Edit::Export { stored, edited, .. } => {
                let order = edited
                    .clone()
                    .or_else(|| stored.clone())
                    .unwrap_or_else(export_dialog::new_order);
                Some(Nested::Export {
                    row,
                    dialog: Box::new(ExportOrder::new(order, names)),
                })
            }
            Edit::Import { edited, .. } => {
                let step = &self.original;
                let stored = |key: u8| {
                    step.data_leaf(key)
                        .and_then(om_data::scripts::node_bytes)
                        .map(|b| b.into_owned())
                };
                // A stored part that does not decode would be written over
                // by a default: the order is not specified then.
                let unreadable = [
                    (StepData::PROFILE, "source type"),
                    (IMPORT_PROFILE_EX, "source options"),
                    (IMPORT_FIELD_FLAGS, "per-field import options"),
                ]
                .into_iter()
                .find(|(key, _)| {
                    stored(*key).is_some_and(|b| match *key {
                        StepData::PROFILE => Profile::decode(&b).is_err(),
                        IMPORT_PROFILE_EX => ProfileEx::decode(&b).is_err(),
                        _ => FieldFlags::decode(&b).is_err(),
                    })
                });
                if let Some((_, what)) = unreadable {
                    self.alert = Some(format!(
                        "OpenMaker cannot read this step's stored {what}, so its import order \
                         is kept as stored."
                    ));
                    return;
                }
                let (order, profile_ex) = match edited {
                    Some((order, ex, _)) => (Some(order.clone()), ex.clone()),
                    None => (
                        stored(StepData::IMPORT_ORDER).and_then(|b| ImportOptions::decode(&b).ok()),
                        stored(IMPORT_PROFILE_EX).and_then(|b| ProfileEx::decode(&b).ok()),
                    ),
                };
                // The data source as the options now give it.
                let paths = self
                    .rows
                    .iter()
                    .find_map(|r| match &r.edit {
                        Edit::Paths { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .or_else(|| step.path_list())
                    .unwrap_or_default();
                let parts = ImportParts {
                    paths: paths
                        .split(['\r', '\n'])
                        .filter(|l| !l.trim().is_empty())
                        .map(str::to_owned)
                        .collect(),
                    order,
                    profile: stored(StepData::PROFILE).and_then(|b| Profile::decode(&b).ok()),
                    profile_ex,
                    field_flags: stored(IMPORT_FIELD_FLAGS)
                        .and_then(|b| FieldFlags::decode(&b).ok()),
                };
                match ImportMapping::open(parts, names) {
                    Ok(dialog) => Some(Nested::Import {
                        row,
                        dialog: Box::new(dialog),
                    }),
                    Err(message) => {
                        self.alert = Some(message);
                        None
                    }
                }
            }
            Edit::Paths { text, .. } => Some(Nested::Paths {
                row,
                dialog: Box::new(SpecifyFile::new(self.original.id(), text.clone())),
            }),
            _ => None,
        };
    }

    /// The label column's width: the widest label with its colon.
    fn label_width(&self, ctx: &egui::Context) -> f32 {
        self.rows
            .iter()
            .filter(|r| !r.edit.is_check())
            .map(|r| text_width(ctx, &format!("{}:", r.label), pickers::body()))
            .fold(0.0, f32::max)
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<OptionsOutcome> {
        let on_top = self.nested.is_none() && self.alert.is_none();
        let label_width = self.label_width(ctx);
        let width = (MARGIN_LEFT
            + label_width
            + LABEL_GAP
            + FIELD_WIDTH
            + SPECIFY_GAP
            + BUTTON_WIDTH
            + MARGIN_RIGHT)
            .max(MIN_WIDTH)
            .ceil();
        let mut outcome = None;
        let mut ok = false;
        let mut specify = None;
        let first_frame = self.frames == 1;
        self.frames += 1;
        let title = self.title.clone();
        let shown = pickers::dialog(ctx, "step_options", &title, width, on_top, |ui| {
            let full = ui.available_width();
            ui.add_space(18.0);
            if !self.description.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&self.description)
                            .font(FontId::proportional(DESCRIPTION))
                            .line_height(Some(DESCRIPTION_LINE))
                            .color(theme::TEXT),
                    )
                    .wrap(),
                );
                ui.add_space(11.0);
            }
            let left = ui.cursor().left();
            let field_x = if label_width > 0.0 {
                left + label_width + LABEL_GAP
            } else {
                left
            };
            let right = left + full;
            let field_right = right - BUTTON_WIDTH - SPECIFY_GAP;
            let mut focused = false;
            for i in 0..self.rows.len() {
                if !self.visible(i) {
                    continue;
                }
                let check = self.rows[i].edit.is_check();
                let (band, _) = ui.allocate_exact_size(
                    vec2(full, if check { CHECK_ROW } else { FIELD_ROW }),
                    Sense::hover(),
                );
                let control = Rect::from_min_max(
                    pos2(field_x, band.center().y - CONTROL_HEIGHT / 2.0),
                    pos2(field_right, band.center().y + CONTROL_HEIGHT / 2.0),
                );
                let button = Rect::from_min_max(
                    pos2(right - BUTTON_WIDTH, control.top()),
                    pos2(right, control.bottom()),
                );
                if !check {
                    let label_rect = Rect::from_min_max(
                        pos2(left, band.top()),
                        pos2(field_x - LABEL_GAP, band.bottom()),
                    );
                    row_label(ui, label_rect, &format!("{}:", self.rows[i].label), true);
                }
                let name = self.shown_name(&self.rows[i].edit);
                let label = self.rows[i].label.clone();
                let specify_label = format!("Specify {label}…");
                let mut clicked_specify = false;
                let enabled = self.enabled(i);
                match &mut self.rows[i].edit {
                    Edit::Flag { on, .. } => {
                        checkbox(ui, control, on, &label, enabled);
                    }
                    Edit::Choice { choices, value, .. } => {
                        let current = value.map_or_else(String::new, |v| {
                            choices
                                .iter()
                                .find(|(n, _)| *n == v)
                                .map_or_else(|| v.to_string(), |(_, l)| l.clone())
                        });
                        let items: Vec<String> = choices.iter().map(|(_, l)| l.clone()).collect();
                        if let Some(n) = popup_menu(ui, control, &label, &current, &items, true) {
                            *value = Some(choices[n].0);
                        }
                    }
                    Edit::FileType { value, .. } => {
                        let items: Vec<String> =
                            OUTPUT_TYPES.iter().map(|t| t.name().to_owned()).collect();
                        if let Some(n) = popup_menu(ui, control, &label, value.name(), &items, true)
                        {
                            *value = OUTPUT_TYPES[n];
                        }
                    }
                    Edit::WindowStyle { value, .. } => {
                        let items: Vec<String> =
                            WINDOW_STYLES.iter().map(|(_, n)| (*n).to_owned()).collect();
                        let current = WINDOW_STYLES
                            .iter()
                            .find(|(w, _)| w == value)
                            .map_or("", |(_, n)| *n);
                        if let Some(n) = popup_menu(ui, control, &label, current, &items, true) {
                            *value = WINDOW_STYLES[n].0;
                        }
                    }
                    Edit::Window { by_name, .. } => {
                        let items = ["Current Window".to_owned(), "Window Name".to_owned()];
                        let current = &items[usize::from(*by_name)];
                        if let Some(n) = popup_menu(ui, control, &label, current, &items, true) {
                            *by_name = n == 1;
                        }
                    }
                    Edit::ScriptBy { by_name, .. } => {
                        let items = ["From list".to_owned(), "By name".to_owned()];
                        let current = &items[usize::from(*by_name)];
                        if let Some(n) = popup_menu(ui, control, &label, current, &items, true) {
                            *by_name = n == 1;
                        }
                    }
                    Edit::Calculation {
                        text, specified, ..
                    } => {
                        let before = text.clone();
                        let edit = text_field(ui, control, text, &label, true);
                        if *text != before {
                            *specified = None;
                        }
                        if first_frame && !focused {
                            pickers::select_all(ui, &edit, text);
                        }
                        focused = true;
                        clicked_specify =
                            push_button(ui, button, "Specify…", &specify_label, false, true)
                                .clicked();
                    }
                    Edit::Text { text, .. } | Edit::Number { text, .. } => {
                        let edit = text_field(ui, control, text, &label, true);
                        if first_frame && !focused {
                            pickers::select_all(ui, &edit, text);
                        }
                        focused = true;
                    }
                    Edit::Field { .. }
                    | Edit::Layout { .. }
                    | Edit::Script { .. }
                    | Edit::Table { .. } => {
                        value_box(ui, control, &name, &label, true);
                        clicked_specify =
                            push_button(ui, button, "Specify…", &specify_label, false, true)
                                .clicked();
                    }
                    Edit::Sort {
                        restore, editable, ..
                    }
                    | Edit::Find {
                        restore, editable, ..
                    }
                    | Edit::Export {
                        restore, editable, ..
                    }
                    | Edit::Import {
                        restore, editable, ..
                    } => {
                        checkbox(ui, control, restore, &label, true);
                        let accessible = format!("{}…", title_case(&label));
                        clicked_specify =
                            push_button(ui, button, "Specify…", &accessible, false, *editable)
                                .clicked();
                    }
                    Edit::Paths { text, .. } => {
                        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
                        let shown = match (lines.next(), lines.next()) {
                            (None, _) => String::new(),
                            (Some(first), None) => first.to_owned(),
                            (Some(first), Some(_)) => format!("{first} …"),
                        };
                        value_box(ui, control, &shown, &label, true);
                        clicked_specify =
                            push_button(ui, button, "Specify…", &specify_label, false, true)
                                .clicked();
                    }
                    Edit::Kept => left_label(
                        ui,
                        control,
                        "Kept as stored",
                        pickers::body(),
                        theme::WEAK_TEXT,
                    ),
                }
                if clicked_specify {
                    specify = Some(i);
                }
            }
            ui.add_space(18.0);
            let (cancel, ok_clicked) = pickers::cancel_ok(ui, true);
            if cancel {
                outcome = Some(OptionsOutcome::Cancel);
            }
            ok = ok_clicked;
        });
        if shown.escape {
            outcome = Some(OptionsOutcome::Cancel);
        }
        if (ok || shown.enter) && outcome.is_none() {
            // Return submits the options. A validation alert raised below
            // must wait for its own acknowledgment.
            ctx.input_mut(|input| {
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            });
            match self.result() {
                Ok(step) => outcome = Some(OptionsOutcome::Ok(step)),
                Err(message) => self.alert = Some(message),
            }
        }
        if let Some(row) = specify {
            self.specify(row);
        }
        self.nested_dialogs(ctx);
        if let Some(message) = self.alert.clone() {
            if super::alert(ctx, &message, &["OK"]).is_some() {
                self.alert = None;
            }
        }
        outcome
    }

    /// Show the dialog open above the options and take its answer.
    fn nested_dialogs(&mut self, ctx: &egui::Context) {
        let Some(nested) = &mut self.nested else {
            return;
        };
        let names = self.names.clone();
        let close = match nested {
            Nested::Calculation { row, dialog } => match dialog.show(ctx) {
                Some(CalcOutcome::Ok(calc)) => {
                    let text = calculation_text(&calc, step_names(&names));
                    if let Edit::Calculation {
                        text: shown,
                        specified,
                        ..
                    } = &mut self.rows[*row].edit
                    {
                        *shown = text;
                        *specified = Some(calc);
                    }
                    true
                }
                Some(CalcOutcome::Cancel) => true,
                None => false,
            },
            Nested::Field { row, dialog } => match dialog.show(ctx, true) {
                Some(Picked::Ok(target)) => {
                    if let Edit::Field { value, .. } = &mut self.rows[*row].edit {
                        *value = Some(target);
                    }
                    true
                }
                Some(Picked::Cancel) => true,
                None => false,
            },
            Nested::List { row, dialog } => match dialog.show(ctx, true) {
                Some(Picked::Ok(id)) => {
                    match &mut self.rows[*row].edit {
                        Edit::Layout { value, picked, .. } => {
                            *value = (id != pickers::CURRENT_LAYOUT).then_some(id);
                            *picked = true;
                        }
                        Edit::Script { value, .. } | Edit::Table { value, .. } => {
                            *value = Some(id);
                        }
                        _ => {}
                    }
                    true
                }
                Some(Picked::Cancel) => true,
                None => false,
            },
            Nested::Sort { row, dialog } => match dialog.show(ctx, true) {
                Some(Picked::Ok(spec)) => {
                    if let Edit::Sort {
                        restore, edited, ..
                    } = &mut self.rows[*row].edit
                    {
                        *edited = Some(spec);
                        *restore = true;
                    }
                    true
                }
                Some(Picked::Cancel) => true,
                None => false,
            },
            Nested::Paths { row, dialog } => match dialog.show(ctx, true) {
                Some(Picked::Ok(list)) => {
                    if let Edit::Paths { text, .. } = &mut self.rows[*row].edit {
                        *text = list;
                    }
                    true
                }
                Some(Picked::Cancel) => true,
                None => false,
            },
            Nested::Find { row, dialog } => match dialog.show(ctx, true) {
                Some(Picked::Ok(requests)) => {
                    if let Edit::Find {
                        restore, edited, ..
                    } = &mut self.rows[*row].edit
                    {
                        *edited = Some(requests);
                        *restore = true;
                    }
                    true
                }
                Some(Picked::Cancel) => true,
                None => false,
            },
            Nested::Import { row, dialog } => match dialog.show(ctx, true) {
                Some(Picked::Ok(mapped)) => {
                    if let Edit::Import {
                        restore, edited, ..
                    } = &mut self.rows[*row].edit
                    {
                        *edited = Some(mapped);
                        *restore = true;
                    }
                    true
                }
                Some(Picked::Cancel) => true,
                None => false,
            },
            Nested::Export { row, dialog } => match dialog.show(ctx, true) {
                Some(Picked::Ok(order)) => {
                    if let Edit::Export {
                        restore, edited, ..
                    } = &mut self.rows[*row].edit
                    {
                        *edited = Some(order);
                        *restore = true;
                    }
                    true
                }
                Some(Picked::Cancel) => true,
                None => false,
            },
        };
        if close {
            self.nested = None;
        }
    }
}

fn text_width(ctx: &egui::Context, text: &str, font: FontId) -> f32 {
    ctx.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
            .size()
            .x
    })
}

/// Whether a find request node decodes without losing a cell (an
/// unpacked criterion key, a malformed text): only then is it edited.
fn requests_editable(node: &OwnedNode) -> Option<Vec<StoredRequest>> {
    let requests = specs::requests(node)?;
    let rows = node.child(&[0x06])?;
    let cells = rows
        .children
        .iter()
        .filter(|row| row.segment.as_slice() != [0x00])
        .flat_map(|row| &row.children)
        .filter(|cell| !matches!(cell.segment.as_slice(), [0x00] | [0xfc]))
        .count();
    let criteria: usize = requests.iter().map(|r| r.criteria.len()).sum();
    (cells == criteria).then_some(requests)
}

/// Options that show what is stored and take no input:
///
/// - Replace Field Contents' (and by Name's, [`replaces`]) "Update serial
///   number in Entry Options" (`[00 4, 03]`) and "Perform auto-enter
///   options for fields" (`[00 6, 03]` with `kRestore`): FileMaker Pro
///   26.0.3 greys the first out for a field without an auto-entered serial
///   number, and what its own editor writes for either is not measured
///   (#798's Pro check).
/// - Show Custom Dialog's "Close after" (`[00 6, 03]`) and its seconds
///   (slot 13), FileMaker Pro 26.1's: known from the binaries only
///   (script-steps.md 12.4), the Boolean's byte width unmeasured, so
///   OpenMaker writes neither.
fn read_only_option(step: u16, edit: &Edit) -> bool {
    let number = |slots: &[u16]| {
        matches!(
            edit,
            Edit::Flag {
                location: Location::Slot {
                    slot,
                    kind: SlotKind::Number,
                },
                ..
            } if slots.contains(slot)
        )
    };
    match step {
        id if replaces(id) => number(&[4, 6]),
        87 => number(&[6]) || matches!(edit, Edit::Calculation { slot: 13, .. }),
        _ => false,
    }
}

/// A row before its condition is known.
fn row(label: impl Into<String>, edit: Edit) -> Row {
    Row {
        label: label.into(),
        edit,
        when: None,
        param: String::new(),
    }
}

/// The rows of a step's options, in schema order.
fn rows(step: &ScriptStep, names: &Option<Arc<CalcSchema>>) -> Vec<Row> {
    let Some(schema) = step.schema() else {
        return Vec::new();
    };
    let id = step.id();
    let parameters = step.parameters();
    let calc_text = |slot: u16| {
        step.calculation(slot)
            .map(|c| calculation_text(&c, step_names(names)))
    };
    let mut rows: Vec<Row> = Vec::new();
    let mut skip: Vec<usize> = Vec::new();
    for (index, p) in schema.params.iter().enumerate() {
        if p.name == "Collapsed" || p.kind == ParamKind::Index || skip.contains(&index) {
            continue;
        }
        let param = &parameters[index];
        let label = title_case(&p.name);
        let first = rows.len();
        let kept = param.is_stored().then(|| row(label.clone(), Edit::Kept));
        match p.kind {
            ParamKind::Flag if p.name == "Restore" && id == 39 => {
                let restore = step.has_flag(flag::RESTORE);
                let node = step.data_leaf(StepData::SORT_ORDER);
                let stored = node.and_then(SortSpec::from_node);
                rows.push(row(
                    restore_label(id),
                    Edit::Sort {
                        restore,
                        was: restore,
                        editable: node.is_none() || stored.is_some(),
                        stored,
                        edited: None,
                    },
                ));
            }
            ParamKind::Flag if p.name == "Restore" && matches!(id, 22 | 28 | 126 | 127) => {
                let restore = step.has_flag(flag::RESTORE);
                let node = step.data_leaf(StepData::REQUESTS);
                let stored = node.and_then(requests_editable);
                rows.push(row(
                    restore_label(id),
                    Edit::Find {
                        restore,
                        was: restore,
                        editable: node.is_none() || stored.is_some(),
                        stored,
                        edited: None,
                    },
                ));
            }
            // Import Records: "Specify import order" and its order (one row
            // for the parameter's keys, however the schema lists them).
            ParamKind::ImportOrder if id == 35 => {
                if !rows.iter().any(|r| matches!(r.edit, Edit::Import { .. })) {
                    let restore = step.has_flag(flag::RESTORE);
                    let node = step.data_leaf(StepData::IMPORT_ORDER);
                    let decodes = node
                        .and_then(om_data::scripts::node_bytes)
                        .is_some_and(|bytes| ImportOptions::decode(&bytes).is_ok());
                    rows.push(row(
                        restore_label(id),
                        Edit::Import {
                            restore,
                            was: restore,
                            edited: None,
                            editable: node.is_none() || decodes,
                        },
                    ));
                }
            }
            // Export Records' output file type, unless the stored profile
            // has options (XML) or does not decode: then it stays as stored.
            ParamKind::FileReference if id == 36 && p.name == "Output format" => {
                let node = step.data_leaf(StepData::PROFILE);
                let profile = node
                    .and_then(om_data::scripts::node_bytes)
                    .and_then(|bytes| Profile::decode(&bytes).ok());
                match (node, profile) {
                    (None, _) => rows.push(row(
                        "Type",
                        Edit::FileType {
                            value: FileType::TabSeparated,
                            was: FileType::TabSeparated,
                            field_names: 0,
                        },
                    )),
                    (Some(_), Some(p)) if OUTPUT_TYPES.contains(&p.file_type) => rows.push(row(
                        "Type",
                        Edit::FileType {
                            value: p.file_type,
                            was: p.file_type,
                            field_names: p.field_names,
                        },
                    )),
                    _ => rows.extend(kept),
                }
            }
            // Export Records: "Specify export order" and its order.
            ParamKind::ExportOrder if id == 36 => {
                let restore = step.has_flag(flag::RESTORE);
                let node = step.data_leaf(StepData::EXPORT_ORDER);
                let stored = node
                    .and_then(om_data::scripts::node_bytes)
                    .and_then(|bytes| ExportOptions::decode(&bytes).ok());
                rows.push(row(
                    restore_label(id),
                    Edit::Export {
                        restore,
                        was: restore,
                        editable: node.is_none() || stored.is_some(),
                        stored,
                        edited: None,
                    },
                ));
            }
            // Edited through their Restore rows.
            ParamKind::SortOrder | ParamKind::FindRequests
                if matches!(id, 39 | 22 | 28 | 126 | 127) => {}
            // Show Custom Dialog's inputs: each target (input 1 in
            // `mReserved`, inputs 2-3 at `[00 1, 10]` and `[00 2, 10]`, a
            // variable's name at `0x20 + n - 1`), its label (the calculation
            // in slot n + 1) and "Use password character" (`[00 n - 1, 03]`),
            // script-steps.md, Show Custom Dialog (87) inputs.
            ParamKind::Field | ParamKind::Target if id == 87 && p.name == "Input fields" => {
                for slot in 0..3u16 {
                    let n = slot + 1;
                    // FileMaker Pro stores an unused input as the empty
                    // key (table 0, field 0): no input, kept as stored.
                    let value = field_target(step, slot).filter(|t| {
                        !matches!(
                            t,
                            FieldTarget::Field {
                                occurrence: 0,
                                field: 0,
                                ..
                            }
                        )
                    });
                    rows.push(row(
                        format!("Input {n}"),
                        Edit::Field {
                            slot,
                            target: true,
                            was: value.clone(),
                            value,
                        },
                    ));
                    let text = calc_text(slot + 2).unwrap_or_default();
                    rows.push(row(
                        format!("Input {n} label"),
                        Edit::Calculation {
                            slot: slot + 2,
                            flags: 0,
                            was: text.clone(),
                            text,
                            specified: None,
                        },
                    ));
                    let on = step.number(slot).is_some_and(|c| c != 0);
                    rows.push(row(
                        format!("Input {n} uses password character"),
                        Edit::Flag {
                            location: Location::Slot {
                                slot,
                                kind: SlotKind::Number,
                            },
                            on,
                            was: on,
                        },
                    ));
                }
            }
            // Edited with the inputs.
            ParamKind::Calculation if id == 87 && p.name == "Input labels" => {}
            ParamKind::Flag if id == 87 && p.name == "Password" => {}
            // Edited with the buttons' labels.
            ParamKind::Flag if id == 87 && p.name == "Button commit" => {}
            ParamKind::Flag => match p.storage.as_slice() {
                [location @ (Location::Flag { .. }
                | Location::Slot {
                    kind: SlotKind::Number,
                    ..
                })] => {
                    let on = match param.value() {
                        Some(ParamValue::Bool(b)) => b,
                        _ => p.default == "On",
                    };
                    let label = if p.name == "Restore" {
                        restore_label(id).to_owned()
                    } else {
                        p.name.clone()
                    };
                    rows.push(row(
                        label,
                        Edit::Flag {
                            location: *location,
                            on,
                            was: on,
                        },
                    ));
                }
                // Allow User Abort's and Set Error Capture's State: On is
                // kTurnOn; kSetOnOff, which some files store beside it, is
                // kept as stored (script-steps.md, "Implementation notes
                // (Script Workspace)").
                [turn_on @ Location::Flag {
                    bits: flag::TURN_ON,
                    inverted: false,
                }, Location::Flag {
                    bits: flag::SET_ON_OFF,
                    ..
                }] => {
                    let on = step.has_flag(flag::TURN_ON);
                    rows.push(row(
                        p.name.clone(),
                        Edit::Flag {
                            location: *turn_on,
                            on,
                            was: on,
                        },
                    ));
                }
                _ => rows.extend(kept),
            },
            // Go to Related Record's options (script-steps.md 11.8): "Use
            // external table's layouts" (`kOption`), "Show in new window"
            // (`kTurnOn`) with the window's name (slot 0), size and position
            // (slots 1-4) and style word (`[00 0, 03]`, as New Window), and
            // "Show only related records" (`kRestore`) with "Match all
            // records in current found set" (`kOption2`).
            // Replace Field Contents' "Replace with" (11.8): `[00 1, 03]` 1
            // current contents (also when absent), 2 serial numbers with
            // the initial value text `[00 3, 01]`, the increment `[00 2,
            // 03]` and "Update serial number in Entry Options" (one byte at
            // `[00 4, 03]`), 3 a calculated result (slot 0). Another stored
            // choice, or "Use Entry Options" (`[00 5, 03]` set: the field's
            // own serial settings, which these rows do not offer and which
            // would override them), keeps the parameter as stored.
            ParamKind::Compound if replaces(id) && p.name == "Replace with" => match step.number(1)
            {
                Some(n) if !(0..=3).contains(&n) => rows.extend(kept),
                _ if step.number(5).is_some_and(|n| n != 0) => rows.extend(kept),
                stored => {
                    let shown = match stored {
                        Some(2) => 2,
                        Some(3) => 3,
                        _ => 1,
                    };
                    let with = rows.len();
                    rows.push(row(
                        "Replace with",
                        Edit::Choice {
                            name: p.name.clone(),
                            location: Location::Slot {
                                slot: 1,
                                kind: SlotKind::Number,
                            },
                            flags: 0,
                            choices: vec![
                                (1, "Current contents".into()),
                                (2, "Serial numbers".into()),
                                (3, "Calculated result".into()),
                            ],
                            value: Some(shown),
                            was: Some(shown),
                        },
                    ));
                    let shown_with = |values: &'static [u32], label: &str, edit: Edit| Row {
                        when: Some((with, values)),
                        ..row(label, edit)
                    };
                    // FileMaker's dialog starts both at 1, as the runtime
                    // reads them when absent.
                    let initial = step.text(3).unwrap_or_else(|| "1".into());
                    rows.push(shown_with(
                        &[2],
                        "Initial value",
                        Edit::Text {
                            slot: 3,
                            was: initial.clone(),
                            text: initial,
                        },
                    ));
                    let increment = step.number(2).unwrap_or(1).to_string();
                    rows.push(shown_with(
                        &[2],
                        "Increment by",
                        Edit::Number {
                            slot: 2,
                            was: increment.clone(),
                            text: increment,
                        },
                    ));
                    let update = step.number(4).is_some_and(|n| n != 0);
                    rows.push(shown_with(
                        &[2],
                        "Update serial number in Entry Options",
                        Edit::Flag {
                            location: Location::Slot {
                                slot: 4,
                                kind: SlotKind::Number,
                            },
                            on: update,
                            was: update,
                        },
                    ));
                    let text = calc_text(0).unwrap_or_default();
                    rows.push(shown_with(
                        &[3],
                        "Calculated result",
                        Edit::Calculation {
                            slot: 0,
                            flags: 0,
                            was: text.clone(),
                            text,
                            specified: None,
                        },
                    ));
                    // FileMaker Pro 26.0.3's "Perform auto-enter options for
                    // fields", checked with nothing stored: the XML reader's
                    // "Skip auto-enter options" is kRestore with [00 6, 03]
                    // = 0 (11.8).
                    let perform = !(step.has_flag(flag::RESTORE) && step.number(6) == Some(0));
                    rows.push(row(
                        "Perform auto-enter options for fields",
                        Edit::Flag {
                            location: Location::Slot {
                                slot: 6,
                                kind: SlotKind::Number,
                            },
                            on: perform,
                            was: perform,
                        },
                    ));
                }
            },
            // Send Mail by the email client (script-steps.md 12.3): "One
            // email per record" (`[00 3, 03]`), To, CC and BCC (calculations
            // 0-2), each with "Collect addresses across found set"
            // (`[00 0..2, 03]`, read only without one email per record),
            // Subject (3), Message (4) and the attachments' path list
            // (`[00 0, 82]` with `kFilePath`). A step that sends by SMTP
            // (`[00 4, 03]`) or OAuth (`[00 7, 03]`) keeps the parameter as
            // stored: those options have no rows yet.
            ParamKind::Compound if id == 63 && p.name == "Send Mail" => {
                let server = [4, 7]
                    .iter()
                    .any(|&slot| step.number(slot).is_some_and(|n| n != 0));
                if server {
                    rows.extend(kept);
                } else {
                    let check = |label: &str, slot: u16| {
                        let on = step.number(slot).is_some_and(|n| n != 0);
                        row(
                            label,
                            Edit::Flag {
                                location: Location::Slot {
                                    slot,
                                    kind: SlotKind::Number,
                                },
                                on,
                                was: on,
                            },
                        )
                    };
                    let calc = |label: &str, slot: u16| {
                        let text = calc_text(slot).unwrap_or_default();
                        row(
                            label,
                            Edit::Calculation {
                                slot,
                                flags: 0,
                                was: text.clone(),
                                text,
                                specified: None,
                            },
                        )
                    };
                    let multiple = rows.len();
                    rows.push(check("One email per record", 3));
                    for (slot, label, collect) in [
                        (0, "To", "Collect To addresses across found set"),
                        (1, "CC", "Collect CC addresses across found set"),
                        (2, "BCC", "Collect BCC addresses across found set"),
                    ] {
                        rows.push(calc(label, slot));
                        rows.push(Row {
                            when: Some((multiple, &[0])),
                            ..check(collect, slot)
                        });
                    }
                    rows.push(calc("Subject", 3));
                    rows.push(calc("Message", 4));
                    let text = step.path_list().unwrap_or_default().replace('\r', "\n");
                    rows.push(row(
                        "Attachments",
                        Edit::Paths {
                            was: text.clone(),
                            text,
                        },
                    ));
                }
            }
            // The records a save step takes (script-steps.md 11.7): 1 the
            // records being browsed (also when nothing is stored), 2 the
            // current record. Save Records as PDF keeps it in the step
            // value, Save Records as Excel in slot 1 (two bytes), which
            // FileMaker and the runtime apply only with Restore, so its row
            // shows only then. Another stored choice stays as stored, and so
            // do the PDF and Excel options this row does not cover. Print's
            // records have a row of their own (#843).
            ParamKind::Compound
                if matches!(
                    (id, p.name.as_str()),
                    (144, "PDF options") | (143, "Excel options")
                ) =>
            {
                let (stored, location) = match id {
                    143 => (
                        step.number(1).unwrap_or(0),
                        Location::Slot {
                            slot: 1,
                            kind: SlotKind::Number,
                        },
                    ),
                    _ => (i64::from(step.value), Location::Value),
                };
                if (0..=2).contains(&stored) {
                    let shown = if stored == 2 { 2 } else { 1 };
                    let restore = rows.iter().position(|r| {
                        matches!(
                            r.edit,
                            Edit::Flag {
                                location: Location::Flag {
                                    bits: flag::RESTORE,
                                    inverted: false,
                                },
                                ..
                            }
                        )
                    });
                    rows.push(Row {
                        when: (id == 143)
                            .then_some(restore)
                            .flatten()
                            .map(|r| (r, &[1][..])),
                        ..row(
                            "Records",
                            Edit::Choice {
                                name: p.name.clone(),
                                location,
                                flags: 0,
                                choices: vec![
                                    (1, "Records being browsed".into()),
                                    (2, "Current record".into()),
                                ],
                                value: Some(shown),
                                was: Some(shown),
                            },
                        )
                    });
                    // Options beside the records, which stay as stored and
                    // show as such: Excel's slots 0-5 other than the
                    // records, its export order and profile (85, 86,
                    // kExtendedData); the PDF's numeric and calculation
                    // slots (11.7: options in slots 0-10, calculations at
                    // their XML positions) and 83 with its flag.
                    let data =
                        |slot: u16| step.number(slot).is_some() || step.calculation(slot).is_some();
                    let others = match id {
                        143 => {
                            (0..=5).any(|slot| {
                                step.calculation(slot).is_some()
                                    || slot != 1 && step.number(slot).is_some()
                            }) || step.data_leaf(0x85).is_some()
                                || step.data_leaf(0x86).is_some()
                                || step.has_flag(flag::EXTENDED_DATA)
                        }
                        144 => {
                            (0..32).any(data)
                                || step.data_leaf(0x83).is_some()
                                || step.has_flag(flag::TURN_ON)
                        }
                        _ => false,
                    };
                    if others {
                        rows.extend(kept);
                    }
                } else {
                    rows.extend(kept);
                }
            }
            // Print's records (script-steps.md 11.6): 1 the records being
            // browsed (also when nothing is stored), 2 the current record,
            // shown while the print options are specified, as FileMaker Pro
            // 26 applies the choice only then (#843). Its blank-record
            // variants stay as stored.
            ParamKind::Choice if id == 43 && p.name == "Print" => {
                if step.value <= 2 {
                    let shown = if step.value == 2 { 2 } else { 1 };
                    let restore = rows.iter().position(|r| {
                        matches!(
                            r.edit,
                            Edit::Flag {
                                location: Location::Flag {
                                    bits: flag::RESTORE,
                                    inverted: false,
                                },
                                ..
                            }
                        )
                    });
                    rows.push(Row {
                        when: restore.map(|r| (r, &[1][..])),
                        ..row(
                            "Records",
                            Edit::Choice {
                                name: p.name.clone(),
                                location: Location::Key(0x88),
                                flags: 0,
                                choices: vec![
                                    (1, "Records being browsed".into()),
                                    (2, "Current record".into()),
                                ],
                                value: Some(shown),
                                was: Some(shown),
                            },
                        )
                    });
                } else {
                    rows.extend(kept);
                }
            }
            ParamKind::Choice if id == 74 && p.name == "Options" => {
                let check = |label: &str, bits: u32| {
                    let on = step.has_flag(bits);
                    row(
                        label,
                        Edit::Flag {
                            location: Location::Flag {
                                bits,
                                inverted: false,
                            },
                            on,
                            was: on,
                        },
                    )
                };
                rows.push(check("Use external table's layouts", flag::OPTION));
                let new_window = rows.len();
                rows.push(check("Show in new window", flag::TURN_ON));
                let window = |label: &str, edit: Edit| Row {
                    when: Some((new_window, &[1])),
                    ..row(label, edit)
                };
                for (slot, label) in [
                    (0, "Window Name"),
                    (1, "Height"),
                    (2, "Width"),
                    (3, "Top"),
                    (4, "Left"),
                ] {
                    let text = calc_text(slot).unwrap_or_default();
                    rows.push(window(
                        label,
                        Edit::Calculation {
                            slot,
                            flags: 0,
                            was: text.clone(),
                            text,
                            specified: None,
                        },
                    ));
                }
                let stored = step.number(0).map(|word| word as u32);
                if stored.is_none_or(|word| WINDOW_STYLES.iter().any(|(w, _)| *w == word)) {
                    let word = stored.unwrap_or(WINDOW_STYLES[0].0);
                    rows.push(window(
                        "Window Style",
                        Edit::WindowStyle {
                            value: word,
                            was: word,
                        },
                    ));
                }
                let only = rows.len();
                rows.push(check("Show only related records", flag::RESTORE));
                rows.push(Row {
                    when: Some((only, &[1])),
                    ..check("Match all records in current found set", flag::OPTION2)
                });
            }
            // New Window's style: the word at `[00 0, 03]`, one of the words
            // FileMaker is observed to write (script-steps.md, New Window
            // style word); another stays as stored.
            ParamKind::Choice if id == 122 && p.name == "Window style and options" => {
                let stored = step.number(0).map(|word| word as u32);
                match stored {
                    Some(word) if !WINDOW_STYLES.iter().any(|(w, _)| *w == word) => {
                        rows.extend(kept)
                    }
                    _ => {
                        let word = stored.unwrap_or(WINDOW_STYLES[0].0);
                        rows.push(row(
                            "Window Style",
                            Edit::WindowStyle {
                                value: word,
                                was: word,
                            },
                        ));
                    }
                }
            }
            ParamKind::Choice => {
                let choices = choices_for(id, p);
                let location = p.storage.iter().copied().find(|l| {
                    matches!(
                        l,
                        Location::Value
                            | Location::Slot {
                                kind: SlotKind::List,
                                ..
                            }
                    )
                });
                match location {
                    Some(location) if !choices.is_empty() => {
                        let value = match param.value() {
                            Some(ParamValue::Choice(v))
                                if v != 0 || choices.iter().any(|(n, _)| *n == 0) =>
                            {
                                Some(v)
                            }
                            // No animation stored is None, the first item
                            // of FileMaker's pop-up; shown, not written
                            // (#785).
                            None if p.name == "Animation" => Some(0),
                            _ => None,
                        };
                        // Pause/Resume Script with a duration stored at
                        // choice 0 pauses for it, as the runtime and the
                        // step text read it; shown so, not written.
                        let value = match value {
                            Some(0) if id == 62 && step.calculation(0).is_some() => Some(1),
                            // The account steps' options are on for any
                            // nonzero value (local-files.md 2.4): a stored
                            // 2 or 4 shows as the "on" choice, and an
                            // unchanged save keeps it.
                            Some(v) if matches!(id, 134 | 136 | 137) && v != 0 => Some(1),
                            other => other,
                        };
                        rows.push(row(
                            label,
                            Edit::Choice {
                                name: p.name.clone(),
                                location,
                                flags: companion_flags(p),
                                choices,
                                value,
                                was: value,
                            },
                        ));
                    }
                    _ => rows.extend(kept),
                }
            }
            // New Window's and Move/Resize Window's size and position: four
            // calculations in slots 1-4 (Claris's New Window Options and
            // Move/Resize Window Options: Height and Width, Top and Left).
            ParamKind::Calculation if matches!(id, 119 | 122) && p.name == "Bounds" => {
                for (slot, label) in [(1, "Height"), (2, "Width"), (3, "Top"), (4, "Left")] {
                    let text = calc_text(slot).unwrap_or_default();
                    rows.push(row(
                        label,
                        Edit::Calculation {
                            slot,
                            flags: 0,
                            was: text.clone(),
                            text,
                            specified: None,
                        },
                    ));
                }
            }
            ParamKind::Calculation => match slot_of(p, SlotKind::Calculation) {
                Some(slot) if slots_of(p, SlotKind::Calculation) == 1 => {
                    // FileMaker shows the default repetition (35b).
                    let text = calc_text(slot).unwrap_or_else(|| {
                        if p.name == "Repetition" {
                            "1".into()
                        } else {
                            String::new()
                        }
                    });
                    rows.push(row(
                        label,
                        Edit::Calculation {
                            slot,
                            flags: companion_flags(p),
                            was: text.clone(),
                            text,
                            specified: None,
                        },
                    ));
                }
                _ => rows.extend(kept),
            },
            // Show Custom Dialog's buttons: each label (text in slots 0-2)
            // with its "Commit data" (one byte at `[00 3..5, 03]`,
            // script-steps.md, Show Custom Dialog inputs).
            ParamKind::Text if id == 87 && p.name == "Button labels" => {
                for (n, label) in ["Default Button", "Button 2", "Button 3"]
                    .iter()
                    .enumerate()
                {
                    let slot = n as u16;
                    if step.calculation(5 + slot).is_some() {
                        // A calculated label (slots 5-7) is what the dialog
                        // shows: a typed label would not be.
                        rows.push(row(*label, Edit::Kept));
                    } else {
                        let text = step.text(slot).unwrap_or_default();
                        rows.push(row(
                            *label,
                            Edit::Text {
                                slot,
                                was: text.clone(),
                                text,
                            },
                        ));
                    }
                    // As the runtime reads it: without a stored byte the
                    // default button commits, the others do not.
                    let on = step.number(3 + slot).map_or(n == 0, |c| c != 0);
                    rows.push(row(
                        format!("{label} commits data"),
                        Edit::Flag {
                            location: Location::Slot {
                                slot: 3 + slot,
                                kind: SlotKind::Number,
                            },
                            on,
                            was: on,
                        },
                    ));
                }
            }
            ParamKind::Text => {
                let slots: Vec<u16> = p
                    .storage
                    .iter()
                    .filter_map(|l| match *l {
                        Location::Slot {
                            slot,
                            kind: SlotKind::Text,
                        } if slot < VARIABLE_NAME_SLOT => Some(slot),
                        _ => None,
                    })
                    .collect();
                // Insert Text's literal is in slot 1 or 0 (as its step text
                // reads it); other steps have one text slot.
                let slot = match (id, slots.as_slice()) {
                    (_, [slot]) => Some(*slot),
                    (61, _) => slots
                        .iter()
                        .rev()
                        .copied()
                        .find(|s| step.text(*s).is_some())
                        .or_else(|| slots.first().copied()),
                    _ => None,
                };
                match slot {
                    Some(slot) => {
                        let text = step.text(slot).unwrap_or_default();
                        rows.push(row(
                            label,
                            Edit::Text {
                                slot,
                                was: text.clone(),
                                text,
                            },
                        ));
                    }
                    None => rows.extend(kept),
                }
            }
            ParamKind::Number => match p.storage.as_slice() {
                [Location::Slot {
                    slot,
                    kind: SlotKind::Number,
                }] => {
                    let text = step
                        .number(*slot)
                        .map(|n| n.to_string())
                        .unwrap_or_default();
                    rows.push(row(
                        label,
                        Edit::Number {
                            slot: *slot,
                            was: text.clone(),
                            text,
                        },
                    ));
                }
                _ => rows.extend(kept),
            },
            ParamKind::Field | ParamKind::Target => {
                let specs: Vec<u16> = p
                    .storage
                    .iter()
                    .filter_map(|l| match *l {
                        Location::Reserved(RefKind::Field) => Some(0),
                        Location::Slot {
                            slot,
                            kind: SlotKind::Field,
                        } => Some(slot),
                        _ => None,
                    })
                    .collect();
                match specs.as_slice() {
                    [slot] => {
                        let value = field_target(step, *slot);
                        rows.push(row(
                            label,
                            Edit::Field {
                                slot: *slot,
                                target: p.kind == ParamKind::Target,
                                was: value.clone(),
                                value,
                            },
                        ));
                    }
                    _ => rows.extend(kept),
                }
            }
            // Go to Related Record by a layout name or number calculation
            // (step value 3 or 4, inferred from Go to Layout; 11.8): kept.
            ParamKind::Layout if id == 74 && matches!(step.value, 3 | 4) => rows.extend(kept),
            ParamKind::Layout if p.storage.contains(&Location::Reserved(RefKind::Member)) => {
                // Go to Related Record's step value 1 is <Current Layout>,
                // whatever key a step may still hold.
                let value = match (id, step.value) {
                    (74, 1) => None,
                    _ => step.member_key().and_then(|k| k.id()),
                };
                // Go to Layout's "Layout" pop-up comes first: its rows say
                // which layout it names.
                let label = if id == 6 {
                    "Specified Layout"
                } else {
                    "Layout"
                };
                rows.push(row(
                    label,
                    Edit::Layout {
                        value,
                        was: value,
                        picked: false,
                    },
                ));
                if id == 6 {
                    // Go to Layout's name or number calculation (slot 5).
                    let text = calc_text(5).unwrap_or_default();
                    rows.push(row(
                        "Calculation",
                        Edit::Calculation {
                            slot: 5,
                            flags: 0,
                            was: text.clone(),
                            text,
                            specified: None,
                        },
                    ));
                }
            }
            ParamKind::Script if p.storage.contains(&Location::Reserved(RefKind::Script)) => {
                let value = step.script_id();
                if !matches!(id, 1 | 164) {
                    rows.push(row("Script", Edit::Script { value, was: value }));
                } else {
                    // Perform Script: from the list or by a name calculation.
                    let legacy_name =
                        id == 164 && step.script_key().is_none() && step.calculation(1).is_some();
                    let by_name = step.list_value(1) == Some(1) || legacy_name;
                    let by = rows.len();
                    rows.push(row(
                        "Specified",
                        Edit::ScriptBy {
                            by_name,
                            was: by_name,
                        },
                    ));
                    rows.push(Row {
                        when: Some((by, &[0])),
                        ..row("Script", Edit::Script { value, was: value })
                    });
                    if let Some(name) = schema
                        .params
                        .iter()
                        .position(|q| q.name == "Specify by name")
                    {
                        let text = calc_text(2)
                            .or_else(|| legacy_name.then(|| calc_text(1)).flatten())
                            .unwrap_or_default();
                        rows.push(Row {
                            when: Some((by, &[1])),
                            ..row(
                                "Script Name",
                                Edit::Calculation {
                                    slot: 2,
                                    flags: BY_NAME_FLAGS,
                                    was: text.clone(),
                                    text,
                                    specified: None,
                                },
                            )
                        });
                        skip.push(name);
                    }
                }
            }
            // The window: current, or by a name calculation in slot 0.
            ParamKind::Window
                if p.storage.contains(&Location::Value)
                    && p.storage.contains(&Location::Slot {
                        slot: 0,
                        kind: SlotKind::Calculation,
                    }) =>
            {
                let by_name = step.value != 0;
                let by = rows.len();
                rows.push(row(
                    "Window",
                    Edit::Window {
                        by_name,
                        was: by_name,
                    },
                ));
                let text = calc_text(0).unwrap_or_default();
                rows.push(Row {
                    when: Some((by, &[1])),
                    ..row(
                        "Window Name",
                        Edit::Calculation {
                            slot: 0,
                            flags: 0,
                            was: text.clone(),
                            text,
                            specified: None,
                        },
                    )
                });
            }
            // A path list, unless the step names a data source instead
            // (`20`, `kFileReference`), which stays as stored.
            ParamKind::FileReference
                if (p.name == "File paths"
                    || id == 35 && p.name == "Data source" && step.value <= 1)
                    && p.storage.contains(&Location::Key(StepData::PATH_STRING))
                    && !step.has_flag(flag::FILE_REFERENCE) =>
            {
                let text = step.path_list().unwrap_or_default().replace('\r', "\n");
                rows.push(row(
                    label,
                    Edit::Paths {
                        was: text.clone(),
                        text,
                    },
                ));
            }
            ParamKind::TableOccurrence => {
                if p.storage.contains(&Location::Reserved(RefKind::Member)) {
                    // Truncate Table: a base table key (corpus).
                    let value = step.member_key().and_then(|k| k.id());
                    rows.push(row(
                        label,
                        Edit::Table {
                            aliases: None,
                            value,
                            was: value,
                        },
                    ));
                } else {
                    match slot_of(p, SlotKind::Aliases).map(|s| (s, aliases_occurrence(step, s))) {
                        Some((slot, Ok(value))) => rows.push(row(
                            label,
                            Edit::Table {
                                aliases: Some(slot),
                                value,
                                was: value,
                            },
                        )),
                        _ => rows.extend(kept),
                    }
                }
            }
            _ => rows.extend(kept),
        }
        for row in &mut rows[first..] {
            if row.param.is_empty() {
                row.param = p.name.clone();
            }
        }
    }
    // Rows shown only with some values of the step's choice.
    let choice = rows.iter().position(|r| {
        matches!(
            r.edit,
            Edit::Choice {
                location: Location::Value,
                ..
            }
        )
    });
    if let Some(choice) = choice {
        for row in &mut rows {
            if row.when.is_some() {
                continue;
            }
            let values: Option<&'static [u32]> = match (id, row.param.as_str(), &row.edit) {
                (6, "Layout", Edit::Layout { .. }) => Some(&[0, 5]),
                (6, "Layout", Edit::Calculation { .. }) => Some(&[3, 4]),
                (16 | 99, "Number" | "Row number", _) => Some(&[5, 6]),
                (16 | 99, "Exit after last", _) => Some(&[3, 4]),
                (16 | 99, "With dialog", _) => Some(&[0, 5, 6]),
                (62, "Duration (seconds)", _) => Some(&[1]),
                _ => None,
            };
            row.when = values.map(|v| (choice, v));
        }
    }
    rows
}

#[cfg(test)]
mod tests;

/// Shared by the options dialog and inline/Specify Calculation edits.
pub(super) fn set_calculation(
    step: &mut ScriptStep,
    slot: u16,
    flags: u32,
    calc: Option<&om_data::Calculation>,
) {
    step.set_calculation(slot, calc);
    if calc.is_some() && slot == 2 && matches!(step.id(), 1 | 164) {
        step.set_list_value(1, Some(1));
    }
    if flags != 0 {
        step.set_flag(flags, calc.is_some());
    }
}
